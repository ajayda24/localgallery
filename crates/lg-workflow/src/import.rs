//! Importing arbitrary ComfyUI workflows and publishing them as simple tools.
//!
//! Accepts both API-format graphs and the editor ("UI") format, which is converted
//! using `/object_info` to know which values belong to which widget.

use crate::BindError;
use crate::expand::as_link;
use crate::objectinfo::{InputDef, inputs_of};
use lg_types::{InputKind, InputSpec, Level, Manifest, Media, OpSpec, Origin, WorkflowRef};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

/// Nodes that only exist in the editor.
const EDITOR_ONLY: &[&str] = &["Note", "MarkdownNote", "PrimitiveNode", "Label (rgthree)"];

pub fn is_api_format(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| !o.is_empty() && o.values().all(|n| n.get("class_type").is_some()))
}

/// Return an API-format graph from either format.
pub fn to_api(workflow: &Value, object_info: &Value) -> Result<Value, BindError> {
    if is_api_format(workflow) {
        return Ok(workflow.clone());
    }
    // Some exports wrap the API graph in {"prompt": ...}.
    if let Some(p) = workflow.get("prompt").filter(|p| is_api_format(p)) {
        return Ok(p.clone());
    }
    let nodes = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| BindError::Template("not a ComfyUI workflow (no nodes)".into()))?;
    if crate::subgraph::has_subgraphs(workflow) {
        return import(workflow, object_info).map(|i| i.graph);
    }

    // link id -> (from node, from slot)
    let mut links: HashMap<i64, (i64, u64)> = HashMap::new();
    for l in workflow
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(a) = l.as_array() {
            if let (Some(id), Some(from), Some(slot)) = (
                a.first().and_then(Value::as_i64),
                a.get(1).and_then(Value::as_i64),
                a.get(2).and_then(Value::as_u64),
            ) {
                links.insert(id, (from, slot));
            }
        } else if let (Some(id), Some(from), Some(slot)) = (
            l.get("id").and_then(Value::as_i64),
            l.get("origin_id").and_then(Value::as_i64),
            l.get("origin_slot").and_then(Value::as_u64),
        ) {
            links.insert(id, (from, slot));
        }
    }
    let by_id: HashMap<i64, &Value> = nodes
        .iter()
        .filter_map(|n| Some((n.get("id")?.as_i64()?, n)))
        .collect();

    let mut missing = Vec::new();
    let mut out = Map::new();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let class = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let mode = node.get("mode").and_then(Value::as_i64).unwrap_or(0);
        if mode == 2 || mode == 4 || class == "Reroute" || EDITOR_ONLY.contains(&class) {
            continue;
        }
        let Some(defs) = inputs_of(object_info, class) else {
            missing.push(class.to_string());
            continue;
        };
        let ui_inputs: Vec<&Value> = node
            .get("inputs")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let linked = |name: &str| -> Option<i64> {
            ui_inputs
                .iter()
                .find(|i| i.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|i| i.get("link"))
                .and_then(Value::as_i64)
        };
        let widgets = node.get("widgets_values");
        let mut wi = 0usize;
        let mut inputs = Map::new();
        let ctx = Fill {
            widgets,
            linked: &linked,
            links: &links,
            by_id: &by_id,
        };
        fill(&ctx, &defs, "", &mut wi, &mut inputs);
        let mut n = json!({"class_type": class, "inputs": inputs});
        if let Some(title) = node.get("title").and_then(Value::as_str) {
            n["_meta"] = json!({"title": title});
        }
        out.insert(id.to_string(), n);
    }
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(BindError::Template(format!(
            "the engine is missing these node types (install their custom nodes): {}",
            missing.join(", ")
        )));
    }
    Ok(Value::Object(out))
}

struct Fill<'a> {
    widgets: Option<&'a Value>,
    linked: &'a dyn Fn(&str) -> Option<i64>,
    links: &'a HashMap<i64, (i64, u64)>,
    by_id: &'a HashMap<i64, &'a Value>,
}

