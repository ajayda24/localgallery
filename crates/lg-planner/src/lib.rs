//! The memory planner.
//!
//! Given the PC, a model manifest and the job's settings, it decides which weight
//! variant to use, whether weights stay in VRAM, stream from RAM or from disk, which
//! ComfyUI flags to launch with, and whether to tile the VAE decode, and explains each
//! decision in plain words. Measured history beats estimates.
//!
//! ComfyUI flags used (verified in `comfy/cli_args.py`): `--reserve-vram`,
//! `--disable-pinned-memory`, `--async-offload`, `--fast-disk`, `--fast`, `--cpu`.

use lg_store::Benchmark;
use lg_types::{Fit, GpuInfo, HardwareInfo, Manifest, ModelFile, Plan, Precision};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Best quality that still runs at full speed.
    #[default]
    Balanced,
    /// Highest precision that fits in RAM, even if it has to stream.
    Quality,
    /// Smallest fast format the GPU supports (NVFP4/fp8), plus fast math.
    Speed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// VRAM kept free for Windows, the desktop and browser.
    pub vram_headroom_gb: f64,
    /// RAM kept free for the OS and other apps.
    pub ram_reserve_gb: f64,
    pub policy: Policy,
    /// Allow ComfyUI's `--fast` optimisations (may change results slightly).
    pub fast_math: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            vram_headroom_gb: 1.0,
            ram_reserve_gb: 6.0,
            policy: Policy::Balanced,
            fast_math: false,
        }
    }
}

pub struct Input<'a> {
    pub hw: &'a HardwareInfo,
    pub manifest: &'a Manifest,
    pub op: &'a str,
    pub values: &'a BTreeMap<String, Value>,
    /// Size on disk of model files that are present, by file name.
    pub on_disk: &'a BTreeMap<String, u64>,
    /// Past runs of this model (newest first).
    pub history: &'a [Benchmark],
    pub settings: &'a Settings,
    /// User overrides from the job.
    pub force_fit: Option<Fit>,
    pub force_precision: Option<Precision>,
    pub force_vae_tiled: Option<bool>,
    /// Retry level after an out-of-memory error (0 = first try).
    pub escalate: u8,
}

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    Main,
    TextEncoder,
    Vae,
    Other,
}

fn component(f: &ModelFile) -> Component {
    match f.folder.as_str() {
        "checkpoints" | "diffusion_models" | "unet" => Component::Main,
        "text_encoders" | "clip" => Component::TextEncoder,
        "vae" => Component::Vae,
        _ => Component::Other,
    }
}

/// Job shape: megapixels per image, frames, images per run, steps.
fn job_shape(m: &Manifest, op: &str, values: &BTreeMap<String, Value>) -> (f64, f64, f64, f64) {
    let size = m
        .inputs_for(op)
        .find(|i| i.kind == lg_types::InputKind::Size)
        .and_then(|i| values.get(&i.id))
        .and_then(|v| Some(v.get("w")?.as_f64()? * v.get("h")?.as_f64()?))
        .unwrap_or(1024.0 * 1024.0);
    let frames = m
        .inputs_for(op)
        .find(|i| {
            i.extra.get("frames").is_some_and(|f| f == &json!(true))
                || i.id == "frames"
                || i.id == "length"
        })
        .and_then(|i| values.get(&i.id))
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let batch = values
        .get("batch")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .max(1.0);
    let steps = values.get("steps").and_then(Value::as_f64).unwrap_or(20.0);
    (size / 1e6, frames.max(1.0), batch, steps)
}

fn size_of(v: &lg_types::FileVariant, on_disk: &BTreeMap<String, u64>) -> (f64, bool) {
    match on_disk.get(&v.name) {
        Some(b) => (*b as f64 / GB, true),
        None => (v.size_gb, false),
    }
}

/// Speed rank of a precision on this GPU (higher = faster tensor-core path).
fn speed_rank(p: Precision, gpu: Option<&GpuInfo>) -> i32 {
    match p {
        Precision::Nvfp4 if gpu.is_some_and(GpuInfo::supports_nvfp4) => 3,
        Precision::Fp8 if gpu.is_some_and(GpuInfo::supports_fp8) => 2,
        Precision::Fp16 | Precision::Bf16 => 1,
        // GGUF and unsupported formats are dequantised on the fly: smaller, not faster.
        _ => 0,
    }
}

