//! A fake ComfyUI server.
//!
//! It speaks the same HTTP and WebSocket protocol as ComfyUI (as read from ComfyUI's
//! `server.py`/`execution.py`), validates submitted graphs against an `/object_info`
//! document like ComfyUI does, then "runs" them: sampler nodes emit progress and
//! preview frames, and output nodes write synthetic PNG/GIF files.
//!
//! Failure injection for tests: a string input containing `FAKE_OOM` raises an
//! out-of-memory error, `FAKE_ERROR` raises a generic node error.

use axum::{
    Json, Router,
    extract::{Multipart, Query, State, WebSocketUpgrade, ws::Message},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

/// Hand-written definitions for the core nodes. A captured real `/object_info` can be
/// passed instead with [`Config::object_info`].
pub const MIN_OBJECT_INFO: &str = include_str!("object_info.min.json");

#[derive(Clone)]
pub struct Config {
    pub object_info: Value,
    /// Delay per sampler step.
    pub step_ms: u64,
    /// Root for input/, output/, temp/.
    pub dir: PathBuf,
    /// Accept model file names that are not in the object_info choice lists
    /// (the fake has no model files).
    pub accept_any_model: bool,
}

impl Config {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            object_info: serde_json::from_str(MIN_OBJECT_INFO).expect("object_info.min.json"),
            step_ms: 20,
            dir,
            accept_any_model: true,
        }
    }
}

struct Queued {
    number: u64,
    id: String,
    prompt: Value,
    client_id: String,
}

struct Inner {
    cfg: Config,
    queue: Mutex<VecDeque<Queued>>,
    running: Mutex<Option<(u64, String)>>,
    history: Mutex<HashMap<String, Value>>,
    sockets: Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>,
    interrupt: AtomicBool,
    number: AtomicU64,
    wake: Notify,
    counter: AtomicU64,
}

type S = Arc<Inner>;

