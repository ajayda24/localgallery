//! The real ComfyUI client against the protocol-faithful fake server.

use lg_engine::comfy::ComfyClient;
use lg_engine::{ComfyEngine, Engine, EngineError, EngineEvent};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn start(step_ms: u64) -> (ComfyEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lg_fakecomfy::Config::new(dir.path().to_path_buf());
    cfg.step_ms = step_ms;
    let (addr, _) = lg_fakecomfy::serve(cfg, ([127, 0, 0, 1], 0).into())
        .await
        .unwrap();
    (
        ComfyEngine {
            client: ComfyClient::new(format!("http://{addr}")),
        },
        dir,
    )
}

fn txt2img(prompt: &str, steps: u32) -> Value {
    json!({
        "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "model.safetensors"}},
        "6": {"class_type": "CLIPTextEncode", "inputs": {"text": prompt, "clip": ["4", 1]}},
        "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["4", 1]}},
        "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 320, "height": 192, "batch_size": 1}},
        "3": {"class_type": "KSampler", "inputs": {"model": ["4", 0], "seed": 7, "steps": steps, "cfg": 7.0, "sampler_name": "euler",
              "scheduler": "normal", "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0], "denoise": 1.0}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "lg/job1"}}
    })
}

#[tokio::test]
async fn runs_and_streams_progress() {
    let (engine, _d) = start(5).await;
    assert!(engine.healthy().await);
    let (tx, mut rx) = mpsc::channel(256);
    let outs = engine
        .run(&txt2img("a cat", 6), tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0].subfolder, "lg");
    let png = engine.fetch(&outs[0]).await.unwrap();
    let img = image_size(&png);
    assert_eq!(img, (320, 192));
    let mut steps = 0;
    let mut previews = 0;
    while let Ok(e) = rx.try_recv() {
        match e {
            EngineEvent::Progress { total, .. } => {
                assert_eq!(total, 6);
                steps += 1
            }
            EngineEvent::Preview { .. } => previews += 1,
            _ => {}
        }
    }
    assert_eq!(steps, 6);
    assert_eq!(previews, 6);
}

fn image_size(png: &[u8]) -> (u32, u32) {
    // PNG IHDR: width and height at bytes 16..24.
    assert_eq!(&png[1..4], b"PNG");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

#[tokio::test]
async fn cancel_mid_run() {
    let (engine, _d) = start(50).await;
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        c2.cancel();
    });
    let (tx, _rx) = mpsc::channel(256);
    let t0 = std::time::Instant::now();
    let r = engine.run(&txt2img("slow", 200), tx, cancel).await;
    assert_eq!(r.unwrap_err(), EngineError::Cancelled);
    assert!(t0.elapsed().as_secs() < 5, "cancel must be prompt");
    // The engine is free again afterwards.
    let (tx, _rx) = mpsc::channel(256);
    assert!(
        engine
            .run(&txt2img("after", 2), tx, CancellationToken::new())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn oom_and_errors_are_classified() {
    let (engine, _d) = start(1).await;
    let (tx, _rx) = mpsc::channel(256);
    let r = engine
        .run(
            &txt2img("FAKE_OOM", 2),
            tx.clone(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(r, Err(EngineError::OutOfMemory(_))), "{r:?}");
    let r = engine
        .run(&txt2img("FAKE_ERROR", 2), tx, CancellationToken::new())
        .await;
    assert!(matches!(r, Err(EngineError::Failed(m)) if m.contains("fake node failure")));
}

#[tokio::test]
async fn invalid_graph_is_rejected_with_details() {
    let (engine, _d) = start(1).await;
    let mut g = txt2img("x", 2);
    g["10"] = json!({"class_type": "NotARealNode", "inputs": {}});
    let (tx, _rx) = mpsc::channel(8);
    let r = engine.run(&g, tx, CancellationToken::new()).await;
    assert!(
        matches!(&r, Err(EngineError::Rejected(m)) if m.contains("NotARealNode")),
        "{r:?}"
    );
}

#[tokio::test]
async fn upload_then_reference() {
    let (engine, _d) = start(1).await;
    let mut png = Vec::new();
    image::RgbImage::new(8, 8)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok();
    let name = engine.upload(png, "lg_asset_ab.png").await.unwrap();
    assert_eq!(name, "lg_asset_ab.png");
    let g = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": name}},
        "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "x"}}
    });
    let (tx, _rx) = mpsc::channel(8);
    engine.run(&g, tx, CancellationToken::new()).await.unwrap();
    let bad = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "missing.png"}},
        "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "x"}}
    });
    let (tx, _rx) = mpsc::channel(8);
    assert!(matches!(
        engine.run(&bad, tx, CancellationToken::new()).await,
        Err(EngineError::Rejected(_))
    ));
}
