//! Capability manifest registry.
//!
//! Sources, in increasing precedence: built-in (`manifests/models`, templates under
//! `manifests/`), published tools (`data/tools`), generated blueprint tools, and the user's
//! own manifests (`data/user-manifests`). A later source overrides an earlier one with the
//! same id. Every manifest is checked; problems are reported, never fatal.

pub mod sample;

use lg_types::{InputKind, Manifest, Origin, WorkflowRef};
use lg_workflow::objectinfo::{Issue, validate};
use parking_lot::RwLock;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Source {
    /// Folder of `*.json` manifests.
    pub dir: PathBuf,
    /// Where relative template paths resolve from.
    pub root: PathBuf,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub manifest: Arc<Manifest>,
    #[serde(skip)]
    pub root: PathBuf,
    pub path: Option<PathBuf>,
    /// Authoring problems (bad placeholders, missing templates...).
    pub problems: Vec<String>,
    /// Set when the engine lacks a node type the manifest needs.
    pub unavailable: Option<String>,
}

#[derive(Default)]
pub struct Registry {
    sources: Vec<Source>,
    entries: RwLock<BTreeMap<String, Entry>>,
    generated: RwLock<Vec<Manifest>>,
    load_errors: RwLock<Vec<(PathBuf, String)>>,
    object_info: RwLock<Option<Arc<Value>>>,
}

impl Registry {
    pub fn new(sources: Vec<Source>) -> Self {
        let r = Self {
            sources,
            ..Default::default()
        };
        r.reload();
        r
    }

    /// Re-read every source. Returns the number of manifests loaded.
    pub fn reload(&self) -> usize {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        let mut errors = Vec::new();
        let mut ordered = self.sources.clone();
        // User manifests win over everything, generated tools sit below published ones.
        ordered.sort_by_key(|s| match s.origin {
            Origin::Builtin => 0,
            Origin::Tool => 2,
            Origin::User => 3,
        });
        let mut push = |m: Manifest, root: PathBuf, path: Option<PathBuf>| {
            let problems = check(&m, &root);
            entries.insert(
                m.id.clone(),
                Entry {
                    manifest: Arc::new(m),
                    root,
                    path,
                    problems,
                    unavailable: None,
                },
            );
        };
        for src in ordered.iter().filter(|s| s.origin == Origin::Builtin) {
            for (path, res) in read_dir(&src.dir, src.origin.clone()) {
                match res {
                    Ok(m) => push(m, src.root.clone(), Some(path)),
                    Err(e) => errors.push((path, e)),
                }
            }
        }
        for m in self.generated.read().iter() {
            push(m.clone(), PathBuf::new(), None);
        }
        for src in ordered.iter().filter(|s| s.origin != Origin::Builtin) {
            for (path, res) in read_dir(&src.dir, src.origin.clone()) {
                match res {
                    Ok(m) => push(m, src.root.clone(), Some(path)),
                    Err(e) => errors.push((path, e)),
                }
            }
        }
        if let Some(info) = self.object_info.read().clone() {
            for e in entries.values_mut() {
                e.unavailable = availability(e, &info);
            }
        }
        for (p, e) in &errors {
            tracing::warn!("manifest {}: {e}", p.display());
        }
        let n = entries.len();
        *self.entries.write() = entries;
        *self.load_errors.write() = errors;
        n
    }

    /// Engine node definitions arrived (or changed): recheck availability.
    pub fn set_object_info(&self, info: Arc<Value>) {
        *self.object_info.write() = Some(info.clone());
        for e in self.entries.write().values_mut() {
            e.unavailable = availability(e, &info);
        }
    }

    /// Replace the generated (blueprint) tools.
    pub fn set_generated(&self, tools: Vec<Manifest>) {
        *self.generated.write() = tools;
        self.reload();
    }

    pub fn get(&self, id: &str) -> Option<Entry> {
        self.entries.read().get(id).cloned()
    }

    pub fn list(&self) -> Vec<Entry> {
        self.entries.read().values().cloned().collect()
    }

    pub fn load_errors(&self) -> Vec<(PathBuf, String)> {
        self.load_errors.read().clone()
    }

    /// The workflow template of one op.
    pub fn template(&self, id: &str, op: &str) -> Result<Value, String> {
        let e = self.get(id).ok_or_else(|| format!("unknown model {id}"))?;
        let spec = e
            .manifest
            .ops
            .get(op)
            .ok_or_else(|| format!("{id} has no operation {op}"))?;
        lg_workflow::load_template(&spec.workflow, &e.root).map_err(|e| e.to_string())
    }

