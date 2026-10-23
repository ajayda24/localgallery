//! Every built-in manifest loads cleanly, and every op binds (with boxes, LoRAs,
//! references and uploads filled in) into a graph the real ComfyUI node set accepts.

use lg_manifests::sample::sample;
use lg_manifests::{Registry, Source};
use lg_types::{InputKind, Origin};
use lg_workflow::objectinfo::{Issue, validate};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub fn registry() -> Registry {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests");
    Registry::new(vec![Source {
        dir: root.join("models"),
        root,
        origin: Origin::Builtin,
    }])
}

fn object_info() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/object_info.json")).unwrap()
}

#[test]
fn builtin_manifests_are_clean() {
    let r = registry();
    assert!(r.load_errors().is_empty(), "{:?}", r.load_errors());
    let list = r.list();
    assert!(list.len() >= 5, "{}", list.len());
    for e in &list {
        assert!(e.problems.is_empty(), "{}: {:?}", e.manifest.id, e.problems);
    }
    r.set_object_info(std::sync::Arc::new(object_info()));
    for e in r.list() {
        assert!(
            e.unavailable.is_none(),
            "{}: {:?}",
            e.manifest.id,
            e.unavailable
        );
    }
}

#[test]
fn every_op_binds_to_a_valid_graph() {
    let r = registry();
    let info = object_info();
    for e in r.list() {
        let m = &e.manifest;
        for op in m.ops.keys() {
            let (values, assets, files) = sample(m, op);
            let spec: lg_types::JobSpec =
                serde_json::from_value(json!({"model": m.id, "op": op, "values": values})).unwrap();
            let t = r.template(&m.id, op).unwrap();
            let opts = lg_workflow::BindOptions {
                files,
                assets,
                vae_tiled: false,
                output_prefix: Some("lg/test".into()),
            };
            let (_, g) = lg_workflow::bind(m, &spec, &t, &opts)
                .unwrap_or_else(|err| panic!("{}/{op}: {err}", m.id));
            let issues: Vec<String> = validate(&g, &info)
                .into_iter()
                .filter(|i| !matches!(i, Issue::BadChoice { .. }))
                .map(|i| i.message())
                .collect();
            assert!(
                issues.is_empty(),
                "{}/{op}: {issues:?}\n{}",
                m.id,
                serde_json::to_string_pretty(&g).unwrap()
            );
            let text = g.to_string();
            assert!(
                !text.contains("{{"),
                "{}/{op}: unresolved placeholder",
                m.id
            );
            // Mechanisms really reach the graph.
            if m.inputs_for(op).any(|i| i.kind == InputKind::Regions) {
                assert!(
                    text.contains("a red boat"),
                    "{}/{op}: regions missing",
                    m.id
                );
            }
            if m.inputs_for(op).any(|i| i.kind == InputKind::LoraStack) {
                assert!(
                    text.contains("style.safetensors"),
                    "{}/{op}: lora missing",
                    m.id
                );
            }
            if m.inputs_for(op).any(|i| i.kind == InputKind::ImageList) {
                assert!(text.contains("lg_ref2.png"), "{}/{op}: refs missing", m.id);
            }
            println!("ok {}/{op}: {} nodes", m.id, g.as_object().unwrap().len());
        }
    }
}
