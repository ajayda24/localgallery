//! Placeholder substitution in workflow templates.
//!
//! A JSON string that is exactly `"{{expr}}"` is replaced by the typed value of `expr`
//! (numbers stay numbers, links stay arrays). Placeholders inside longer strings are
//! interpolated as text.
//!
//! Grammar: `root(.field)*(|filter(:arg)?)*`
//! - roots: an input id, `file.<role>`, `item`, `index`, `prev:<slot>`
//! - filters: `json`, `str`, `int`, `float`, `asset`, `default:<literal>`,
//!   `scale:<path>` (multiply by another value), `add:<n>`, `join:<sep>`,
//!   `bbox_state:<size input>` (regions -> ComfyUI bounding-box editor state),
//!   `elements` (regions -> layout elements with 0..1000 `[ymin,xmin,ymax,xmax]` boxes)

use crate::BindError;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default, Clone, Copy)]
pub struct Scope<'a> {
    pub values: Option<&'a BTreeMap<String, Value>>,
    pub files: Option<&'a BTreeMap<String, String>>,
    /// Asset id -> file name as the engine knows it (after upload).
    pub assets: Option<&'a BTreeMap<String, String>>,
    pub item: Option<&'a Value>,
    pub index: Option<usize>,
    pub prev: Option<&'a [Value]>,
}