/// Assign widget values (in order) and links to `defs`, recursing into the inputs a
/// dynamic combo's chosen option adds (`<combo>.<sub>`).
fn fill(
    c: &Fill,
    defs: &[InputDef],
    prefix: &str,
    wi: &mut usize,
    inputs: &mut Map<String, Value>,
) {
    for def in defs {
        let name = format!("{prefix}{}", def.name);
        let src = (c.linked)(&name).and_then(|l| resolve_source(l, c.links, c.by_id, &def.ty));
        if def.is_widget() {
            let wv = match c.widgets {
                Some(Value::Object(o)) => o.get(&name).cloned(),
                Some(Value::Array(a)) => {
                    let v = a.get(*wi).cloned();
                    *wi += 1 + usize::from(def.control_after_generate);
                    v
                }
                _ => None,
            };
            // A widget linked to an editor-only primitive keeps its value in widgets_values.
            match (src, wv.clone()) {
                (Some(s), _) => {
                    inputs.insert(name.clone(), s);
                }
                (None, Some(v)) => {
                    inputs.insert(name.clone(), v);
                }
                _ => {}
            }
            if !def.sub.is_empty()
                && let Some(key) = inputs.get(&name).cloned()
                && let Some((_, sub)) = def.sub.iter().find(|(k, _)| *k == key)
            {
                fill(c, sub, &format!("{name}."), wi, inputs);
            }
        } else if let Some(s) = src {
            inputs.insert(name, s);
        }
    }
}

/// A workflow converted to API format, with what its author chose to expose.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Imported {
    pub graph: Value,
    /// Widgets exposed by subgraph instances (blueprints), as `(label, [(node, input)])`.
    pub exposed: Vec<(String, Vec<(String, String)>)>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    /// Model files referenced by loader nodes (`properties.models`).
    pub models: Vec<ModelRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRef {
    pub name: String,
    pub url: Option<String>,
    pub directory: String,
}

/// Convert any workflow (API, editor format, or subgraph blueprint) to API format.
pub fn import(workflow: &Value, object_info: &Value) -> Result<Imported, BindError> {
    if !crate::subgraph::has_subgraphs(workflow) {
        let graph = to_api(workflow, object_info)?;
        return Ok(Imported {
            models: collect_models(workflow),
            graph,
            exposed: vec![],
            title: None,
            description: None,
            category: None,
        });
    }
    let flat = crate::subgraph::flatten(workflow)?;
    let mut graph = to_api(&flat.workflow, object_info)?;
    prune_unfed(&mut graph, object_info);
    let nodes = graph.as_object_mut().expect("api graph is an object");
    for ((node, input), v) in &flat.overrides {
        if let Some(inputs) = nodes
            .get_mut(&node.to_string())
            .and_then(|n| n.get_mut("inputs"))
            .and_then(Value::as_object_mut)
        {
            // Never overwrite a link with a widget value.
            if inputs.get(input).is_none_or(|cur| as_link(cur).is_none()) {
                inputs.insert(input.clone(), v.clone());
            }
        }
    }
    let exposed = flat
        .exposed
        .iter()
        .map(|e| {
            let targets: Vec<(String, String)> = e
                .targets
                .iter()
                .map(|(n, i)| (n.to_string(), i.clone()))
                .filter(|(n, i)| {
                    nodes
                        .get(n)
                        .and_then(|x| x["inputs"].get(i))
                        .is_some_and(|v| as_link(v).is_none())
                })
                .collect();
            (e.label.clone(), targets)
        })
        .filter(|(_, t)| !t.is_empty())
        .collect();
    Ok(Imported {
        models: collect_models(&flat.workflow),
        graph,
        exposed,
        title: flat.title,
        description: flat.description,
        category: flat.category,
    })
}

/// Remove file loaders left empty (optional uploads not given) and everything that
/// can no longer run without them.
pub fn drop_empty_loaders(graph: &mut Value, object_info: &Value) {
    if let Some(nodes) = graph.as_object_mut() {
        nodes.retain(|_, n| {
            let empty = |k: &str| n["inputs"].get(k).and_then(Value::as_str) == Some("");
            !match n["class_type"].as_str().unwrap_or_default() {
                "LoadImage" | "LoadImageMask" => empty("image"),
                "LoadVideo" => empty("file"),
                "LoadAudio" => empty("audio"),
                _ => false,
            }
        });
    }
    prune_unfed(graph, object_info);
}

