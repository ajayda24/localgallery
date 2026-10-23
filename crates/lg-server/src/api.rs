//! HTTP + WebSocket API, served on 127.0.0.1 only.
//!
//! Security: the UI proves it was opened by the app with a per-launch token (query on
//! first load -> HttpOnly cookie; or `Authorization: Bearer`). The Host header must name
//! loopback, which blocks DNS-rebinding pages from talking to the API.

use crate::jobs::JobError;
use crate::state::App;
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Multipart, Path as AxPath, Query, Request, State, WebSocketUpgrade,
        ws::Message,
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use lg_types::{CoreEvent, JobSpec, Origin};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

type S = Arc<App>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

type R<T = Json<Value>> = Result<T, ApiError>;

pub struct ServeOptions {
    /// Accept any Host header (development behind a proxy).
    pub any_host: bool,
    /// Skip the token check (development only).
    pub no_auth: bool,
}

pub fn router(app: S, opts: ServeOptions) -> Router {
    let api = Router::new()
        .route("/api/system", get(system))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/models", get(models))
        .route("/api/models/{id}", get(model).delete(delete_model))
        .route("/api/plan", post(plan))
        .route("/api/jobs", get(jobs).post(create_job))
        .route("/api/jobs/reorder", post(reorder))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}/retry", post(retry_job))
        .route("/api/jobs/{id}", delete(delete_job))
        .route("/api/outputs", get(outputs))
        .route("/api/outputs/{id}", get(output).delete(delete_output))
        .route("/api/outputs/{id}/favorite", post(favorite))
        .route("/api/outputs/{id}/as-asset", post(output_as_asset))
        .route("/api/assets", post(upload_asset))
        .route("/api/library", get(library))
        .route("/api/library/rescan", post(rescan))
        .route("/api/downloads", get(downloads).post(start_downloads))
        .route("/api/downloads/{id}/pause", post(pause_download))
        .route("/api/downloads/{id}", delete(remove_download))
        .route("/api/engine", get(engine))
        .route("/api/engine/install", post(install_engine))
        .route("/api/engine/cancel-install", post(cancel_install))
        .route("/api/engine/start", post(start_engine))
        .route("/api/engine/stop", post(stop_engine))
        .route("/api/engine/object_info", get(object_info))
        .route("/api/workflows/import", post(import_workflow))
        .route("/api/tools", post(publish_tool))
        .route("/api/benchmarks", get(benchmarks))
        .route("/api/logs", get(logs))
        .route("/api/events", get(events))
        .route("/files/{*path}", get(file))
        .layer(DefaultBodyLimit::max(512 * 1024 * 1024));
    let any_host = opts.any_host;
    let no_auth = opts.no_auth;
    Router::new()
        .route("/api/health", get(|| async { Json(json!({"ok": true, "app": "LocalGallery", "version": env!("CARGO_PKG_VERSION")})) }))
        .merge(api)
        .fallback(ui)
        .layer(middleware::from_fn_with_state((app.clone(), any_host, no_auth), guard))
        .with_state(app)
}

const COOKIE: &str = "lg_token";

fn cookie_token(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn guard(
    State((app, any_host, no_auth)): State<(S, bool, bool)>,
    req: Request,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let host_name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if !any_host && !matches!(host_name, "127.0.0.1" | "localhost" | "[::1]") {
        return (
            StatusCode::FORBIDDEN,
            "LocalGallery only answers on this computer",
        )
            .into_response();
    }
    let path = req.uri().path().to_string();
    let query_token = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .map(str::to_string)
    });
    // First load from the app window: trade the token for a cookie.
    if let Some(t) = &query_token
        && constant_eq(t, &app.token)
        && !path.starts_with("/api/")
    {
        let mut r = Redirect::to("/").into_response();
        let c = format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", app.token);
        r.headers_mut()
            .insert(header::SET_COOKIE, HeaderValue::from_str(&c).unwrap());
        return r;
    }
    let protected =
        path.starts_with("/api/") && path != "/api/health" || path.starts_with("/files/");
    if protected && !no_auth {
        let bearer = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string);
        let ok = [bearer, cookie_token(req.headers()), query_token]
            .into_iter()
            .flatten()
            .any(|t| constant_eq(&t, &app.token));
        if !ok {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "open LocalGallery from its own window"})),
            )
                .into_response();
        }
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    r
}