pub fn plan(i: &Input) -> Plan {
    let gpu = i.hw.primary_gpu();
    let s = i.settings;
    let mut explain = Vec::new();
    let mut warnings = Vec::new();
    let (mp, frames, batch, steps) = job_shape(i.manifest, i.op, i.values);
    let mem = &i.manifest.memory;
    let activation = mp * batch * mem.activation_gb_per_mp
        + mp * frames * batch * mem.activation_gb_per_mp_frame;
    let vae_peak = mp * frames.clamp(1.0, 16.0) * batch * mem.vae_gb_per_mp;

    let Some(gpu) = gpu.filter(|g| g.vram_total_gb > 0.5) else {
        explain
            .push("No supported GPU found, so this runs on the CPU. It will be very slow.".into());
        // Still choose files: downloaded first, otherwise the smallest, which suits RAM best.
        let mut variants = Vec::new();
        let mut downloads = Vec::new();
        let mut precision = None;
        for f in i.manifest.files.iter().filter(|f| f.needed_for(i.op)) {
            let v = f
                .variants
                .iter()
                .find(|v| i.on_disk.contains_key(&v.name))
                .or_else(|| {
                    f.variants
                        .iter()
                        .filter(|v| v.size_gb > 0.0)
                        .min_by(|a, b| a.size_gb.total_cmp(&b.size_gb))
                })
                .or(f.variants.first());
            let Some(v) = v else { continue };
            if !i.on_disk.contains_key(&v.name) {
                downloads.push((v.name.clone(), v.size_gb));
            }
            if component(f) == Component::Main {
                precision = Some(v.precision);
            }
            variants.push((f.role.clone(), v.name.clone()));
        }
        return finish(
            Fit::Offload,
            variants,
            precision,
            vec!["--cpu".into()],
            false,
            BTreeMap::new(),
            0.0,
            0.0,
            None,
            explain,
            warnings,
            downloads,
        );
    };
    let laptop_extra = if i.hw.laptop == Some(true) { 0.3 } else { 0.0 };
    let vram_budget = (gpu.vram_total_gb - s.vram_headroom_gb - laptop_extra).max(0.5);
    let ram_budget = (i.hw.ram_total_gb - s.ram_reserve_gb).max(1.0);

    // Pick a variant per needed file.
    let needed: Vec<&ModelFile> = i
        .manifest
        .files
        .iter()
        .filter(|f| f.needed_for(i.op))
        .collect();
    let mut chosen: Vec<(String, String)> = Vec::new();
    let mut sizes: Vec<(Component, f64)> = Vec::new();
    let mut downloads = Vec::new();
    let mut main_precision = None;
    let mut unknown_sizes = false;
    let mut main_unknown = false;
    for f in &needed {
        let comp = component(f);
        let mut variants: Vec<&lg_types::FileVariant> = f.variants.iter().collect();
        if let Some(p) = i.force_precision.filter(|_| comp == Component::Main)
            && variants.iter().any(|v| v.precision == p)
        {
            variants.retain(|v| v.precision == p);
        }
        let best = match comp {
            Component::Main => choose_main(
                &variants,
                i.on_disk,
                s.policy,
                gpu,
                vram_budget - activation,
                ram_budget,
            ),
            // Smaller encoders are fine; keep whatever is downloaded, else the first listed.
            _ => variants
                .iter()
                .copied()
                .find(|v| i.on_disk.contains_key(&v.name))
                .or(variants.first().copied()),
        };
        let Some(v) = best else { continue };
        let (gb, present) = size_of(v, i.on_disk);
        if gb <= 0.0 {
            if comp == Component::Main {
                main_unknown = true;
            } else {
                unknown_sizes = true;
            }
        }
        if !present {
            downloads.push((v.name.clone(), gb));
        }
        if comp == Component::Main {
            main_precision = Some(v.precision);
        }
        chosen.push((f.role.clone(), v.name.clone()));
        sizes.push((comp, gb));
    }
    // Two-expert models (Wan 2.2) run one expert at a time: VRAM needs the largest, RAM all.
    let main_peak = sizes
        .iter()
        .filter(|(c, _)| *c == Component::Main)
        .map(|(_, g)| *g)
        .fold(0.0, f64::max);
    let te_gb: f64 = sizes
        .iter()
        .filter(|(c, _)| *c == Component::TextEncoder)
        .map(|(_, g)| *g)
        .sum();
    let vae_gb: f64 = sizes
        .iter()
        .filter(|(c, _)| *c == Component::Vae)
        .map(|(_, g)| *g)
        .sum();
    let weights: f64 = sizes.iter().map(|(_, g)| g).sum();
    let mut value_overrides = BTreeMap::new();

    // Measured history for this model, op, precision and a similar size beats the estimate.
    let measured = i
        .history
        .iter()
        .filter(|b| b.op == i.op && b.precision.as_deref() == main_precision.map(prec_name))
        .find(|b| {
            ((b.width as f64 * b.height as f64) / 1e6 - mp).abs() / mp.max(0.01) < 0.15
                && (b.frames as f64 - frames).abs() <= frames * 0.15
        });

    let need_resident = main_peak + activation;
    let mut fit = if need_resident <= vram_budget {
        Fit::Resident
    } else if weights <= ram_budget {
        Fit::Offload
    } else {
        Fit::Stream
    };
    if activation > vram_budget {
        fit = Fit::TooBig;
    }
    if let Some(m) = measured {
        if let Some(peak) = m.peak_vram_gb {
            explain.push(format!(
                "Last run at this size used {peak:.1} GB of GPU memory and took {:.0} s.",
                m.seconds
            ));
        }
        // Spill into Windows shared GPU memory makes everything crawl: plan lighter.
        if m.shared_gpu_gb.is_some_and(|g| g > 0.5) && fit == Fit::Resident {
            fit = Fit::Offload;
            warnings.push(format!("Last time {:.1} GB spilled into shared GPU memory, so this run keeps less in VRAM.", m.shared_gpu_gb.unwrap_or(0.0)));
        }
    }
    if let Some(f) = i.force_fit {
        fit = f.max(fit.min(Fit::Stream));
    }
    // Escalation after an out-of-memory error.
    let escalate = i.escalate;
    if escalate >= 2 && fit == Fit::Resident {
        fit = Fit::Offload;
    }
    if escalate >= 3 {
        fit = Fit::Stream;
    }

    // Without the model's size the numbers below would be made up ("model 0.0 GB fits").
    // Plan conservatively (weights may live in RAM) and say what is missing.
    if main_unknown && measured.is_none() {
        if fit == Fit::Resident {
            fit = Fit::Offload;
        }
        explain.push(
            "The model's file size isn't known yet, so whether it fits your GPU can't be estimated. \
             It is looked up when the app can reach the download server, or read from the file once downloaded."
                .into(),
        );
    }
    match fit {
        _ if main_unknown && measured.is_none() => {}
        Fit::Resident => explain.push(format!(
            "Fits in your {} ({:.0} GB): model {main_peak:.1} GB + working memory {activation:.1} GB stays on the GPU.",
            short_gpu(&gpu.name),
            gpu.vram_total_gb
        )),
        Fit::Offload => explain.push(format!(
            "The model ({main_peak:.1} GB) and working memory ({activation:.1} GB) are more than the {vram_budget:.1} GB the GPU can spare, so parts stream from RAM while it runs. Expect it to be slower."
        )),
        Fit::Stream => {
            explain.push(format!(
                "All weights ({weights:.1} GB) do not fit in the {ram_budget:.0} GB of RAM available, so they stream from disk."
            ));
            warnings.push("Streaming from disk is very slow. A smaller (fp8 or GGUF) version of this model would help.".into());
        }
        Fit::TooBig => warnings.push(format!(
            "This size needs about {activation:.1} GB of working memory, more than your GPU has. Lower the resolution or the number of frames."
        )),
    }
    if te_gb > 1.0 && fit != Fit::Resident {
        explain.push(format!(
            "The {te_gb:.1} GB text encoder runs first and is moved off the GPU before generating."
        ));
    }
    if unknown_sizes {
        explain.push(
            "Some file sizes are unknown until they are downloaded, so this estimate may change."
                .into(),
        );
    }

    // weight_dtype: store fp16 weights as fp8 when that is what makes it fit.
    let main_is_full = main_precision.is_some_and(|p| p.quality() == 100);
    if main_is_full
        && i.manifest.input("weight_dtype").is_some()
        && (fit > Fit::Resident || escalate >= 2)
        && main_peak / 2.0 <= vram_budget
    {
        let fast = gpu.supports_fp8() && s.policy == Policy::Speed;
        value_overrides.insert(
            "weight_dtype".into(),
            json!(if fast {
                "fp8_e4m3fn_fast"
            } else {
                "fp8_e4m3fn"
            }),
        );
        if main_peak / 2.0 + activation <= vram_budget {
            fit = Fit::Resident;
            explain.push("Weights are stored in fp8 so the model fits in GPU memory (about half the size, almost the same quality).".into());
        } else {
            explain
                .push("Weights are stored in fp8 to halve how much has to stream from RAM.".into());
        }
    }

    // VAE decode after the main model has been offloaded.
    let vae_room = vram_budget - vae_gb;
    let mut vae_tiled = vae_peak > vae_room * 0.85 || frames > 33.0 || escalate >= 1;
    if let Some(t) = i.force_vae_tiled {
        vae_tiled = t;
    }
    if vae_tiled {
        explain.push("The final decode runs in tiles to save memory.".into());
    }

    // Launch flags. Kept stable so the engine is rarely restarted.
    let mut flags = vec![
        "--reserve-vram".to_string(),
        format!("{:.1}", s.vram_headroom_gb),
    ];
    let ram_free_after = i.hw.ram_free_gb - weights;
    if ram_free_after < 4.0 || fit == Fit::Stream || escalate >= 2 {
        // Pinned (page-locked) RAM speeds up transfers but cannot be paged out.
        flags.push("--disable-pinned-memory".into());
        if fit != Fit::Resident {
            explain.push("Pinned memory is off to leave RAM for the model.".into());
        }
    }
    if fit == Fit::Offload && gpu.vendor == "nvidia" {
        flags.push("--async-offload".into());
    }
    if fit == Fit::Stream {
        flags.push("--fast-disk".into());
    }
    if s.fast_math {
        let mut fast = vec!["fp16_accumulation"];
        if gpu.supports_fp8() {
            fast.push("fp8_matrix_mult");
        }
        flags.push("--fast".into());
        flags.extend(fast.into_iter().map(str::to_string));
        explain.push("Fast math is on (may change results slightly).".into());
    }
    if gpu.vendor == "nvidia"
        && gpu.compute_capability.is_some_and(|(maj, _)| maj >= 12)
        && main_precision == Some(Precision::Nvfp4)
    {
        explain.push(
            "Your Blackwell GPU runs this NVFP4 model on its native FP4 tensor cores.".into(),
        );
    }

    // Time estimate from history: seconds per (megapixel x frame x step), median.
    let work = mp * frames * batch * steps;
    let mut rates: Vec<f64> = i
        .history
        .iter()
        .filter(|b| {
            b.op == i.op && b.steps > 0 && b.precision.as_deref() == main_precision.map(prec_name)
        })
        .map(|b| {
            b.seconds
                / ((b.width as f64 * b.height as f64 / 1e6)
                    * b.frames.max(1) as f64
                    * b.steps as f64)
        })
        .filter(|r| r.is_finite() && *r > 0.0)
        .collect();
    rates.sort_by(f64::total_cmp);
    let eta = rates.get(rates.len() / 2).map(|r| r * work);
    if let Some(e) = eta
        && e > 15.0 * 60.0
    {
        warnings.push(format!("This will take about {:.0} minutes.", e / 60.0));
    }
    if !downloads.is_empty() {
        let total: f64 = downloads.iter().map(|(_, g)| g).sum();
        if total > 0.0 {
            explain.push(format!(
                "{} file(s) to download first ({total:.1} GB).",
                downloads.len()
            ));
        } else {
            explain.push(format!("{} file(s) to download first.", downloads.len()));
        }
    }
    finish(
        fit,
        chosen,
        main_precision,
        flags,
        vae_tiled,
        value_overrides,
        vram_budget,
        need_resident.max(weights.min(vram_budget)),
        eta,
        explain,
        warnings,
        downloads,
    )
    .with_weights(weights)
    .with_sizes_known(!main_unknown || measured.is_some())
}

