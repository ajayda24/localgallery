//! `localgallery-core`: the core without the desktop window (development, tests, headless use).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "LocalGallery core service")]
struct Args {
    /// App folder (portable root). Defaults to the folder of this executable.
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Engine: managed | fake | http://host:port (an existing ComfyUI).
    #[arg(long, default_value = "managed")]
    engine: String,
    /// For the fake engine: node definitions to serve (a captured /object_info). Repeat to
    /// merge several files, e.g. core nodes plus an add-on's.
    #[arg(long)]
    fake_object_info: Vec<PathBuf>,
    #[arg(long, default_value_t = 40)]
    fake_step_ms: u64,
    #[arg(long)]
    manifests: Option<PathBuf>,
    #[arg(long)]
    ui: Option<PathBuf>,
    /// Fixed token (tests). Random per launch otherwise.
    #[arg(long)]
    token: Option<String>,
    /// Hugging Face endpoint for size lookups and the model browser: a mirror URL, or "off".
    #[arg(long)]
    hf_endpoint: Option<String>,
    /// Development only: accept any Host header and skip the token.
    #[arg(long)]
    dev: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let a = Args::parse();
    let root = a.root.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| ".".into())
    });
    let mode = match a.engine.as_str() {
        "managed" => lg_server::Mode::Managed,
        "fake" => lg_server::Mode::Fake {
            object_info: a.fake_object_info,
            step_ms: a.fake_step_ms,
        },
        url if url.starts_with("http://") || url.starts_with("https://") => {
            lg_server::Mode::External(url.to_string())
        }
        other => anyhow::bail!("unknown engine {other:?}"),
    };
    let running = lg_server::serve(
        lg_server::Options {
            root,
            mode,
            manifests: a.manifests,
            ui: a.ui,
            token: a.token,
            hf_endpoint: a.hf_endpoint,
        },
        a.port,
        lg_server::api::ServeOptions {
            any_host: a.dev,
            no_auth: a.dev,
        },
    )
    .await?;
    // Machine-readable line for launchers and tests.
    println!("LOCALGALLERY_URL {}", running.open_url);
    tokio::select! {
        _ = running.handle => {}
        _ = tokio::signal::ctrl_c() => { running.app.engine.stop().await; }
    }
    Ok(())
}