// ---------- system & settings ----------

async fn system(State(app): State<S>) -> Json<Value> {
    let (state, detail) = app.engine.status();
    let mode = match &app.engine.mode {
        crate::engine::Mode::Managed => {
            json!({"kind": "managed", "installed": app.engine.layout().is_installed()})
        }
        crate::engine::Mode::External(u) => json!({"kind": "external", "url": u}),
        crate::engine::Mode::Fake { .. } => json!({"kind": "fake"}),
    };
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hardware": *app.hw.read(),
        "engine": {"state": state, "detail": detail, "mode": mode, "flags": app.engine.running_flags()},
        "root": app.paths.root,
        "running_job": app.running_job(),
        "manifest_errors": app.registry.load_errors().iter().map(|(p, e)| json!({"path": p, "error": e})).collect::<Vec<_>>(),
    }))
}

async fn get_settings(State(app): State<S>) -> Json<Value> {
    Json(app.settings.read().redacted())
}

async fn put_settings(State(app): State<S>, Json(patch): Json<Value>) -> R {
    let current = app.settings.read().clone();
    let mut v = serde_json::to_value(&current).map_err(|e| bad(e.to_string()))?;
    let keep_token = patch
        .get("hf_token")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    merge(&mut v, &patch);
    if keep_token {
        v["hf_token"] = json!(current.hf_token);
    }
    if patch.get("clear_hf_token").and_then(Value::as_bool) == Some(true) {
        v["hf_token"] = json!("");
    }
    let new: crate::settings::Settings =
        serde_json::from_value(v).map_err(|e| bad(format!("invalid settings: {e}")))?;
    new.save(&app.store)?;
    if new.extra_model_dirs != current.extra_model_dirs {
        app.engine.set_extra_dirs(new.extra_model_dirs.clone());
        let mut roots = vec![app.paths.models.clone()];
        roots.extend(new.extra_model_dirs.iter().map(std::path::PathBuf::from));
        app.library.set_roots(roots);
        app.library.scan();
        app.engine.invalidate();
    }
    *app.settings.write() = new.clone();
    Ok(Json(new.redacted()))
}

fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b.clone(),
    }
}

// ---------- models ----------

/// Manifest with live status: availability, files present, and how it fits this PC.
fn model_view(app: &App, e: &lg_manifests::Entry, full: bool) -> Value {
    let m = &e.manifest;
    let files: Vec<Value> = m
        .files
        .iter()
        .map(|f| {
            let present: Vec<&str> = f.variants.iter().filter(|v| app.library.has(&f.folder, &v.name)).map(|v| v.name.as_str()).collect();
            json!({"role": f.role, "folder": f.folder, "ops": f.ops, "optional": f.optional, "present": present,
                   "variants": f.variants.iter().map(|v| json!({"name": v.name, "precision": v.precision, "size_gb": v.size_gb, "downloadable": v.url.is_some()})).collect::<Vec<_>>()})
        })
        .collect();
    let first_op = main_op(m);
    // The badge depends on the model's shape, not on what the user types: fill required
    // text with a placeholder and leave uploads out.
    let mut values = std::collections::BTreeMap::new();
    for i in m
        .inputs_for(&first_op)
        .filter(|i| i.required || i.kind.is_asset())
    {
        values.insert(i.id.clone(), json!("preview"));
    }
    let spec = JobSpec {
        version: 1,
        model: m.id.clone(),
        op: first_op.clone(),
        values,
        batch: 1,
        memory: Default::default(),
        overrides: Default::default(),
    };
    let fit = lg_workflow::resolve_values(m, &spec)
        .or_else(|_| {
            lg_workflow::resolve_values(
                m,
                &JobSpec {
                    values: Default::default(),
                    ..spec.clone()
                },
            )
        })
        .ok()
        .map(|vals| crate::state::plan_for(app, m, &spec, &vals, 0));
    let installed = matches!(app.engine.mode, crate::engine::Mode::Fake { .. })
        || m.files.iter().filter(|f| f.needed_for(&first_op)).all(|f| {
            f.variants
                .iter()
                .any(|v| app.library.has(&f.folder, &v.name))
        });
    let mut v = json!({
        "id": m.id, "display_name": m.display_name, "family": m.family, "media": m.media, "description": m.description,
        "tags": m.tags, "license": m.license, "origin": m.origin, "ops": m.ops.keys().collect::<Vec<_>>(), "main_op": first_op,
        "problems": e.problems, "unavailable": e.unavailable, "files": files, "installed": installed,
        "size_gb": m.total_size_gb(),
        "fit": fit.as_ref().map(|p| json!({"fit": p.fit, "badge": p.fit.badge(), "explain": p.explain, "warnings": p.warnings, "precision": p.precision})),
    });
    if full {
        v["inputs"] = serde_json::to_value(&m.inputs).unwrap_or_default();
        v["presets"] = serde_json::to_value(&m.presets).unwrap_or_default();
        v["memory"] = serde_json::to_value(&m.memory).unwrap_or_default();
        v["manifest"] = serde_json::to_value(&**m).unwrap_or_default();
    }
    v
}

