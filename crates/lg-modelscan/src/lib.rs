//! Identify model files by reading only their headers.
//!
//! safetensors: an 8-byte little-endian length, then a JSON table of tensors.
//! GGUF: magic, version, metadata key/values, then tensor infos.
//! Architecture signatures follow ComfyUI's `comfy/model_detection.py`.

use lg_types::Precision;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a model file: {0}")]
    Format(String),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TensorInfo {
    pub dtype: String,
    pub shape: Vec<u64>,
    /// (start, end) byte range inside the data section (safetensors only).
    #[serde(skip)]
    pub range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub size_bytes: u64,
    pub format: String,
    /// What the file is: checkpoint, diffusion, lora, vae, text_encoder, controlnet, upscaler, unknown.
    pub kind: String,
    /// Architecture, e.g. "sdxl", "flux", "flux2", "wan2.1", "qwen_image", "t5", "clip_l".
    pub arch: Option<String>,
    pub precision: Option<Precision>,
    pub tensor_count: usize,
    pub parameters: u64,
    pub metadata: BTreeMap<String, String>,
}

/// Read a model file's header and identify it.
pub fn inspect(path: &Path) -> Result<ModelInfo, ScanError> {
    let size_bytes = std::fs::metadata(path)?.len();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (format, tensors, metadata, quant) = match ext.as_str() {
        "safetensors" | "sft" => {
            let (t, m, q) = read_safetensors(path)?;
            ("safetensors".to_string(), t, m, q)
        }
        "gguf" => {
            let (t, m) = read_gguf(path)?;
            ("gguf".to_string(), t, m, None)
        }
        "ckpt" | "pt" | "pth" | "bin" => {
            // Pickled files cannot be read safely without unpickling; report what we know.
            return Ok(ModelInfo {
                path: path.to_path_buf(),
                file_name: file_name(path),
                size_bytes,
                format: ext,
                kind: "unknown".into(),
                arch: None,
                precision: None,
                tensor_count: 0,
                parameters: 0,
                metadata: BTreeMap::new(),
            });
        }
        _ => return Err(ScanError::Format(format!("unsupported extension .{ext}"))),
    };
    let (kind, arch) = classify(&tensors);
    let precision = quant.or_else(|| dominant_precision(&tensors, &format));
    let parameters = tensors
        .values()
        .map(|t| t.shape.iter().product::<u64>().max(1))
        .sum();
    Ok(ModelInfo {
        path: path.to_path_buf(),
        file_name: file_name(path),
        size_bytes,
        format,
        kind,
        arch,
        precision,
        tensor_count: tensors.len(),
        parameters,
        metadata,
    })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

type GgufHeader = (BTreeMap<String, TensorInfo>, BTreeMap<String, String>);
type Header = (
    BTreeMap<String, TensorInfo>,
    BTreeMap<String, String>,
    Option<Precision>,
);

fn read_safetensors(path: &Path) -> Result<Header, ScanError> {
    let mut f = File::open(path)?;
    let mut len = [0u8; 8];
    f.read_exact(&mut len)?;
    let n = u64::from_le_bytes(len);
    if n == 0 || n > 200 * 1024 * 1024 {
        return Err(ScanError::Format("bad safetensors header length".into()));
    }
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).map_err(|e| ScanError::Format(e.to_string()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| ScanError::Format("header is not an object".into()))?;
    let mut tensors = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    for (k, t) in obj {
        if k == "__metadata__" {
            for (mk, mv) in t.as_object().into_iter().flatten() {
                let s = mv
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| mv.to_string());
                // Metadata can embed whole workflows; keep it small.
                metadata.insert(mk.clone(), s.chars().take(2000).collect());
            }
            continue;
        }
        let range = t["data_offsets"]
            .as_array()
            .and_then(|a| Some((a.first()?.as_u64()?, a.get(1)?.as_u64()?)));
        tensors.insert(
            k.clone(),
            TensorInfo {
                dtype: t["dtype"].as_str().unwrap_or_default().to_string(),
                shape: t["shape"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default(),
                range,
            },
        );
    }
    // Comfy quantization stores `{"format": ...}` per layer in a small uint8 tensor.
    let data_start = 8 + n;
    let quant = tensors
        .iter()
        .find(|(k, t)| {
            k.ends_with(".comfy_quant") && t.range.is_some_and(|(a, b)| b > a && b - a < 4096)
        })
        .and_then(|(_, t)| {
            let (a, b) = t.range?;
            f.seek(SeekFrom::Start(data_start + a)).ok()?;
            let mut j = vec![0u8; (b - a) as usize];
            f.read_exact(&mut j).ok()?;
            let v: Value = serde_json::from_slice(&j).ok()?;
            quant_format(v["format"].as_str()?)
        });
    Ok((tensors, metadata, quant))
}

fn quant_format(f: &str) -> Option<Precision> {
    Some(match f {
        "nvfp4" => Precision::Nvfp4,
        "float8_e4m3fn" | "float8_e5m2" | "mxfp8" => Precision::Fp8,
        "int8_tensorwise" => Precision::Fp8,
        "convrot_w4a4" => Precision::Q4,
        _ => return None,
    })
}

// ---- GGUF ----

fn rd<const N: usize>(r: &mut impl Read) -> Result<[u8; N], ScanError> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn u32le(r: &mut impl Read) -> Result<u32, ScanError> {
    Ok(u32::from_le_bytes(rd::<4>(r)?))
}
fn u64le(r: &mut impl Read) -> Result<u64, ScanError> {
    Ok(u64::from_le_bytes(rd::<8>(r)?))
}
fn gguf_str(r: &mut impl Read) -> Result<String, ScanError> {
    let n = u64le(r)?;
    if n > 1 << 20 {
        return Err(ScanError::Format("gguf string too long".into()));
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// Read one GGUF value of type `t`; arrays are summarised.
fn gguf_value(r: &mut impl Read, t: u32) -> Result<String, ScanError> {
    Ok(match t {
        0 => (rd::<1>(r)?[0]).to_string(),
        1 => (rd::<1>(r)?[0] as i8).to_string(),
        2 => u16::from_le_bytes(rd::<2>(r)?).to_string(),
        3 => i16::from_le_bytes(rd::<2>(r)?).to_string(),
        4 => u32le(r)?.to_string(),
        5 => (u32le(r)? as i32).to_string(),
        6 => f32::from_le_bytes(rd::<4>(r)?).to_string(),
        7 => (rd::<1>(r)?[0] != 0).to_string(),
        8 => gguf_str(r)?,
        9 => {
            let et = u32le(r)?;
            let n = u64le(r)?;
            if n > 50_000_000 {
                return Err(ScanError::Format("gguf array too long".into()));
            }
            for _ in 0..n {
                gguf_value(r, et)?;
            }
            format!("[{n} items]")
        }
        10 => u64le(r)?.to_string(),
        11 => (u64le(r)? as i64).to_string(),
        12 => f64::from_le_bytes(rd::<8>(r)?).to_string(),
        _ => return Err(ScanError::Format(format!("unknown gguf type {t}"))),
    })
}

/// ggml tensor type id -> name.
fn ggml_type(t: u32) -> &'static str {
    match t {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        6 => "Q5_0",
        7 => "Q5_1",
        8 => "Q8_0",
        9 => "Q8_1",
        10 => "Q2_K",
        11 => "Q3_K",
        12 => "Q4_K",
        13 => "Q5_K",
        14 => "Q6_K",
        15 => "Q8_K",
        30 => "BF16",
        _ => "Q_OTHER",
    }
}

fn read_gguf(path: &Path) -> Result<GgufHeader, ScanError> {
    let mut r = BufReader::new(File::open(path)?);
    if &rd::<4>(&mut r)? != b"GGUF" {
        return Err(ScanError::Format("missing GGUF magic".into()));
    }
    let version = u32le(&mut r)?;
    if version < 2 {
        return Err(ScanError::Format(format!("GGUF v{version} is too old")));
    }
    let n_tensors = u64le(&mut r)?;
    let n_kv = u64le(&mut r)?;
    if n_tensors > 1_000_000 || n_kv > 1_000_000 {
        return Err(ScanError::Format("implausible GGUF counts".into()));
    }
    let mut metadata = BTreeMap::new();
    for _ in 0..n_kv {
        let k = gguf_str(&mut r)?;
        let t = u32le(&mut r)?;
        let v = gguf_value(&mut r, t)?;
        metadata.insert(k, v);
    }
    let mut tensors = BTreeMap::new();
    for _ in 0..n_tensors {
        let name = gguf_str(&mut r)?;
        let dims = u32le(&mut r)?;
        let mut shape = Vec::new();
        for _ in 0..dims.min(8) {
            shape.push(u64le(&mut r)?);
        }
        // GGUF lists dimensions innermost first.
        shape.reverse();
        let ty = u32le(&mut r)?;
        let _offset = u64le(&mut r)?;
        tensors.insert(
            name,
            TensorInfo {
                dtype: ggml_type(ty).to_string(),
                shape,
                range: None,
            },
        );
    }
    Ok((tensors, metadata))
}

// ---- classification ----

const PREFIXES: &[&str] = &[
    "model.diffusion_model.",
    "diffusion_model.",
    "net.",
    "model.",
    "",
];

/// (architecture, keys that must all exist after the prefix). Order matters: first match wins.
const DIFFUSION: &[(&str, &[&str])] = &[
    (
        "flux3",
        &[
            "content_mode_blocks.image.0.attn_out.weight",
            "single_stream_modulations.txt.lin.weight",
        ],
    ),
    ("flux2", &["double_stream_modulation_img.lin.weight"]),
    ("chroma", &["distilled_guidance_layer.0.norms.0.scale"]),
    ("chroma", &["distilled_guidance_layer.norms.0.scale"]),
    (
        "flux",
        &[
            "double_blocks.0.img_attn.norm.key_norm.scale",
            "img_in.weight",
        ],
    ),
    (
        "flux",
        &[
            "double_blocks.0.img_attn.norm.key_norm.weight",
            "img_in.weight",
        ],
    ),
    (
        "hunyuan_video",
        &["txt_in.individual_token_refiner.blocks.0.norm1.weight"],
    ),
    ("sd3", &["joint_blocks.0.context_block.attn.qkv.weight"]),
    ("mochi", &["t5_yproj.weight"]),
    (
        "minimax_h3",
        &["video_patch_proj.weight", "audio_patch_proj.weight"],
    ),
    (
        "ltxav",
        &[
            "adaln_single.emb.timestep_embedder.linear_1.bias",
            "audio_adaln_single.linear.weight",
        ],
    ),
    (
        "ltxv",
        &["adaln_single.emb.timestep_embedder.linear_1.bias"],
    ),
    (
        "cosmos",
        &["blocks.block0.blocks.0.block.attn.to_q.0.weight"],
    ),
    (
        "lumina2",
        &[
            "cap_embedder.1.weight",
            "noise_refiner.0.attention.k_norm.weight",
        ],
    ),
    ("cogvideox", &["blocks.0.norm1.linear.weight"]),
    ("wan2.1", &["head.modulation"]),
    ("hunyuan3d2", &["latent_in.weight"]),
    ("hidream", &["caption_projection.0.linear.weight"]),
    ("anima", &["llm_adapter.blocks.0.cross_attn.q_proj.weight"]),
    ("cosmos_predict2", &["blocks.0.mlp.layer1.weight"]),
    (
        "omnigen2",
        &["time_caption_embed.timestep_embedder.linear_1.bias"],
    ),
    ("ideogram4", &["embed_image_indicator.weight"]),
    (
        "fibo2",
        &["perceiver.latent_queries", "layers.0.attention.qkv.weight"],
    ),
    (
        "moss_image",
        &[
            "shared_block_modulation.weight",
            "layers.0.proj.ffn_up_gate.weight",
        ],
    ),
    ("krea2", &["txtfusion.projector.weight"]),
    (
        "kandinsky5",
        &["visual_transformer_blocks.0.cross_attention.key_norm.weight"],
    ),
    ("qwen_image", &["txt_norm.weight", "img_in.weight"]),
    ("ernie", &["layers.0.mlp.linear_fc2.weight"]),
];

fn has(keys: &BTreeMap<String, TensorInfo>, k: &str) -> bool {
    keys.contains_key(k)
}

/// (kind, arch).
pub fn classify(t: &BTreeMap<String, TensorInfo>) -> (String, Option<String>) {
    let any = |pred: &dyn Fn(&str) -> bool| t.keys().any(|k| pred(k));
    // LoRAs: low-rank pairs, no full layers.
    let lora_like = any(&|k| {
        k.contains("lora_up")
            || k.contains("lora_down")
            || k.contains("lora_A")
            || k.contains("lora_B")
            || k.contains(".lora.")
    });
    if lora_like {
        let arch = if any(&|k| {
            k.contains("double_blocks")
                || k.contains("single_blocks")
                || k.contains("single_transformer_blocks")
        }) {
            "flux"
        } else if any(&|k| {
            k.contains("input_blocks") || k.contains("lora_te") || k.contains("down_blocks")
        }) {
            "sd"
        } else if any(&|k| k.contains("blocks.") && k.contains("cross_attn")) {
            "wan"
        } else if any(&|k| k.contains("transformer_blocks")) {
            "dit"
        } else {
            "unknown"
        };
        return ("lora".into(), Some(arch.into()));
    }
    // Classic UNet checkpoints.
    if has(t, "model.diffusion_model.input_blocks.0.0.weight") {
        let arch = if t.keys().any(|k| k.starts_with("conditioner.embedders.1."))
            || t.get("model.diffusion_model.label_emb.0.0.weight")
                .is_some_and(|x| x.shape.get(1) == Some(&2816))
        {
            "sdxl"
        } else if t.keys().any(|k| k.starts_with("cond_stage_model.model.")) {
            "sd2"
        } else {
            "sd15"
        };
        let full = t.keys().any(|k| k.starts_with("first_stage_model."));
        return (
            if full { "checkpoint" } else { "diffusion" }.into(),
            Some(arch.into()),
        );
    }
    if has(t, "input_blocks.0.0.weight") {
        let ctx = t
            .get("input_blocks.4.1.transformer_blocks.0.attn2.to_k.weight")
            .and_then(|x| x.shape.get(1).copied());
        let arch = match ctx {
            Some(2048) => "sdxl",
            Some(1024) => "sd2",
            _ => "sd15",
        };
        return ("diffusion".into(), Some(arch.into()));
    }
    for p in PREFIXES {
        for (arch, keys) in DIFFUSION {
            if keys.iter().all(|k| has(t, &format!("{p}{k}"))) {
                let full = t
                    .keys()
                    .any(|k| k.starts_with("first_stage_model.") || k.starts_with("vae."));
                let arch = if *arch == "flux" && has(t, &format!("{p}guidance_in.in_layer.weight"))
                {
                    "flux-dev"
                } else if *arch == "flux" {
                    "flux-schnell"
                } else {
                    arch
                };
                return (
                    if full { "checkpoint" } else { "diffusion" }.into(),
                    Some(arch.to_string()),
                );
            }
        }
    }
    // ControlNets and model patches.
    if any(&|k| {
        k.starts_with("control_model.")
            || k.starts_with("controlnet_")
            || k.contains("controlnet_blocks")
    }) {
        return ("controlnet".into(), None);
    }
    // VAEs.
    if has(t, "decoder.conv_in.weight") && has(t, "encoder.conv_in.weight")
        || has(t, "decoder.conv_in.weight") && t.len() < 400
    {
        let ch = t
            .get("decoder.conv_in.weight")
            .and_then(|x| x.shape.get(1).copied());
        let arch = match ch {
            Some(4) => "sd",
            Some(16) => "flux",
            Some(32) => "flux2",
            Some(c) => return ("vae".into(), Some(format!("{c}ch"))),
            None => "unknown",
        };
        return ("vae".into(), Some(arch.into()));
    }
    if any(&|k| k.starts_with("decoder.") && k.contains("conv"))
        && any(&|k| k.contains("time_conv") || k.contains("causal"))
    {
        return ("vae".into(), Some("video".into()));
    }
    // Text encoders.
    if any(&|k| k.contains("encoder.block.0.layer.0.SelfAttention.q.weight")) {
        return ("text_encoder".into(), Some("t5".into()));
    }
    if let Some(w) = t
        .iter()
        .find(|(k, _)| k.ends_with("text_model.encoder.layers.0.self_attn.q_proj.weight"))
        .map(|(_, v)| v)
    {
        let arch = match w.shape.first() {
            Some(768) => "clip_l",
            Some(1280) => "clip_g",
            Some(1024) => "clip_h",
            _ => "clip",
        };
        return ("text_encoder".into(), Some(arch.into()));
    }
    if any(&|k| {
        k.ends_with("layers.0.self_attn.q_proj.weight") || k.ends_with("blk.0.attn_q.weight")
    }) {
        return ("text_encoder".into(), Some("llm".into()));
    }
    // ESRGAN-style upscalers.
    if has(t, "conv_first.weight") || has(t, "model.0.weight") && t.len() < 1200 {
        return ("upscaler".into(), None);
    }
    ("unknown".into(), None)
}

fn dominant_precision(t: &BTreeMap<String, TensorInfo>, format: &str) -> Option<Precision> {
    let mut by: BTreeMap<&str, u64> = BTreeMap::new();
    for v in t.values() {
        *by.entry(v.dtype.as_str()).or_default() += v.shape.iter().product::<u64>().max(1);
    }
    let (dtype, _) = by.into_iter().max_by_key(|(_, n)| *n)?;
    Some(match (format, dtype) {
        (_, "F32") => Precision::Fp32,
        (_, "BF16") => Precision::Bf16,
        (_, "F16") => Precision::Fp16,
        ("safetensors", d) if d.starts_with("F8") => Precision::Fp8,
        ("safetensors", "U8") if t.keys().any(|k| k.ends_with("weight_scale_2")) => {
            Precision::Nvfp4
        }
        ("safetensors", "I8") => Precision::Fp8,
        ("gguf", d) if d.starts_with("Q8") => Precision::Q8,
        ("gguf", d) if d.starts_with("Q6") => Precision::Q6,
        ("gguf", d) if d.starts_with("Q5") => Precision::Q5,
        ("gguf", d) if d.starts_with("Q4") => Precision::Q4,
        ("gguf", d) if d.starts_with("Q3") || d.starts_with("Q2") => Precision::Q3,
        _ => return None,
    })
}

/// ComfyUI model folders this app manages, and the file kinds that live in each.
pub const FOLDERS: &[&str] = &[
    "checkpoints",
    "diffusion_models",
    "unet",
    "text_encoders",
    "clip",
    "vae",
    "loras",
    "controlnet",
    "upscale_models",
    "clip_vision",
    "model_patches",
    "latent_upscale_models",
    "embeddings",
    "vae_approx",
];

const MODEL_EXTS: &[&str] = &["safetensors", "sft", "gguf", "ckpt", "pt", "pth", "bin"];

/// Every model file under `root/<folder>/**`, as (folder, path relative to the folder).
pub fn list_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for folder in FOLDERS {
        let base = root.join(folder);
        walk(&base, &base, folder, &mut out, 0);
    }
    out.sort();
    out
}

fn walk(base: &Path, dir: &Path, folder: &str, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(base, &p, folder, out, depth + 1);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| MODEL_EXTS.contains(&x.to_ascii_lowercase().as_str()))
            && let Ok(rel) = p.strip_prefix(base)
        {
            out.push((folder.to_string(), rel.to_path_buf()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// A header-only safetensors file (tensor data is zero-length or tiny).
    pub fn write_safetensors(path: &Path, tensors: &[(&str, &str, &[u64])], quant: Option<&str>) {
        let mut h = serde_json::Map::new();
        let mut off = 0u64;
        let mut data = Vec::new();
        for (name, dtype, shape) in tensors {
            h.insert(
                name.to_string(),
                json!({"dtype": dtype, "shape": shape, "data_offsets": [off, off]}),
            );
        }
        if let Some(q) = quant {
            let j = format!("{{\"format\": \"{q}\"}}");
            h.insert("blocks.0.proj.comfy_quant".into(), json!({"dtype": "U8", "shape": [j.len()], "data_offsets": [off, off + j.len() as u64]}));
            data.extend_from_slice(j.as_bytes());
            off += j.len() as u64;
        }
        let _ = off;
        h.insert("__metadata__".into(), json!({"modelspec.title": "test"}));
        let hs = serde_json::to_vec(&h).unwrap();
        let mut f = File::create(path).unwrap();
        f.write_all(&(hs.len() as u64).to_le_bytes()).unwrap();
        f.write_all(&hs).unwrap();
        f.write_all(&data).unwrap();
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn identifies_architectures() {
        let d = tempfile::tempdir().unwrap();
        let cases: Vec<(&str, Vec<(&str, &str, &[u64])>, &str, &str)> = vec![
            (
                "sdxl.safetensors",
                vec![
                    (
                        "model.diffusion_model.input_blocks.0.0.weight",
                        "F16",
                        &[320, 4, 3, 3],
                    ),
                    (
                        "conditioner.embedders.1.model.ln_final.weight",
                        "F16",
                        &[1280],
                    ),
                    (
                        "first_stage_model.decoder.conv_in.weight",
                        "F16",
                        &[512, 4, 3, 3],
                    ),
                ],
                "checkpoint",
                "sdxl",
            ),
            (
                "flux.safetensors",
                vec![
                    (
                        "double_blocks.0.img_attn.norm.key_norm.scale",
                        "BF16",
                        &[128],
                    ),
                    ("img_in.weight", "BF16", &[3072, 64]),
                    ("guidance_in.in_layer.weight", "BF16", &[3072, 256]),
                ],
                "diffusion",
                "flux-dev",
            ),
            (
                "klein.safetensors",
                vec![(
                    "double_stream_modulation_img.lin.weight",
                    "F8_E4M3",
                    &[18432, 3072],
                )],
                "diffusion",
                "flux2",
            ),
            (
                "wan.safetensors",
                vec![(
                    "model.diffusion_model.head.modulation",
                    "F16",
                    &[1, 2, 5120],
                )],
                "diffusion",
                "wan2.1",
            ),
            (
                "lora.safetensors",
                vec![(
                    "lora_unet_double_blocks_0_img_attn_qkv.lora_up.weight",
                    "F16",
                    &[9216, 16],
                )],
                "lora",
                "flux",
            ),
            (
                "vae.safetensors",
                vec![
                    ("decoder.conv_in.weight", "F32", &[512, 16, 3, 3]),
                    ("encoder.conv_in.weight", "F32", &[128, 3, 3, 3]),
                ],
                "vae",
                "flux",
            ),
            (
                "t5.safetensors",
                vec![(
                    "encoder.block.0.layer.0.SelfAttention.q.weight",
                    "F16",
                    &[4096, 4096],
                )],
                "text_encoder",
                "t5",
            ),
            (
                "clipl.safetensors",
                vec![(
                    "text_model.encoder.layers.0.self_attn.q_proj.weight",
                    "F16",
                    &[768, 768],
                )],
                "text_encoder",
                "clip_l",
            ),
        ];
        for (name, tensors, kind, arch) in cases {
            let p = d.path().join(name);
            write_safetensors(&p, &tensors, None);
            let i = inspect(&p).unwrap();
            assert_eq!(
                (i.kind.as_str(), i.arch.as_deref()),
                (kind, Some(arch)),
                "{name}"
            );
        }
        let p = d.path().join("klein.safetensors");
        assert_eq!(inspect(&p).unwrap().precision, Some(Precision::Fp8));
    }

    #[test]
    fn reads_comfy_quant_format() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("q.safetensors");
        write_safetensors(
            &p,
            &[(
                "double_stream_modulation_img.lin.weight",
                "U8",
                &[1000, 500],
            )],
            Some("nvfp4"),
        );
        let i = inspect(&p).unwrap();
        assert_eq!(i.precision, Some(Precision::Nvfp4));
        assert_eq!(i.metadata["modelspec.title"], "test");
    }

    #[test]
    fn reads_gguf() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("m-Q4_K_M.gguf");
        let mut b = Vec::new();
        b.extend_from_slice(b"GGUF");
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&2u64.to_le_bytes()); // tensors
        b.extend_from_slice(&1u64.to_le_bytes()); // kv
        let s = |b: &mut Vec<u8>, x: &str| {
            b.extend_from_slice(&(x.len() as u64).to_le_bytes());
            b.extend_from_slice(x.as_bytes());
        };
        s(&mut b, "general.architecture");
        b.extend_from_slice(&8u32.to_le_bytes());
        s(&mut b, "flux");
        for (name, ty) in [
            ("double_blocks.0.img_attn.norm.key_norm.scale", 0u32),
            ("img_in.weight", 12u32),
        ] {
            s(&mut b, name);
            b.extend_from_slice(&2u32.to_le_bytes());
            b.extend_from_slice(&64u64.to_le_bytes());
            b.extend_from_slice(&3072u64.to_le_bytes());
            b.extend_from_slice(&ty.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes());
        }
        std::fs::write(&p, b).unwrap();
        let i = inspect(&p).unwrap();
        assert_eq!(i.format, "gguf");
        assert_eq!(i.arch.as_deref(), Some("flux-schnell"));
        assert_eq!(i.metadata["general.architecture"], "flux");
        assert_eq!(i.precision, Some(Precision::Q4), "Q4_K dominates");
    }

    #[test]
    fn rejects_garbage_and_lists_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.safetensors");
        std::fs::write(&p, [0xffu8; 64]).unwrap();
        assert!(inspect(&p).is_err());
        std::fs::create_dir_all(d.path().join("loras/styles")).unwrap();
        std::fs::write(d.path().join("loras/styles/a b.safetensors"), b"").unwrap();
        std::fs::write(d.path().join("loras/readme.txt"), b"").unwrap();
        let files = list_files(d.path());
        assert_eq!(
            files,
            vec![("loras".to_string(), PathBuf::from("styles/a b.safetensors"))]
        );
    }
}
