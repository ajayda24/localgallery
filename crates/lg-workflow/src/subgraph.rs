//! Flattening editor-format subgraphs (ComfyUI "blueprints") into plain node lists.
//!
//! Subgraph instances are nodes whose `type` is the id of an entry in
//! `definitions.subgraphs`. Inside a definition, links from node -10 come from the
//! instance's inputs and links into node -20 feed its outputs. Instances expose inner
//! widgets through `properties.proxyWidgets` (`[inner_node_id, widget]`, or `["-1", name]`
//! for a subgraph input), with values in `widgets_values` at the same positions.
//!
//! Exposed widgets and value overrides are tracked as handles on `(node, input)`; when a
//! subgraph is expanded, a handle on one of its inputs moves to every inner target.

use crate::BindError;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

const INPUT_NODE: i64 = -10;
const OUTPUT_NODE: i64 = -20;

/// A widget exposed by the top-level instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Exposed {
    pub label: String,
    /// `(node id, input name)` in the flattened workflow.
    pub targets: Vec<(i64, String)>,
}

#[derive(Debug, Default)]
pub struct Flattened {
    /// Editor-format workflow without subgraphs.
    pub workflow: Value,
    /// Widgets the outermost instances exposed, in order.
    pub exposed: Vec<Exposed>,
    /// Values set on instances that must override inner widget values.
    pub overrides: Vec<((i64, String), Value)>,
    /// Name and description of the (single) top-level subgraph, if any.
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
}

type Link = [Value; 6];

fn link_parts(l: &Value) -> Option<(i64, i64, i64, i64, i64, Value)> {
    if let Some(a) = l.as_array() {
        return Some((
            a.first()?.as_i64()?,
            a.get(1)?.as_i64()?,
            a.get(2)?.as_i64()?,
            a.get(3)?.as_i64()?,
            a.get(4)?.as_i64()?,
            a.get(5).cloned().unwrap_or(Value::Null),
        ));
    }
    Some((
        l.get("id")?.as_i64()?,
        l.get("origin_id")?.as_i64()?,
        l.get("origin_slot")?.as_i64()?,
        l.get("target_id")?.as_i64()?,
        l.get("target_slot")?.as_i64()?,
        l.get("type").cloned().unwrap_or(Value::Null),
    ))
}

fn mk(id: i64, o: i64, os: i64, t: i64, ts: i64, ty: Value) -> Link {
    [json!(id), json!(o), json!(os), json!(t), json!(ts), ty]
}

fn node_id(n: &Value) -> i64 {
    n.get("id").and_then(Value::as_i64).unwrap_or(i64::MIN)
}

pub fn has_subgraphs(workflow: &Value) -> bool {
    workflow
        .pointer("/definitions/subgraphs")
        .and_then(Value::as_array)
        .is_some_and(|s| !s.is_empty())
}

