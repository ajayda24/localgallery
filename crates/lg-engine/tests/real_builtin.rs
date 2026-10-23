//! Built-in manifests against a real ComfyUI: every op, with every mechanism filled in,
//! must pass the engine's validation except for model files that are not downloaded.
//!
//! `LG_REAL_COMFY_URL=http://127.0.0.1:8199 cargo test -p lg-engine --test real_builtin -- --ignored --nocapture`

use lg_engine::comfy::ComfyClient;
use lg_manifests::{Registry, Source};
use lg_types::Origin;
use serde_json::{Value, json};
use std::path::Path;

#[tokio::test]
#[ignore = "needs a running ComfyUI (LG_REAL_COMFY_URL)"]
async fn builtin_manifests_pass_real_validation() {
    let url = std::env::var("LG_REAL_COMFY_URL").expect("LG_REAL_COMFY_URL");
    let client = ComfyClient::new(url.clone());
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests");
    let r = Registry::new(vec![Source {
        dir: root.join("models"),
        root,
        origin: Origin::Builtin,
    }]);
    let mut bad = Vec::new();
    let mut n = 0;
    for e in r.list() {
        let m = &e.manifest;
        for op in m.ops.keys() {
            let (values, assets, files) = lg_manifests::sample::sample(m, op);
            // Real input images, so only model files can be missing.
            for name in assets.values() {
                let mut png = Vec::new();
                image::RgbImage::from_pixel(64, 64, image::Rgb([90, 120, 200]))
                    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                    .unwrap();
                client.upload_image(png, name).await.unwrap();
            }
            let spec: lg_types::JobSpec =
                serde_json::from_value(json!({"model": m.id, "op": op, "values": values})).unwrap();
            let t = r.template(&m.id, op).unwrap();
            let opts = lg_workflow::BindOptions {
                files,
                assets,
                vae_tiled: false,
                output_prefix: Some("lg/test".into()),
            };
            let (_, g) = lg_workflow::bind(m, &spec, &t, &opts).unwrap();
            let resp = http
                .post(format!("{url}/prompt"))
                .json(&json!({"prompt": g, "client_id": "lg-test"}))
                .send()
                .await
                .unwrap();
            let status_ok = resp.status().is_success();
            let body: Value = resp.json().await.unwrap();
            let ok = status_ok && body["node_errors"].as_object().is_none_or(|o| o.is_empty());
            n += 1;
            if status_ok {
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
            }
            if ok {
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
            let mut other = Vec::new();
            for (node, ne) in body["node_errors"].as_object().into_iter().flatten() {
                for err in ne["errors"].as_array().into_iter().flatten() {
                    let details = err["details"].as_str().unwrap_or_default();
                    let model_file = err["type"] == "value_not_in_list"
                        && [".safetensors", ".gguf", ".pt", ".ckpt"]
                            .iter()
                            .any(|x| details.contains(&format!("{x}'")));
                    if !model_file {
                        other.push(format!(
                            "{node} {}: {} {details}",
                            ne["class_type"], err["type"]
                        ));
                    }
                }
            }
            if body["node_errors"].as_object().is_none_or(|o| o.is_empty()) {
                other.push(body["error"].to_string());
            }
            if other.is_empty() {
                println!("ok {}/{op} (only model files missing)", m.id);
            } else {
                bad.push(format!("{}/{op}: {}", m.id, other.join("; ")));
            }
        }
    }
    for b in &bad {
        println!("BAD {b}");
    }
    assert!(n >= 11);
    assert!(bad.is_empty());
}