fn choose_main<'a>(
    variants: &[&'a lg_types::FileVariant],
    on_disk: &BTreeMap<String, u64>,
    policy: Policy,
    gpu: &GpuInfo,
    vram_for_weights: f64,
    ram_budget: f64,
) -> Option<&'a lg_types::FileVariant> {
    if variants.len() == 1 {
        return variants.first().copied();
    }
    let gb = |v: &lg_types::FileVariant| size_of(v, on_disk).0;
    let present = |v: &lg_types::FileVariant| on_disk.contains_key(&v.name);
    let mut vs: Vec<&lg_types::FileVariant> = variants.to_vec();
    match policy {
        Policy::Quality => {
            vs.sort_by_key(|v| (std::cmp::Reverse(v.precision.quality()), !present(v)));
            vs.iter()
                .copied()
                .find(|v| gb(v) <= ram_budget)
                .or(vs.last().copied())
        }
        Policy::Speed => {
            vs.sort_by_key(|v| {
                (
                    std::cmp::Reverse(speed_rank(v.precision, Some(gpu))),
                    std::cmp::Reverse(v.precision.quality()),
                )
            });
            vs.iter()
                .copied()
                .find(|v| gb(v) <= vram_for_weights)
                .or(vs.first().copied())
        }
        Policy::Balanced => {
            // Best quality that stays resident. Otherwise the one that overflows the GPU the
            // least (streaming 0.3 GB beats streaming 6 GB), as long as it fits in RAM.
            vs.sort_by_key(|v| {
                (
                    std::cmp::Reverse(v.precision.quality()),
                    std::cmp::Reverse(speed_rank(v.precision, Some(gpu))),
                    !present(v),
                )
            });
            vs.iter()
                .copied()
                .find(|v| gb(v) <= vram_for_weights)
                .or_else(|| {
                    vs.iter()
                        .copied()
                        .filter(|v| gb(v) <= ram_budget)
                        .min_by(|a, b| {
                            (gb(a) - vram_for_weights).total_cmp(&(gb(b) - vram_for_weights))
                        })
                        .or(vs.last().copied())
                })
        }
    }
}