pub async fn serve(
    cfg: Config,
    addr: SocketAddr,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    for d in ["input", "output", "temp"] {
        std::fs::create_dir_all(cfg.dir.join(d))?;
    }
    let state: S = Arc::new(Inner {
        cfg,
        queue: Mutex::new(VecDeque::new()),
        running: Mutex::new(None),
        history: Mutex::new(HashMap::new()),
        sockets: Mutex::new(HashMap::new()),
        interrupt: AtomicBool::new(false),
        number: AtomicU64::new(0),
        wake: Notify::new(),
        counter: AtomicU64::new(0),
    });
    tokio::spawn(worker(state.clone()));
    let app = Router::new()
        .route("/ws", get(ws))
        .route("/prompt", post(post_prompt).get(get_prompt))
        .route("/queue", get(get_queue).post(post_queue))
        .route("/interrupt", post(interrupt))
        .route("/free", post(|| async { StatusCode::OK }))
        .route("/history", get(history_all))
        .route("/history/{id}", get(history_one))
        .route("/view", get(view))
        .route("/upload/image", post(upload))
        .route("/upload/mask", post(upload))
        .route("/object_info", get(object_info))
        .route("/object_info/{class}", get(object_info_one))
        .route("/system_stats", get(system_stats))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(200 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let h = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((local, h))
}

fn send(s: &S, client: Option<&str>, ty: &str, data: Value) {
    let msg = Message::Text(json!({"type": ty, "data": data}).to_string().into());
    let socks = s.sockets.lock();
    match client {
        Some(c) => {
            if let Some(tx) = socks.get(c) {
                let _ = tx.send(msg);
            }
        }
        None => {
            for tx in socks.values() {
                let _ = tx.send(msg.clone());
            }
        }
    }
}

fn send_bin(s: &S, client: &str, bytes: Vec<u8>) {
    if let Some(tx) = s.sockets.lock().get(client) {
        let _ = tx.send(Message::Binary(bytes.into()));
    }
}

fn queue_info(s: &S) -> Value {
    let n = s.queue.lock().len() + usize::from(s.running.lock().is_some());
    json!({"exec_info": {"queue_remaining": n}})
}

#[derive(Deserialize)]
struct WsQ {
    #[serde(rename = "clientId")]
    client_id: Option<String>,
}

async fn ws(State(s): State<S>, Query(q): Query<WsQ>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| async move {
        let sid = q
            .client_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let (mut tx, mut rx) = socket.split();
        let (mtx, mut mrx) = mpsc::unbounded_channel::<Message>();
        s.sockets.lock().insert(sid.clone(), mtx);
        let _ = tx
            .send(Message::Text(
                json!({"type": "status", "data": {"status": queue_info(&s), "sid": sid}})
                    .to_string()
                    .into(),
            ))
            .await;
        let fwd = tokio::spawn(async move {
            while let Some(m) = mrx.recv().await {
                if tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(m)) = rx.next().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
        s.sockets.lock().remove(&sid);
        fwd.abort();
    })
}

fn bad_request(body: Value) -> Response {
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

async fn post_prompt(State(s): State<S>, Json(body): Json<Value>) -> Response {
    let Some(prompt) = body.get("prompt").cloned() else {
        return bad_request(
            json!({"error": {"type": "no_prompt", "message": "No prompt provided", "details": "", "extra_info": {}}, "node_errors": {}}),
        );
    };
    let id = match body.get("prompt_id").and_then(Value::as_str) {
        Some(p) if uuid::Uuid::parse_str(p).is_ok() => p.to_string(),
        Some(_) => {
            return bad_request(
                json!({"error": {"type": "invalid_prompt_id", "message": "prompt_id must be a valid UUID", "details": "", "extra_info": {}}, "node_errors": {}}),
            );
        }
        None => uuid::Uuid::new_v4().to_string(),
    };
    if let Some(errors) = validate(&s, &prompt) {
        return bad_request(json!({
            "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation", "details": "", "extra_info": {}},
            "node_errors": errors
        }));
    }
    let number = s.number.fetch_add(1, Ordering::SeqCst);
    let client_id = body
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    s.queue.lock().push_back(Queued {
        number,
        id: id.clone(),
        prompt,
        client_id,
    });
    s.wake.notify_one();
    send(&s, None, "status", json!({"status": queue_info(&s)}));
    Json(json!({"prompt_id": id, "number": number, "node_errors": {}})).into_response()
}

/// Mirrors ComfyUI's validation closely enough to catch template mistakes:
/// unknown node types, missing inputs, bad choices, dangling links, missing input images.
fn validate(s: &S, prompt: &Value) -> Option<Map<String, Value>> {
    let mut errors: Map<String, Value> = Map::new();
    let mut add = |node: &str, class: &str, ty: &str, msg: String| {
        let e = errors
            .entry(node.to_string())
            .or_insert_with(|| json!({"errors": [], "dependent_outputs": [], "class_type": class}));
        e["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": ty, "message": msg, "details": msg, "extra_info": {}}));
    };
    let Some(nodes) = prompt.as_object() else {
        add("", "", "invalid_prompt", "prompt must be an object".into());
        return Some(errors);
    };
    for issue in lg_workflow::objectinfo::validate(prompt, &s.cfg.object_info) {
        use lg_workflow::objectinfo::Issue;
        match &issue {
            Issue::BadChoice { input, .. }
                if s.cfg.accept_any_model && input.ends_with("_name") =>
            {
                continue;
            }
            Issue::UnknownNode { node, class_type }
            | Issue::MissingInput {
                node, class_type, ..
            }
            | Issue::BadChoice {
                node, class_type, ..
            } => add(node, class_type, "value_not_valid", issue.message()),
            Issue::DanglingLink { node, .. } => add(node, "", "bad_linked_input", issue.message()),
        }
    }
    for (id, n) in nodes {
        let class = n["class_type"].as_str().unwrap_or_default();
        if matches!(class, "LoadImage" | "LoadImageMask")
            && let Some(name) = n["inputs"]["image"].as_str()
            && safe_join(&s.cfg.dir.join("input"), name).is_none_or(|p| !p.exists())
        {
            add(
                id,
                class,
                "custom_validation_failed",
                format!("Invalid image file: {name}"),
            );
        }
    }
    if !nodes
        .values()
        .any(|n| is_output(n["class_type"].as_str().unwrap_or_default()))
    {
        add("", "", "prompt_no_outputs", "Prompt has no outputs".into());
    }
    (!errors.is_empty()).then_some(errors)
}

