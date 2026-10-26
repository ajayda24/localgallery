//! `lg-fakecomfy --port 8188 --dir ./fake-data [--object-info file.json] [--step-ms 50]`

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 8188)]
    port: u16,
    #[arg(long, default_value = "fake-comfy-data")]
    dir: PathBuf,
    #[arg(long)]
    object_info: Option<PathBuf>,
    #[arg(long, default_value_t = 60)]
    step_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let a = Args::parse();
    let mut cfg = lg_fakecomfy::Config::new(a.dir);
    cfg.step_ms = a.step_ms;
    if let Some(p) = a.object_info {
        cfg.object_info = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    }
    let (addr, h) = lg_fakecomfy::serve(cfg, ([127, 0, 0, 1], a.port).into()).await?;
    println!("fake ComfyUI listening on http://{addr}");
    h.await?;
    Ok(())
}
