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
    apply_overrides(map, &spec.overrides)?;
    wrap_list_values(map);
    Ok(graph)
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
