//! Turns a [`JobSpec`] plus its [`Manifest`] into a ComfyUI API graph.
//!
//! Pipeline: resolve values -> load template -> substitute placeholders ->
//! run expanders -> apply plan transforms and raw overrides.

pub mod expand;
pub mod import;
pub mod objectinfo;
pub mod resolve;
pub mod subgraph;
pub mod template;

use lg_types::{JobSpec, Manifest, WorkflowRef};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use template::{Scope, substitute};

pub use resolve::resolve_values;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BindError {
    #[error("this model has no operation {0:?}")]
    UnknownOp(String),
    #[error("invalid settings: {}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error("template error: {0}")]
    Template(String),
    #[error("model file for role {0:?} is not chosen")]
    MissingFile(String),
    #[error("uploaded file {0} is missing")]
    MissingAsset(String),
    #[error("cannot read workflow {0}: {1}")]
    Io(String, String),
}

/// Inputs to the binder beyond the spec itself.
#[derive(Debug, Default, Clone)]
pub struct BindOptions {
    /// File role -> chosen file name (from the planner's variant choice).
    pub files: BTreeMap<String, String>,
    /// Asset id -> file name inside the engine's input folder.
    pub assets: BTreeMap<String, String>,
    pub vae_tiled: bool,
    /// Prefix for output nodes' `filename_prefix`, so outputs are easy to collect.
    pub output_prefix: Option<String>,
}

/// Load a template referenced by an op, relative to the manifests root.
pub fn load_template(r: &WorkflowRef, root: &Path) -> Result<Value, BindError> {
    match r {
        WorkflowRef::Inline(v) => Ok(v.clone()),
        WorkflowRef::Path(p) => {
            let path = root.join(p);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| BindError::Io(path.display().to_string(), e.to_string()))?;
            serde_json::from_str(&text)
                .map_err(|e| BindError::Io(path.display().to_string(), e.to_string()))
        }
    }
}

/// Build the final graph from already-resolved values.
pub fn build_graph(
    manifest: &Manifest,
    spec: &JobSpec,
    values: &BTreeMap<String, Value>,
    template: &Value,
    opts: &BindOptions,
) -> Result<Value, BindError> {
    let op = manifest
        .ops
        .get(&spec.op)
        .ok_or_else(|| BindError::UnknownOp(spec.op.clone()))?;
    let mut graph = template.clone();
    let scope = Scope {
        values: Some(values),
        files: Some(&opts.files),
        assets: Some(&opts.assets),
        ..Default::default()
    };
    // Substitute the base graph first, so user text inserted here is never re-scanned.
    substitute(&mut graph, &scope)?;
    let map = graph
        .as_object_mut()
        .ok_or_else(|| BindError::Template("workflow must be a JSON object of nodes".into()))?;
    strip_ui_keys(map);
    for (i, exp) in op.expand.iter().enumerate() {
        expand::expand(map, exp, i, &scope)?;
    }
    if opts.vae_tiled {
        tile_vae(map);
    }
    if let Some(prefix) = &opts.output_prefix {
        set_output_prefix(map, prefix);
    }
    use_chosen_files(map, &opts.files);
    apply_overrides(map, &spec.overrides)?;
    gguf_loaders(map)?;
    wrap_list_values(map);
    Ok(graph)
}

/// Workflow tools name their model files literally in the graph and use the file name as the
/// role. When another file was chosen for a role (e.g. a local GGUF version), put it in the
/// loaders that named the original. Only `*_name` inputs whose value is exactly that file.
pub fn use_chosen_files(map: &mut Map<String, Value>, files: &BTreeMap<String, String>) {
    let renames: Vec<(&String, &String)> = files
        .iter()
        .filter(|(role, chosen)| role != chosen && role.contains('.'))
        .collect();
    if renames.is_empty() {
        return;
    }
    for node in map.values_mut() {
        let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) else {
            continue;
        };
        for (key, v) in inputs.iter_mut() {
            let is_file_input = key.ends_with("_name")
                || key
                    .strip_prefix("clip_name")
                    .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()));
            if !is_file_input {
                continue;
            }
            if let Some((_, chosen)) = renames
                .iter()
                .find(|(role, _)| v.as_str() == Some(role.as_str()))
            {
                *v = json!(chosen);
            }
        }
    }
}

