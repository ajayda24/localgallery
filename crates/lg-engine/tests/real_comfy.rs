//! Against a real ComfyUI install. Opt-in:
//!
//! ```text
//! LG_COMFY_PYTHON=/path/to/venv/bin/python LG_COMFY_DIR=/path/to/ComfyUI \
//!   cargo test -p lg-engine --test real_comfy -- --ignored --nocapture
//! ```
//!
//! Uses only core nodes (no model weights), so it runs on CPU-only machines.

use lg_engine::comfy::ComfyClient;
use lg_engine::supervisor::{LaunchConfig, Supervisor, free_port};
use lg_engine::{ComfyEngine, Engine, EngineError};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).map(PathBuf::from)
}

#[tokio::test]
#[ignore = "needs a real ComfyUI install (LG_COMFY_PYTHON, LG_COMFY_DIR)"]
async fn supervisor_launches_real_comfy_and_runs_core_workflow() {
    let (Some(python), Some(comfy_dir)) = (env_path("LG_COMFY_PYTHON"), env_path("LG_COMFY_DIR"))
    else {
        panic!("set LG_COMFY_PYTHON and LG_COMFY_DIR");
    };
    // A path with spaces and non-ASCII characters, like a real portable folder might have.
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("Local Gallery ü");
    let cfg = LaunchConfig {
        python,
        comfy_dir,
        base_dir: app.join("engines/full/base"),
        output_dir: app.join("data/engine-output"),
        input_dir: app.join("data/engine-input"),
        temp_dir: app.join("data/tmp/comfy"),
        user_dir: app.join("data/engine-user"),
        extra_model_paths: None,
        port: free_port(),
        flags: vec!["--cpu".into()],
        env: vec![],
        log_file: app.join("data/logs/engine.log"),
    };
    let sup = Supervisor::new();
    let lines = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let l2 = lines.clone();
    let client: ComfyClient = sup
        .start(
            cfg.clone(),
            Arc::new(move |l| l2.lock().push(l)),
            Duration::from_secs(240),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", lines.lock().join("\n")));
    let engine = ComfyEngine { client };
    assert!(engine.healthy().await);
    let info = engine.object_info().await.unwrap();
    assert!(info.get("KSampler").is_some() && info.get("CreateBoundingBoxes").is_some());

    // Upload, process with core nodes, save.
    let mut png = Vec::new();
    image::RgbImage::from_pixel(64, 48, image::Rgb([200, 30, 30]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let name = engine.upload(png, "lg_test_src.png").await.unwrap();
    let graph = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": name}},
        "2": {"class_type": "ImageScale", "inputs": {"image": ["1", 0], "upscale_method": "bilinear", "width": 128, "height": 96, "crop": "disabled"}},
        "3": {"class_type": "ImageInvert", "inputs": {"image": ["2", 0]}},
        "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "lg/realtest"}}
    });
    let issues = lg_workflow::objectinfo::validate(&graph, &info);
    assert!(issues.is_empty(), "{issues:?}");
    let (tx, _rx) = mpsc::channel(64);
    let outs = engine
        .run(&graph, tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outs.len(), 1);
    let bytes = engine.fetch(&outs[0]).await.unwrap();
    let img = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert_eq!(img.dimensions(), (128, 96));
    let px = img.get_pixel(64, 48);
    assert!(
        px[0] < 80 && px[1] > 180,
        "inverted red should be cyan-ish: {px:?}"
    );
    assert!(
        cfg.output_dir.join("lg").exists(),
        "outputs land in the configured folder"
    );

    // A broken graph is rejected with a readable reason.
    let bad = json!({"1": {"class_type": "SaveImage", "inputs": {"images": ["9", 0], "filename_prefix": "x"}}});
    let (tx, _rx) = mpsc::channel(8);
    let r = engine.run(&bad, tx, CancellationToken::new()).await;
    assert!(matches!(r, Err(EngineError::Rejected(_))), "{r:?}");

    sup.stop().await;
    assert!(!sup.is_running());
    println!(
        "--- engine log tail ---\n{}",
        lines
            .lock()
            .iter()
            .rev()
            .take(15)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
