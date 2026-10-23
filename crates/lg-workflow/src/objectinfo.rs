//! Reading ComfyUI `/object_info` and checking graphs against it.

use crate::expand::as_link;
use serde::Serialize;
use serde_json::Value;

/// One widget or socket input of a node class.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InputDef {
    pub name: String,
    /// INT, FLOAT, STRING, BOOLEAN, COMBO, or a socket type (MODEL, IMAGE, ...).
    pub ty: String,
    pub required: bool,
    pub options: Vec<Value>,
    pub default: Option<Value>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub multiline: bool,
    pub tooltip: Option<String>,
    /// Seed-like INT inputs carry an extra "control_after_generate" widget value in UI workflows.
    pub control_after_generate: bool,
}

impl InputDef {
    pub fn is_widget(&self) -> bool {
        matches!(
            self.ty.as_str(),
            "INT" | "FLOAT" | "STRING" | "BOOLEAN" | "COMBO"
        )
    }
}

/// Inputs of `class` in declaration order (required first, then optional).
pub fn inputs_of(object_info: &Value, class: &str) -> Option<Vec<InputDef>> {
    let node = object_info.get(class)?;
    let mut out = Vec::new();
    for (section, required) in [("required", true), ("optional", false)] {
        let Some(map) = node
            .get("input")
            .and_then(|i| i.get(section))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (name, spec) in map {
            out.push(parse_input(name, spec, required));
        }
    }
    Some(out)
}