/// ComfyUI's own loaders cannot read GGUF files; the ComfyUI-GGUF nodes can. Wherever a loader
/// was given a `.gguf` file, swap in the GGUF loader with the inputs it takes. Outputs are the
/// same (MODEL / CLIP in slot 0), so links stay valid. Works for every model and tool.
pub fn gguf_loaders(map: &mut Map<String, Value>) -> Result<(), BindError> {
    let is_gguf = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|s| s.to_ascii_lowercase().ends_with(".gguf"))
    };
    for (id, node) in map.iter_mut() {
        let class = node
            .get("class_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some(inputs) = node.get("inputs").and_then(Value::as_object).cloned() else {
            continue;
        };
        let keep = |names: &[&str]| -> Map<String, Value> {
            names
                .iter()
                .filter_map(|n| inputs.get(*n).map(|v| (n.to_string(), v.clone())))
                .collect()
        };
        let any_gguf = |names: &[&str]| names.iter().any(|n| is_gguf(inputs.get(*n)));
        let (new_class, new_inputs) = match class.as_str() {
            "UNETLoader" if any_gguf(&["unet_name"]) => ("UnetLoaderGGUF", keep(&["unet_name"])),
            "CLIPLoader" if any_gguf(&["clip_name"]) => {
                ("CLIPLoaderGGUF", keep(&["clip_name", "type"]))
            }
            "DualCLIPLoader" if any_gguf(&["clip_name1", "clip_name2"]) => (
                "DualCLIPLoaderGGUF",
                keep(&["clip_name1", "clip_name2", "type"]),
            ),
            "TripleCLIPLoader" if any_gguf(&["clip_name1", "clip_name2", "clip_name3"]) => (
                "TripleCLIPLoaderGGUF",
                keep(&["clip_name1", "clip_name2", "clip_name3"]),
            ),
            "QuadrupleCLIPLoader"
                if any_gguf(&["clip_name1", "clip_name2", "clip_name3", "clip_name4"]) =>
            {
                (
                    "QuadrupleCLIPLoaderGGUF",
                    keep(&["clip_name1", "clip_name2", "clip_name3", "clip_name4"]),
                )
            }
            "CheckpointLoaderSimple" | "CheckpointLoader" if any_gguf(&["ckpt_name"]) => {
                return Err(BindError::Template(format!(
                    "node {id}: a GGUF file holds only the diffusion model, not a full checkpoint. \
                     Put it in models\\diffusion_models and use it with a model that loads its text encoder and VAE separately."
                )));
            }
            _ => continue,
        };
        node["class_type"] = json!(new_class);
        node["inputs"] = Value::Object(new_inputs);
    }
    Ok(())
}

/// Templates may carry editor-only keys (e.g. `"_comment"`); the engine must not see them.
fn strip_ui_keys(map: &mut Map<String, Value>) {
    map.retain(|k, _| !k.starts_with('_'));
}

/// In API graphs a list means a link, so list-valued widgets (bounding boxes, arrays)
/// travel as `{"__value__": [...]}`, which ComfyUI unwraps before validation.
pub fn wrap_list_values(map: &mut Map<String, Value>) {
    for node in map.values_mut() {
        if let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) {
            for v in inputs.values_mut() {
                let is_link = v
                    .as_array()
                    .is_some_and(|a| a.len() == 2 && a[0].is_string() && a[1].is_u64());
                if v.is_array() && !is_link {
                    *v = json!({"__value__": v.take()});
                }
            }
        }
    }
}

/// Swap VAEDecode for VAEDecodeTiled to cap decode memory.
pub fn tile_vae(map: &mut Map<String, Value>) {
    for node in map.values_mut() {
        if node.get("class_type").and_then(Value::as_str) == Some("VAEDecode") {
            node["class_type"] = json!("VAEDecodeTiled");
            let inputs = &mut node["inputs"];
            inputs["tile_size"] = json!(512);
            inputs["overlap"] = json!(64);
            inputs["temporal_size"] = json!(64);
            inputs["temporal_overlap"] = json!(8);
        }
    }
}

pub fn set_output_prefix(map: &mut Map<String, Value>, prefix: &str) {
    for node in map.values_mut() {
        if let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut)
            && inputs.contains_key("filename_prefix")
        {
            inputs.insert("filename_prefix".into(), json!(prefix));
        }
    }
}

/// `"node_id.input" -> value` overrides from the Ultra drawer.
pub fn apply_overrides(
    map: &mut Map<String, Value>,
    overrides: &BTreeMap<String, Value>,
) -> Result<(), BindError> {
    for (key, v) in overrides {
        let (node, input) = key
            .split_once('.')
            .ok_or_else(|| BindError::Template(format!("override {key:?} must be node.input")))?;
        let n = map
            .get_mut(node)
            .ok_or_else(|| BindError::Template(format!("override targets missing node {node}")))?;
        n["inputs"][input] = v.clone();
    }
    Ok(())
}

/// Convenience: resolve and build in one go (used by tests and the dry-run CLI).
pub fn bind(
    manifest: &Manifest,
    spec: &JobSpec,
    template: &Value,
    opts: &BindOptions,
) -> Result<(BTreeMap<String, Value>, Value), BindError> {
    let values = resolve_values(manifest, spec)?;
    let graph = build_graph(manifest, spec, &values, template, opts)?;
    Ok((values, graph))
}

#[cfg(test)]
mod gguf_tests {
    use super::*;