pub fn flatten(workflow: &Value) -> Result<Flattened, BindError> {
    let defs: HashMap<String, Value> = workflow
        .pointer("/definitions/subgraphs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d.get("id")?.as_str()?.to_string(), d.clone())))
        .collect();
    let mut nodes: Vec<Value> = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut links: Vec<Link> = workflow
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| link_parts(l).map(|(a, b, c, d, e, f)| mk(a, b, c, d, e, f)))
        .collect();
    let mut next_id = nodes.iter().map(node_id).max().unwrap_or(0).max(0) + 1;
    let mut next_link = links
        .iter()
        .filter_map(|l| l[0].as_i64())
        .max()
        .unwrap_or(0)
        .max(0)
        + 1;

    let mut out = Flattened::default();
    let top_instances: Vec<i64> = nodes
        .iter()
        .filter(|n| is_instance(n, &defs))
        .map(node_id)
        .collect();
    // Unconnected sockets (the user is meant to wire in an image, mask, video...) get a
    // loader node, exposed as an upload or editor field.
    for &inst_id in &top_instances {
        let inst = nodes
            .iter()
            .find(|n| node_id(n) == inst_id)
            .cloned()
            .unwrap();
        for (slot, input) in inst
            .get("inputs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if input.get("widget").is_some() || input.get("link").and_then(Value::as_i64).is_some()
            {
                continue;
            }
            let ty = input
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let first = ty.split(',').next().unwrap_or_default();
            let (class, widgets, widget, out_slot) = match first {
                "IMAGE" => (
                    "LoadImage",
                    json!({"image": "", "upload": "image"}),
                    "image",
                    0,
                ),
                "MASK" => (
                    "LoadImageMask",
                    json!({"image": "", "channel": "red"}),
                    "image",
                    0,
                ),
                "VIDEO" => ("LoadVideo", json!({"file": ""}), "file", 0),
                "AUDIO" => ("LoadAudio", json!({"audio": ""}), "audio", 0),
                "STRING" => ("PrimitiveStringMultiline", json!({"value": ""}), "value", 0),
                "BOUNDING_BOX" => (
                    "CreateBoundingBoxes",
                    json!({"width": 1024, "height": 1024, "editor_state": []}),
                    "editor_state",
                    1,
                ),
                _ => continue,
            };
            let id = next_id;
            next_id += 1;
            let lid = next_link;
            next_link += 1;
            nodes.push(json!({"id": id, "type": class, "mode": 0, "inputs": [], "widgets_values": widgets}));
            links.push(mk(lid, id, out_slot, inst_id, slot as i64, json!(first)));
            let label = input
                .get("label")
                .or(input.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(widget);
            out.exposed.push(Exposed {
                label: label.to_string(),
                targets: vec![(id, widget.to_string())],
            });
        }
    }
    // Handles start on the top-level instances' proxied widgets.
    for inst in nodes.iter().filter(|n| is_instance(n, &defs)) {
        let id = node_id(inst);
        let def = &defs[inst["type"].as_str().unwrap()];
        if top_instances.len() == 1 {
            out.title = inst
                .get("title")
                .and_then(Value::as_str)
                .or(def.get("name").and_then(Value::as_str))
                .map(str::to_string);
            out.description = def
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
            out.category = def
                .get("category")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        let proxies = inst
            .pointer("/properties/proxyWidgets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let values = inst
            .get("widgets_values")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (i, p) in proxies.iter().enumerate() {
            let (Some(inner), Some(widget)) = (
                p.get(0).and_then(Value::as_str),
                p.get(1).and_then(Value::as_str),
            ) else {
                continue;
            };
            if widget == "control_after_generate" {
                continue;
            }
            // "-1" names a subgraph input, which lives on the instance until expanded.
            let target = if inner == "-1" {
                (id, widget.to_string())
            } else {
                (
                    inner
                        .parse::<i64>()
                        .map_err(|_| BindError::Template(format!("bad proxy node id {inner}")))?,
                    widget.to_string(),
                )
            };
            let label = def
                .get("inputs")
                .and_then(Value::as_array)
                .and_then(|ins| ins.iter().find(|x| x["name"] == widget))
                .and_then(|x| x.get("label").or(x.get("name")))
                .and_then(Value::as_str)
                .unwrap_or(widget)
                .to_string();
            // Direct proxies point into this instance's subgraph, so they are re-targeted on expansion too.
            let scoped = if inner == "-1" {
                target.clone()
            } else {
                (encode_scoped(id, target.0), target.1.clone())
            };
            out.exposed.push(Exposed {
                label,
                targets: vec![scoped.clone()],
            });
            if let Some(v) = values.get(i).filter(|v| !v.is_null()) {
                out.overrides.push((scoped, v.clone()));
            }
        }
    }

    for _round in 0..32 {
        let Some(pos) = nodes.iter().position(|n| is_instance(n, &defs)) else {
            break;
        };
        let inst = nodes.remove(pos);
        let inst_id = node_id(&inst);
        let def = defs[inst["type"].as_str().unwrap()].clone();
        let inst_mode = inst.get("mode").and_then(Value::as_i64).unwrap_or(0);
        let sg_inputs: Vec<Value> = def
            .get("inputs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let inst_inputs: Vec<Value> = inst
            .get("inputs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut idmap: HashMap<i64, i64> = HashMap::new();
        let inner_nodes: Vec<Value> = def
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for n in &inner_nodes {
            idmap.insert(node_id(n), next_id);
            next_id += 1;
        }
        // Outer link feeding each instance input, by subgraph input index.
        let outer_in: HashMap<i64, (i64, i64)> = links
            .iter()
            .filter(|l| l[3].as_i64() == Some(inst_id))
            .filter_map(|l| {
                let slot = l[4].as_i64()?;
                Some((slot, (l[1].as_i64()?, l[2].as_i64()?)))
            })
            .collect();
        let input_slot = |k: i64| -> Option<i64> {
            // Instance inputs are matched to subgraph inputs by name; fall back to position.
            let name = sg_inputs.get(k as usize)?.get("name")?.as_str()?;
            inst_inputs
                .iter()
                .position(|i| i["name"] == name)
                .map(|p| p as i64)
                .or(Some(k))
        };

        let mut linkmap: HashMap<i64, Option<i64>> = HashMap::new();
        let mut new_links: Vec<Link> = Vec::new();
        let mut out_src: HashMap<i64, (i64, i64)> = HashMap::new();
        // Inner targets of each subgraph input, for moving handles.
        let mut input_targets: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
        // Editors leave stale entries in `links`; the target's own `link` field and the
        // subgraph inputs' `linkIds` are authoritative.
        let referenced: std::collections::HashSet<i64> = inner_nodes
            .iter()
            .flat_map(|n| {
                n.get("inputs")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|i| i.get("link").and_then(Value::as_i64))
            .collect();
        let input_of_link: HashMap<i64, i64> = sg_inputs
            .iter()
            .enumerate()
            .flat_map(|(k, i)| {
                i.get("linkIds")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(move |l| Some((l.as_i64()?, k as i64)))
            })
            .collect();
        for l in def
            .get("links")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some((id, o, os, t, ts, ty)) = link_parts(l) else {
                continue;
            };
            if t != OUTPUT_NODE && !referenced.contains(&id) {
                continue;
            }
            let os = if o == INPUT_NODE {
                match input_of_link.get(&id) {
                    Some(&k) => k,
                    // Listed by no subgraph input: stale.
                    None if !input_of_link.is_empty() => continue,
                    None => os,
                }
            } else {
                os
            };
            if o == INPUT_NODE {
                let Some(&nt) = idmap.get(&t) else { continue };
                input_targets.entry(os).or_default().push((nt, ts));
                match input_slot(os).and_then(|s| outer_in.get(&s)) {
                    Some(&(oo, oos)) => {
                        let nl = next_link;
                        next_link += 1;
                        new_links.push(mk(nl, oo, oos, nt, ts, ty));
                        linkmap.insert(id, Some(nl));
                    }
                    None => {
                        linkmap.insert(id, None);
                    }
                }
            } else if t == OUTPUT_NODE {
                if let Some(&no) = idmap.get(&o) {
                    out_src.insert(ts, (no, os));
                }
            } else if let (Some(&no), Some(&nt)) = (idmap.get(&o), idmap.get(&t)) {
                let nl = next_link;
                next_link += 1;
                new_links.push(mk(nl, no, os, nt, ts, ty));
                linkmap.insert(id, Some(nl));
            }
        }

        let mut added: Vec<Value> = Vec::new();
        for n in inner_nodes {
            let mut n = n.clone();
            let old = node_id(&n);
            n["id"] = json!(idmap[&old]);
            if inst_mode != 0 && n.get("mode").and_then(Value::as_i64).unwrap_or(0) == 0 {
                n["mode"] = json!(inst_mode);
            }
            if let Some(ins) = n.get_mut("inputs").and_then(Value::as_array_mut) {
                for i in ins {
                    if let Some(l) = i.get("link").and_then(Value::as_i64) {
                        i["link"] = linkmap
                            .get(&l)
                            .copied()
                            .flatten()
                            .map(Value::from)
                            .unwrap_or(Value::Null);
                    }
                }
            }
            added.push(n);
        }

        // Re-point outer consumers of the instance's outputs; drop links into the instance.
        links.retain(|l| l[3].as_i64() != Some(inst_id));
        for l in links.iter_mut() {
            if l[1].as_i64() == Some(inst_id)
                && let Some(&(no, os)) = l[2].as_i64().and_then(|s| out_src.get(&s))
            {
                l[1] = json!(no);
                l[2] = json!(os);
            }
        }
        links.retain(|l| l[1].as_i64() != Some(inst_id));
        links.extend(new_links);

        // Move handles: instance input names -> inner targets; scoped inner ids -> new ids.
        let input_name_to_targets = |name: &str| -> Vec<(i64, String)> {
            let Some(k) = sg_inputs.iter().position(|x| x["name"] == name) else {
                return vec![];
            };
            input_targets
                .get(&(k as i64))
                .into_iter()
                .flatten()
                .filter_map(|&(nt, ts)| {
                    let node = added.iter().find(|n| node_id(n) == nt)?;
                    let input = node.get("inputs")?.get(ts as usize)?;
                    let name = input
                        .pointer("/widget/name")
                        .or(input.get("name"))?
                        .as_str()?;
                    Some((nt, name.to_string()))
                })
                .collect()
        };
        let remap = |h: &(i64, String)| -> Vec<(i64, String)> {
            if h.0 == inst_id {
                return input_name_to_targets(&h.1);
            }
            if let Some((scope, inner)) = decode_scoped(h.0)
                && scope == inst_id
                && let Some(&new) = idmap.get(&inner)
            {
                return vec![(new, h.1.clone())];
            }
            vec![h.clone()]
        };
        for e in out.exposed.iter_mut() {
            e.targets = e.targets.iter().flat_map(remap).collect();
        }
        out.overrides = out
            .overrides
            .iter()
            .flat_map(|(h, v)| remap(h).into_iter().map(move |t| (t, v.clone())))
            .collect();
        // Nested instances' own proxies are handled when they expand: scope them now.
        for n in &added {
            if is_instance(n, &defs) {
                let nid = node_id(n);
                let proxies = n
                    .pointer("/properties/proxyWidgets")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let values = n
                    .get("widgets_values")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (i, p) in proxies.iter().enumerate() {
                    let (Some(inner), Some(widget)) = (
                        p.get(0).and_then(Value::as_str),
                        p.get(1).and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    if widget == "control_after_generate" {
                        continue;
                    }
                    let h = if inner == "-1" {
                        (nid, widget.to_string())
                    } else {
                        (
                            encode_scoped(nid, inner.parse().unwrap_or(0)),
                            widget.to_string(),
                        )
                    };
                    // An outer handle on this nested instance's widget input follows the proxy.
                    for e in out.exposed.iter_mut() {
                        if e.targets.contains(&(nid, widget.to_string())) && h.0 != nid {
                            e.targets.push(h.clone());
                        }
                    }
                    if let Some(v) = values.get(i).filter(|v| !v.is_null()) {
                        out.overrides.push((h, v.clone()));
                    }
                }
            }
        }
        nodes.extend(added);
    }
    if nodes.iter().any(|n| is_instance(n, &defs)) {
        return Err(BindError::Template(
            "subgraphs are nested too deeply".into(),
        ));
    }
    for e in out.exposed.iter_mut() {
        e.targets.retain(|(n, _)| decode_scoped(*n).is_none());
        e.targets.dedup();
    }
    out.overrides
        .retain(|((n, _), _)| decode_scoped(*n).is_none());
    let mut wf: Map<String, Value> = workflow.as_object().cloned().unwrap_or_default();
    wf.remove("definitions");
    wf.insert("nodes".into(), Value::Array(nodes));
    wf.insert(
        "links".into(),
        Value::Array(
            links
                .into_iter()
                .map(|l| Value::Array(l.to_vec()))
                .collect(),
        ),
    );
    out.workflow = Value::Object(wf);
    Ok(out)
}

fn is_instance(n: &Value, defs: &HashMap<String, Value>) -> bool {
    n.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| defs.contains_key(t))
}

// A handle to an inner node of a not-yet-expanded instance is encoded as a negative id.
const SCOPE: i64 = 1 << 24;
fn encode_scoped(instance: i64, inner: i64) -> i64 {
    -(instance * SCOPE + inner) - 1_000_000_000
}
fn decode_scoped(id: i64) -> Option<(i64, i64)> {
    if id > -1_000_000_000 {
        return None;
    }
    let v = -(id + 1_000_000_000);
    Some((v / SCOPE, v % SCOPE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_ids_roundtrip() {
        assert_eq!(decode_scoped(encode_scoped(126, 27)), Some((126, 27)));
        assert_eq!(decode_scoped(42), None);
        assert_eq!(decode_scoped(-10), None);
    }
}