    /// Save a published tool (or user manifest) into the first source of that origin.
    pub fn save(&self, m: &Manifest, origin: Origin) -> Result<PathBuf, String> {
        let src = self
            .sources
            .iter()
            .find(|s| s.origin == origin)
            .ok_or_else(|| "no folder for that kind of manifest".to_string())?;
        std::fs::create_dir_all(&src.dir).map_err(|e| e.to_string())?;
        let problems = check(m, &src.root);
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        let path = src.dir.join(format!("{}.json", m.id));
        let text = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        self.reload();
        Ok(path)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let e = self.get(id).ok_or("unknown manifest")?;
        if e.manifest.origin == Origin::Builtin {
            return Err("built-in models cannot be deleted".into());
        }
        let path = e
            .path
            .ok_or("this tool is generated and cannot be deleted")?;
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    pub fn source_dirs(&self) -> Vec<PathBuf> {
        self.sources.iter().map(|s| s.dir.clone()).collect()
    }
}

fn read_dir(dir: &Path, origin: Origin) -> Vec<(PathBuf, Result<Manifest, String>)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let r = std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Manifest>(&t).map_err(|e| e.to_string()))
                .map(|mut m| {
                    m.origin = origin.clone();
                    m
                });
            (p, r)
        })
        .collect()
}

/// Authoring checks that need no engine.
pub fn check(m: &Manifest, root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if m.schema != lg_types::MANIFEST_SCHEMA_VERSION {
        problems.push(format!(
            "schema {} is not supported (expected {})",
            m.schema,
            lg_types::MANIFEST_SCHEMA_VERSION
        ));
    }
    if m.id.is_empty()
        || !m
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        problems.push(format!(
            "id {:?} must be letters, digits, '-', '_' or '.'",
            m.id
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for i in &m.inputs {
        if !seen.insert(i.id.as_str()) {
            problems.push(format!("input {} is declared twice", i.id));
        }
        if matches!(i.kind, InputKind::Enum | InputKind::Motion)
            && i.options.is_empty()
            && !i.extra.contains_key("object_info")
        {
            problems.push(format!("input {} is a choice with no options", i.id));
        }
        for op in &i.ops {
            if !m.ops.contains_key(op) {
                problems.push(format!("input {} refers to unknown op {op}", i.id));
            }
        }
    }
    let roles: Vec<String> = m.files.iter().map(|f| format!("file.{}", f.role)).collect();
    for (op, spec) in &m.ops {
        let template = match lg_workflow::load_template(&spec.workflow, root) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{op}: {e}"));
                continue;
            }
        };
        if !template.is_object() {
            problems.push(format!("{op}: workflow must be an object of nodes"));
            continue;
        }
        let known: Vec<&str> = m.inputs_for(op).map(|i| i.id.as_str()).collect();
        let mut refs = lg_workflow::template::references(&template);
        for e in &spec.expand {
            refs.extend(lg_workflow::template::references(&Value::Object(
                e.fragment.clone().into_iter().collect(),
            )));
            match m.inputs_for(op).find(|i| i.id == e.input) {
                Some(i)
                    if matches!(
                        i.kind,
                        InputKind::LoraStack | InputKind::Regions | InputKind::ImageList
                    ) => {}
                Some(i) => problems.push(format!(
                    "{op}: expander input {} must be a list kind, not {:?}",
                    i.id, i.kind
                )),
                None => problems.push(format!("{op}: expander uses unknown input {}", e.input)),
            }
            if !e.fragment.contains_key(&e.head) {
                problems.push(format!(
                    "{op}: expander head {} is not in its fragment",
                    e.head
                ));
            }
            for link in &e.chain {
                if let Some((node, _)) = lg_workflow::expand::as_link(link)
                    && template.get(&node).is_none()
                {
                    problems.push(format!(
                        "{op}: expander chain refers to missing node {node}"
                    ));
                }
            }
        }
        refs.sort();
        refs.dedup();
        for r in refs {
            if r.starts_with("file.") {
                if !roles.contains(&r) {
                    problems.push(format!("{op}: template uses {r} but no file has that role"));
                }
            } else if !known.contains(&r.as_str()) {
                problems.push(format!(
                    "{op}: template uses {{{{{r}}}}} which is not an input of this op"
                ));
            }
        }
    }
    for f in &m.files {
        if f.variants.is_empty() {
            problems.push(format!("file {} has no variants", f.role));
        }
    }
    problems
}

/// Node types the manifest needs that the engine does not have.
fn availability(e: &Entry, info: &Value) -> Option<String> {
    let mut missing = Vec::new();
    for spec in e.manifest.ops.values() {
        let Ok(t) = lg_workflow::load_template(&spec.workflow, &e.root) else {
            continue;
        };
        let mut nodes = t.clone();
        // Fragment nodes count too.
        if let Some(map) = nodes.as_object_mut() {
            for (i, ex) in spec.expand.iter().enumerate() {
                for (k, v) in &ex.fragment {
                    map.insert(format!("lgx{i}_{k}"), v.clone());
                }
            }
        }
        for issue in validate(&nodes, info) {
            if let Issue::UnknownNode { class_type, .. } = issue
                && !missing.contains(&class_type)
            {
                missing.push(class_type);
            }
        }
    }
    (!missing.is_empty())
        .then(|| format!("the engine is missing node types: {}", missing.join(", ")))
}