pub fn prec_name(p: Precision) -> &'static str {
    match p {
        Precision::Fp32 => "fp32",
        Precision::Bf16 => "bf16",
        Precision::Fp16 => "fp16",
        Precision::Fp8 => "fp8",
        Precision::Q8 => "q8",
        Precision::Nvfp4 => "nvfp4",
        Precision::Q6 => "q6",
        Precision::Q5 => "q5",
        Precision::Q4 => "q4",
        Precision::Q3 => "q3",
    }
}

fn short_gpu(name: &str) -> String {
    name.trim_start_matches("NVIDIA ")
        .trim_start_matches("GeForce ")
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn finish(
    fit: Fit,
    variants: Vec<(String, String)>,
    precision: Option<Precision>,
    launch_flags: Vec<String>,
    vae_tiled: bool,
    value_overrides: BTreeMap<String, Value>,
    vram_budget_gb: f64,
    estimated_vram_gb: f64,
    estimated_seconds: Option<f64>,
    explain: Vec<String>,
    warnings: Vec<String>,
    downloads: Vec<(String, f64)>,
) -> Plan {
    Plan {
        fit,
        variants,
        precision: precision.map(|p| prec_name(p).to_string()),
        launch_flags,
        vae_tiled,
        value_overrides,
        vram_budget_gb,
        weights_gb: 0.0,
        downloads,
        estimated_vram_gb,
        estimated_seconds,
        explain,
        warnings,
        sizes_known: true,
    }
}

trait WithWeights {
    fn with_weights(self, w: f64) -> Self;
    fn with_sizes_known(self, known: bool) -> Self;
}
impl WithWeights for Plan {
    fn with_weights(mut self, w: f64) -> Self {
        self.weights_gb = w;
        self
    }
    fn with_sizes_known(mut self, known: bool) -> Self {
        self.sizes_known = known;
        self
    }
}

/// Engine launch flags that require a restart when they change.
pub fn restart_needed(running: &[String], wanted: &[String]) -> bool {
    let norm = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    norm(running) != norm(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lg_types::{GpuInfo, HardwareInfo};

    fn hw(vram: f64, ram: f64, cc: (u32, u32)) -> HardwareInfo {
        HardwareInfo {
            ram_total_gb: ram,
            ram_free_gb: ram - 6.0,
            gpus: vec![GpuInfo {
                name: "NVIDIA GeForce RTX 5060".into(),
                vendor: "nvidia".into(),
                vram_total_gb: vram,
                vram_free_gb: vram - 0.5,
                compute_capability: Some(cc),
                ..Default::default()
            }],
            laptop: Some(false),
            ..Default::default()
        }
    }

    fn model(variants: &[(&str, Precision, f64)], te: f64) -> Manifest {
        let v: Vec<Value> = variants
            .iter()
            .map(|(n, p, g)| json!({"name": n, "precision": p, "size_gb": g}))
            .collect();
        serde_json::from_value(json!({
            "schema": 1, "id": "m", "display_name": "M", "family": "f", "media": "image",
            "files": [
                {"role": "diffusion", "folder": "diffusion_models", "variants": v},
                {"role": "text_encoder", "folder": "text_encoders", "variants": [{"name": "te.safetensors", "precision": "fp8", "size_gb": te}]},
                {"role": "vae", "folder": "vae", "variants": [{"name": "vae.safetensors", "precision": "fp32", "size_gb": 0.33}]}],
            "memory": {"activation_gb_per_mp": 1.2, "vae_gb_per_mp": 1.5},
            "ops": {"txt2img": {"workflow": {}}},
            "inputs": [{"id": "size", "kind": "size", "label": "Size"}, {"id": "weight_dtype", "kind": "enum", "label": "W", "options": ["default", "fp8_e4m3fn"]}]
        }))
        .unwrap()
    }

    fn run(
        hw: &HardwareInfo,
        m: &Manifest,
        w: f64,
        h: f64,
        settings: &Settings,
        history: &[Benchmark],
        escalate: u8,
    ) -> Plan {
        let mut values = BTreeMap::new();
        values.insert("size".into(), json!({"w": w, "h": h}));
        values.insert("steps".into(), json!(20));
        plan(&Input {
            hw,
            manifest: m,
            op: "txt2img",
            values: &values,
            on_disk: &BTreeMap::new(),
            history,
            settings,
            force_fit: None,
            force_precision: None,
            force_vae_tiled: None,
            escalate,
        })
    }

    #[test]
    fn unknown_model_size_is_not_a_fit() {
        // The screenshot case: size_gb 0 used to read as "model 0.0 GB, fits your GPU".
        let pc = hw(8.0, 32.0, (12, 0));
        let s = Settings::default();
        let m = model(
            &[(
                "wan2.2_high_noise_14B_fp8_scaled.safetensors",
                Precision::Fp8,
                0.0,
            )],
            0.0,
        );
        let p = run(&pc, &m, 1024.0, 1024.0, &s, &[], 0);
        assert!(!p.sizes_known, "{p:?}");
        assert_ne!(
            p.fit,
            Fit::Resident,
            "a model of unknown size is planned as if it may not fit"
        );
        assert!(
            p.explain.iter().any(|e| e.contains("isn't known yet")),
            "{:?}",
            p.explain
        );
        assert!(
            !p.explain.iter().any(|e| e.contains("0.0 GB")),
            "no made-up numbers: {:?}",
            p.explain
        );
        // Once the size is known the normal estimate comes back.
        let m = model(&[("wan.safetensors", Precision::Fp8, 14.3)], 6.7);
        let p = run(&pc, &m, 1024.0, 1024.0, &s, &[], 0);
        assert!(p.sizes_known);
        assert_eq!(p.fit, Fit::Offload);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn table() {
        let pc = hw(8.0, 32.0, (12, 0));
        let s = Settings::default();
        // (case, variants, te, w, h, expected fit, expected precision)
        let cases: Vec<(&str, Vec<(&str, Precision, f64)>, f64, f64, f64, Fit, &str)> = vec![
            (
                "SDXL-size model stays resident",
                vec![("sdxl.safetensors", Precision::Fp16, 5.1)],
                1.6,
                1024.0,
                1024.0,
                Fit::Resident,
                "fp16",
            ),
            (
                "12 GB fp16 offloads to RAM",
                vec![("big.safetensors", Precision::Fp16, 12.0)],
                4.9,
                1024.0,
                1024.0,
                Fit::Offload,
                "fp16",
            ),
            (
                "prefer the fp8 variant that fits",
                vec![
                    ("a_bf16", Precision::Bf16, 12.0),
                    ("a_fp8", Precision::Fp8, 5.5),
                ],
                4.9,
                1024.0,
                1024.0,
                Fit::Resident,
                "fp8",
            ),
            (
                "least overflow when nothing fits",
                vec![
                    ("a_bf16", Precision::Bf16, 12.0),
                    ("a_fp8", Precision::Fp8, 6.0),
                ],
                4.9,
                1024.0,
                1024.0,
                Fit::Offload,
                "fp8",
            ),
            (
                "too big for RAM streams from disk",
                vec![("huge", Precision::Fp16, 40.0)],
                9.0,
                1024.0,
                1024.0,
                Fit::Stream,
                "fp16",
            ),
            (
                "enormous canvas is too big",
                vec![("sdxl", Precision::Fp16, 5.1)],
                1.6,
                4096.0,
                4096.0,
                Fit::TooBig,
                "fp16",
            ),
        ];
        for (name, variants, te, w, h, fit, prec) in cases {
            let mut m = model(&variants, te);
            m.inputs.retain(|i| i.id != "weight_dtype");
            let p = run(&pc, &m, w, h, &s, &[], 0);
            assert_eq!(p.fit, fit, "{name}: {:?}", p.explain);
            assert_eq!(p.precision.as_deref(), Some(prec), "{name}");
            assert!(!p.explain.is_empty());
        }
    }

    #[test]
    fn flags_follow_the_fit() {
        let pc = hw(8.0, 32.0, (12, 0));
        let s = Settings::default();
        let mut m = model(&[("huge", Precision::Fp16, 40.0)], 9.0);
        m.inputs.retain(|i| i.id != "weight_dtype");
        let p = run(&pc, &m, 1024.0, 1024.0, &s, &[], 0);
        assert!(p.launch_flags.contains(&"--fast-disk".into()));
        assert!(p.launch_flags.contains(&"--disable-pinned-memory".into()));
        assert!(!p.warnings.is_empty());
        let mut m = model(&[("big", Precision::Fp16, 12.0)], 4.9);
        m.inputs.retain(|i| i.id != "weight_dtype");
        let p = run(&pc, &m, 1024.0, 1024.0, &s, &[], 0);
        assert!(p.launch_flags.contains(&"--async-offload".into()));
        assert_eq!(
            &p.launch_flags[..2],
            &["--reserve-vram".to_string(), "1.0".to_string()]
        );
    }

    #[test]
    fn fp8_storage_to_fit() {
        let pc = hw(8.0, 32.0, (12, 0));
        let m = model(&[("z_bf16", Precision::Bf16, 11.5)], 7.5);
        let p = run(&pc, &m, 1024.0, 1024.0, &Settings::default(), &[], 0);
        assert_eq!(
            p.value_overrides.get("weight_dtype"),
            Some(&json!("fp8_e4m3fn"))
        );
        assert_eq!(
            p.fit,
            Fit::Offload,
            "5.75 GB + 1.3 GB working memory still exceeds 7 GB"
        );
        let small = model(&[("z_bf16", Precision::Bf16, 10.0)], 7.5);
        assert_eq!(
            run(&pc, &small, 1024.0, 1024.0, &Settings::default(), &[], 0).fit,
            Fit::Resident
        );
        let fast = Settings {
            policy: Policy::Speed,
            ..Default::default()
        };
        let p = run(&pc, &m, 1024.0, 1024.0, &fast, &[], 0);
        assert_eq!(
            p.value_overrides.get("weight_dtype"),
            Some(&json!("fp8_e4m3fn_fast")),
            "Blackwell supports fp8 math"
        );
    }

    #[test]
    fn speed_policy_prefers_nvfp4_on_blackwell() {
        let vs = [
            ("q_bf16", Precision::Bf16, 7.0),
            ("q_fp8", Precision::Fp8, 3.6),
            ("q_nvfp4", Precision::Nvfp4, 2.2),
        ];
        let mut m = model(&vs, 2.0);
        m.inputs.retain(|i| i.id != "weight_dtype");
        let speed = Settings {
            policy: Policy::Speed,
            ..Default::default()
        };
        assert_eq!(
            run(&hw(8.0, 32.0, (12, 0)), &m, 1024.0, 1024.0, &speed, &[], 0)
                .precision
                .as_deref(),
            Some("nvfp4")
        );
        assert_eq!(
            run(&hw(8.0, 32.0, (8, 9)), &m, 1024.0, 1024.0, &speed, &[], 0)
                .precision
                .as_deref(),
            Some("fp8"),
            "Ada has no fp4"
        );
        assert_eq!(
            run(
                &hw(24.0, 64.0, (12, 0)),
                &m,
                1024.0,
                1024.0,
                &Settings::default(),
                &[],
                0
            )
            .precision
            .as_deref(),
            Some("bf16")
        );
    }

    #[test]
    fn history_spill_and_eta() {
        let pc = hw(8.0, 32.0, (12, 0));
        let mut m = model(&[("sdxl", Precision::Fp16, 5.1)], 1.6);
        m.inputs.retain(|i| i.id != "weight_dtype");
        let b = Benchmark {
            model: "m".into(),
            op: "txt2img".into(),
            precision: Some("fp16".into()),
            plan: "resident".into(),
            width: 1024,
            height: 1024,
            frames: 1,
            steps: 20,
            seconds: 12.0,
            peak_vram_gb: Some(7.6),
            peak_ram_gb: None,
            shared_gpu_gb: Some(1.2),
            created_at: 0,
        };
        let p = run(&pc, &m, 1024.0, 1024.0, &Settings::default(), &[b], 0);
        assert_eq!(p.fit, Fit::Offload, "spill last time means plan lighter");
        assert!((p.estimated_seconds.unwrap() - 12.0).abs() < 0.01);
    }

    #[test]
    fn escalation_after_oom() {
        let pc = hw(8.0, 32.0, (12, 0));
        let mut m = model(&[("sdxl", Precision::Fp16, 5.1)], 1.6);
        m.inputs.retain(|i| i.id != "weight_dtype");
        let s = Settings::default();
        assert!(!run(&pc, &m, 1024.0, 1024.0, &s, &[], 0).vae_tiled);
        assert!(run(&pc, &m, 1024.0, 1024.0, &s, &[], 1).vae_tiled);
        let p2 = run(&pc, &m, 1024.0, 1024.0, &s, &[], 2);
        assert_eq!(p2.fit, Fit::Offload);
        assert!(p2.launch_flags.contains(&"--disable-pinned-memory".into()));
        assert_eq!(run(&pc, &m, 1024.0, 1024.0, &s, &[], 3).fit, Fit::Stream);
    }

    #[test]
    fn no_gpu_runs_on_cpu() {
        let pc = HardwareInfo {
            ram_total_gb: 16.0,
            ram_free_gb: 10.0,
            ..Default::default()
        };
        let m = model(&[("sdxl", Precision::Fp16, 5.1)], 1.6);
        let p = run(&pc, &m, 512.0, 512.0, &Settings::default(), &[], 0);
        assert_eq!(p.launch_flags, vec!["--cpu".to_string()]);
        assert_eq!(
            p.variants.len(),
            3,
            "files are chosen even without a GPU: {:?}",
            p.variants
        );
    }

    #[test]
    fn restart_only_when_flags_change() {
        let a = vec![
            "--reserve-vram".to_string(),
            "1.0".into(),
            "--async-offload".into(),
        ];
        let b = vec![
            "--async-offload".to_string(),
            "--reserve-vram".into(),
            "1.0".into(),
        ];
        assert!(!restart_needed(&a, &b));
        assert!(restart_needed(&a, &["--fast-disk".to_string()]));
    }
}