fn is_output(class: &str) -> bool {
    class.starts_with("Save") || class.starts_with("Preview") || class == "CreateBoundingBoxes"
}

async fn get_prompt(State(s): State<S>) -> Json<Value> {
    Json(json!({"exec_info": queue_info(&s)["exec_info"]}))
}

async fn get_queue(State(s): State<S>) -> Json<Value> {
    let running: Vec<Value> = s
        .running
        .lock()
        .iter()
        .map(|(n, id)| json!([n, id, {}, {}, []]))
        .collect();
    let pending: Vec<Value> = s
        .queue
        .lock()
        .iter()
        .map(|q| json!([q.number, q.id, {}, {}, []]))
        .collect();
    Json(json!({"queue_running": running, "queue_pending": pending}))
}

async fn post_queue(State(s): State<S>, Json(body): Json<Value>) -> StatusCode {
    if body["clear"].as_bool() == Some(true) {
        s.queue.lock().clear();
    }
    if let Some(del) = body["delete"].as_array() {
        let ids: Vec<&str> = del.iter().filter_map(Value::as_str).collect();
        s.queue.lock().retain(|q| !ids.contains(&q.id.as_str()));
    }
    StatusCode::OK
}

async fn interrupt(State(s): State<S>, body: Option<Json<Value>>) -> StatusCode {
    let target = body.and_then(|b| {
        b.0.get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let running = s.running.lock().as_ref().map(|r| r.1.clone());
    if target.is_none() || target == running {
        s.interrupt.store(true, Ordering::SeqCst);
    }
    StatusCode::OK
}

async fn history_all(State(s): State<S>) -> Json<Value> {
    Json(Value::Object(
        s.history
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    ))
}

async fn history_one(
    State(s): State<S>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let h = s.history.lock().get(&id).cloned();
    Json(match h {
        Some(v) => json!({ id: v }),
        None => json!({}),
    })
}

#[derive(Deserialize)]
struct ViewQ {
    filename: String,
    #[serde(default)]
    subfolder: String,
    #[serde(default = "output")]
    r#type: String,
}
fn output() -> String {
    "output".into()
}

/// Join without escaping the base (rejects `..` and absolute parts).
fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(base.join(p))
}

async fn view(State(s): State<S>, Query(q): Query<ViewQ>) -> Response {
    let base = match q.r#type.as_str() {
        "output" | "input" | "temp" => s.cfg.dir.join(&q.r#type),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let rel = if q.subfolder.is_empty() {
        q.filename.clone()
    } else {
        format!("{}/{}", q.subfolder, q.filename)
    };
    let Some(path) = safe_join(&base, &rel) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(b) => {
            let mime = if rel.ends_with(".png") {
                "image/png"
            } else if rel.ends_with(".gif") {
                "image/gif"
            } else {
                "application/octet-stream"
            };
            ([(axum::http::header::CONTENT_TYPE, mime)], b).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn upload(State(s): State<S>, mut mp: Multipart) -> Response {
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut subfolder = String::new();
    while let Ok(Some(field)) = mp.next_field().await {
        match field.name().unwrap_or_default() {
            "image" => {
                let name = field.file_name().unwrap_or("upload.png").to_string();
                let Ok(bytes) = field.bytes().await else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                file = Some((name, bytes.to_vec()));
            }
            "subfolder" => subfolder = field.text().await.unwrap_or_default(),
            _ => {}
        }
    }
    let Some((name, bytes)) = file else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let rel = if subfolder.is_empty() {
        name.clone()
    } else {
        format!("{subfolder}/{name}")
    };
    let Some(path) = safe_join(&s.cfg.dir.join("input"), &rel) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if std::fs::write(&path, bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(json!({"name": name, "subfolder": subfolder, "type": "input"})).into_response()
}

async fn object_info(State(s): State<S>) -> Json<Value> {
    Json(s.cfg.object_info.clone())
}

async fn object_info_one(
    State(s): State<S>,
    axum::extract::Path(class): axum::extract::Path<String>,
) -> Json<Value> {
    Json(match s.cfg.object_info.get(&class) {
        Some(v) => json!({ class: v }),
        None => json!({}),
    })
}

async fn system_stats() -> Json<Value> {
    Json(json!({
        "system": {"os": "fake", "comfyui_version": "fake-0", "python_version": "3.13", "pytorch_version": "fake", "ram_total": 34359738368u64, "ram_free": 20000000000u64, "argv": []},
        "devices": [{"name": "cuda:0 Fake GPU", "type": "cuda", "index": 0, "vram_total": 8589934592u64, "vram_free": 7516192768u64, "torch_vram_total": 0, "torch_vram_free": 0}]
    }))
}

// ---- execution ----

async fn worker(s: S) {
    loop {
        let next = s.queue.lock().pop_front();
        let Some(job) = next else {
            s.wake.notified().await;
            continue;
        };
        s.interrupt.store(false, Ordering::SeqCst);
        *s.running.lock() = Some((job.number, job.id.clone()));
        let result = execute(&s, &job).await;
        *s.running.lock() = None;
        let (status, outputs, messages) = match result {
            Ok(outputs) => {
                send(
                    &s,
                    Some(&job.client_id),
                    "execution_success",
                    json!({"prompt_id": job.id, "timestamp": 0}),
                );
                (
                    "success",
                    outputs,
                    json!([["execution_success", {"prompt_id": job.id}]]),
                )
            }
            Err(Failure::Interrupted(node)) => {
                let m =
                    json!({"prompt_id": job.id, "node_id": node, "node_type": "", "executed": []});
                send(&s, None, "execution_interrupted", m.clone());
                ("error", Map::new(), json!([["execution_interrupted", m]]))
            }
            Err(Failure::Error {
                node,
                class,
                ty,
                message,
            }) => {
                let m = json!({"prompt_id": job.id, "node_id": node, "node_type": class, "executed": [], "exception_message": message, "exception_type": ty, "traceback": [], "current_inputs": {}, "current_outputs": {}});
                send(&s, Some(&job.client_id), "execution_error", m.clone());
                ("error", Map::new(), json!([["execution_error", m]]))
            }
        };
        s.history.lock().insert(
            job.id.clone(),
            json!({
                "prompt": [job.number, job.id, job.prompt, {"client_id": job.client_id}, []],
                "outputs": outputs,
                "status": {"status_str": status, "completed": status == "success", "messages": messages},
                "meta": {}
            }),
        );
        send(
            &s,
            Some(&job.client_id),
            "executing",
            json!({"node": null, "prompt_id": job.id}),
        );
        send(&s, None, "status", json!({"status": queue_info(&s)}));
    }
}

enum Failure {
    Interrupted(String),
    Error {
        node: String,
        class: String,
        ty: String,
        message: String,
    },
}

fn find_str(v: &Value, needle: &str) -> bool {
    match v {
        Value::String(s) => s.contains(needle),
        Value::Array(a) => a.iter().any(|x| find_str(x, needle)),
        Value::Object(o) => o.values().any(|x| find_str(x, needle)),
        _ => false,
    }
}

/// Order nodes so that every node runs after the nodes it links to.
fn topo(nodes: &Map<String, Value>) -> Vec<String> {
    let mut done: Vec<String> = Vec::new();
    fn visit(
        id: &str,
        nodes: &Map<String, Value>,
        done: &mut Vec<String>,
        stack: &mut Vec<String>,
    ) {
        if done.iter().any(|d| d == id) || stack.iter().any(|d| d == id) {
            return;
        }
        stack.push(id.to_string());
        if let Some(inputs) = nodes.get(id).and_then(|n| n["inputs"].as_object()) {
            for v in inputs.values() {
                if let Some(src) = v
                    .as_array()
                    .filter(|a| a.len() == 2)
                    .and_then(|a| a[0].as_str())
                {
                    visit(src, nodes, done, stack);
                }
            }
        }
        stack.pop();
        done.push(id.to_string());
    }
    for id in nodes.keys() {
        visit(id, nodes, &mut done, &mut Vec::new());
    }
    done
}

async fn execute(s: &S, job: &Queued) -> Result<Map<String, Value>, Failure> {
    let nodes = job.prompt.as_object().cloned().unwrap_or_default();
    let client = job.client_id.as_str();
    send(
        s,
        Some(client),
        "execution_start",
        json!({"prompt_id": job.id, "timestamp": 0}),
    );
    let (w, h, frames) = guess_size(&nodes);
    let seed = nodes
        .values()
        .find_map(|n| {
            n["inputs"]["seed"]
                .as_u64()
                .or(n["inputs"]["noise_seed"].as_u64())
        })
        .unwrap_or(0);
    let text: String = nodes
        .values()
        .flat_map(|n| n["inputs"].as_object().into_iter().flat_map(|i| i.values()))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let mut outputs = Map::new();
    for id in topo(&nodes) {
        let node = &nodes[&id];
        let class = node["class_type"].as_str().unwrap_or_default().to_string();
        send(
            s,
            Some(client),
            "executing",
            json!({"node": id, "display_node": id, "prompt_id": job.id}),
        );
        if find_str(&node["inputs"], "FAKE_OOM") {
            return Err(Failure::Error {
                node: id,
                class,
                ty: "torch.OutOfMemoryError".into(),
                message: "Allocation on device 0 would exceed allowed memory. (out of memory)"
                    .into(),
            });
        }
        if find_str(&node["inputs"], "FAKE_ERROR") {
            return Err(Failure::Error {
                node: id,
                class,
                ty: "RuntimeError".into(),
                message: "fake node failure".into(),
            });
        }
        if matches!(
            class.as_str(),
            "KSampler" | "KSamplerAdvanced" | "SamplerCustom" | "SamplerCustomAdvanced"
        ) {
            let steps = node["inputs"]["steps"].as_u64().unwrap_or(4).clamp(1, 200) as u32;
            for step in 1..=steps {
                tokio::time::sleep(Duration::from_millis(s.cfg.step_ms)).await;
                if s.interrupt.swap(false, Ordering::SeqCst) {
                    return Err(Failure::Interrupted(id));
                }
                send(
                    s,
                    Some(client),
                    "progress",
                    json!({"value": step, "max": steps, "prompt_id": job.id, "node": id}),
                );
                let mut bin = vec![0, 0, 0, 1, 0, 0, 0, 1];
                bin.extend(render_jpeg(64, 64, seed, &text, step as f32 / steps as f32));
                send_bin(s, client, bin);
            }
        }
        if is_output(&class) {
            let out = write_output(s, &class, node, w, h, frames, seed, &text).map_err(|e| {
                Failure::Error {
                    node: id.clone(),
                    class: class.clone(),
                    ty: "OSError".into(),
                    message: e.to_string(),
                }
            })?;
            send(
                s,
                Some(client),
                "executed",
                json!({"node": id, "display_node": id, "output": out, "prompt_id": job.id}),
            );
            outputs.insert(id.clone(), out);
        }
    }
    Ok(outputs)
}

fn guess_size(nodes: &Map<String, Value>) -> (u32, u32, u32) {
    let mut w = 512;
    let mut h = 512;
    let mut frames = 1;
    for n in nodes.values() {
        let i = &n["inputs"];
        if let (Some(a), Some(b)) = (i["width"].as_u64(), i["height"].as_u64())
            && n["class_type"]
                .as_str()
                .is_some_and(|c| c.contains("Latent") || c.contains("Video") || c.contains("Empty"))
        {
            w = a as u32;
            h = b as u32;
        }
        if let Some(l) = i["length"].as_u64() {
            frames = l as u32;
        }
    }
    (w.clamp(16, 2048), h.clamp(16, 2048), frames.clamp(1, 241))
}

fn hash(text: &str, seed: u64) -> u64 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    for b in text.bytes() {
        x = (x ^ b as u64).wrapping_mul(0x100_0000_01B3);
    }
    x
}

fn render(w: u32, h: u32, seed: u64, text: &str, t: f32) -> image::RgbImage {
    let hv = hash(text, seed);
    let (r0, g0, b0) = (
        (hv & 0xff) as f32,
        ((hv >> 8) & 0xff) as f32,
        ((hv >> 16) & 0xff) as f32,
    );
    image::RgbImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let wave = ((fx * 6.0 + fy * 4.0 + t * 3.0 + (hv % 7) as f32).sin() * 0.5 + 0.5) * 80.0;
        image::Rgb([
            (r0 * (1.0 - fx) + 40.0 * fx + wave * 0.5).min(255.0) as u8,
            (g0 * (1.0 - fy) + wave).min(255.0) as u8,
            (b0 * 0.6 + 100.0 * fx * fy + wave * 0.3).min(255.0) as u8,
        ])
    })
}

fn render_jpeg(w: u32, h: u32, seed: u64, text: &str, t: f32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = render(w, h, seed, text, t).write_to(&mut out, image::ImageFormat::Jpeg);
    out.into_inner()
}

#[allow(clippy::too_many_arguments)]
fn write_output(
    s: &S,
    class: &str,
    node: &Value,
    w: u32,
    h: u32,
    frames: u32,
    seed: u64,
    text: &str,
) -> anyhow::Result<Value> {
    let temp = class.starts_with("Preview") || class == "CreateBoundingBoxes";
    let ty = if temp { "temp" } else { "output" };
    let prefix = node["inputs"]["filename_prefix"]
        .as_str()
        .unwrap_or("ComfyUI");
    let (sub, base) = match prefix.rsplit_once('/') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (String::new(), prefix.to_string()),
    };
    let dir = if sub.is_empty() {
        s.cfg.dir.join(ty)
    } else {
        safe_join(&s.cfg.dir.join(ty), &sub)
            .ok_or_else(|| anyhow::anyhow!("bad filename_prefix"))?
    };
    std::fs::create_dir_all(&dir)?;
    let n = s.counter.fetch_add(1, Ordering::SeqCst) + 1;
    let video = class.contains("Video") || class.contains("WEBM") || class.contains("Animated");
    if video {
        let name = format!("{base}_{n:05}_.gif");
        let file = std::fs::File::create(dir.join(&name))?;
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        let count = frames.min(12);
        let (fw, fh) = (w.min(256), h.min(256));
        for f in 0..count {
            let img =
                image::DynamicImage::ImageRgb8(render(fw, fh, seed, text, f as f32 / count as f32))
                    .to_rgba8();
            enc.encode_frame(image::Frame::from_parts(
                img,
                0,
                0,
                image::Delay::from_numer_denom_ms(80, 1),
            ))?;
        }
        return Ok(
            json!({"images": [{"filename": name, "subfolder": sub, "type": ty}], "animated": [true]}),
        );
    }
    let name = format!("{base}_{n:05}_.png");
    render(w, h, seed, text, 1.0).save(dir.join(&name))?;
    Ok(json!({"images": [{"filename": name, "subfolder": sub, "type": ty}]}))
}
