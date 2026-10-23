//! Fill defaults, validate and normalise job values against a manifest.

use crate::BindError;
use lg_types::{InputKind, InputSpec, JobSpec, Manifest};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Largest seed ComfyUI accepts that is also safe in JavaScript numbers.
pub const MAX_SEED: i64 = (1 << 53) - 1;

/// Returns the values every template placeholder can rely on: each input of the op
/// has a value of the right type. Seeds of -1 become concrete random seeds.
pub fn resolve_values(
    manifest: &Manifest,
    spec: &JobSpec,
) -> Result<BTreeMap<String, Value>, BindError> {
    if !manifest.ops.contains_key(&spec.op) {
        return Err(BindError::UnknownOp(spec.op.clone()));
    }
    let op = &manifest.ops[&spec.op];
    let mut out = BTreeMap::new();
    let mut problems = Vec::new();
    for input in manifest.inputs_for(&spec.op) {
        let raw = spec
            .values
            .get(&input.id)
            .cloned()
            .filter(|v| !v.is_null())
            .or_else(|| input.default.clone());
        let needed = input.required || op.requires.contains(&input.id);
        match normalise(input, raw) {
            Ok(Some(v)) => {
                out.insert(input.id.clone(), v);
            }
            Ok(None) if needed => problems.push(format!("{} is required", input.label)),
            Ok(None) => {
                out.insert(input.id.clone(), empty_value(input.kind));
            }
            Err(e) => problems.push(format!("{}: {e}", input.label)),
        }
    }
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(BindError::Invalid(problems))
    }
}

fn empty_value(kind: InputKind) -> Value {
    match kind {
        InputKind::Regions | InputKind::ImageList | InputKind::LoraStack => json!([]),
        InputKind::StructuredPrompt => json!({}),
        InputKind::Bool => json!(false),
        InputKind::Int | InputKind::Float | InputKind::Seed => json!(0),
        _ => json!(""),
    }
}

/// `Ok(None)` means "no value given".
fn normalise(input: &InputSpec, raw: Option<Value>) -> Result<Option<Value>, String> {
    let Some(v) = raw else { return Ok(None) };
    Ok(Some(match input.kind {
        InputKind::Text | InputKind::NegativeText | InputKind::Raw => match v {
            Value::String(s) => Value::String(s),
            other => Value::String(other.to_string()),
        },
        InputKind::Seed => {
            let s = v.as_i64().ok_or("seed must be an integer")?;
            if s < 0 {
                json!(rand::random_range(0..MAX_SEED))
            } else {
                json!(s.min(MAX_SEED))
            }
        }
        InputKind::Int => {
            let n = v.as_f64().ok_or("must be a number")?.round();
            json!(clamp(input, n) as i64)
        }
        InputKind::Float => json!(clamp(input, v.as_f64().ok_or("must be a number")?)),
        InputKind::Bool => json!(v.as_bool().ok_or("must be true or false")?),
        InputKind::Enum | InputKind::Motion => {
            if !input.options.is_empty() && !input.options.iter().any(|o| option_value(o) == &v) {
                return Err(format!("{v} is not one of the allowed choices"));
            }
            v
        }
        InputKind::Size => {
            let w = v.get("w").and_then(Value::as_f64).ok_or("size needs w")?;
            let h = v.get("h").and_then(Value::as_f64).ok_or("size needs h")?;
            let m = input
                .extra
                .get("multiple_of")
                .and_then(Value::as_f64)
                .unwrap_or(8.0);
            let round = |x: f64| ((x / m).round() * m).max(m);
            let (w, h) = (round(w), round(h));
            if let Some(max_px) = input.extra.get("max_pixels").and_then(Value::as_f64)
                && w * h > max_px * 1.0001
            {
                return Err(format!("{w}x{h} is larger than this model allows"));
            }
            json!({"w": w as i64, "h": h as i64})
        }
        InputKind::StructuredPrompt => match v {
            Value::String(s) if s.trim().is_empty() => return Ok(None),
            Value::String(s) => {
                serde_json::from_str(&s).map_err(|e| format!("invalid JSON: {e}"))?
            }
            Value::Object(_) => v,
            _ => return Err("must be a JSON object".into()),
        },
        InputKind::Regions => {
            let arr = v.as_array().ok_or("must be a list of boxes")?;
            let mut boxes = Vec::new();
            for r in arr {
                let mut region: lg_types::Region =
                    serde_json::from_value(r.clone()).map_err(|e| format!("bad box: {e}"))?;
                // Normalise negative sizes and clamp to the canvas.
                if region.w < 0.0 {
                    region.x += region.w;
                    region.w = -region.w;
                }
                if region.h < 0.0 {
                    region.y += region.h;
                    region.h = -region.h;
                }
                region.x = region.x.clamp(0.0, 1.0);
                region.y = region.y.clamp(0.0, 1.0);
                region.w = region.w.clamp(0.0, 1.0 - region.x);
                region.h = region.h.clamp(0.0, 1.0 - region.y);
                if region.w > 0.0 && region.h > 0.0 {
                    boxes.push(serde_json::to_value(region).unwrap());
                }
            }
            if let Some(max) = input.extra.get("max_items").and_then(Value::as_u64) {
                boxes.truncate(max as usize);
            }
            Value::Array(boxes)
        }
        InputKind::ImageList => {
            let arr = match v {
                Value::Array(a) => a,
                Value::String(s) if s.is_empty() => vec![],
                Value::String(s) => vec![Value::String(s)],
                _ => return Err("must be a list of images".into()),
            };
            if arr.iter().any(|x| !x.is_string()) {
                return Err("images must be uploaded assets".into());
            }
            if let Some(max) = input.extra.get("max_items").and_then(Value::as_u64)
                && arr.len() as u64 > max
            {
                return Err(format!("at most {max} images"));
            }
            Value::Array(arr)
        }
        InputKind::LoraStack => {
            let arr = v.as_array().ok_or("must be a list")?;
            let mut out = Vec::new();
            for l in arr {
                let file = l
                    .get("file")
                    .and_then(Value::as_str)
                    .ok_or("LoRA needs a file")?;
                if file.is_empty() {
                    continue;
                }
                let strength = l.get("strength").and_then(Value::as_f64).unwrap_or(1.0);
                let clip = l
                    .get("clip_strength")
                    .and_then(Value::as_f64)
                    .unwrap_or(strength);
                out.push(json!({"file": file, "strength": strength, "clip_strength": clip}));
            }
            Value::Array(out)
        }
        k if k.is_asset() => match v {
            Value::String(s) if s.is_empty() => return Ok(None),
            Value::String(_) => v,
            _ => return Err("must be an uploaded file".into()),
        },
        InputKind::ModelFile => match v {
            Value::String(s) if s.is_empty() => return Ok(None),
            Value::String(_) => v,
            _ => return Err("must be a file name".into()),
        },
        _ => v,
    }))
}