/// Substitute every placeholder in `v` in place.
pub fn substitute(v: &mut Value, scope: &Scope) -> Result<(), BindError> {
    match v {
        Value::String(s) => {
            if let Some(expr) = whole_placeholder(s) {
                *v = eval(expr, scope)?;
            } else if s.contains("{{") {
                *v = Value::String(interpolate(s, scope)?);
            }
        }
        Value::Array(a) => {
            for x in a {
                substitute(x, scope)?;
            }
        }
        Value::Object(o) => {
            for (_, x) in o.iter_mut() {
                substitute(x, scope)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn whole_placeholder(s: &str) -> Option<&str> {
    let t = s.strip_prefix("{{")?.strip_suffix("}}")?;
    (!t.contains("{{") && !t.contains("}}")).then_some(t.trim())
}

fn interpolate(s: &str, scope: &Scope) -> Result<String, BindError> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| BindError::Template(format!("unclosed placeholder in {s:?}")))?;
        let v = eval(after[..end].trim(), scope)?;
        match v {
            Value::String(t) => out.push_str(&t),
            Value::Null => {}
            other => out.push_str(&other.to_string()),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Evaluate one placeholder expression.
pub fn eval(expr: &str, scope: &Scope) -> Result<Value, BindError> {
    let mut parts = expr.split('|');
    let path = parts.next().unwrap_or("").trim();
    let mut v = lookup(path, scope)?;
    for f in parts {
        let (name, arg) = match f.split_once(':') {
            Some((n, a)) => (n.trim(), Some(a)),
            None => (f.trim(), None),
        };
        v = apply_filter(name, arg, v, scope)?;
    }
    Ok(v)
}

fn lookup(path: &str, scope: &Scope) -> Result<Value, BindError> {
    if let Some(slot) = path.strip_prefix("prev:") {
        let slot: usize = slot
            .parse()
            .map_err(|_| BindError::Template(format!("bad slot in {path}")))?;
        let prev = scope
            .prev
            .ok_or_else(|| BindError::Template("prev: used outside a fragment".into()))?;
        return prev
            .get(slot)
            .cloned()
            .ok_or_else(|| BindError::Template(format!("{path}: chain has no slot {slot}")));
    }
    let mut segs = path.split('.');
    let root = segs.next().unwrap_or("");
    let mut v = match root {
        "item" => scope
            .item
            .cloned()
            .ok_or_else(|| BindError::Template("item used outside a fragment".into()))?,
        "index" => json!(
            scope
                .index
                .ok_or_else(|| BindError::Template("index used outside a fragment".into()))?
        ),
        "file" => {
            let role = segs
                .next()
                .ok_or_else(|| BindError::Template("file needs a role".into()))?;
            let name = scope
                .files
                .and_then(|f| f.get(role))
                .ok_or_else(|| BindError::MissingFile(role.to_string()))?;
            return Ok(Value::String(name.clone()));
        }
        id => scope
            .values
            .and_then(|vals| vals.get(id))
            .cloned()
            .ok_or_else(|| BindError::Template(format!("unknown input {id:?}")))?,
    };
    for seg in segs {
        v = match (&v, seg.parse::<usize>()) {
            (Value::Array(a), Ok(i)) => a.get(i).cloned().unwrap_or(Value::Null),
            _ => v.get(seg).cloned().unwrap_or(Value::Null),
        };
    }
    Ok(v)
}

fn apply_filter(
    name: &str,
    arg: Option<&str>,
    v: Value,
    scope: &Scope,
) -> Result<Value, BindError> {
    let need_arg =
        || arg.ok_or_else(|| BindError::Template(format!("filter {name} needs an argument")));
    Ok(match name {
        "json" => Value::String(serde_json::to_string(&v).unwrap()),
        "str" => match v {
            Value::String(_) => v,
            Value::Null => json!(""),
            other => Value::String(other.to_string()),
        },
        "int" => json!(as_f64(&v, name)?.round() as i64),
        "float" => json!(as_f64(&v, name)?),
        "default" => {
            let empty = v.is_null() || v.as_str().is_some_and(str::is_empty);
            if empty {
                let a = need_arg()?;
                serde_json::from_str(a).unwrap_or_else(|_| Value::String(a.to_string()))
            } else {
                v
            }
        }
        "scale" => {
            let other = lookup(need_arg()?.trim(), scope)?;
            json!(as_f64(&v, name)? * as_f64(&other, name)?)
        }
        "add" => {
            let n: f64 = need_arg()?
                .trim()
                .parse()
                .map_err(|_| BindError::Template("add needs a number".into()))?;
            json!(as_f64(&v, name)? + n)
        }
        "join" => {
            let sep = arg.unwrap_or("\n");
            let items: Vec<String> = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            x.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| x.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            Value::String(items.join(sep))
        }
        "asset" => map_assets(v, scope)?,
        "bbox_state" => {
            let size = lookup(need_arg()?.trim(), scope)?;
            bbox_editor_state(&v, &size)?
        }
        "elements" => layout_elements(&v),
        other => return Err(BindError::Template(format!("unknown filter {other:?}"))),
    })
}

fn as_f64(v: &Value, filter: &str) -> Result<f64, BindError> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| BindError::Template(format!("{filter}: {v} is not a number")))
}

fn map_assets(v: Value, scope: &Scope) -> Result<Value, BindError> {
    let assets = scope
        .assets
        .ok_or_else(|| BindError::Template("no assets available".into()))?;
    let one = |id: &str| {
        assets
            .get(id)
            .cloned()
            .map(Value::String)
            .ok_or_else(|| BindError::MissingAsset(id.to_string()))
    };
    match v {
        Value::String(s) => one(&s),
        Value::Array(a) => a
            .iter()
            .map(|x| one(x.as_str().unwrap_or_default()))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Err(BindError::Template(format!(
            "asset: {other} is not an asset id"
        ))),
    }
}

/// Regions (fractions) -> `CreateBoundingBoxes.editor_state` (pixel boxes with metadata).
fn bbox_editor_state(regions: &Value, size: &Value) -> Result<Value, BindError> {
    let w = size.get("w").and_then(Value::as_f64).unwrap_or(1024.0);
    let h = size.get("h").and_then(Value::as_f64).unwrap_or(1024.0);
    let list = regions.as_array().cloned().unwrap_or_default();
    Ok(Value::Array(
        list.iter()
            .map(|r| {
                let f = |k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                json!({
                    "x": (f("x") * w).round() as i64,
                    "y": (f("y") * h).round() as i64,
                    "width": (f("w") * w).round() as i64,
                    "height": (f("h") * h).round() as i64,
                    "metadata": {
                        "type": r.get("type").and_then(Value::as_str).unwrap_or("obj"),
                        "text": r.get("text").and_then(Value::as_str).unwrap_or(""),
                        "desc": r.get("prompt").and_then(Value::as_str).unwrap_or(""),
                        "palette": r.get("palette").cloned().unwrap_or(json!([])),
                    }
                })
            })
            .collect(),
    ))
}

/// Regions -> layout elements in the 0..1000 `[ymin, xmin, ymax, xmax]` grid used by
/// layout-aware models, matching ComfyUI's `CreateBoundingBoxes` "elements" output.
fn layout_elements(regions: &Value) -> Value {
    let grid = |x: f64| (x * 1000.0).round().clamp(0.0, 1000.0) as i64;
    let list = regions.as_array().cloned().unwrap_or_default();
    Value::Array(
        list.iter()
            .map(|r| {
                let f = |k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                let is_text = r.get("type").and_then(Value::as_str) == Some("text");
                let mut e = json!({
                    "type": if is_text { "text" } else { "obj" },
                    "bbox": [grid(f("y")), grid(f("x")), grid(f("y") + f("h")), grid(f("x") + f("w"))],
                    "desc": r.get("prompt").and_then(Value::as_str).unwrap_or(""),
                });
                if is_text {
                    e["text"] = r.get("text").cloned().unwrap_or(json!(""));
                }
                e
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals() -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("prompt".into(), json!("a cat"));
        m.insert("steps".into(), json!(20));
        m.insert("size".into(), json!({"w": 1000, "h": 500}));
        m.insert("neg".into(), json!(""));
        m.insert("sp".into(), json!({"subject": "cat"}));
        m.insert(
            "regions".into(),
            json!([{"x": 0.1, "y": 0.2, "w": 0.5, "h": 0.4, "prompt": "dog", "type": "obj"}]),
        );
        m.insert("img".into(), json!("asset1"));
        m
    }

    #[test]
    fn typed_and_interpolated() {
        let v = vals();
        let scope = Scope {
            values: Some(&v),
            ..Default::default()
        };
        let mut t = json!({"a": "{{steps}}", "b": "steps={{steps}} {{prompt}}", "c": "{{size.w}}", "d": "{{size.h|scale:size.w}}"});
        substitute(&mut t, &scope).unwrap();
        assert_eq!(
            t,
            json!({"a": 20, "b": "steps=20 a cat", "c": 1000, "d": 500000.0})
        );
    }

    #[test]
    fn filters() {
        let v = vals();
        let mut assets = BTreeMap::new();
        assets.insert("asset1".to_string(), "lg_asset1.png".to_string());
        let scope = Scope {
            values: Some(&v),
            assets: Some(&assets),
            ..Default::default()
        };
        assert_eq!(eval("neg|default:ugly", &scope).unwrap(), json!("ugly"));
        assert_eq!(
            eval("sp|json", &scope).unwrap(),
            json!("{\"subject\":\"cat\"}")
        );
        assert_eq!(eval("img|asset", &scope).unwrap(), json!("lg_asset1.png"));
        assert_eq!(eval("steps|add:0.6|int", &scope).unwrap(), json!(21));
        let state = eval("regions|bbox_state:size", &scope).unwrap();
        assert_eq!(state[0]["x"], json!(100));
        assert_eq!(state[0]["height"], json!(200));
        assert_eq!(state[0]["metadata"]["desc"], json!("dog"));
        let el = eval("regions|elements", &scope).unwrap();
        assert_eq!(el[0]["bbox"], json!([200, 100, 600, 600]));
        assert!(eval("nope", &scope).is_err());
        assert!(eval("prompt|bogus", &scope).is_err());
    }

    #[test]
    fn fragment_scope() {
        let item = json!({"file": "a.safetensors", "strength": 0.7});
        let prev = vec![json!(["4", 0]), json!(["4", 1])];
        let scope = Scope {
            item: Some(&item),
            index: Some(2),
            prev: Some(&prev),
            ..Default::default()
        };
        let mut t = json!({"model": "{{prev:0}}", "clip": "{{prev:1}}", "name": "{{item.file}}", "s": "{{item.strength}}", "i": "{{index}}"});
        substitute(&mut t, &scope).unwrap();
        assert_eq!(
            t,
            json!({"model": ["4", 0], "clip": ["4", 1], "name": "a.safetensors", "s": 0.7, "i": 2})
        );
    }
}