/// The op a model is mainly for: the one shown first and used for fit badges.
pub fn main_op(m: &lg_types::Manifest) -> String {
    ["txt2img", "txt2vid", "run", "edit", "img2vid"]
        .iter()
        .find(|o| m.ops.contains_key(**o))
        .map(|o| o.to_string())
        .or_else(|| m.ops.keys().next().cloned())
        .unwrap_or_default()
}

async fn models(State(app): State<S>) -> Json<Value> {
    let list: Vec<Value> = app
        .registry
        .list()
        .iter()
        .map(|e| model_view(&app, e, false))
        .collect();
    Json(json!(list))
}

async fn model(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    let e = app
        .registry
        .get(&id)
        .ok_or_else(|| not_found("unknown model"))?;
    let mut v = model_view(&app, &e, true);
    // Live choices for model-file pickers.
    let mut choices = serde_json::Map::new();
    for i in e.manifest.inputs.iter().filter(|i| {
        i.kind == lg_types::InputKind::ModelFile || i.kind == lg_types::InputKind::LoraStack
    }) {
        if let Some(folder) = i.extra.get("folder").and_then(Value::as_str) {
            choices.insert(i.id.clone(), json!(app.library.names_in(folder)));
        }
    }
    v["choices"] = Value::Object(choices);
    Ok(Json(v))
}

async fn delete_model(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    app.registry.delete(&id).map_err(bad)?;
    let _ = app.events.send(CoreEvent::ManifestsChanged);
    Ok(Json(json!({"ok": true})))
}

async fn plan(State(app): State<S>, Json(spec): Json<JobSpec>) -> R {
    let e = app
        .registry
        .get(&spec.model)
        .ok_or_else(|| not_found("unknown model"))?;
    let values = match lg_workflow::resolve_values(&e.manifest, &spec) {
        Ok(v) => v,
        Err(err) => return Ok(Json(json!({"ok": false, "error": err.to_string()}))),
    };
    let p = crate::state::plan_for(&app, &e.manifest, &spec, &values, 0);
    Ok(Json(json!({"ok": true, "plan": p, "values": values})))
}

// ---------- jobs ----------

async fn jobs(State(app): State<S>) -> R {
    Ok(Json(json!(app.store.jobs(100)?)))
}

