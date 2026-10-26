//! Sample values that exercise every input mechanism of an op. Used by tests and by the
//! "check this model" dry run.

use lg_types::{InputKind, Manifest};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Returns `(values, assets, files)`: input values, asset id -> engine file name, and
/// file role -> file name for the op.
pub fn sample(
    m: &Manifest,
    op: &str,
) -> (Value, BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut values = serde_json::Map::new();
    let mut assets = BTreeMap::new();
    for i in m.inputs_for(op) {
        let v = match i.kind {
            InputKind::Text => json!("a lighthouse on a cliff at dusk"),
            k if k.is_asset() => {
                assets.insert(format!("asset-{}", i.id), format!("lg_{}.png", i.id));
                json!(format!("asset-{}", i.id))
            }
            InputKind::ImageList => {
                assets.insert("ref-1".into(), "lg_ref1.png".into());
                assets.insert("ref-2".into(), "lg_ref2.png".into());
                json!(["ref-1", "ref-2"])
            }
            InputKind::Regions => json!([
                {"x": 0.05, "y": 0.1, "w": 0.4, "h": 0.5, "prompt": "a red boat", "weight": 1.0},
                {"x": 0.55, "y": 0.6, "w": 0.4, "h": 0.2, "prompt": "sign", "type": "text", "text": "OPEN"}]),
            InputKind::LoraStack => json!([{"file": "style.safetensors", "strength": 0.8}]),
            InputKind::ModelFile => match &i.default {
                Some(Value::String(s)) if !s.is_empty() => continue,
                _ => json!(format!("sample_{}.safetensors", i.id)),
            },
            InputKind::StructuredPrompt => json!({
                "short_description": "a lighthouse on a cliff at dusk",
                "objects": ["a white lighthouse, center, beam on"],
                "lighting": {"conditions": "golden hour", "direction": "from the left"},
                "text_render": ["HARBOR"]
            }),
            _ => continue,
        };
        values.insert(i.id.clone(), v);
    }
    let files = m
        .files
        .iter()
        .filter(|f| f.needed_for(op))
        .map(|f| (f.role.clone(), f.variants[0].name.clone()))
        .collect();
    (Value::Object(values), assets, files)
}
