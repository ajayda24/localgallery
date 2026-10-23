//! Repeat-chain graph expansion (see [`lg_types::Expander`]).

use crate::BindError;
use crate::template::{Scope, substitute};
use lg_types::Expander;
use serde_json::{Map, Value, json};

/// A link is `[node_id, slot]`. Node ids are compared as strings.
pub fn as_link(v: &Value) -> Option<(String, u64)> {
    let a = v.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let id = match &a[0] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    Some((id, a[1].as_u64()?))
}

pub fn expand(
    graph: &mut Map<String, Value>,
    exp: &Expander,
    exp_index: usize,
    base: &Scope,
) -> Result<(), BindError> {
    let items = base
        .values
        .and_then(|v| v.get(&exp.input))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return Ok(());
    }
    let chain: Vec<(String, u64)> = exp
        .chain
        .iter()
        .map(|l| as_link(l).ok_or_else(|| BindError::Template(format!("bad chain link {l}"))))
        .collect::<Result<_, _>>()?;
    for (id, _) in &chain {
        if !graph.contains_key(id) {
            return Err(BindError::Template(format!(
                "chain refers to missing node {id}"
            )));
        }
    }
    if !exp.fragment.contains_key(&exp.head) {
        return Err(BindError::Template(format!(
            "fragment has no head node {:?}",
            exp.head
        )));
    }

    let original: Vec<String> = graph.keys().cloned().collect();
    let mut prev: Vec<Value> = chain.iter().map(|(id, s)| json!([id, s])).collect();
    let mut added = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let id_of = |name: &str| format!("lg{exp_index}_{i}_{name}");
        for (name, node) in &exp.fragment {
            let mut n = node.clone();
            let scope = Scope {
                item: Some(item),
                index: Some(i),
                prev: Some(&prev),
                ..*base
            };
            substitute(&mut n, &scope)?;
            resolve_local_refs(&mut n, &id_of, exp)?;
            added.push((id_of(name), n));
        }
        let head = id_of(&exp.head);
        prev = (0..chain.len()).map(|k| json!([head, k])).collect();
    }

    for id in &original {
        if let Some(inputs) = graph
            .get_mut(id)
            .and_then(|n| n.get_mut("inputs"))
            .and_then(Value::as_object_mut)
        {
            for (_, v) in inputs.iter_mut() {
                if let Some(link) = as_link(v)
                    && let Some(k) = chain.iter().position(|c| *c == link)
                {
                    *v = prev[k].clone();
                }
            }
        }
    }
    for (id, n) in added {
        graph.insert(id, n);
    }
    Ok(())
}

/// Replace `["@name", slot]` with the generated node id.
fn resolve_local_refs(
    v: &mut Value,
    id_of: &dyn Fn(&str) -> String,
    exp: &Expander,
) -> Result<(), BindError> {
    match v {
        Value::Array(a) => {
            if a.len() == 2
                && let Some(name) = a[0].as_str().and_then(|s| s.strip_prefix('@'))
            {
                if !exp.fragment.contains_key(name) {
                    return Err(BindError::Template(format!(
                        "fragment has no node {name:?}"
                    )));
                }
                a[0] = Value::String(id_of(name));
                return Ok(());
            }
            for x in a {
                resolve_local_refs(x, id_of, exp)?;
            }
        }
        Value::Object(o) => {
            for (_, x) in o.iter_mut() {
                resolve_local_refs(x, id_of, exp)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn lora_expander() -> Expander {
        serde_json::from_value(json!({
            "input": "loras",
            "chain": [["4", 0], ["4", 1]],
            "head": "lora",
            "fragment": {"lora": {"class_type": "LoraLoader", "inputs": {
                "model": "{{prev:0}}", "clip": "{{prev:1}}", "lora_name": "{{item.file}}",
                "strength_model": "{{item.strength}}", "strength_clip": "{{item.clip_strength}}"}}}
        }))
        .unwrap()
    }

    fn base_graph() -> Map<String, Value> {
        json!({
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "x"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "p", "clip": ["4", 1]}},
            "3": {"class_type": "KSampler", "inputs": {"model": ["4", 0], "positive": ["6", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"vae": ["4", 2]}}
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn lora_chain_rewires_consumers() {
        let mut g = base_graph();
        let mut v = BTreeMap::new();
        v.insert(
            "loras".into(),
            json!([{"file": "a", "strength": 0.5, "clip_strength": 0.5}, {"file": "b", "strength": 1.0, "clip_strength": 0.2}]),
        );
        let scope = Scope {
            values: Some(&v),
            ..Default::default()
        };
        expand(&mut g, &lora_expander(), 0, &scope).unwrap();
        assert_eq!(g["lg0_0_lora"]["inputs"]["model"], json!(["4", 0]));
        assert_eq!(g["lg0_1_lora"]["inputs"]["model"], json!(["lg0_0_lora", 0]));
        assert_eq!(g["lg0_1_lora"]["inputs"]["lora_name"], json!("b"));
        assert_eq!(g["3"]["inputs"]["model"], json!(["lg0_1_lora", 0]));
        assert_eq!(g["6"]["inputs"]["clip"], json!(["lg0_1_lora", 1]));
        assert_eq!(
            g["8"]["inputs"]["vae"],
            json!(["4", 2]),
            "unrelated slot untouched"
        );
    }

    #[test]
    fn empty_list_is_noop() {
        let mut g = base_graph();
        let before = g.clone();
        let mut v = BTreeMap::new();
        v.insert("loras".into(), json!([]));
        expand(
            &mut g,
            &lora_expander(),
            0,
            &Scope {
                values: Some(&v),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(g, before);
    }

    #[test]
    fn multi_node_fragment_with_local_refs() {
        let exp: Expander = serde_json::from_value(json!({
            "input": "regions", "chain": [["6", 0]], "head": "combine",
            "fragment": {
                "enc": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{item.prompt}}", "clip": ["4", 1]}},
                "area": {"class_type": "ConditioningSetAreaPercentage", "inputs": {
                    "conditioning": ["@enc", 0], "x": "{{item.x}}", "y": "{{item.y}}",
                    "width": "{{item.w}}", "height": "{{item.h}}", "strength": "{{item.weight}}"}},
                "combine": {"class_type": "ConditioningCombine", "inputs": {"conditioning_1": "{{prev:0}}", "conditioning_2": ["@area", 0]}}
            }
        }))
        .unwrap();
        let mut g = base_graph();
        let mut v = BTreeMap::new();
        v.insert("regions".into(), json!([{"x": 0.0, "y": 0.0, "w": 0.5, "h": 1.0, "prompt": "{{steps}} dog", "weight": 1.0}]));
        expand(
            &mut g,
            &exp,
            1,
            &Scope {
                values: Some(&v),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            g["lg1_0_area"]["inputs"]["conditioning"],
            json!(["lg1_0_enc", 0])
        );
        assert_eq!(
            g["lg1_0_combine"]["inputs"]["conditioning_2"],
            json!(["lg1_0_area", 0])
        );
        assert_eq!(g["3"]["inputs"]["positive"], json!(["lg1_0_combine", 0]));
        assert_eq!(
            g["lg1_0_enc"]["inputs"]["text"],
            json!("{{steps}} dog"),
            "user text is never re-evaluated"
        );
    }
}
