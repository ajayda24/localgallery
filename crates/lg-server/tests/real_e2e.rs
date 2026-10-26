//! The whole stack against a real ComfyUI (no model weights needed). Opt-in:
//!
//! `LG_REAL_COMFY_URL=http://127.0.0.1:8199 LG_COMFY_DIR=/path/to/ComfyUI \
//!   cargo test -p lg-server --test real_e2e -- --ignored --nocapture`

use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const TOKEN: &str = "real-token";

struct C {
    base: String,
    http: reqwest::Client,
}

impl C {
    async fn get(&self, p: &str) -> Value {
        self.http
            .get(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn post(&self, p: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }
    async fn upload(&self, name: &str, bytes: Vec<u8>) -> String {
        let r = self
            .http
            .post(format!("{}/api/assets?name={name}", self.base))
            .bearer_auth(TOKEN)
            .body(bytes)
            .send()
            .await
            .unwrap();
        r.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
    async fn bytes(&self, p: &str) -> Vec<u8> {
        self.http
            .get(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .to_vec()
    }
    async fn run(&self, spec: Value) -> Value {
        let (s, job) = self.post("/api/jobs", spec).await;
        assert_eq!(s, 200, "{job}");
        let id = job["id"].as_str().unwrap().to_string();
        for _ in 0..1200 {
            let jobs = self.get("/api/jobs").await;
            if let Some(j) = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == id.as_str())
                && matches!(j["status"].as_str(), Some("done" | "failed" | "cancelled"))
            {
                return j.clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timeout");
    }
}

fn png(img: &image::RgbImage) -> Vec<u8> {
    let mut b = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png)
        .unwrap();
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a running ComfyUI (LG_REAL_COMFY_URL) and its checkout (LG_COMFY_DIR)"]
async fn whole_stack_on_real_comfyui() {
    let url = std::env::var("LG_REAL_COMFY_URL").expect("LG_REAL_COMFY_URL");
    assert!(
        std::env::var_os("LG_COMFY_DIR").is_some(),
        "LG_COMFY_DIR is needed for blueprints"
    );
    let tmp = tempfile::tempdir().unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let r = lg_server::serve(
        lg_server::Options {
            root: tmp.path().join("Local Gallery"),
            mode: lg_server::Mode::External(url),
            manifests: Some(repo.join("manifests")),
            ui: None,
            token: Some(TOKEN.into()),
        },
        0,
        lg_server::api::ServeOptions {
            any_host: false,
            no_auth: false,
        },
    )
    .await
    .unwrap();
    let c = C {
        base: format!("http://{}", r.addr),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };

    // Starting the engine turns its official blueprints into tools.
    let (s, b) = c.post("/api/engine/start", json!({})).await;
    assert_eq!(s, 200, "{b}");
    let models = c.get("/api/models").await;
    let tools: Vec<&str> = models
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .filter(|id| id.starts_with("bp-"))
        .collect();
    println!("{} blueprint tools", tools.len());
    assert!(tools.len() >= 100, "{}", tools.len());
    let unavailable: Vec<&Value> = models
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| !m["unavailable"].is_null())
        .collect();
    for m in &unavailable {
        println!("unavailable: {} - {}", m["id"], m["unavailable"]);
    }
    // Built-in manifests all have their nodes in this ComfyUI.
    for id in [
        "sd-checkpoint",
        "z-image-turbo",
        "ideogram-v4",
        "flux2-klein-4b",
        "wan22-14b",
    ] {
        let m = models
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap();
        assert!(m["unavailable"].is_null(), "{id}: {}", m["unavailable"]);
    }

    // A real blueprint tool, run through the queue: crop an image into 2x2 tiles.
    let mut src = image::RgbImage::new(256, 256);
    for (x, y, p) in src.enumerate_pixels_mut() {
        *p = image::Rgb([
            (x * 7 % 256) as u8,
            (y * 13 % 256) as u8,
            ((x ^ y) % 256) as u8,
        ]);
    }
    let src_id = c.upload("source.png", png(&src)).await;
    let crop = tools
        .iter()
        .find(|t| t.contains("crop-images-2x2"))
        .expect("crop tool");
    let tool = c.get(&format!("/api/models/{crop}")).await;
    let image_input = tool["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "image")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let j = c
        .run(json!({"model": crop, "op": "run", "values": {image_input: src_id}}))
        .await;
    assert_eq!(j["status"], "done", "{j}");
    let outs = c.get(&format!("/api/outputs?model={crop}")).await;
    assert!(!outs.as_array().unwrap().is_empty());
    let first = &outs.as_array().unwrap()[0];
    assert_eq!(
        first["width"].as_u64(),
        Some(128),
        "a 256 px image cut 2x2 gives 128 px tiles: {first}"
    );

    // The inpaint composite stage keeps every pixel outside the (grown) mask.
    let mut mask = image::RgbImage::new(256, 256);
    for (x, y, p) in mask.enumerate_pixels_mut() {
        let inside = (96..160).contains(&x) && (96..160).contains(&y);
        *p = if inside {
            image::Rgb([255, 255, 255])
        } else {
            image::Rgb([0, 0, 0])
        };
    }
    let mask_id = c.upload("mask.png", png(&mask)).await;
    let graph = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "x.png"}},
        "2": {"class_type": "LoadImageMask", "inputs": {"image": "m.png", "channel": "red"}},
        "3": {"class_type": "GrowMask", "inputs": {"mask": ["2", 0], "expand": 8, "tapered_corners": true}},
        "4": {"class_type": "EmptyImage", "inputs": {"width": 256, "height": 256, "batch_size": 1, "color": 255}},
        "5": {"class_type": "ImageCompositeMasked", "inputs": {"destination": ["1", 0], "source": ["4", 0], "x": 0, "y": 0, "resize_source": true, "mask": ["3", 0]}},
        "6": {"class_type": "SaveImage", "inputs": {"images": ["5", 0], "filename_prefix": "x"}}
    });
    let fields = json!([
        {"id": "image", "label": "Image", "kind": "image", "targets": [{"node": "1", "input": "image"}]},
        {"id": "mask", "label": "Mask", "kind": "mask", "targets": [{"node": "2", "input": "image"}]}
    ]);
    let (s, p) = c
        .post(
            "/api/tools",
            json!({"graph": graph, "fields": fields, "meta": {"name": "Composite check"}}),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let j = c
        .run(json!({"model": p["id"], "op": "run", "values": {"image": src_id, "mask": mask_id}}))
        .await;
    assert_eq!(j["status"], "done", "{j}");
    let o = &c
        .get(&format!("/api/outputs?model={}", p["id"].as_str().unwrap()))
        .await[0];
    let out = image::load_from_memory(
        &c.bytes(&format!("/files/{}", o["path"].as_str().unwrap()))
            .await,
    )
    .unwrap()
    .to_rgb8();
    let mut changed_outside = 0;
    let mut changed_inside = 0;
    for (x, y, px) in out.enumerate_pixels() {
        let far_outside = !(80..176).contains(&x) || !(80..176).contains(&y);
        let deep_inside = (100..156).contains(&x) && (100..156).contains(&y);
        if far_outside && px != src.get_pixel(x, y) {
            changed_outside += 1;
        }
        if deep_inside && px != src.get_pixel(x, y) {
            changed_inside += 1;
        }
    }
    assert_eq!(
        changed_outside, 0,
        "pixels outside the mask must be untouched"
    );
    assert!(changed_inside > 3000, "the masked area is replaced");
    r.handle.abort();
}
