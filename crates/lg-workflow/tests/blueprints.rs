//! Official ComfyUI blueprints become working tools.
//!
//! Always runs on the fixtures in `tests/fixtures/blueprints`. Set `LG_COMFY_DIR` to also
//! run every blueprint shipped with a ComfyUI checkout.

use lg_workflow::import::{blueprint_tool, import};
use lg_workflow::objectinfo::{Issue, validate};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn object_info() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/object_info.json")).unwrap()
}

/// Issues that matter: everything except "file not downloaded" (no models or inputs here).
fn real_issues(g: &Value, info: &Value) -> Vec<String> {
    validate(g, info)
        .into_iter()
        .filter(|i| {
            !matches!(i, Issue::BadChoice { input, .. }
                if input.ends_with("_name") || matches!(input.as_str(), "image" | "video" | "audio" | "file"))
        })
        .map(|i| i.message())
        .collect()
}

fn check(path: &Path, info: &Value) -> Result<String, String> {
    let wf: Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).map_err(|e| e.to_string())?;
    let imp = import(&wf, info).map_err(|e| e.to_string())?;
    let issues = real_issues(&imp.graph, info);
    if !issues.is_empty() {
        return Err(format!("imported graph invalid: {issues:?}"));
    }
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let m = blueprint_tool(&imp, info, &stem).map_err(|e| e.to_string())?;
    let lg_types::WorkflowRef::Inline(t) = &m.ops["run"].workflow else {
        unreachable!()
    };
    // Bind with defaults; asset inputs get a placeholder upload.
    let mut values = serde_json::Map::new();
    let mut assets = std::collections::BTreeMap::new();
    for i in &m.inputs {
        if i.kind.is_asset() {
            values.insert(i.id.clone(), Value::String(format!("asset-{}", i.id)));
            assets.insert(format!("asset-{}", i.id), format!("{}.png", i.id));
        } else if i.kind == lg_types::InputKind::Text {
            values.insert(i.id.clone(), Value::String("a test prompt".into()));
        }
    }
    let spec: lg_types::JobSpec =
        serde_json::from_value(serde_json::json!({"model": m.id, "op": "run", "values": values}))
            .unwrap();
    let opts = lg_workflow::BindOptions {
        assets,
        ..Default::default()
    };
    let (_, g) = lg_workflow::bind(&m, &spec, t, &opts).map_err(|e| {
        let k: Vec<String> = m
            .inputs
            .iter()
            .map(|i| format!("{}:{:?}={:?}", i.id, i.kind, i.default))
            .collect();
        format!("bind: {e} [{}]", k.join(", "))
    })?;
    let issues = real_issues(&g, info);
    if !issues.is_empty() {
        return Err(format!("bound graph invalid: {issues:?}"));
    }
    let kinds: Vec<String> = m
        .inputs
        .iter()
        .map(|i| format!("{}:{:?}", i.id, i.kind))
        .collect();
    Ok(format!(
        "{} [{}] files={}",
        m.display_name,
        kinds.join(", "),
        m.files.len()
    ))
}

#[test]
fn fixture_blueprints_become_tools() {
    let info = object_info();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blueprints");
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let summary = check(&p, &info).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        println!("{summary}");
        n += 1;
    }
    assert!(n >= 6);
}

#[test]
fn zimage_tool_has_expected_fields() {
    let info = object_info();
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/blueprints/Text to Image (Z-Image-Turbo).json");
    let wf: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let imp = import(&wf, &info).unwrap();
    let m = blueprint_tool(&imp, &info, "x").unwrap();
    let ids: Vec<&str> = m.inputs.iter().map(|i| i.id.as_str()).collect();
    assert!(
        ids.contains(&"prompt") && ids.contains(&"size") && ids.contains(&"seed"),
        "{ids:?}"
    );
    assert_eq!(
        m.input("unet_name").unwrap().extra["folder"],
        "diffusion_models"
    );
    assert!(m.files.iter().any(|f| {
        f.variants[0]
            .url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://huggingface.co/"))
    }));
    assert_eq!(
        m.input("size").unwrap().default,
        Some(serde_json::json!({"w": 1024, "h": 1024}))
    );
}

#[test]
fn every_shipped_blueprint() {
    let Some(dir) = std::env::var_os("LG_COMFY_DIR").map(|d| PathBuf::from(d).join("blueprints"))
    else {
        eprintln!("LG_COMFY_DIR not set; skipping the full blueprint sweep");
        return;
    };
    let info = object_info();
    let mut ok = 0;
    let mut failed = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    for p in paths {
        match check(&p, &info) {
            Ok(s) => {
                ok += 1;
                println!("ok   {s}");
            }
            // Paid cloud API nodes are disabled on purpose (`--offline`).
            Err(e) if e.contains("missing these node types") && e.contains("GeminiNode") => {
                println!("skip {} (cloud API node)", p.display());
            }
            Err(e) => failed.push(format!("{}: {e}", p.file_name().unwrap().to_string_lossy())),
        }
    }
    println!("{ok} ok, {} failed", failed.len());
    for f in &failed {
        println!("FAIL {f}");
    }
    assert!(failed.is_empty());
}