async fn create_job(State(app): State<S>, Json(spec): Json<JobSpec>) -> R {
    let e = app
        .registry
        .get(&spec.model)
        .ok_or_else(|| not_found(format!("unknown model {}", spec.model)))?;
    // Reject bad settings now, with the reason, rather than in the queue.
    lg_workflow::resolve_values(&e.manifest, &spec).map_err(|err| bad(err.to_string()))?;
    if spec.batch > 64 {
        return Err(bad("at most 64 runs per job"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let job = app.store.insert_job(&id, &spec)?;
    let _ = app.events.send(CoreEvent::JobUpdated {
        job: Box::new(job.clone()),
    });
    app.wake.notify_one();
    Ok(Json(json!(job)))
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<String>,
}

async fn reorder(State(app): State<S>, Json(b): Json<Ids>) -> R {
    app.store.reorder(&b.ids)?;
    Ok(Json(json!(app.store.jobs(100)?)))
}

async fn cancel_job(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    Ok(Json(json!({"ok": app.cancel(&id)})))
}

async fn retry_job(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    let job = app
        .store
        .job(&id)?
        .ok_or_else(|| not_found("unknown job"))?;
    let nid = uuid::Uuid::new_v4().to_string();
    let j = app.store.insert_job(&nid, &job.spec)?;
    let _ = app.events.send(CoreEvent::JobUpdated {
        job: Box::new(j.clone()),
    });
    app.wake.notify_one();
    Ok(Json(json!(j)))
}

async fn delete_job(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    app.cancel(&id);
    app.store.delete_job(&id)?;
    Ok(Json(json!({"ok": true})))
}

// ---------- gallery ----------

async fn outputs(State(app): State<S>, Query(q): Query<lg_store::OutputQuery>) -> R {
    Ok(Json(json!(app.store.outputs(&q)?)))
}

async fn output(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    let o = app
        .store
        .output(&id)?
        .ok_or_else(|| not_found("unknown output"))?;
    let sidecar = app
        .paths
        .abs_data(&format!("{}.json", o.path))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    Ok(Json(json!({"output": o, "sidecar": sidecar})))
}

async fn delete_output(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    if let Some(o) = app.store.delete_output(&id)? {
        for rel in [
            Some(o.path.clone()),
            Some(format!("{}.json", o.path)),
            o.thumb.clone(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(p) = app.paths.abs_data(&rel) {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Fav {
    favorite: bool,
}

async fn favorite(State(app): State<S>, AxPath(id): AxPath<String>, Json(b): Json<Fav>) -> R {
    app.store.set_favorite(&id, b.favorite)?;
    Ok(Json(json!({"ok": true})))
}

/// Reuse a result as the input of another job (img2img, edit, video start frame...).
async fn output_as_asset(State(app): State<S>, AxPath(id): AxPath<String>) -> R {
    let o = app
        .store
        .output(&id)?
        .ok_or_else(|| not_found("unknown output"))?;
    let src = app.paths.abs_data(&o.path).ok_or_else(|| bad("bad path"))?;
    let bytes = tokio::fs::read(&src)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image.png".into());
    Ok(Json(save_asset(&app, &name, bytes).await?))
}

// ---------- assets ----------

#[derive(Deserialize)]
struct AssetQ {
    name: Option<String>,
}

async fn upload_asset(
    State(app): State<S>,
    Query(q): Query<AssetQ>,
    headers: HeaderMap,
    body: Body,
) -> R {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (name, bytes) = if ct.starts_with("multipart/form-data") {
        let req = Request::builder()
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .map_err(|e| bad(e.to_string()))?;
        let mut mp = <Multipart as axum::extract::FromRequest<()>>::from_request(req, &())
            .await
            .map_err(|e| bad(e.to_string()))?;
        let mut found = None;
        while let Ok(Some(f)) = mp.next_field().await {
            if f.name() == Some("file") || f.file_name().is_some() {
                let n = f.file_name().unwrap_or("upload.png").to_string();
                let b = f.bytes().await.map_err(|e| bad(e.to_string()))?;
                found = Some((n, b.to_vec()));
                break;
            }
        }
        found.ok_or_else(|| bad("no file in the upload"))?
    } else {
        let b = axum::body::to_bytes(body, 512 * 1024 * 1024)
            .await
            .map_err(|e| bad(e.to_string()))?;
        (q.name.unwrap_or_else(|| "upload.png".into()), b.to_vec())
    };
    Ok(Json(save_asset(&app, &name, bytes).await?))
}

async fn save_asset(app: &App, name: &str, bytes: Vec<u8>) -> Result<Value, ApiError> {
    if bytes.is_empty() {
        return Err(bad("empty file"));
    }
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let allowed = [
        "png", "jpg", "jpeg", "webp", "gif", "bmp", "mp4", "webm", "mov", "mkv", "wav", "mp3",
        "flac", "ogg",
    ];
    if !allowed.contains(&ext.as_str()) {
        return Err(bad(format!(".{ext} files are not supported")));
    }
    let (w, h) = if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
    ) {
        let img = image::load_from_memory(&bytes)
            .map_err(|e| bad(format!("not a readable image: {e}")))?;
        (Some(img.width()), Some(img.height()))
    } else {
        (None, None)
    };
    let id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let path = app.paths.assets().join(format!("{id}.{ext}"));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let a = lg_store::Asset {
        id: id.clone(),
        path: app.paths.rel_data(&path),
        name: name.to_string(),
        mime: mime_guess::from_ext(&ext)
            .first_or_octet_stream()
            .to_string(),
        width: w,
        height: h,
        created_at: lg_types::now_ms(),
    };
    app.store.insert_asset(&a)?;
    Ok(
        json!({"id": id, "url": format!("/files/{}", a.path), "width": w, "height": h, "name": name}),
    )
}

async fn file(State(app): State<S>, AxPath(path): AxPath<String>) -> Response {
    let allowed = ["outputs/", "thumbs/", "assets/"];
    if !allowed.iter().any(|p| path.starts_with(p)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(abs) = app.paths.abs_data(&path) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match tokio::fs::read(&abs).await {
        Ok(b) => {
            let mime = mime_guess::from_path(&abs).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (
                        header::CACHE_CONTROL,
                        "private, max-age=31536000, immutable".into(),
                    ),
                ],
                b,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------- library & downloads ----------

async fn library(State(app): State<S>) -> Json<Value> {
    Json(json!(app.library.files()))
}

async fn rescan(State(app): State<S>) -> Json<Value> {
    let n = app.library.scan();
    let _ = app.events.send(CoreEvent::ManifestsChanged);
    Json(json!({"files": n}))
}

async fn downloads(State(app): State<S>) -> Json<Value> {
    Json(json!(app.downloads.list()))
}

#[derive(Deserialize)]
struct DlReq {
    model: Option<String>,
    op: Option<String>,
    url: Option<String>,
    folder: Option<String>,
    name: Option<String>,
}

/// Download a model's missing files (its first op, or the op given), or one URL.
async fn start_downloads(State(app): State<S>, Json(b): Json<DlReq>) -> R {
    let token = Some(app.settings.read().hf_token.clone()).filter(|t| !t.is_empty());
    let mut started = Vec::new();
    if let Some(id) = b.model {
        let e = app
            .registry
            .get(&id)
            .ok_or_else(|| not_found("unknown model"))?;
        let m = &e.manifest;
        let op = b.op.unwrap_or_else(|| main_op(m));
        let spec = JobSpec {
            version: 1,
            model: m.id.clone(),
            op: op.clone(),
            values: Default::default(),
            batch: 1,
            memory: Default::default(),
            overrides: Default::default(),
        };
        let vals = lg_workflow::resolve_values(m, &spec).unwrap_or_default();
        let plan = crate::state::plan_for(&app, m, &spec, &vals, 0);
        for (role, name) in plan.variants {
            let Some(f) = m.files.iter().find(|f| {
                f.role == role && f.needed_for(&op) && f.variants.iter().any(|v| v.name == name)
            }) else {
                continue;
            };
            let v = f.variants.iter().find(|v| v.name == name).unwrap();
            if app.library.has(&f.folder, &v.name) {
                continue;
            }
            let Some(url) = v.url.clone() else { continue };
            let lib = app.library.clone();
            let events = app.events.clone();
            started.push(app.downloads.start(
                url,
                app.paths.models.join(&f.folder).join(&v.name),
                v.sha256.clone(),
                token.clone(),
                move |ok| {
                    if ok {
                        lib.scan();
                        let _ = events.send(CoreEvent::ManifestsChanged);
                    }
                },
            ));
        }
    } else if let (Some(url), Some(folder), Some(name)) = (b.url, b.folder, b.name) {
        if !lg_modelscan::FOLDERS.contains(&folder.as_str())
            || name.contains("..")
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(bad("bad folder or file name"));
        }
        let lib = app.library.clone();
        started.push(app.downloads.start(
            url,
            app.paths.models.join(&folder).join(&name),
            None,
            token,
            move |ok| {
                if ok {
                    lib.scan();
                }
            },
        ));
    } else {
        return Err(bad("give a model id, or url + folder + name"));
    }
    Ok(Json(json!({"started": started})))
}

async fn pause_download(State(app): State<S>, AxPath(id): AxPath<String>) -> Json<Value> {
    Json(json!({"ok": app.downloads.pause(&id)}))
}

async fn remove_download(State(app): State<S>, AxPath(id): AxPath<String>) -> Json<Value> {
    app.downloads.remove(&id);
    Json(json!({"ok": true}))
}

// ---------- engine ----------

async fn engine(State(app): State<S>) -> Json<Value> {
    let (state, detail) = app.engine.status();
    Json(
        json!({"state": state, "detail": detail, "flags": app.engine.running_flags(), "installed": app.engine.layout().is_installed()}),
    )
}

#[derive(Deserialize)]
struct InstallReq {
    #[serde(default = "cuda")]
    profile: String,
}
fn cuda() -> String {
    "cuda".into()
}

async fn install_engine(State(app): State<S>, Json(b): Json<InstallReq>) -> R {
    let opts = lg_engine::install::InstallOptions {
        torch_profile: b.profile,
        uv: None,
        comfy_source: None,
        torch_index: None,
    };
    app.engine.install(opts).map_err(bad)?;
    Ok(Json(json!({"ok": true})))
}

async fn cancel_install(State(app): State<S>) -> Json<Value> {
    app.engine.cancel_install();
    Json(json!({"ok": true}))
}

async fn start_engine(State(app): State<S>) -> R {
    let flags = vec![
        "--reserve-vram".to_string(),
        format!("{:.1}", app.settings.read().planner.vram_headroom_gb),
    ];
    app.ensure_engine(&flags).await.map_err(bad)?;
    Ok(Json(json!({"ok": true})))
}

async fn stop_engine(State(app): State<S>) -> Json<Value> {
    app.engine.stop().await;
    Json(json!({"ok": true}))
}

async fn object_info(State(app): State<S>) -> R {
    Ok(Json(
        app.engine
            .object_info()
            .map(|v| (*v).clone())
            .ok_or_else(|| bad("the engine has not started yet"))?,
    ))
}

// ---------- workflow import & tools ----------

async fn engine_info(app: &App) -> Result<Arc<Value>, ApiError> {
    if let Some(i) = app.engine.object_info() {
        return Ok(i);
    }
    app.ensure_engine(&[])
        .await
        .map_err(|e| bad(format!("start the engine first: {e}")))?;
    app.engine
        .object_info()
        .ok_or_else(|| bad("the engine gave no node definitions"))
}

async fn import_workflow(State(app): State<S>, Json(b): Json<Value>) -> R {
    let wf = b.get("workflow").cloned().unwrap_or(b);
    let info = engine_info(&app).await?;
    let imp = lg_workflow::import::import(&wf, &info).map_err(|e| bad(e.to_string()))?;
    let candidates = lg_workflow::import::analyze(&imp.graph, &info);
    let issues: Vec<String> = lg_workflow::objectinfo::validate(&imp.graph, &info)
        .iter()
        .map(|i| i.message())
        .collect();
    // Blueprints and subgraph workflows carry their own field choice: offer it ready-made.
    let suggested = if imp.exposed.is_empty() {
        None
    } else {
        lg_workflow::import::blueprint_tool(&imp, &info, "Imported tool").ok()
    };
    Ok(Json(
        json!({"graph": imp.graph, "candidates": candidates, "exposed": imp.exposed, "title": imp.title, "description": imp.description, "models": imp.models, "issues": issues, "suggested_tool": suggested}),
    ))
}

#[derive(Deserialize)]
struct PublishReq {
    graph: Option<Value>,
    fields: Option<Vec<lg_workflow::import::ToolField>>,
    meta: Option<lg_workflow::import::ToolMeta>,
    /// Or a complete manifest (e.g. the suggested tool from import).
    manifest: Option<lg_types::Manifest>,
}

async fn publish_tool(State(app): State<S>, Json(b): Json<PublishReq>) -> R {
    let m = match (b.manifest, b.graph, b.fields, b.meta) {
        (Some(mut m), _, _, _) => {
            if !m.id.starts_with("tool-") {
                m.id = format!("tool-{}", m.id.trim_start_matches("bp-"));
            }
            m.origin = Origin::Tool;
            m
        }
        (None, Some(g), Some(f), Some(meta)) => {
            let info = engine_info(&app).await?;
            lg_workflow::import::publish_tool(&g, &info, &f, &meta)
                .map_err(|e| bad(e.to_string()))?
        }
        _ => return Err(bad("give a manifest, or graph + fields + meta")),
    };
    app.registry.save(&m, Origin::Tool).map_err(bad)?;
    let _ = app.events.send(CoreEvent::ManifestsChanged);
    Ok(Json(json!({"id": m.id})))
}

// ---------- benchmarks, logs ----------

#[derive(Deserialize)]
struct BenchQ {
    model: Option<String>,
}

async fn benchmarks(State(app): State<S>, Query(q): Query<BenchQ>) -> R {
    Ok(Json(json!(app.store.benchmarks(q.model.as_deref(), 200)?)))
}

async fn logs(State(app): State<S>) -> Json<Value> {
    let text = std::fs::read_to_string(app.paths.logs().join("engine.log")).unwrap_or_default();
    let lines: Vec<String> = text
        .lines()
        .rev()
        .take(400)
        .map(crate::engine::strip_ansi)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Json(json!(lines))
}

// ---------- events ----------

async fn events(State(app): State<S>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |mut socket| async move {
        let mut rx = app.events.subscribe();
        let (state, detail) = app.engine.status();
        let hello = CoreEvent::EngineStatus { status: state, detail };
        if socket.send(Message::Text(serde_json::to_string(&hello).unwrap().into())).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                e = rx.recv() => match e {
                    Ok(e) => {
                        if socket.send(Message::Text(serde_json::to_string(&e).unwrap().into())).await.is_err() { break; }
                    }
                    // A slow client missed events; it refetches state on this marker.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let _ = socket.send(Message::Text(json!({"type": "resync"}).to_string().into())).await;
                    }
                    Err(_) => break,
                },
                m = socket.recv() => match m {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    })
}

// ---------- UI ----------

async fn ui(State(app): State<S>, req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let candidate = if path.is_empty() {
        None
    } else {
        app.paths.ui.join(path).canonicalize().ok().filter(|p| {
            p.starts_with(app.paths.ui.canonicalize().unwrap_or_default()) && p.is_file()
        })
    };
    let file = candidate.unwrap_or_else(|| app.paths.ui.join("index.html"));
    match tokio::fs::read(&file).await {
        Ok(b) => {
            let mime = mime_guess::from_path(&file).first_or_octet_stream();
            let cache = if file.ends_with("index.html") { "no-cache" } else { "public, max-age=31536000, immutable" };
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, cache.to_string())], b).into_response()
        }
        Err(_) => (StatusCode::OK, [(header::CONTENT_TYPE, "text/html")], "<h1>LocalGallery</h1><p>The interface has not been built. Run <code>npm run build</code> in apps/desktop.</p>").into_response(),
    }
}

/// Map job errors for the API (used by tests and tools).
pub fn job_error_text(e: &JobError) -> String {
    match e {
        JobError::Cancelled => "cancelled".into(),
        JobError::Failed(s) => s.clone(),
    }
}
