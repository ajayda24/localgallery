//! The whole core end to end, with the fake engine serving the real ComfyUI node set.

use futures::StreamExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TOKEN: &str = "test-token-123";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn start(root: &Path, step_ms: u64) -> lg_server::Running {
    lg_server::serve(
        lg_server::Options {
            root: root.to_path_buf(),
            mode: lg_server::Mode::Fake {
                object_info: Some(repo().join("tests/fixtures/object_info.json")),
                step_ms,
            },
            manifests: Some(repo().join("manifests")),
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
    .unwrap()
}

struct C {
    base: String,
    http: reqwest::Client,
}

impl C {
    fn new(r: &lg_server::Running) -> Self {
        Self {
            base: format!("http://{}", r.addr),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }
    async fn get(&self, p: &str) -> Value {
        let r = self
            .http
            .get(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {p}: {}", r.status());
        r.json().await.unwrap()
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
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
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
        assert!(r.status().is_success());
        r.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
    async fn bytes(&self, p: &str) -> Vec<u8> {
        let r = self
            .http
            .get(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {p}");
        r.bytes().await.unwrap().to_vec()
    }
    /// Submit and wait for the job to end.
    async fn run(&self, spec: Value) -> Value {
        let (s, job) = self.post("/api/jobs", spec).await;
        assert_eq!(s, 200, "{job}");
        self.wait(job["id"].as_str().unwrap()).await
    }
    async fn wait(&self, id: &str) -> Value {
        for _ in 0..600 {
            let jobs = self.get("/api/jobs").await;
            if let Some(j) = jobs.as_array().unwrap().iter().find(|j| j["id"] == id)
                && matches!(j["status"].as_str(), Some("done" | "failed" | "cancelled"))
            {
                return j.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job {id} did not finish");
    }
}

fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut b = Vec::new();
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png)
        .unwrap();
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn core_end_to_end_with_fake_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Local Gallery ü");
    let r = start(&root, 3).await;
    let c = C::new(&r);

    // ---- security ----
    let h = c
        .http
        .get(format!("{}/api/health", c.base))
        .send()
        .await
        .unwrap();
    assert_eq!(h.status(), 200);
    assert_eq!(
        c.http
            .get(format!("{}/api/system", c.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let evil = c
        .http
        .get(format!("{}/api/health", c.base))
        .header("host", "evil.example:80")
        .send()
        .await
        .unwrap();
    assert_eq!(evil.status(), 403, "DNS rebinding is blocked");
    let login = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r1 = login
        .get(format!("{}/?token={TOKEN}", c.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 303);
    let cookie = r1.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let with_cookie = login
        .get(format!("{}/api/system", c.base))
        .header("cookie", cookie.split(';').next().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(with_cookie.status(), 200);

    // ---- events over the websocket ----
    let ws_url = format!("ws://{}/api/events?token={TOKEN}", r.addr);
    let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    let (_, mut ws_rx) = ws.split();
    let events = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<Value>::new()));
    let ev2 = events.clone();
    tokio::spawn(async move {
        while let Some(Ok(m)) = ws_rx.next().await {
            if let Ok(v) = serde_json::from_str::<Value>(m.to_text().unwrap_or("")) {
                ev2.lock().push(v);
            }
        }
    });

    // ---- models ----
    let models = c.get("/api/models").await;
    let ids: Vec<&str> = models
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    for want in [
        "sd-checkpoint",
        "z-image-turbo",
        "ideogram-v4",
        "flux2-klein-4b",
        "wan22-14b",
    ] {
        assert!(ids.contains(&want), "{ids:?}");
    }
    let sd = c.get("/api/models/sd-checkpoint").await;
    assert!(
        sd["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "regions")
    );
    assert!(
        sd["fit"]["explain"]
            .as_array()
            .is_some_and(|e| !e.is_empty())
    );

    // ---- plan ----
    let (_, p) = c.post("/api/plan", json!({"model": "sd-checkpoint", "op": "txt2img", "values": {"prompt": "x", "size": {"w": 1216, "h": 832}}})).await;
    assert_eq!(p["ok"], true, "{p}");
    assert_eq!(p["values"]["size"], json!({"w": 1216, "h": 832}));
    let (_, p) = c
        .post(
            "/api/plan",
            json!({"model": "sd-checkpoint", "op": "txt2img", "values": {}}),
        )
        .await;
    assert_eq!(p["ok"], false, "missing prompt is reported");

    // ---- txt2img with regions and LoRA ----
    let j = c
        .run(
            json!({"model": "sd-checkpoint", "op": "txt2img", "values": {
            "prompt": "a harbour at dawn", "size": {"w": 832, "h": 1216}, "seed": 7, "steps": 6,
            "regions": [{"x": 0.1, "y": 0.5, "w": 0.3, "h": 0.3, "prompt": "a red boat"}],
            "loras": [{"file": "detail.safetensors", "strength": 0.6}]}}),
        )
        .await;
    assert_eq!(j["status"], "done", "{j}");
    let outs = c.get("/api/outputs").await;
    let o = &outs[0];
    assert_eq!(
        (o["width"].as_u64(), o["height"].as_u64()),
        (Some(832), Some(1216))
    );
    assert_eq!(o["seed"], 7);
    assert_eq!(o["prompt"], "a harbour at dawn");
    let img = image::load_from_memory(
        &c.bytes(&format!("/files/{}", o["path"].as_str().unwrap()))
            .await,
    )
    .unwrap();
    assert_eq!((img.width(), img.height()), (832, 1216));
    let thumb = image::load_from_memory(
        &c.bytes(&format!("/files/{}", o["thumb"].as_str().unwrap()))
            .await,
    )
    .unwrap();
    assert!(thumb.width() <= 384 && thumb.height() <= 384);
    let detail = c
        .get(&format!("/api/outputs/{}", o["id"].as_str().unwrap()))
        .await;
    let graph = detail["sidecar"]["engine_graph"].to_string();
    assert!(
        graph.contains("ConditioningSetAreaPercentage") && graph.contains("a red boat"),
        "regions reached the engine"
    );
    assert!(
        graph.contains("detail.safetensors"),
        "lora reached the engine"
    );
    assert!(
        events.lock().iter().any(|e| e["type"] == "progress"),
        "progress streamed"
    );
    assert!(
        events.lock().iter().any(|e| e["type"] == "preview"),
        "previews streamed"
    );
    // Path traversal through /files is refused.
    let t = c
        .http
        .get(format!("{}/files/outputs/../localgallery.db", c.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_ne!(t.status(), 200);

    // ---- img2img and inpaint with uploads ----
    let src = c.upload("photo.png", png(640, 480, [10, 200, 30])).await;
    let mask = c.upload("mask.png", png(640, 480, [255, 255, 255])).await;
    let j = c.run(json!({"model": "sd-checkpoint", "op": "img2img", "values": {"prompt": "make it autumn", "image": src, "denoise": 0.5}})).await;
    assert_eq!(j["status"], "done", "{j}");
    let j = c.run(json!({"model": "sd-checkpoint", "op": "inpaint", "values": {"prompt": "a cat", "image": src, "mask": mask}})).await;
    assert_eq!(j["status"], "done", "{j}");
    let (s, e) = c.post("/api/jobs", json!({"model": "sd-checkpoint", "op": "inpaint", "values": {"prompt": "a cat", "image": src}})).await;
    assert_eq!(s, 400, "mask is required for inpaint: {e}");

    // ---- JSON layout prompt with boxes (Ideogram 4) ----
    let j = c
        .run(json!({"model": "ideogram-v4", "op": "layout", "values": {"prompt": "a poster", "background": "a beach",
            "regions": [{"x": 0.1, "y": 0.1, "w": 0.8, "h": 0.2, "type": "text", "text": "SUMMER"}], "palette": "#ff0000, #00ff00"}}))
        .await;
    assert_eq!(j["status"], "done", "{j}");

    // ---- video ----
    let first = c.upload("start.png", png(832, 480, [200, 100, 50])).await;
    let j = c.run(json!({"model": "wan22-14b", "op": "img2vid", "values": {"prompt": "waves", "first_frame": first, "frames": 17}})).await;
    assert_eq!(j["status"], "done", "{j}");
    let vids = c.get("/api/outputs?media=video").await;
    assert_eq!(vids.as_array().unwrap().len(), 1);

    // ---- batch runs use consecutive seeds ----
    let j = c.run(json!({"model": "z-image-turbo", "op": "txt2img", "batch": 3, "values": {"prompt": "fox", "seed": 100, "size": {"w": 512, "h": 512}}})).await;
    assert_eq!(j["outputs"].as_array().unwrap().len(), 3);
    let seeds: Vec<i64> = c
        .get("/api/outputs?model=z-image-turbo")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["seed"].as_i64().unwrap())
        .collect();
    let mut sorted = seeds.clone();
    sorted.sort();
    assert_eq!(sorted, vec![100, 101, 102]);

    // ---- out-of-memory: retried with lighter plans, then reported ----
    let j = c.run(json!({"model": "sd-checkpoint", "op": "txt2img", "values": {"prompt": "FAKE_OOM", "steps": 2}})).await;
    assert_eq!(j["status"], "failed");
    assert!(j["error"].as_str().unwrap().contains("memory"), "{j}");
    let retries = events
        .lock()
        .iter()
        .filter(|e| {
            e["type"] == "log"
                && e["line"]
                    .as_str()
                    .is_some_and(|l| l.contains("lighter plan"))
        })
        .count();
    assert_eq!(retries, 3, "three escalation levels were tried");

    // ---- cancel a running job ----
    let (_, slow) = c.post("/api/jobs", json!({"model": "sd-checkpoint", "op": "txt2img", "values": {"prompt": "slow", "steps": 150}})).await;
    let id = slow["id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        if c.get("/api/system").await["running_job"] == id {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (_, ok) = c.post(&format!("/api/jobs/{id}/cancel"), json!({})).await;
    assert_eq!(ok["ok"], true);
    assert_eq!(c.wait(&id).await["status"], "cancelled");

    // ---- import an official blueprint and run it as a tool ----
    let bp: Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo().join("tests/fixtures/blueprints/Text to Image (Z-Image-Turbo).json"),
        )
        .unwrap(),
    )
    .unwrap();
    let (s, imp) = c
        .post("/api/workflows/import", json!({"workflow": bp}))
        .await;
    assert_eq!(s, 200, "{imp}");
    assert!(imp["candidates"].as_array().unwrap().len() > 3);
    let (s, pubd) = c
        .post("/api/tools", json!({"manifest": imp["suggested_tool"]}))
        .await;
    assert_eq!(s, 200, "{pubd}");
    let tool_id = pubd["id"].as_str().unwrap().to_string();
    assert!(
        root.join("data/tools")
            .join(format!("{tool_id}.json"))
            .exists()
    );
    let j = c.run(json!({"model": tool_id, "op": "run", "values": {"prompt": "a lighthouse", "size": {"w": 768, "h": 512}}})).await;
    assert_eq!(j["status"], "done", "{j}");

    // ---- benchmarks recorded ----
    let b = c.get("/api/benchmarks?model=sd-checkpoint").await;
    assert!(!b.as_array().unwrap().is_empty());

    // ---- settings never echo the token ----
    let (_, s) = c
        .http
        .put(format!("{}/api/settings", c.base))
        .bearer_auth(TOKEN)
        .json(&json!({"hf_token": "hf_secret", "previews": false}))
        .send()
        .await
        .map(|r| (r.status(), ()))
        .unwrap();
    assert_eq!(s, ());
    let st = c.get("/api/settings").await;
    assert_eq!(st["hf_token"], "");
    assert_eq!(st["hf_token_set"], true);
    assert_eq!(st["previews"], false);

    // ---- everything stayed inside the app folder ----
    assert!(root.join("data/localgallery.db").exists());
    assert!(root.join("portable.txt").exists());
    let total_outputs = c.get("/api/outputs").await.as_array().unwrap().len();
    r.handle.abort();
    drop(r);

    // ---- a restart sees the same gallery ----
    let r2 = start(&root, 3).await;
    let c2 = C::new(&r2);
    assert_eq!(
        c2.get("/api/outputs").await.as_array().unwrap().len(),
        total_outputs
    );
}
