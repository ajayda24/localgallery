//! LocalGallery desktop shell: starts the core in-process and opens one WebView2 window on it.
//!
//! Everything stays in the app folder: the WebView profile goes to `data/webview`, logs to
//! `data/logs`. The window has no Tauri commands; the UI talks to the core over HTTP/WS.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// WebView2 switches: no background networking, sync, component updates or SmartScreen calls.
/// Tauri's defaults (`msWebOOUI,msPdfOOUI,msSmartScreenProtection`) are kept in the feature list.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,msEdgeEnhanceTextContrast \
    --disable-background-networking --disable-component-update --disable-sync --no-first-run \
    --disable-domain-reliability --disk-cache-size=33554432";

fn app_root() -> PathBuf {
    if let Some(r) = std::env::var_os("LG_ROOT") {
        return PathBuf::from(r);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| ".".into())
}

fn engine_mode() -> lg_server::Mode {
    match std::env::var("LG_ENGINE").ok().as_deref() {
        Some("fake") => lg_server::Mode::Fake {
            object_info: std::env::var_os("LG_FAKE_OBJECT_INFO").map(PathBuf::from),
            step_ms: 40,
        },
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            lg_server::Mode::External(url.to_string())
        }
        _ => lg_server::Mode::Managed,
    }
}

fn init_logging(root: &Path) {
    let dir = root.join("data").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    {
        Ok(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

/// Open a link outside the app in the user's browser (http/https only).
fn open_external(url: &tauri::Url) {
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer")
        .arg(url.as_str())
        .spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .spawn();
}

fn main() -> anyhow::Result<()> {
    let root = app_root();
    init_logging(&root);
    let opts = lg_server::Options {
        root: root.clone(),
        mode: engine_mode(),
        manifests: std::env::var_os("LG_MANIFESTS").map(PathBuf::from),
        ui: std::env::var_os("LG_UI").map(PathBuf::from),
        token: None,
        hf_endpoint: std::env::var("LG_HF_ENDPOINT").ok(),
    };
    let webview_data = root.join("data").join("webview");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .setup(move |app| {
            // Started here, after the single-instance check, so a second launch never opens
            // the database or the engine. The core shares Tauri's tokio runtime.
            let running = tauri::async_runtime::block_on(lg_server::serve(
                opts,
                0,
                lg_server::api::ServeOptions::default(),
            ))?;
            let open_url: tauri::Url = running.open_url.parse()?;
            let core_origin = open_url.origin();
            app.manage(running.app.clone());
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(open_url))
                .title("LocalGallery")
                .inner_size(1360.0, 880.0)
                .min_inner_size(960.0, 640.0)
                .data_directory(webview_data.clone())
                .additional_browser_args(BROWSER_ARGS)
                .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Suspend)
                .on_navigation(move |url| {
                    if url.origin() == core_origin {
                        true
                    } else {
                        open_external(url);
                        false
                    }
                })
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())?;

    app.run(|app, event| {
        if let RunEvent::Exit = event
            && let Some(core) = app.try_state::<Arc<lg_server::App>>()
        {
            // The Job Object would end ComfyUI anyway; stopping lets it flush cleanly.
            tauri::async_runtime::block_on(core.engine.stop());
        }
    });
    Ok(())
}