/// Blueprints leave some subgraph inputs unconnected on purpose (optional extras,
/// debug previews). Drop nodes missing a required socket input, then optional links to
/// dropped nodes, until nothing changes; the editor does the same at queue time.
pub fn prune_unfed(graph: &mut Value, object_info: &Value) {
    let Some(nodes) = graph.as_object_mut() else {
        return;
    };
    loop {
        let dead: Vec<String> = nodes
            .iter()
            .filter(|(_, n)| {
                let class = n["class_type"].as_str().unwrap_or_default();
                inputs_of(object_info, class)
                    .unwrap_or_default()
                    .iter()
                    .any(|d| d.required && !d.is_widget() && n["inputs"].get(&d.name).is_none())
            })
            .map(|(k, _)| k.clone())
            .collect();
        let mut changed = !dead.is_empty();
        for k in &dead {
            nodes.remove(k);
        }
        let alive: std::collections::HashSet<String> = nodes.keys().cloned().collect();
        for n in nodes.values_mut() {
            let defs = inputs_of(object_info, n["class_type"].as_str().unwrap_or_default())
                .unwrap_or_default();
            if let Some(inputs) = n.get_mut("inputs").and_then(Value::as_object_mut) {
                let cut: Vec<String> = inputs
                    .iter()
                    .filter(|(_, v)| {
                        as_link(v)
                            .filter(|_| v[0].is_string())
                            .is_some_and(|(src, _)| !alive.contains(&src))
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in cut {
                    changed = true;
                    // A widget that was fed by a dropped node falls back to its default.
                    match defs.iter().find(|d| d.name == k && d.is_widget()) {
                        Some(d) => {
                            let v = d.default.clone().unwrap_or_else(|| match d.ty.as_str() {
                                "STRING" => json!(""),
                                "BOOLEAN" => json!(false),
                                "COMBO" => d.options.first().cloned().unwrap_or(json!("")),
                                _ => json!(0),
                            });
                            inputs.insert(k, v);
                        }
                        None => {
                            inputs.remove(&k);
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn collect_models(workflow: &Value) -> Vec<ModelRef> {
    let mut out: Vec<ModelRef> = Vec::new();
    let mut visit = |nodes: Option<&Vec<Value>>| {
        for n in nodes.into_iter().flatten() {
            for m in n
                .pointer("/properties/models")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Ok(r) = serde_json::from_value::<ModelRef>(m.clone())
                    && !out
                        .iter()
                        .any(|o| o.name == r.name && o.directory == r.directory)
                {
                    out.push(r);
                }
            }
        }
    };
    visit(workflow.get("nodes").and_then(Value::as_array));
    for sg in workflow
        .pointer("/definitions/subgraphs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        visit(sg.get("nodes").and_then(Value::as_array));
    }
    out
}

/// Build a ready-to-use tool from an imported blueprint: its exposed widgets become
/// the form, and its referenced model files become downloads.
pub fn blueprint_tool(
    imp: &Imported,
    object_info: &Value,
    fallback_name: &str,
) -> Result<Manifest, BindError> {
    let candidates = analyze(&imp.graph, object_info);
    let mut fields: Vec<ToolField> = Vec::new();
    let mut used = std::collections::HashSet::new();
    for (label, targets) in &imp.exposed {
        let (node, input) = &targets[0];
        let Some(c) = candidates
            .iter()
            .find(|c| &c.node == node && &c.input == input)
        else {
            continue;
        };
        let mut kind = c.kind;
        let mut level = if c.suggested
            || matches!(kind, InputKind::Text | InputKind::Image | InputKind::Mask)
        {
            Level::Simple
        } else {
            Level::Advanced
        };
        if kind == InputKind::Enum && input.ends_with("_name") {
            kind = InputKind::ModelFile;
            level = Level::Ultra;
        }
        // The author's label ("width", "distill_lora") beats the raw widget name ("value").
        let mut base: String = label
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        base = base.trim_matches('_').to_string();
        match kind {
            InputKind::Text if !used.contains("prompt") => base = "prompt".into(),
            InputKind::Seed if !used.contains("seed") => base = "seed".into(),
            InputKind::Image => base = "image".into(),
            InputKind::Mask => base = "mask".into(),
            InputKind::Video => base = "video".into(),
            InputKind::Audio => base = "audio".into(),
            InputKind::Regions => base = "regions".into(),
            _ => {}
        }
        // Names the template language reserves.
        if matches!(
            base.as_str(),
            "file" | "item" | "index" | "lbrace" | "rbrace"
        ) || base.starts_with("prev")
        {
            base.push_str("_in");
        }
        let mut id = base.clone();
        let mut n = 2;
        while !used.insert(id.clone()) {
            id = format!("{base}_{n}");
            n += 1;
        }
        fields.push(ToolField {
            id,
            label: if label.chars().next().is_some_and(char::is_lowercase) {
                humanize(label)
            } else {
                label.clone()
            },
            kind,
            level,
            targets: targets
                .iter()
                .map(|(n, i)| FieldTarget {
                    node: n.clone(),
                    input: i.clone(),
                    part: None,
                })
                .collect(),
        });
    }
    // Pair width/height into one size field.
    if let (Some(wi), Some(hi)) = (
        fields.iter().position(|f| f.id == "width"),
        fields.iter().position(|f| f.id == "height"),
    ) && fields[wi].targets.len() == 1
        && fields[hi].targets.len() == 1
    {
        let mut w = fields[wi].targets[0].clone();
        let mut h = fields[hi].targets[0].clone();
        w.part = Some("w".into());
        h.part = Some("h".into());
        let size = ToolField {
            id: "size".into(),
            label: "Size".into(),
            kind: InputKind::Size,
            level: Level::Simple,
            targets: vec![w, h],
        };
        let (a, b) = (wi.max(hi), wi.min(hi));
        fields.remove(a);
        fields.remove(b);
        fields.insert(b, size);
    }
    let name = imp
        .title
        .clone()
        .unwrap_or_else(|| fallback_name.to_string());
    let mut m = publish_tool(
        &imp.graph,
        object_info,
        &fields,
        &ToolMeta {
            name,
            description: imp.description.clone().unwrap_or_default(),
        },
    )?;
    for f in &fields {
        if f.kind == InputKind::ModelFile
            && let Some(spec) = m.inputs.iter_mut().find(|i| i.id == f.id)
        {
            let folder = imp
                .models
                .iter()
                .find(|r| Some(&Value::String(r.name.clone())) == spec.default.as_ref())
                .map(|r| r.directory.clone());
            if let Some(folder) = folder {
                spec.extra.insert("folder".into(), json!(folder));
            }
        }
    }
    if let Some(cat) = &imp.category {
        m.tags.push(cat.clone());
    }
    m.files = imp
        .models
        .iter()
        .map(|r| lg_types::ModelFile {
            role: r.name.clone(),
            folder: r.directory.clone(),
            optional: false,
            variants: vec![lg_types::FileVariant {
                name: r.name.clone(),
                precision: guess_precision(&r.name),
                size_gb: 0.0,
                url: r.url.clone(),
                sha256: None,
            }],
        })
        .collect();
    Ok(m)
}

/// Weight precision from a model file name (`..._fp8_scaled`, `...-Q4_K_M.gguf`, ...).
pub fn guess_precision(name: &str) -> lg_types::Precision {
    use lg_types::Precision::*;
    let n = name.to_ascii_lowercase();
    if n.ends_with(".gguf") {
        for (k, p) in [
            ("q8", Q8),
            ("q6", Q6),
            ("q5", Q5),
            ("q4", Q4),
            ("q3", Q3),
            ("q2", Q3),
        ] {
            if n.contains(k) {
                return p;
            }
        }
        return Q8;
    }
    if n.contains("nvfp4") || n.contains("fp4") {
        Nvfp4
    } else if n.contains("fp8") || n.contains("e4m3") || n.contains("int8") {
        Fp8
    } else if n.contains("bf16") {
        Bf16
    } else if n.contains("fp32") {
        Fp32
    } else {
        Fp16
    }
}

/// Follow a link back through reroutes and bypassed nodes to a real output.
fn resolve_source(
    link: i64,
    links: &HashMap<i64, (i64, u64)>,
    by_id: &HashMap<i64, &Value>,
    ty: &str,
) -> Option<Value> {
    let mut cur = *links.get(&link)?;
    for _ in 0..64 {
        let node = by_id.get(&cur.0)?;
        let class = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let mode = node.get("mode").and_then(Value::as_i64).unwrap_or(0);
        let ins: Vec<&Value> = node
            .get("inputs")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        if class == "Reroute" {
            let l = ins.first()?.get("link")?.as_i64()?;
            cur = *links.get(&l)?;
        } else if mode == 4 {
            // Bypassed: pass through the first input of the same type.
            let l = ins
                .iter()
                .find(|i| {
                    i.get("type").and_then(Value::as_str) == Some(ty)
                        && i.get("link").and_then(Value::as_i64).is_some()
                })?
                .get("link")?
                .as_i64()?;
            cur = *links.get(&l)?;
        } else if mode == 2 || EDITOR_ONLY.contains(&class) {
            return None;
        } else {
            return Some(json!([cur.0.to_string(), cur.1]));
        }
    }
    None
}

/// A node input the user may expose in a tool.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Candidate {
    pub node: String,
    pub class_type: String,
    pub title: String,
    pub input: String,
    pub value: Value,
    pub kind: InputKind,
    pub label: String,
    /// Pre-selected in the "publish as tool" dialog.
    pub suggested: bool,
    pub def: Option<InputDef>,
}

/// List exposable inputs, guessing a kind and a friendly label for each.
pub fn analyze(graph: &Value, object_info: &Value) -> Vec<Candidate> {
    let Some(nodes) = graph.as_object() else {
        return vec![];
    };
    let negatives = negative_text_nodes(nodes);
    let mut out = Vec::new();
    for (id, node) in nodes {
        let class = node
            .get("class_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = node
            .pointer("/_meta/title")
            .and_then(Value::as_str)
            .unwrap_or(class)
            .to_string();
        let defs = inputs_of(object_info, class).unwrap_or_default();
        let Some(inputs) = node.get("inputs").and_then(Value::as_object) else {
            continue;
        };
        for (name, value) in inputs {
            if as_link(value).is_some() && value[0].is_string() {
                continue;
            }
            let def = defs.iter().find(|d| &d.name == name).cloned();
            let (kind, suggested) =
                guess_kind(class, name, value, def.as_ref(), negatives.contains(id));
            let label = match kind {
                InputKind::Text => "Prompt".to_string(),
                InputKind::NegativeText => "Negative prompt".to_string(),
                InputKind::Seed => "Seed".to_string(),
                InputKind::Image if class == "LoadImage" => format!("Image ({title})"),
                _ => humanize(name),
            };
            out.push(Candidate {
                node: id.clone(),
                class_type: class.to_string(),
                title: title.clone(),
                input: name.clone(),
                value: value.clone(),
                kind,
                label,
                suggested,
                def,
            });
        }
    }
    out
}

fn guess_kind(
    class: &str,
    name: &str,
    value: &Value,
    def: Option<&InputDef>,
    negative: bool,
) -> (InputKind, bool) {
    let ty = def.map(|d| d.ty.as_str()).unwrap_or(match value {
        Value::String(_) => "STRING",
        Value::Bool(_) => "BOOLEAN",
        Value::Number(n) if n.is_f64() => "FLOAT",
        Value::Number(_) => "INT",
        _ => "*",
    });
    match (class, name) {
        ("LoadImage", "image") => return (InputKind::Image, true),
        ("LoadImageMask", "image") => return (InputKind::Mask, true),
        ("LoadVideo", "file") => return (InputKind::Video, true),
        ("LoadAudio", "audio") => return (InputKind::Audio, true),
        ("CreateBoundingBoxes", "editor_state") => return (InputKind::Regions, true),
        _ => {}
    }
    let lname = name.to_ascii_lowercase();
    match ty {
        "INT" if lname == "seed" || lname == "noise_seed" => (InputKind::Seed, true),
        "INT" => (
            InputKind::Int,
            matches!(
                lname.as_str(),
                "steps" | "width" | "height" | "length" | "batch_size"
            ),
        ),
        "FLOAT" => (
            InputKind::Float,
            matches!(lname.as_str(), "cfg" | "denoise" | "guidance"),
        ),
        "BOOLEAN" => (InputKind::Bool, false),
        "COMBO" => (InputKind::Enum, false),
        "STRING" => {
            let prompt_like = lname.contains("text")
                || lname.contains("prompt")
                || def.is_some_and(|d| d.multiline);
            if prompt_like && (lname.contains("neg") || (negative && !lname.contains("pos"))) {
                (InputKind::NegativeText, true)
            } else if prompt_like {
                (InputKind::Text, true)
            } else {
                (InputKind::Raw, false)
            }
        }
        _ => (InputKind::Raw, false),
    }
}

/// Text-encode nodes whose output feeds a sampler's `negative` input.
/// Nodes whose every consumer is a `negative` input (a dedicated negative encoder).
fn negative_text_nodes(nodes: &Map<String, Value>) -> Vec<String> {
    let mut neg: HashMap<String, bool> = HashMap::new();
    for node in nodes.values() {
        for (name, v) in node["inputs"].as_object().into_iter().flatten() {
            if let Some((src, _)) = as_link(v).filter(|_| v[0].is_string()) {
                let is_neg = name.starts_with("negative");
                let e = neg.entry(src).or_insert(true);
                *e &= is_neg;
            }
        }
    }
    neg.into_iter()
        .filter(|(_, n)| *n)
        .map(|(k, _)| k)
        .collect()
}

fn humanize(name: &str) -> String {
    let s = name.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

/// One field of a published tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolField {
    pub id: String,
    pub label: String,
    pub kind: InputKind,
    #[serde(default)]
    pub level: Level,
    /// Node inputs this field writes. For `size`, give two targets with `part` "w" and "h".
    pub targets: Vec<FieldTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldTarget {
    pub node: String,
    pub input: String,
    #[serde(default)]
    pub part: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Turn a workflow plus a field selection into a manifest with a single `run` op.
pub fn publish_tool(
    graph: &Value,
    object_info: &Value,
    fields: &[ToolField],
    meta: &ToolMeta,
) -> Result<Manifest, BindError> {
    let mut g = graph.clone();
    crate::template::escape(&mut g);
    let mut inputs = Vec::new();
    // Regions read the canvas size from their node, so they go after size fields.
    let mut ordered: Vec<&ToolField> = fields.iter().collect();
    ordered.sort_by_key(|f| f.kind == InputKind::Regions);
    for f in ordered {
        if !f.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || f.id.is_empty() {
            return Err(BindError::Template(format!(
                "field id {:?} must be letters, digits or _",
                f.id
            )));
        }
        let mut default = None;
        let mut def: Option<InputDef> = None;
        let mut size = Map::new();
        for t in &f.targets {
            let node = g.get_mut(&t.node).ok_or_else(|| {
                BindError::Template(format!("field {} targets missing node {}", f.id, t.node))
            })?;
            let class = node
                .get("class_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let node_inputs = node["inputs"].clone();
            let slot = node
                .get_mut("inputs")
                .and_then(|i| i.get_mut(&t.input))
                .ok_or_else(|| {
                    BindError::Template(format!("node {} has no input {}", t.node, t.input))
                })?;
            let current = slot.clone();
            let placeholder = match (f.kind, t.part.as_deref()) {
                (InputKind::Size, Some(p @ ("w" | "h"))) => {
                    size.insert(p.to_string(), current);
                    format!("{{{{{}.{p}}}}}", f.id)
                }
                (InputKind::Size, _) => {
                    return Err(BindError::Template(
                        "size fields need targets with part w and h".into(),
                    ));
                }
                (k, _) if k.is_asset() => format!("{{{{{}|asset}}}}", f.id),
                (InputKind::ImageList, _) => format!("{{{{{}|asset}}}}", f.id),
                (InputKind::StructuredPrompt, _) => format!("{{{{{}|json}}}}", f.id),
                (InputKind::Regions, _) => {
                    let dim = |k: &str| node_inputs.get(k).cloned().unwrap_or(json!(1024));
                    let size = match (dim("width"), dim("height")) {
                        // Bound to a size field: `{{size.w}}` -> `size`.
                        (Value::String(w), _) if w.starts_with("{{") => w
                            .trim_matches(|c| c == '{' || c == '}')
                            .split('.')
                            .next()
                            .unwrap_or("size")
                            .to_string(),
                        (w, h) => format!(
                            "{}x{}",
                            w.as_f64().unwrap_or(1024.0),
                            h.as_f64().unwrap_or(1024.0)
                        ),
                    };
                    format!("{{{{{}|bbox_state:{size}}}}}", f.id)
                }
                _ => {
                    default.get_or_insert(current);
                    format!("{{{{{}}}}}", f.id)
                }
            };
            *slot = Value::String(placeholder);
            if def.is_none() {
                def = inputs_of(object_info, &class)
                    .and_then(|d| d.into_iter().find(|d| d.name == t.input));
            }
        }
        if f.kind == InputKind::Size {
            default = Some(Value::Object(size));
        }
        if f.kind.is_asset() {
            default = None;
        }
        if f.kind == InputKind::Regions {
            default = Some(json!([]));
        }
        let mut spec = InputSpec {
            id: f.id.clone(),
            kind: f.kind,
            label: f.label.clone(),
            help: def
                .as_ref()
                .and_then(|d| d.tooltip.clone())
                .unwrap_or_default(),
            level: f.level,
            group: None,
            default,
            // The first upload of each kind is required; further ones (extra references) are optional.
            required: f.kind.is_asset()
                && !fields
                    .iter()
                    .take_while(|o| o.id != f.id)
                    .any(|o| o.kind == f.kind),
            ops: vec![],
            min: def.as_ref().and_then(|d| d.min),
            max: def.as_ref().and_then(|d| d.max),
            step: def.as_ref().and_then(|d| d.step),
            options: if f.kind == InputKind::Enum {
                def.as_ref().map(|d| d.options.clone()).unwrap_or_default()
            } else {
                vec![]
            },
            schema: None,
            extra: BTreeMap::new(),
        };
        if f.kind == InputKind::Seed {
            spec.min = None;
            spec.max = None;
            spec.default = Some(json!(-1));
        }
        inputs.push(spec);
    }
    let media = detect_media(&g);
    let slug: String = meta
        .name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let mut ops = BTreeMap::new();
    ops.insert(
        "run".to_string(),
        OpSpec {
            workflow: WorkflowRef::Inline(g),
            expand: vec![],
            requires: vec![],
        },
    );
    Ok(Manifest {
        schema: lg_types::MANIFEST_SCHEMA_VERSION,
        id: format!(
            "tool-{}",
            if slug.is_empty() {
                "untitled".into()
            } else {
                slug
            }
        ),
        display_name: meta.name.clone(),
        family: "tool".into(),
        media,
        description: meta.description.clone(),
        tags: vec!["tool".into()],
        license: Default::default(),
        files: vec![],
        memory: Default::default(),
        ops,
        inputs,
        presets: vec![],
        origin: Origin::Tool,
    })
}

/// Video if any output node writes video or animation.
pub fn detect_media(graph: &Value) -> Media {
    let video = graph
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .any(|n| {
            let c = n
                .get("class_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            matches!(
                c,
                "SaveVideo" | "SaveWEBM" | "SaveAnimatedWEBP" | "SaveAnimatedPNG" | "CreateVideo"
            ) || c.contains("VideoCombine")
        });
    let audio = graph
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .any(|n| {
            n.get("class_type")
                .and_then(Value::as_str)
                .is_some_and(|c| c.starts_with("SaveAudio"))
        });
    if video {
        Media::Video
    } else if audio {
        Media::Audio
    } else {
        Media::Image
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Value {
        json!({
            "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["m.safetensors"], {}]}}},
            "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}], "clip": ["CLIP"]}}},
            "EmptyLatentImage": {"input": {"required": {"width": ["INT", {"default": 512}], "height": ["INT", {"default": 512}], "batch_size": ["INT", {"default": 1}]}}},
            "KSampler": {"input": {"required": {
                "model": ["MODEL"], "seed": ["INT", {"control_after_generate": true}], "steps": ["INT", {"min": 1, "max": 100}],
                "cfg": ["FLOAT"], "sampler_name": [["euler"], {}], "scheduler": [["normal"], {}],
                "positive": ["CONDITIONING"], "negative": ["CONDITIONING"], "latent_image": ["LATENT"], "denoise": ["FLOAT"]}}},
            "VAEDecode": {"input": {"required": {"samples": ["LATENT"], "vae": ["VAE"]}}},
            "SaveImage": {"input": {"required": {"images": ["IMAGE"], "filename_prefix": ["STRING", {"default": "ComfyUI"}]}}}
        })
    }

    /// A small editor-format workflow with a reroute and a bypassed node.
    fn ui_workflow() -> Value {
        json!({
            "nodes": [
                {"id": 4, "type": "CheckpointLoaderSimple", "mode": 0, "inputs": [], "widgets_values": ["m.safetensors"]},
                {"id": 6, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 3}], "widgets_values": ["a cat"]},
                {"id": 7, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 5}], "widgets_values": ["blurry"]},
                {"id": 5, "type": "EmptyLatentImage", "mode": 0, "inputs": [], "widgets_values": [768, 512, 1]},
                {"id": 3, "type": "KSampler", "mode": 0, "inputs": [
                    {"name": "model", "type": "MODEL", "link": 10},
                    {"name": "positive", "type": "CONDITIONING", "link": 4},
                    {"name": "negative", "type": "CONDITIONING", "link": 6},
                    {"name": "latent_image", "type": "LATENT", "link": 2}],
                 "widgets_values": [42, "randomize", 20, 7.5, "euler", "normal", 1.0]},
                {"id": 20, "type": "Reroute", "mode": 0, "inputs": [{"name": "", "type": "*", "link": 1}]},
                {"id": 8, "type": "VAEDecode", "mode": 0, "inputs": [{"name": "samples", "type": "LATENT", "link": 7}, {"name": "vae", "type": "VAE", "link": 8}]},
                {"id": 9, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 9}], "widgets_values": ["out"]},
                {"id": 30, "type": "Note", "mode": 0, "widgets_values": ["hello"]}
            ],
            "links": [
                [1, 4, 0, 20, 0, "MODEL"], [10, 20, 0, 3, 0, "MODEL"],
                [2, 5, 0, 3, 3, "LATENT"], [3, 4, 1, 6, 0, "CLIP"], [4, 6, 0, 3, 1, "CONDITIONING"],
                [5, 4, 1, 7, 0, "CLIP"], [6, 7, 0, 3, 2, "CONDITIONING"], [7, 3, 0, 8, 0, "LATENT"],
                [8, 4, 2, 8, 1, "VAE"], [9, 8, 0, 9, 0, "IMAGE"]
            ]
        })
    }

    #[test]
    fn converts_ui_format() {
        let api = to_api(&ui_workflow(), &info()).unwrap();
        assert_eq!(
            api["3"]["inputs"]["model"],
            json!(["4", 0]),
            "reroute resolved"
        );
        assert_eq!(api["3"]["inputs"]["seed"], json!(42));
        assert_eq!(
            api["3"]["inputs"]["steps"],
            json!(20),
            "control_after_generate value skipped"
        );
        assert_eq!(api["3"]["inputs"]["cfg"], json!(7.5));
        assert_eq!(api["5"]["inputs"]["width"], json!(768));
        assert!(api.get("30").is_none() && api.get("20").is_none());
        assert!(crate::objectinfo::validate(&api, &info()).is_empty());
    }

    #[test]
    fn missing_nodes_reported() {
        let mut w = ui_workflow();
        w["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": 99, "type": "FancyCustomNode", "mode": 0}));
        let e = to_api(&w, &info()).unwrap_err();
        assert!(e.to_string().contains("FancyCustomNode"));
    }

    #[test]
    fn analyze_and_publish() {
        let api = to_api(&ui_workflow(), &info()).unwrap();
        let c = analyze(&api, &info());
        let pos = c.iter().find(|c| c.node == "6").unwrap();
        let neg = c.iter().find(|c| c.node == "7").unwrap();
        assert_eq!(pos.kind, InputKind::Text);
        assert_eq!(
            neg.kind,
            InputKind::NegativeText,
            "traced through KSampler.negative"
        );
        assert_eq!(
            c.iter().find(|c| c.input == "seed").unwrap().kind,
            InputKind::Seed
        );

        let fields = vec![
            ToolField {
                id: "prompt".into(),
                label: "Prompt".into(),
                kind: InputKind::Text,
                level: Level::Simple,
                targets: vec![FieldTarget {
                    node: "6".into(),
                    input: "text".into(),
                    part: None,
                }],
            },
            ToolField {
                id: "seed".into(),
                label: "Seed".into(),
                kind: InputKind::Seed,
                level: Level::Advanced,
                targets: vec![FieldTarget {
                    node: "3".into(),
                    input: "seed".into(),
                    part: None,
                }],
            },
            ToolField {
                id: "size".into(),
                label: "Size".into(),
                kind: InputKind::Size,
                level: Level::Simple,
                targets: vec![
                    FieldTarget {
                        node: "5".into(),
                        input: "width".into(),
                        part: Some("w".into()),
                    },
                    FieldTarget {
                        node: "5".into(),
                        input: "height".into(),
                        part: Some("h".into()),
                    },
                ],
            },
        ];
        let m = publish_tool(
            &api,
            &info(),
            &fields,
            &ToolMeta {
                name: "My Cat Tool!".into(),
                description: String::new(),
            },
        )
        .unwrap();
        assert_eq!(m.id, "tool-my-cat-tool");
        assert_eq!(
            m.input("size").unwrap().default,
            Some(json!({"w": 768, "h": 512}))
        );
        assert_eq!(m.input("prompt").unwrap().default, Some(json!("a cat")));

        // The published tool binds like any other manifest.
        let spec: lg_types::JobSpec = serde_json::from_value(json!({"model": m.id, "op": "run", "values": {"prompt": "a dog", "seed": 7, "size": {"w": 640, "h": 640}}})).unwrap();
        let WorkflowRef::Inline(t) = &m.ops["run"].workflow else {
            panic!()
        };
        let (_, g) = crate::bind(&m, &spec, t, &Default::default()).unwrap();
        assert_eq!(g["6"]["inputs"]["text"], json!("a dog"));
        assert_eq!(g["3"]["inputs"]["seed"], json!(7));
        assert_eq!(g["5"]["inputs"]["width"], json!(640));
        assert_eq!(
            g["7"]["inputs"]["text"],
            json!("blurry"),
            "unexposed values keep their original value"
        );
    }
}
