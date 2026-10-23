//! Every blueprint tool's bound graph is accepted by a real ComfyUI, except for
//! missing model files or input media (none are present in a test install).
//!
//! `LG_REAL_COMFY_URL=http://127.0.0.1:8199 LG_COMFY_DIR=/path/to/ComfyUI \
//!   cargo test -p lg-engine --test real_blueprints -- --ignored --nocapture`

use serde_json::{Value, json};
use std::path::PathBuf;

#[tokio::test]
#[ignore = "needs a running ComfyUI (LG_REAL_COMFY_URL) and its checkout (LG_COMFY_DIR)"]
async fn real_comfy_accepts_every_blueprint_tool() {
    let url = std::env::var("LG_REAL_COMFY_URL").expect("LG_REAL_COMFY_URL");
    let dir =
        PathBuf::from(std::env::var("LG_COMFY_DIR").expect("LG_COMFY_DIR")).join("blueprints");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let info: Value = http
        .get(format!("{url}/object_info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let (mut accepted, mut only_files, mut skipped) = (0, 0, 0);
    let mut bad = Vec::new();
    for p in paths {
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let wf: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let imp = match lg_workflow::import::import(&wf, &info) {
            Ok(i) => i,
            Err(e) if e.to_string().contains("GeminiNode") => {
                skipped += 1;
                continue;
            }
            Err(e) => {
                bad.push(format!("{name}: import: {e}"));
                continue;
            }
        };
        let m = lg_workflow::import::blueprint_tool(&imp, &info, &name).unwrap();
        let lg_types::WorkflowRef::Inline(t) = &m.ops["run"].workflow else {
            unreachable!()
        };
        let mut values = serde_json::Map::new();
        let mut assets = std::collections::BTreeMap::new();
        for i in &m.inputs {
            if i.kind.is_asset() && i.required {
                values.insert(i.id.clone(), json!(format!("a-{}", i.id)));
                assets.insert(format!("a-{}", i.id), format!("missing_{}.png", i.id));
            } else if i.kind == lg_types::InputKind::Text {
                values.insert(i.id.clone(), json!("a test prompt"));
            }
        }
        let spec: lg_types::JobSpec =
            serde_json::from_value(json!({"model": m.id, "op": "run", "values": values})).unwrap();
        let opts = lg_workflow::BindOptions {
            assets,
            ..Default::default()
        };
        let (_, mut g) = lg_workflow::bind(&m, &spec, t, &opts).unwrap();
        lg_workflow::import::drop_empty_loaders(&mut g, &info);
        if let Some(d) = std::env::var_os("LG_DUMP_DIR") {
            let _ = std::fs::write(
                PathBuf::from(d).join(format!("{name}.json")),
                serde_json::to_string_pretty(&g).unwrap(),
            );
        }
        let r = http
            .post(format!("{url}/prompt"))
            .json(&json!({"prompt": g, "client_id": "lg-test"}))
            .send()
            .await
            .unwrap();
        let ok = r.status().is_success();
        let body: Value = r.json().await.unwrap();
        if ok {
            accepted += 1;
            // Do not actually run it.
            let id = body["prompt_id"].as_str().unwrap_or_default();
            let _ = http
                .post(format!("{url}/queue"))
                .json(&json!({"delete": [id]}))
                .send()
                .await;
            let _ = http
                .post(format!("{url}/interrupt"))
                .json(&json!({"prompt_id": id}))
                .send()
                .await;
            continue;
        }
        // Acceptable: file choices (models, input media) that do not exist here.
        let mut other = Vec::new();
        for (node, e) in body["node_errors"].as_object().into_iter().flatten() {
            for err in e["errors"].as_array().into_iter().flatten() {
                let input = err
                    .pointer("/extra_info/input_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let details = err["details"].as_str().unwrap_or_default();
                let file_value = [
                    ".safetensors",
                    ".gguf",
                    ".pt",
                    ".pth",
                    ".bin",
                    ".ckpt",
                    ".onnx",
                    ".sft",
                    ".json",
                    ".task",
                ]
                .iter()
                .any(|ext| details.contains(&format!("{ext}'")));
                let file_like = input.ends_with("_name") || file_value;
                let ty = err["type"].as_str().unwrap_or_default();
                let missing_file = (ty == "value_not_in_list" && file_like)
                    || [
                        "Invalid image file",
                        "Invalid video file",
                        "Invalid audio file",
                    ]
                    .iter()
                    .any(|m| details.contains(m));
                if !missing_file {
                    other.push(format!(
                        "{node} {}: {} {}",
                        e["class_type"], ty, err["details"]
                    ));
                }
            }
        }
        if body["node_errors"].as_object().is_none_or(|o| o.is_empty()) {
            other.push(format!("{}", body["error"]));
        }
        if other.is_empty() {
            only_files += 1;
        } else {
            bad.push(format!("{name}: {}", other.join("; ")));
        }
    }
    println!(
        "accepted {accepted}, rejected only for missing files {only_files}, skipped {skipped}, bad {}",
        bad.len()
    );
    for b in &bad {
        println!("BAD {b}");
    }
    assert!(bad.is_empty());
}