fn clamp(input: &InputSpec, n: f64) -> f64 {
    let n = match input.min {
        Some(min) => n.max(min),
        None => n,
    };
    match input.max {
        Some(max) => n.min(max),
        None => n,
    }
}

/// Enum options are either plain values or `{"value": .., "label": ..}`.
pub fn option_value(o: &Value) -> &Value {
    o.get("value").unwrap_or(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "schema": 1, "id": "m", "display_name": "M", "family": "f", "media": "image",
            "ops": {"txt2img": {"workflow": {}}, "img2img": {"workflow": {}, "requires": ["image"]}},
            "inputs": [
                {"id": "prompt", "kind": "text", "label": "Prompt", "required": true},
                {"id": "negative", "kind": "negative_text", "label": "Negative"},
                {"id": "seed", "kind": "seed", "label": "Seed", "default": -1},
                {"id": "steps", "kind": "int", "label": "Steps", "default": 20, "min": 1, "max": 50},
                {"id": "size", "kind": "size", "label": "Size", "default": {"w": 1024, "h": 1024},
                 "extra": {"multiple_of": 16, "max_pixels": 1048576}},
                {"id": "sampler", "kind": "enum", "label": "Sampler", "default": "euler", "options": ["euler", "dpmpp_2m"]},
                {"id": "image", "kind": "image", "label": "Image", "ops": ["img2img"]},
                {"id": "regions", "kind": "regions", "label": "Regions"},
                {"id": "loras", "kind": "lora_stack", "label": "LoRAs"}
            ]
        }))
        .unwrap()
    }

    fn spec(op: &str, values: Value) -> JobSpec {
        serde_json::from_value(json!({"model": "m", "op": op, "values": values})).unwrap()
    }

    #[test]
    fn fills_defaults_and_random_seed() {
        let v = resolve_values(&manifest(), &spec("txt2img", json!({"prompt": "cat"}))).unwrap();
        assert_eq!(v["negative"], json!(""));
        assert_eq!(v["steps"], json!(20));
        assert!(v["seed"].as_i64().unwrap() >= 0);
        assert_eq!(v["regions"], json!([]));
        assert!(
            !v.contains_key("image"),
            "img2img-only input must not leak into txt2img"
        );
    }

    #[test]
    fn clamps_and_rounds() {
        let v = resolve_values(
            &manifest(),
            &spec(
                "txt2img",
                json!({"prompt": "x", "steps": 500, "size": {"w": 1000, "h": 770}}),
            ),
        )
        .unwrap();
        assert_eq!(v["steps"], json!(50));
        assert_eq!(v["size"], json!({"w": 1008, "h": 768}));
    }

    #[test]
    fn rejects_bad_values() {
        let err = resolve_values(
            &manifest(),
            &spec(
                "txt2img",
                json!({"sampler": "nope", "size": {"w": 2048, "h": 2048}}),
            ),
        )
        .unwrap_err();
        let BindError::Invalid(p) = err else { panic!() };
        assert_eq!(p.len(), 3, "{p:?}"); // prompt missing, sampler, size
    }

    #[test]
    fn op_requires_input() {
        let err =
            resolve_values(&manifest(), &spec("img2img", json!({"prompt": "x"}))).unwrap_err();
        assert!(matches!(err, BindError::Invalid(p) if p[0].contains("Image")));
    }

    #[test]
    fn normalises_regions() {
        let v = resolve_values(
            &manifest(),
            &spec(
                "txt2img",
                json!({"prompt": "x", "regions": [
                    {"x": 0.5, "y": 0.5, "w": -0.25, "h": 0.8, "prompt": "dog"},
                    {"x": 0.1, "y": 0.1, "w": 0.0, "h": 0.3}
                ]}),
            ),
        )
        .unwrap();
        let r = v["regions"].as_array().unwrap();
        assert_eq!(r.len(), 1, "zero-size box dropped");
        assert_eq!(r[0]["x"], json!(0.25));
        assert_eq!(r[0]["w"], json!(0.25));
        assert!(
            (r[0]["h"].as_f64().unwrap() - 0.5).abs() < 1e-9,
            "clamped to canvas"
        );
    }
}