/// Tools for every official blueprint shipped with the installed ComfyUI.
pub fn blueprint_tools(dir: &Path, object_info: &Value) -> (Vec<Manifest>, Vec<(String, String)>) {
    let mut tools = Vec::new();
    let mut failed = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (tools, failed);
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    for p in paths {
        let name = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let result = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .and_then(|wf| lg_workflow::import::import(&wf, object_info).map_err(|e| e.to_string()))
            .and_then(|imp| {
                lg_workflow::import::blueprint_tool(&imp, object_info, &name)
                    .map_err(|e| e.to_string())
            });
        match result {
            Ok(mut m) => {
                m.id = format!("bp-{}", m.id.trim_start_matches("tool-"));
                m.tags.push("official".into());
                // Blueprints with no visible inputs are building blocks, not tools.
                if m.inputs.is_empty() {
                    continue;
                }
                tools.push(m);
            }
            Err(e) => failed.push((name, e)),
        }
    }
    (tools, failed)
}

/// Inline workflow of a manifest op, if any (tools).
pub fn inline_workflow<'a>(m: &'a Manifest, op: &str) -> Option<&'a Value> {
    match &m.ops.get(op)?.workflow {
        WorkflowRef::Inline(v) => Some(v),
        WorkflowRef::Path(_) => None,
    }
}

/// Watch manifest folders and call `on_change` (debounced) when files change.
pub fn watch(
    dirs: Vec<PathBuf>,
    on_change: impl Fn() + Send + 'static,
) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|e| !e.kind.is_access()) {
            let _ = tx.send(());
        }
    })
    .ok()?;
    for d in &dirs {
        let _ = std::fs::create_dir_all(d);
        let _ = w.watch(d, RecursiveMode::Recursive);
    }
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            // Debounce bursts of events from editors saving files.
            std::thread::sleep(std::time::Duration::from_millis(300));
            while rx.try_recv().is_ok() {}
            on_change();
        }
    });
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, name: &str, v: Value) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), serde_json::to_string(&v).unwrap()).unwrap();
    }

    fn manifest(id: &str, template: Value) -> Value {
        json!({"schema": 1, "id": id, "display_name": id, "family": "f", "media": "image",
               "ops": {"txt2img": {"workflow": template}},
               "inputs": [{"id": "prompt", "kind": "text", "label": "Prompt"}]})
    }

    #[test]
    fn precedence_problems_and_availability() {
        let d = tempfile::tempdir().unwrap();
        let (b, u) = (d.path().join("builtin"), d.path().join("user"));
        write(
            &b,
            "a.json",
            manifest(
                "a",
                json!({"1": {"class_type": "KSampler", "inputs": {"t": "{{prompt}}"}}}),
            ),
        );
        write(
            &b,
            "b.json",
            manifest(
                "b",
                json!({"1": {"class_type": "Nope", "inputs": {"t": "{{missing}}"}}}),
            ),
        );
        write(&b, "broken.json", json!({"id": "x"}));
        let mut over = manifest("a", json!({"1": {"class_type": "KSampler", "inputs": {}}}));
        over["display_name"] = json!("Mine");
        write(&u, "a.json", over);
        let r = Registry::new(vec![
            Source {
                dir: b.clone(),
                root: b.clone(),
                origin: Origin::Builtin,
            },
            Source {
                dir: u.clone(),
                root: u.clone(),
                origin: Origin::User,
            },
        ]);
        assert_eq!(
            r.get("a").unwrap().manifest.display_name,
            "Mine",
            "user overrides built-in"
        );
        assert!(
            r.get("b")
                .unwrap()
                .problems
                .iter()
                .any(|p| p.contains("missing"))
        );
        assert_eq!(r.load_errors().len(), 1);
        r.set_object_info(Arc::new(json!({"KSampler": {"input": {}}})));
        assert!(r.get("a").unwrap().unavailable.is_none());
        assert!(r.get("b").unwrap().unavailable.unwrap().contains("Nope"));
    }

    #[test]
    fn save_and_delete_tools() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("tools");
        let r = Registry::new(vec![Source {
            dir: t.clone(),
            root: t.clone(),
            origin: Origin::Tool,
        }]);
        let m: Manifest = serde_json::from_value(manifest(
            "tool-x",
            json!({"1": {"class_type": "K", "inputs": {"t": "{{prompt}}"}}}),
        ))
        .unwrap();
        r.save(&m, Origin::Tool).unwrap();
        assert!(r.get("tool-x").is_some());
        let bad: Manifest = serde_json::from_value(manifest(
            "tool-y",
            json!({"1": {"class_type": "K", "inputs": {"t": "{{nope}}"}}}),
        ))
        .unwrap();
        assert!(
            r.save(&bad, Origin::Tool).is_err(),
            "broken tools are not saved"
        );
        r.delete("tool-x").unwrap();
        assert!(r.get("tool-x").is_none());
    }
}
