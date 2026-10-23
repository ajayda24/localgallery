//! The job queue worker: one job at a time on the GPU.

use crate::state::App;
use base64::Engine as _;
use lg_engine::{EngineError, EngineEvent, EngineOutput};
use lg_types::{CoreEvent, Fit, InputKind, Job, JobStatus, Manifest, Output, RunMetrics, now_ms};
use lg_workflow::objectinfo::Issue;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum JobError {
    Cancelled,
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for JobError {
    fn from(e: E) -> Self {
        JobError::Failed(e.to_string())
    }
}

pub async fn worker(app: Arc<App>) {
    loop {
        let next = pick_next(&app);
        let Some(job) = next else {
            let _ = tokio::time::timeout(Duration::from_secs(2), app.wake.notified()).await;
            continue;
        };
        run_job(&app, job).await;
    }
}

/// Oldest queued job, preferring the model that is already loaded.
fn pick_next(app: &App) -> Option<Job> {
    let queued = app.store.queued().ok()?;
    let last = app.last_model.lock().clone();
    if app.settings.read().group_by_model
        && let Some(last) = last
        && let Some(j) = queued.iter().find(|j| j.spec.model == last)
    {
        // Only jump the queue within the next few jobs, so nothing waits forever.
        if queued.iter().take(4).any(|q| q.id == j.id) {
            return Some(j.clone());
        }
    }
    queued.into_iter().next()
}

fn emit_job(app: &App, id: &str) {
    if let Ok(Some(job)) = app.store.job(id) {
        let _ = app
            .events
            .send(CoreEvent::JobUpdated { job: Box::new(job) });
    }
}

pub async fn run_job(app: &Arc<App>, job: Job) {
    let token = CancellationToken::new();
    *app.running.lock() = Some((job.id.clone(), token.clone()));
    let _ = app.store.set_status(&job.id, JobStatus::Running, None);
    emit_job(app, &job.id);
    let result = execute(app, &job, &token).await;
    match result {
        Ok((outputs, metrics)) => {
            let _ = app.store.finish_job(&job.id, &outputs, &metrics);
        }
        Err(JobError::Cancelled) => {
            let _ = app.store.set_status(&job.id, JobStatus::Cancelled, None);
        }
        Err(JobError::Failed(e)) => {
            tracing::warn!("job {} failed: {e}", job.id);
            let _ = app.store.set_status(&job.id, JobStatus::Failed, Some(&e));
        }
    }
    *app.running.lock() = None;
    *app.last_model.lock() = Some(job.spec.model.clone());
    emit_job(app, &job.id);
}

/// Everything needed to run one prepared graph.
struct Prepared {
    manifest: Arc<Manifest>,
    values: BTreeMap<String, Value>,
    plan: lg_types::Plan,
}

async fn execute(
    app: &Arc<App>,
    job: &Job,
    cancel: &CancellationToken,
) -> Result<(Vec<String>, RunMetrics), JobError> {
    let entry = app
        .registry
        .get(&job.spec.model)
        .ok_or_else(|| JobError::Failed(format!("Unknown model {}", job.spec.model)))?;
    if let Some(why) = &entry.unavailable {
        return Err(JobError::Failed(format!(
            "{} cannot run: {why}",
            entry.manifest.display_name
        )));
    }
    let m = entry.manifest.clone();
    let base_values = lg_workflow::resolve_values(&m, &job.spec)?;
    let started = Instant::now();
    let mut outputs = Vec::new();
    let mut last_plan = None;
    let mut peaks = lg_hwprobe::Peaks::default();
    for rep in 0..job.spec.batch.max(1) {
        let mut values = base_values.clone();
        // Repeats use consecutive seeds.
        for i in m
            .inputs_for(&job.spec.op)
            .filter(|i| i.kind == InputKind::Seed)
        {
            if let Some(s) = values.get(&i.id).and_then(Value::as_i64) {
                values.insert(
                    i.id.clone(),
                    json!((s + rep as i64) % lg_workflow::resolve::MAX_SEED),
                );
            }
        }
        let mut escalate = 0u8;
        loop {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let prepared = prepare(app, job, &m, values.clone(), escalate, cancel).await?;
            app.monitor.reset_peaks();
            match run_once(app, job, &prepared, &entry, cancel).await {
                Ok(outs) => {
                    let p = app.monitor.peaks();
                    peaks.vram_used_gb = max_opt(peaks.vram_used_gb, p.vram_used_gb);
                    peaks.shared_gpu_gb = max_opt(peaks.shared_gpu_gb, p.shared_gpu_gb);
                    peaks.ram_used_gb = peaks.ram_used_gb.max(p.ram_used_gb);
                    for (n, o) in outs.into_iter().enumerate() {
                        let id = save_output(app, job, &prepared, &o, outputs.len() + n).await?;
                        outputs.push(id);
                    }
                    last_plan = Some(prepared);
                    break;
                }
                Err(EngineError::OutOfMemory(msg)) if escalate < 3 => {
                    escalate += 1;
                    let _ = app.events.send(CoreEvent::Log { source: "planner".into(), line: format!("Out of GPU memory ({msg}); retrying with a lighter plan (level {escalate}).") });
                    if let Some(c) = app.engine.client() {
                        let _ = c.free(true).await;
                    }
                }
                Err(EngineError::Cancelled) => return Err(JobError::Cancelled),
                Err(EngineError::Unreachable(e)) => {
                    app.engine.invalidate();
                    return Err(JobError::Failed(format!(
                        "The engine stopped responding ({e}). It will restart for the next job."
                    )));
                }
                Err(e) => return Err(JobError::Failed(e.to_string())),
            }
        }
    }
    let plan = last_plan.map(|p| p.plan);
    let seconds = started.elapsed().as_secs_f64();
    let metrics = RunMetrics {
        seconds,
        peak_vram_gb: peaks.vram_used_gb,
        peak_ram_gb: Some(peaks.ram_used_gb).filter(|r| *r > 0.0),
        shared_gpu_gb: peaks.shared_gpu_gb,
        plan: plan
            .as_ref()
            .map(|p| p.fit.as_str().to_string())
            .unwrap_or_default(),
        precision: plan.as_ref().and_then(|p| p.precision.clone()),
    };
    // Record a benchmark for the planner's time and memory estimates.
    let (w, h) = size_of(&m, &job.spec.op, &base_values);
    let frames = base_values
        .get("frames")
        .or(base_values.get("length"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let steps = base_values
        .get("steps")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let _ = app.store.add_benchmark(&lg_store::Benchmark {
        model: m.id.clone(),
        op: job.spec.op.clone(),
        precision: metrics.precision.clone(),
        plan: metrics.plan.clone(),
        width: w,
        height: h,
        frames,
        steps,
        seconds: seconds / job.spec.batch.max(1) as f64,
        peak_vram_gb: metrics.peak_vram_gb,
        peak_ram_gb: metrics.peak_ram_gb,
        shared_gpu_gb: metrics.shared_gpu_gb,
        created_at: now_ms(),
    });
    Ok((outputs, metrics))
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn size_of(m: &Manifest, op: &str, values: &BTreeMap<String, Value>) -> (u32, u32) {
    m.inputs_for(op)
        .find(|i| i.kind == InputKind::Size)
        .and_then(|i| values.get(&i.id))
        .map(|v| {
            (
                v["w"].as_u64().unwrap_or(0) as u32,
                v["h"].as_u64().unwrap_or(0) as u32,
            )
        })
        .unwrap_or((1024, 1024))
}

/// Plan, apply the plan, and make sure every file is present.
async fn prepare(
    app: &Arc<App>,
    job: &Job,
    m: &Arc<Manifest>,
    mut values: BTreeMap<String, Value>,
    escalate: u8,
    cancel: &CancellationToken,
) -> Result<Prepared, JobError> {
    let plan = crate::state::plan_for(app, m, &job.spec, &values, escalate);
    if plan.fit == Fit::TooBig {
        return Err(JobError::Failed(plan.warnings.join(" ")));
    }
    // Planner suggestions apply only where the user kept the default.
    for (k, v) in &plan.value_overrides {
        if !job.spec.values.contains_key(k) && m.input(k).is_some() {
            values.insert(k.clone(), v.clone());
        }
    }
    ensure_files(app, job, m, &plan, cancel).await?;
    Ok(Prepared {
        manifest: m.clone(),
        values,
        plan,
    })
}

/// Download missing files (when allowed) and wait for them.
async fn ensure_files(
    app: &Arc<App>,
    job: &Job,
    m: &Manifest,
    plan: &lg_types::Plan,
    cancel: &CancellationToken,
) -> Result<(), JobError> {
    // The fake engine has no weights; an external ComfyUI lists the files it can see.
    if matches!(app.engine.mode, crate::engine::Mode::Fake { .. }) {
        return Ok(());
    }
    let engine_lists = |name: &str| {
        app.engine
            .object_info()
            .is_some_and(|info| engine_has_file(&info, name))
    };
    let mut missing = Vec::new();
    for (role, name) in &plan.variants {
        let Some(f) = m.files.iter().find(|f| {
            &f.role == role
                && f.needed_for(&job.spec.op)
                && f.variants.iter().any(|v| &v.name == name)
        }) else {
            continue;
        };
        if !app.library.has(&f.folder, name) && !engine_lists(name) {
            let v = f.variants.iter().find(|v| &v.name == name).unwrap();
            missing.push((f.folder.clone(), v.clone()));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let auto = app.settings.read().auto_download;
    if !auto || missing.iter().any(|(_, v)| v.url.is_none()) {
        let names: Vec<String> = missing
            .iter()
            .map(|(f, v)| format!("{f}/{}", v.name))
            .collect();
        return Err(JobError::Failed(format!(
            "Missing model files: {}. Download them from the Models page.",
            names.join(", ")
        )));
    }
    let token = Some(app.settings.read().hf_token.clone()).filter(|t| !t.is_empty());
    for (folder, v) in missing {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dest = app.paths.models.join(&folder).join(&v.name);
        let _ = app.events.send(CoreEvent::Progress {
            job_id: job.id.clone(),
            step: 0,
            total: 0,
            overall: 0.0,
            node: Some(format!("Downloading {}", v.name)),
            eta_seconds: None,
        });
        app.downloads.start(
            v.url.clone().unwrap(),
            dest,
            v.sha256.clone(),
            token.clone(),
            move |ok| {
                let _ = tx.send(ok);
            },
        );
        let ok = tokio::select! {
            r = rx => r.unwrap_or(false),
            _ = cancel.cancelled() => return Err(JobError::Cancelled),
        };
        if !ok {
            return Err(JobError::Failed(format!(
                "Downloading {} failed. Check the Models page.",
                v.name
            )));
        }
    }
    app.library.scan();
    Ok(())
}

async fn run_once(
    app: &Arc<App>,
    job: &Job,
    p: &Prepared,
    entry: &lg_manifests::Entry,
    cancel: &CancellationToken,
) -> Result<Vec<EngineOutput>, EngineError> {
    let client = app
        .ensure_engine(&p.plan.launch_flags)
        .await
        .map_err(EngineError::Failed)?;
    let info = app.engine.object_info().unwrap_or_default();
    // Upload the job's images.
    let mut assets = BTreeMap::new();
    for id in asset_ids(&p.manifest, &job.spec.op, &p.values) {
        let a = app
            .store
            .asset(&id)
            .ok()
            .flatten()
            .ok_or_else(|| EngineError::Failed(format!("Uploaded file {id} is missing")))?;
        let path = app
            .paths
            .abs_data(&a.path)
            .ok_or_else(|| EngineError::Failed("bad asset path".into()))?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| EngineError::Failed(format!("{}: {e}", a.name)))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
        let name = client
            .upload_image(bytes, &format!("lg_{id}.{ext}"))
            .await?;
        assets.insert(id, name);
    }
    let template = lg_workflow::load_template(&p.manifest.ops[&job.spec.op].workflow, &entry.root)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    let files: BTreeMap<String, String> = p.plan.variants.iter().cloned().collect();
    let opts = lg_workflow::BindOptions {
        files,
        assets,
        vae_tiled: p.plan.vae_tiled,
        output_prefix: Some(format!("lg/{}", &job.id[..8.min(job.id.len())])),
    };
    let mut graph = lg_workflow::build_graph(&p.manifest, &job.spec, &p.values, &template, &opts)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    lg_workflow::import::drop_empty_loaders(&mut graph, &info);
    // Catch problems before the engine does, with clearer messages.
    let problems: Vec<String> = lg_workflow::objectinfo::validate(&graph, &info)
        .into_iter()
        .filter(|i| !matches!(i, Issue::BadChoice { .. }))
        .map(|i| i.message())
        .collect();
    if !problems.is_empty() {
        return Err(EngineError::Failed(problems.join("; ")));
    }
    let (tx, mut rx) = mpsc::channel::<EngineEvent>(256);
    let events = app.events.clone();
    let job_id = job.id.clone();
    let eta = p.plan.estimated_seconds;
    let previews = app.settings.read().previews;
    let started = Instant::now();
    let fwd = tokio::spawn(async move {
        let mut last_preview = Instant::now() - Duration::from_secs(1);
        while let Some(e) = rx.recv().await {
            match e {
                EngineEvent::Progress {
                    step,
                    total,
                    node,
                    overall,
                } => {
                    let remaining = eta.map(|e| (e - started.elapsed().as_secs_f64()).max(0.0));
                    let _ = events.send(CoreEvent::Progress {
                        job_id: job_id.clone(),
                        step,
                        total,
                        overall,
                        node,
                        eta_seconds: remaining,
                    });
                }
                EngineEvent::Node { node, overall } => {
                    let _ = events.send(CoreEvent::Progress {
                        job_id: job_id.clone(),
                        step: 0,
                        total: 0,
                        overall,
                        node: Some(node),
                        eta_seconds: None,
                    });
                }
                EngineEvent::Preview { mime, bytes }
                    if previews && last_preview.elapsed() > Duration::from_millis(250) =>
                {
                    last_preview = Instant::now();
                    let data = format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&bytes)
                    );
                    let _ = events.send(CoreEvent::Preview {
                        job_id: job_id.clone(),
                        data,
                    });
                }
                _ => {}
            }
        }
    });
    let r = client.run(&graph, &tx, cancel).await;
    drop(tx);
    let _ = fwd.await;
    // Keep the exact graph next to the outputs for reproducibility.
    *app.last_graph.lock() = Some(graph);
    r
}

/// Whether any loader in `/object_info` offers `name` as a choice.
pub fn engine_has_file(info: &Value, name: &str) -> bool {
    info.as_object().into_iter().flatten().any(|(_, node)| {
        ["required", "optional"].iter().any(|sec| {
            node.pointer(&format!("/input/{sec}"))
                .and_then(Value::as_object)
                .is_some_and(|inputs| {
                    inputs.values().any(|spec| {
                        let opts = spec
                            .get(0)
                            .and_then(Value::as_array)
                            .or_else(|| spec.pointer("/1/options").and_then(Value::as_array));
                        opts.is_some_and(|o| o.iter().any(|x| x.as_str() == Some(name)))
                    })
                })
        })
    })
}

/// Asset ids referenced by the op's upload inputs.
pub fn asset_ids(m: &Manifest, op: &str, values: &BTreeMap<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    for i in m.inputs_for(op) {
        match (i.kind, values.get(&i.id)) {
            (k, Some(Value::String(s))) if k.is_asset() && !s.is_empty() => out.push(s.clone()),
            (InputKind::ImageList, Some(Value::Array(a))) => {
                out.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string)))
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

async fn save_output(
    app: &Arc<App>,
    job: &Job,
    p: &Prepared,
    o: &EngineOutput,
    index: usize,
) -> Result<String, JobError> {
    let client = app
        .engine
        .client()
        .ok_or_else(|| JobError::Failed("engine went away".into()))?;
    let bytes = client.view(o).await?;
    let ext = std::path::Path::new(&o.filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let dir = app.paths.outputs().join(&day);
    tokio::fs::create_dir_all(&dir).await?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let stem = format!("{}_{index:02}", &job.id[..8.min(job.id.len())]);
    let path = dir.join(format!("{stem}.{ext}"));
    tokio::fs::write(&path, &bytes).await?;
    let video = matches!(ext.as_str(), "mp4" | "webm" | "mkv" | "mov" | "gif") || o.animated;
    let audio = matches!(ext.as_str(), "mp3" | "wav" | "flac" | "opus" | "ogg");
    let media = if audio {
        "audio"
    } else if video {
        "video"
    } else {
        "image"
    };
    // Thumbnails are made here, in native code, so the UI never decodes full images.
    let (w, h, thumb) = if media == "image" || ext == "gif" {
        let thumbs = app.paths.thumbs().join(&day);
        let tpath = thumbs.join(format!("{stem}.jpg"));
        let b = bytes.clone();
        tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&b).ok()?;
            let (w, h) = (img.width(), img.height());
            std::fs::create_dir_all(tpath.parent()?).ok()?;
            img.thumbnail(384, 384)
                .to_rgb8()
                .save_with_format(&tpath, image::ImageFormat::Jpeg)
                .ok()?;
            Some((w, h, tpath))
        })
        .await
        .ok()
        .flatten()
        .map(|(w, h, t)| (Some(w), Some(h), Some(app.paths.rel_data(&t))))
        .unwrap_or((None, None, None))
    } else {
        let (w, h) = size_of(&p.manifest, &job.spec.op, &p.values);
        (Some(w), Some(h), None)
    };
    let prompt = p
        .manifest
        .inputs_for(&job.spec.op)
        .find(|i| i.kind == InputKind::Text)
        .and_then(|i| p.values.get(&i.id))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let seed = p
        .manifest
        .inputs_for(&job.spec.op)
        .find(|i| i.kind == InputKind::Seed)
        .and_then(|i| p.values.get(&i.id))
        .and_then(Value::as_i64);
    // Sidecar with everything needed to reproduce the result.
    let sidecar = json!({
        "app": "LocalGallery", "version": env!("CARGO_PKG_VERSION"),
        "model": p.manifest.id, "model_name": p.manifest.display_name, "op": job.spec.op,
        "values": p.values, "spec": job.spec, "plan": p.plan,
        "engine_graph": app.last_graph.lock().clone(), "created": chrono::Local::now().to_rfc3339(),
    });
    let _ = tokio::fs::write(
        path.with_extension(format!("{ext}.json")),
        serde_json::to_vec_pretty(&sidecar).unwrap_or_default(),
    )
    .await;
    let out = Output {
        id: id.clone(),
        job_id: job.id.clone(),
        path: app.paths.rel_data(&path),
        thumb,
        media: media.into(),
        width: w,
        height: h,
        model: p.manifest.id.clone(),
        op: job.spec.op.clone(),
        prompt,
        seed,
        created_at: now_ms(),
        favorite: false,
    };
    app.store.insert_output(&out)?;
    let _ = app.events.send(CoreEvent::OutputAdded {
        output: Box::new(out),
    });
    Ok(id)
}
