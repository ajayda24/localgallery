//! LocalGallery core service. Runs inside the desktop shell or as `localgallery-core`.

pub mod api;
pub mod engine;
pub mod jobs;
pub mod library;
pub mod paths;
pub mod settings;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;

pub use engine::Mode;
pub use state::{App, Options};

pub struct Running {
    pub app: Arc<App>,
    pub addr: SocketAddr,
    /// URL the app window should open (carries the launch token once).
    pub open_url: String,
    pub handle: tokio::task::JoinHandle<()>,
}

/// Start the core: state, queue worker, and the HTTP server on `127.0.0.1:port` (0 = any).
pub async fn serve(opts: Options, port: u16, serve: api::ServeOptions) -> anyhow::Result<Running> {
    let app = App::start(opts).await?;
    let router = api::router(app.clone(), serve);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    let open_url = format!("http://127.0.0.1:{}/?token={}", addr.port(), app.token);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("server stopped: {e}");
        }
    });
    tracing::info!("LocalGallery core on http://{addr}");
    Ok(Running {
        app,
        addr,
        open_url,
        handle,
    })
}