fn parse_input(name: &str, spec: &Value, required: bool) -> InputDef {
    let first = spec.get(0).cloned().unwrap_or(Value::Null);
    let opts = spec.get(1).cloned().unwrap_or(Value::Null);
    let (ty, options) = match &first {
        // Legacy combo: a list of choices.
        Value::Array(choices) => ("COMBO".to_string(), choices.clone()),
        Value::String(t) if t == "COMBO" => (
            "COMBO".to_string(),
            opts.get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
        // Dynamic combo: the value is an option key; the chosen option's sub-inputs
        // arrive as `<name>.<sub>`.
        Value::String(t) if t == "COMFY_DYNAMICCOMBO_V3" => (
            "COMBO".to_string(),
            opts.get("options")
                .and_then(Value::as_array)
                .map(|o| o.iter().filter_map(|x| x.get("key").cloned()).collect())
                .unwrap_or_default(),
        ),
        Value::String(t) => (t.clone(), vec![]),
        _ => ("*".to_string(), vec![]),
    };
    // Growable input lists (`<name>.<item>` keys) are never required as a whole.
    let required =
        required && !matches!(ty.as_str(), "COMFY_AUTOGROW_V3" | "COMFY_DYNAMICGROUP_V3");
    let num = |k: &str| opts.get(k).and_then(Value::as_f64);
    InputDef {
        name: name.to_string(),
        ty,
        required,
        options,
        default: opts.get("default").cloned(),
        min: num("min"),
        max: num("max"),
        step: num("step"),
        multiline: opts
            .get("multiline")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tooltip: opts
            .get("tooltip")
            .and_then(Value::as_str)
            .map(str::to_string),
        control_after_generate: opts
            .get("control_after_generate")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Issue {
    /// The node class is not installed (missing custom node or too-old ComfyUI).
    UnknownNode { node: String, class_type: String },
    MissingInput {
        node: String,
        class_type: String,
        input: String,
    },
    /// A choice is not available. For model loaders this means the file is not downloaded.
    BadChoice {
        node: String,
        class_type: String,
        input: String,
        value: Value,
        available: usize,
    },
    DanglingLink {
        node: String,
        input: String,
        target: String,
    },
}

impl Issue {
    pub fn message(&self) -> String {
        match self {
            Issue::UnknownNode { class_type, .. } => {
                format!(
                    "Node type {class_type} is not available in the engine (missing custom node or engine too old)"
                )
            }
            Issue::MissingInput {
                class_type, input, ..
            } => format!("{class_type} is missing required input {input}"),
            Issue::BadChoice {
                class_type,
                input,
                value,
                ..
            } => {
                format!("{class_type}.{input}: {value} is not available (is the file downloaded?)")
            }
            Issue::DanglingLink {
                node,
                input,
                target,
            } => format!("node {node}.{input} links to missing node {target}"),
        }
    }
}

/// Check a ComfyUI API graph against `/object_info` before submitting it.
pub fn validate(graph: &Value, object_info: &Value) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(nodes) = graph.as_object() else {
        return issues;
    };
    for (id, node) in nodes {
        let class = node
            .get("class_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(defs) = inputs_of(object_info, &class) else {
            issues.push(Issue::UnknownNode {
                node: id.clone(),
                class_type: class,
            });
            continue;
        };
        let given = node.get("inputs").and_then(Value::as_object);
        for def in &defs {
            let v = given.and_then(|g| g.get(&def.name));
            match v {
                None if def.required && def.default.is_none() => issues.push(Issue::MissingInput {
                    node: id.clone(),
                    class_type: class.clone(),
                    input: def.name.clone(),
                }),
                Some(v) => {
                    // API-format links always carry the node id as a string.
                    if let Some((target, _)) = as_link(v).filter(|_| v[0].is_string()) {
                        if !nodes.contains_key(&target) {
                            issues.push(Issue::DanglingLink {
                                node: id.clone(),
                                input: def.name.clone(),
                                target,
                            });
                        }
                        continue;
                    }
                    if def.ty == "COMBO" && !def.options.is_empty() && !def.options.contains(v) {
                        issues.push(Issue::BadChoice {
                            node: id.clone(),
                            class_type: class.clone(),
                            input: def.name.clone(),
                            value: v.clone(),
                            available: def.options.len(),
                        });
                    }
                }
                None => {}
            }
        }
        // Sub-inputs of dynamic inputs (`images.image_1`) are not in the class definition.
        for (name, v) in given.into_iter().flatten().filter(|(k, _)| k.contains('.')) {
            if let Some((target, _)) = as_link(v).filter(|_| v[0].is_string())
                && !nodes.contains_key(&target)
            {
                issues.push(Issue::DanglingLink {
                    node: id.clone(),
                    input: name.clone(),
                    target,
                });
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info() -> Value {
        json!({
            "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["a.safetensors", "b.safetensors"], {}]}}},
            "KSampler": {"input": {"required": {
                "model": ["MODEL"], "seed": ["INT", {"default": 0, "control_after_generate": true}],
                "sampler_name": ["COMBO", {"options": ["euler", "dpmpp_2m"]}]
            }}}
        })
    }

    #[test]
    fn parses_both_combo_styles() {
        let i = inputs_of(&info(), "KSampler").unwrap();
        assert_eq!(i[0].ty, "MODEL");
        assert!(i[1].control_after_generate);
        assert_eq!(i[2].options.len(), 2);
        let c = inputs_of(&info(), "CheckpointLoaderSimple").unwrap();
        assert_eq!(c[0].ty, "COMBO");
    }

    #[test]
    fn finds_issues() {
        let g = json!({
            "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "missing.safetensors"}},
            "2": {"class_type": "KSampler", "inputs": {"model": ["9", 0], "sampler_name": "euler"}},
            "3": {"class_type": "SomeCustomNode", "inputs": {}}
        });
        let issues = validate(&g, &info());
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, Issue::BadChoice { input, .. } if input == "ckpt_name"))
        );
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, Issue::DanglingLink { target, .. } if target == "9"))
        );
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, Issue::UnknownNode { .. }))
        );
        assert_eq!(
            issues.len(),
            3,
            "seed has a default so it is not missing: {issues:?}"
        );
    }

    #[test]
    fn dynamic_inputs_from_real_object_info() {
        let real: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/object_info.json")).unwrap();
        let fibo = inputs_of(&real, "TextEncodeFibo2").unwrap();
        assert!(!fibo.iter().find(|d| d.name == "images").unwrap().required);
        let ideo = inputs_of(&real, "BuildJsonPromptIdeogram").unwrap();
        let style = ideo.iter().find(|d| d.name == "style").unwrap();
        assert_eq!(style.ty, "COMBO");
        assert!(
            style.options.contains(&json!("photo")),
            "{:?}",
            style.options
        );
        let g = json!({
            "1": {"class_type": "CLIPLoader", "inputs": {"clip_name": "x.safetensors", "type": "fibo2"}},
            "2": {"class_type": "TextEncodeFibo2", "inputs": {"clip": ["1", 0], "prompt": "{}", "negative_prompt": "", "width": 0, "height": 0, "batch_size": 1, "images.image_1": ["9", 0]}}
        });
        let issues = validate(&g, &real);
        assert!(
            issues.iter().any(
                |i| matches!(i, Issue::DanglingLink { input, .. } if input == "images.image_1")
            ),
            "{issues:?}"
        );
        assert!(
            !issues
                .iter()
                .any(|i| matches!(i, Issue::MissingInput { .. })),
            "{issues:?}"
        );
    }
}