    fn graph() -> Map<String, Value> {
        json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "flux1-dev-Q4_K_S.gguf", "weight_dtype": "default"}},
            "2": {"class_type": "DualCLIPLoader", "inputs": {"clip_name1": "clip_l.safetensors", "clip_name2": "t5-v1_1-xxl-Q5_K_M.gguf", "type": "flux", "device": "default"}},
            "3": {"class_type": "CLIPLoader", "inputs": {"clip_name": "qwen_3_4b.safetensors", "type": "lumina2", "device": "default"}},
            "4": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "seed": 1}},
            "5": {"class_type": "UNETLoader", "inputs": {"unet_name": ["9", 0], "weight_dtype": "default"}}
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn gguf_files_get_gguf_loaders() {
        let mut g = graph();
        gguf_loaders(&mut g).unwrap();
        assert_eq!(
            g["1"],
            json!({"class_type": "UnetLoaderGGUF", "inputs": {"unet_name": "flux1-dev-Q4_K_S.gguf"}})
        );
        assert_eq!(
            g["2"],
            json!({"class_type": "DualCLIPLoaderGGUF", "inputs": {"clip_name1": "clip_l.safetensors", "clip_name2": "t5-v1_1-xxl-Q5_K_M.gguf", "type": "flux"}}),
            "a safetensors encoder can sit next to a GGUF one"
        );
        // Not GGUF: untouched. Links into the loader stay as they were.
        assert_eq!(g["3"], graph()["3"]);
        assert_eq!(g["4"]["inputs"]["model"], json!(["1", 0]));
        assert_eq!(g["5"], graph()["5"], "a linked file name is left alone");
    }

    #[test]
    fn chosen_local_file_replaces_the_workflow_default() {
        let mut g = json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "flux1-dev.safetensors", "weight_dtype": "default"}},
            "2": {"class_type": "DualCLIPLoader", "inputs": {"clip_name1": "clip_l.safetensors", "clip_name2": "t5xxl_fp16.safetensors", "type": "flux"}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "flux1-dev.safetensors"}}
        })
        .as_object()
        .unwrap()
        .clone();
        let files: BTreeMap<String, String> = [
            (
                "flux1-dev.safetensors".to_string(),
                "flux1-dev-Q4_K_S.gguf".to_string(),
            ),
            (
                "t5xxl_fp16.safetensors".to_string(),
                "t5-v1_1-xxl-encoder-Q5_K_M.gguf".to_string(),
            ),
            (
                "clip_l.safetensors".to_string(),
                "clip_l.safetensors".to_string(),
            ),
        ]
        .into();
        use_chosen_files(&mut g, &files);
        gguf_loaders(&mut g).unwrap();
        assert_eq!(
            g["1"],
            json!({"class_type": "UnetLoaderGGUF", "inputs": {"unet_name": "flux1-dev-Q4_K_S.gguf"}})
        );
        assert_eq!(g["2"]["class_type"], "DualCLIPLoaderGGUF");
        assert_eq!(
            g["2"]["inputs"]["clip_name2"],
            "t5-v1_1-xxl-encoder-Q5_K_M.gguf"
        );
        assert_eq!(
            g["3"]["inputs"]["text"], "flux1-dev.safetensors",
            "prompt text is never touched"
        );
        // Built-in roles ("diffusion", "vae") are not file names: nothing to rename.
        let mut g2 = json!({"1": {"class_type": "VAELoader", "inputs": {"vae_name": "vae"}}})
            .as_object()
            .unwrap()
            .clone();
        use_chosen_files(
            &mut g2,
            &[("vae".to_string(), "ae.safetensors".to_string())].into(),
        );
        assert_eq!(g2["1"]["inputs"]["vae_name"], "vae");
    }

    #[test]
    fn workflow_file_architectures() {
        use crate::import::guess_arch;
        assert_eq!(
            guess_arch("flux1-dev.safetensors", "diffusion_models").0,
            vec!["flux"]
        );
        assert_eq!(
            guess_arch("flux2_dev_fp8mixed.safetensors", "diffusion_models").0,
            vec!["flux2"]
        );
        assert_eq!(
            guess_arch("qwen_image_fp8_e4m3fn.safetensors", "diffusion_models").0,
            vec!["qwen_image"]
        );
        assert_eq!(
            guess_arch("z_image_turbo_bf16.safetensors", "diffusion_models").0,
            vec!["lumina2"]
        );
        let (a, part) = guess_arch(
            "wan2.2_t2v_high_noise_14B_fp8_scaled.safetensors",
            "diffusion_models",
        );
        assert_eq!(
            (a, part.as_deref()),
            (vec!["wan2.1".to_string()], Some("high_noise"))
        );
        assert_eq!(
            guess_arch("umt5_xxl_fp8_e4m3fn_scaled.safetensors", "text_encoders")
                .1
                .as_deref(),
            Some("umt5")
        );
        assert_eq!(
            guess_arch("qwen_2.5_vl_7b_fp8_scaled.safetensors", "text_encoders").0[0],
            "qwen2vl"
        );
        assert!(guess_arch("ae.safetensors", "vae").0.is_empty());
        assert!(
            guess_arch("mystery.safetensors", "diffusion_models")
                .0
                .is_empty()
        );
    }

    #[test]
    fn gguf_checkpoint_is_explained() {
        let mut g = json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sdxl-Q8_0.GGUF"}}})
            .as_object()
            .unwrap()
            .clone();
        let e = gguf_loaders(&mut g).unwrap_err().to_string();
        assert!(e.contains("diffusion_models"), "{e}");
    }
}
