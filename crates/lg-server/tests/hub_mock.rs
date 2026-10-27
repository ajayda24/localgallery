//! The Hugging Face browser and size lookups against a mock Hugging Face server: repo search,
//! file listing (paginated), folder mapping, "used by", sizes filled into model cards, a
//! download into the right folder, and a gated repo.

use axum::extract::{Path as AxPath, Query};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TOKEN: &str = "test-token-123";
const DIT: u64 = 12_309_866_400;
const TE: u64 = 8_044_982_048;
const VAE_BYTES: usize = 4096;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn size_of(path: &str) -> Option<u64> {
    if path.ends_with("z_image_turbo_bf16.safetensors") {
        Some(DIT)
    } else if path.ends_with("qwen_3_4b.safetensors") {
        Some(TE)
    } else if path.ends_with("ae.safetensors") {
        Some(VAE_BYTES as u64)
    } else {
        None
    }
}

async fn mock_hub() -> String {
    async fn models(Query(q): Query<HashMap<String, String>>) -> Json {
        let all = vec![
            json!({"id": "Comfy-Org/z_image_turbo", "lastModified": "2026-10-01T00:00:00.000Z", "downloads": 1000, "likes": 50, "gated": false}),
            json!({"id": "Comfy-Org/MOSS-Image", "lastModified": "2026-10-20T00:00:00.000Z", "downloads": 10, "likes": 3, "gated": false}),
            json!({"id": "Comfy-Org/gated_model", "gated": "auto"}),
        ];
        assert_eq!(q.get("author").map(String::as_str), Some("Comfy-Org"));
        let s = q.get("search").cloned().unwrap_or_default().to_lowercase();
        axum::Json(
            all.into_iter()
                .filter(|r| r["id"].as_str().unwrap().to_lowercase().contains(&s))
                .collect(),
        )
    }
    type Json = axum::Json<Vec<Value>>;
    async fn tree(
        AxPath((org, name)): AxPath<(String, String)>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Response {
        match (org.as_str(), name.as_str()) {
            ("Comfy-Org", "z_image_turbo") => {
                // Two pages, like the real API for large repos.
                if !q.contains_key("cursor") {
                    let mut h = HeaderMap::new();
                    h.insert(header::LINK, "</api/models/Comfy-Org/z_image_turbo/tree/main?recursive=true&cursor=p2>; rel=\"next\"".parse().unwrap());
                    (h, axum::Json(json!([
                        {"type": "directory", "path": "split_files"},
                        {"type": "file", "path": "README.md", "size": 1200},
                        {"type": "file", "path": "split_files/diffusion_models/z_image_turbo_bf16.safetensors", "size": 135, "lfs": {"size": DIT}},
                    ]))).into_response()
                } else {
                    axum::Json(json!([
                        {"type": "file", "path": "split_files/text_encoders/qwen_3_4b.safetensors", "size": 135, "lfs": {"size": TE}},
                        {"type": "file", "path": "split_files/vae/ae.safetensors", "size": VAE_BYTES},
                        {"type": "file", "path": "extra/flux1-dev-Q4_K_S.gguf", "size": 6_800_000_000u64},
                    ])).into_response()
                }
            }
            ("Comfy-Org", "gated_model") => StatusCode::UNAUTHORIZED.into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }
    async fn resolve(
        method: Method,
        AxPath((_org, _name, path)): AxPath<(String, String, String)>,
        headers: HeaderMap,
    ) -> Response {
        // The access token must never leave for a host that is not Hugging Face.
        assert!(
            headers.get(header::AUTHORIZATION).is_none(),
            "token sent to a non-HF host"
        );
        let Some(size) = size_of(&path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if method == Method::HEAD {
            let mut h = HeaderMap::new();
            h.insert("x-linked-size", size.to_string().parse().unwrap());
            h.insert(header::LOCATION, "/cdn/blob".parse().unwrap());
            return (StatusCode::FOUND, h).into_response();
        }
        assert!(
            path.ends_with("ae.safetensors"),
            "only the small file is downloaded in tests"
        );
        vec![7u8; VAE_BYTES].into_response()
    }
    let app = axum::Router::new()
        .route("/api/models", get(models))
        .route("/api/models/{org}/{name}/tree/main", get(tree))
        .route("/{org}/{name}/resolve/main/{*path}", any(resolve));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

struct C {
    base: String,
    http: reqwest::Client,
}

impl C {
    async fn get(&self, p: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{p}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn ok(&self, p: &str) -> Value {
        let (s, v) = self.get(p).await;
        assert_eq!(s, 200, "GET {p}: {v}");
        v
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hugging_face_browser_and_sizes() {
    let hub = mock_hub().await;
    let tmp = tempfile::tempdir().unwrap();
    let r = lg_server::serve(
        lg_server::Options {
            root: tmp.path().join("Local Gallery ü"),
            mode: lg_server::Mode::Fake {
                object_info: Some(repo().join("tests/fixtures/object_info.json")),
                step_ms: 5,
            },
            manifests: Some(repo().join("manifests")),
            ui: None,
            token: Some(TOKEN.into()),
            hf_endpoint: Some(hub.clone()),
        },
        0,
        lg_server::api::ServeOptions::default(),
    )
    .await
    .unwrap();
    let c = C {
        base: format!("http://{}", r.addr),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    // A token is set, to prove it is not sent to the (non-Hugging Face) mock host.
    let (s, _) = {
        let r = c
            .http
            .put(format!("{}/api/settings", c.base))
            .bearer_auth(TOKEN)
            .json(&json!({"hf_token": "hf_secret"}))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), ())
    };
    assert_eq!(s, 200);

    // ---- sizes: looked up in the background, then shown on the model card ----
    let mut card = Value::Null;
    for _ in 0..100 {
        card = c.ok("/api/models/z-image-turbo").await;
        let dit = card["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["role"] == "diffusion")
            .cloned();
        if card["fit"]["sizes_known"] == true
            && dit.is_some_and(|d| d["variants"][0]["size_gb"].as_f64() > Some(0.0))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        card["fit"]["sizes_known"], true,
        "sizes were not looked up: {}",
        card["fit"]
    );
    let dit = card["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["role"] == "diffusion")
        .unwrap();
    let gb = dit["variants"][0]["size_gb"].as_f64().unwrap();
    assert!((gb - DIT as f64 / 1024f64.powi(3)).abs() < 1e-6, "{gb}");
    assert!(
        !card["fit"]["explain"][0]
            .as_str()
            .unwrap()
            .contains("0.0 GB"),
        "{}",
        card["fit"]
    );
    // Cached for the next launch.
    let cache = std::fs::read_to_string(
        tmp.path()
            .join("Local Gallery ü/data/cache/file-sizes.json"),
    )
    .unwrap();
    assert!(cache.contains("z_image_turbo_bf16.safetensors"), "{cache}");

    // ---- repos ----
    let repos = c.ok("/api/hub/repos").await;
    assert_eq!(repos["repos"].as_array().unwrap().len(), 3, "{repos}");
    let moss = c.ok("/api/hub/repos?q=moss").await;
    assert_eq!(moss["repos"][0]["id"], "Comfy-Org/MOSS-Image");
    assert_eq!(moss["repos"].as_array().unwrap().len(), 1);
    let gated = repos["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "Comfy-Org/gated_model")
        .unwrap();
    assert_eq!(gated["gated"], true);

    // ---- files (two pages), folders, used by ----
    let files = c.ok("/api/hub/files?repo=Comfy-Org/z_image_turbo").await;
    let list = files["files"].as_array().unwrap();
    assert_eq!(list.len(), 4, "{files}");
    assert_eq!(files["other_files"], 1, "README is not a model file");
    let by = |n: &str| list.iter().find(|f| f["name"] == n).unwrap().clone();
    let dit = by("z_image_turbo_bf16.safetensors");
    assert_eq!(dit["folder"], "diffusion_models");
    assert_eq!(dit["size_bytes"], DIT);
    assert!(
        dit["used_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == "z-image-turbo"),
        "{dit}"
    );
    assert_eq!(by("qwen_3_4b.safetensors")["folder"], "text_encoders");
    assert!(
        by("flux1-dev-Q4_K_S.gguf")["folder"].is_null(),
        "no folder in its path: the user picks one"
    );
    let vae = by("ae.safetensors");
    assert_eq!(vae["folder"], "vae");
    assert_eq!(vae["present"], false);
    assert!(
        vae["url"].as_str().unwrap().starts_with(&hub),
        "downloads go through the configured endpoint"
    );

    // ---- bad input and errors ----
    assert_eq!(c.get("/api/hub/files?repo=../../etc").await.0, 400);
    assert_eq!(c.get("/api/hub/files?repo=Comfy-Org/nope").await.0, 404);
    let (s, v) = c.get("/api/hub/files?repo=Comfy-Org/gated_model").await;
    assert_eq!(s, 403);
    assert!(
        v["error"]
            .as_str()
            .unwrap_or(&v.to_string())
            .contains("token"),
        "{v}"
    );

    // ---- download into the right folder ----
    let r2 = c
        .http
        .post(format!("{}/api/downloads", c.base))
        .bearer_auth(TOKEN)
        .json(&json!({"url": vae["url"], "folder": "vae", "name": "ae.safetensors"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
    let mut present = false;
    for _ in 0..100 {
        let f = c.ok("/api/hub/files?repo=Comfy-Org/z_image_turbo").await;
        if f["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "ae.safetensors" && f["present"] == true)
        {
            present = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(present, "downloaded file shows as on disk");
    let path = tmp.path().join("Local Gallery ü/models/vae/ae.safetensors");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), VAE_BYTES as u64);
    // Unsafe direct downloads are refused.
    for body in [
        json!({"url": "file:///etc/passwd", "folder": "vae", "name": "x.safetensors"}),
        json!({"url": vae["url"], "folder": "vae", "name": "..\\x.safetensors"}),
        json!({"url": vae["url"], "folder": "vae", "name": "C:x.safetensors"}),
        json!({"url": vae["url"], "folder": "secrets", "name": "x.safetensors"}),
    ] {
        let r = c
            .http
            .post(format!("{}/api/downloads", c.base))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 400, "{body}");
    }
}
