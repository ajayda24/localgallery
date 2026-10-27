//! A GGUF model the user downloaded themselves: found in the models folder, offered as a
//! variant of the built-in model it matches, chosen, and loaded with the ComfyUI-GGUF nodes.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TOKEN: &str = "gguf-test-token";
const UNET: &str = "z_image_turbo-Q4_K_M.gguf";
const TE: &str = "Qwen3-4B-Q4_K_M.gguf";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct C {
    base: String,
    http: reqwest::Client,
}

impl C {
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
    async fn run(&self, spec: Value) -> Value {
        let (s, job) = self.post("/api/jobs", spec).await;
        assert_eq!(s, 200, "{job}");
        let id = job["id"].as_str().unwrap().to_string();
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
    async fn engine_graph(&self, job: &Value) -> Value {
        let out = &job["outputs"][0];
        let id = out
            .as_str()
            .or_else(|| out["id"].as_str())
            .unwrap_or_else(|| panic!("no output in {job}"));
        let detail = self.get(&format!("/api/outputs/{id}")).await;
        detail["sidecar"]["engine_graph"].clone()
    }
}

/// The file the plan chose for a role (`variants` is a list of [role, file] pairs).
fn chosen<'a>(plan: &'a Value, role: &str) -> &'a Value {
    plan["plan"]["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p[0] == role)
        .map(|p| &p[1])
        .unwrap_or(&Value::Null)
}

fn classes_with(graph: &Value, file: &str) -> Vec<String> {
    graph
        .as_object()
        .unwrap()
        .values()
        .filter(|n| {
            n["inputs"]
                .as_object()
                .is_some_and(|i| i.values().any(|v| v == file))
        })
        .map(|n| n["class_type"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_gguf_files_are_found_chosen_and_loaded_with_gguf_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let models = root.join("models");
    std::fs::create_dir_all(models.join("diffusion_models")).unwrap();
    std::fs::create_dir_all(models.join("text_encoders")).unwrap();
    let weight = [("some.weight", 12)];
    std::fs::write(
        models.join("diffusion_models").join(UNET),
        lg_modelscan::test_gguf("lumina2", &weight),
    )
    .unwrap();
    std::fs::write(
        models.join("text_encoders").join(TE),
        lg_modelscan::test_gguf("qwen3", &weight),
    )
    .unwrap();
    // A GGUF of another family must not be offered for Z-Image.
    std::fs::write(
        models
            .join("diffusion_models")
            .join("flux1-dev-Q4_K_S.gguf"),
        lg_modelscan::test_gguf("flux", &weight),
    )
    .unwrap();

    let r = lg_server::serve(
        lg_server::Options {
            root: root.clone(),
            mode: lg_server::Mode::Fake {
                object_info: vec![
                    repo().join("tests/fixtures/object_info.json"),
                    repo().join("tests/fixtures/object_info.gguf.json"),
                ],
                step_ms: 2,
            },
            manifests: Some(repo().join("manifests")),
            ui: None,
            token: Some(TOKEN.into()),
            hf_endpoint: Some("off".into()),
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
    c.post("/api/library/rescan", json!({})).await;

    // ---- the card offers the local files as variants ----
    let card = c.get("/api/models/z-image-turbo").await;
    let files = card["files"].as_array().unwrap();
    let role = |r: &str| files.iter().find(|f| f["role"] == r).unwrap().clone();
    let names = |f: &Value| -> Vec<String> {
        f["variants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap().to_string())
            .collect()
    };
    let diffusion = role("diffusion");
    assert!(names(&diffusion).contains(&UNET.to_string()), "{diffusion}");
    assert!(
        !names(&diffusion).iter().any(|n| n.starts_with("flux1")),
        "a Flux GGUF is not a Z-Image file: {diffusion}"
    );
    assert!(
        diffusion["present"]
            .as_array()
            .unwrap()
            .contains(&json!(UNET))
    );
    let te = role("text_encoder");
    assert!(names(&te).contains(&TE.to_string()), "{te}");

    // ---- the Files tab says which models can use each file ----
    let lib = c.get("/api/library").await;
    let used_by = |name: &str| -> Vec<String> {
        lib.as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("{name} not in library: {lib}"))["used_by"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(used_by(UNET).contains(&"z-image-turbo".to_string()));
    assert!(!used_by("flux1-dev-Q4_K_S.gguf").contains(&"z-image-turbo".to_string()));

    // ---- the planner prefers what is already on disk ----
    let spec = json!({"model": "z-image-turbo", "op": "txt2img", "values": {"prompt": "a fox"}});
    let (_, p) = c.post("/api/plan", spec.clone()).await;
    assert_eq!(chosen(&p, "diffusion"), UNET, "{p}");
    assert_eq!(chosen(&p, "text_encoder"), TE, "{p}");

    // ---- and the engine graph loads them with the GGUF nodes ----
    let j = c.run(spec).await;
    assert_eq!(j["status"], "done", "{j}");
    let g = c.engine_graph(&j).await;
    assert_eq!(classes_with(&g, UNET), ["UnetLoaderGGUF"], "{g}");
    assert_eq!(classes_with(&g, TE), ["CLIPLoaderGGUF"], "{g}");
    let unet_node = g
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["class_type"] == "UnetLoaderGGUF")
        .unwrap();
    assert!(
        unet_node["inputs"].get("weight_dtype").is_none(),
        "UnetLoaderGGUF takes no weight_dtype"
    );

    // ---- a file the user picks wins over the on-disk preference ----
    let mut forced =
        json!({"model": "z-image-turbo", "op": "txt2img", "values": {"prompt": "a fox"}});
    let safetensors = names(&diffusion)
        .into_iter()
        .find(|n| n.ends_with(".safetensors"))
        .expect("the built-in safetensors variant");
    forced["memory"] = json!({"files": {"diffusion": safetensors}});
    let (_, p) = c.post("/api/plan", forced.clone()).await;
    assert_eq!(chosen(&p, "diffusion"), &json!(safetensors), "{p}");
    let j = c.run(forced).await;
    assert_eq!(j["status"], "done", "{j}");
    let g = c.engine_graph(&j).await;
    assert_eq!(classes_with(&g, &safetensors), ["UNETLoader"], "{g}");
    // The text encoder still comes from the local GGUF.
    assert_eq!(classes_with(&g, TE), ["CLIPLoaderGGUF"], "{g}");

    r.handle.abort();
}
