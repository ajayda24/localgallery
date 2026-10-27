//! Runs the managed installer for real. Opt-in (downloads Python and PyTorch):
//!
//! ```text
//! LG_UV=$(which uv) LG_COMFY_SRC=/path/to/ComfyUI [LG_UV_CACHE=/shared/cache] [LG_TORCH_INDEX=https://pypi.org/simple] \
//!   cargo test -p lg-engine --test real_install -- --ignored --nocapture
//! ```

use lg_engine::install::{InstallOptions, Layout, plan, read_lock, run};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "downloads Python and PyTorch"]
async fn managed_install_into_portable_folder() {
    let uv = PathBuf::from(std::env::var("LG_UV").expect("LG_UV"));
    let src = PathBuf::from(std::env::var("LG_COMFY_SRC").expect("LG_COMFY_SRC"));
    let lock =
        read_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../engines.lock.json")).unwrap();
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("Local Gallery");
    let mut layout = Layout::new(&app);
    layout.uv_cache = std::env::var_os("LG_UV_CACHE").map(PathBuf::from);
    let opts = InstallOptions {
        torch_profile: "cpu".into(),
        uv: Some(uv),
        comfy_source: Some(src),
        torch_index: std::env::var("LG_TORCH_INDEX").ok(),
        // Offline machines can point at a ComfyUI-GGUF checkout instead of downloading it.
        addon_sources: std::env::var_os("LG_GGUF_SRC")
            .map(|p| [("ComfyUI-GGUF".to_string(), PathBuf::from(p))].into())
            .unwrap_or_default(),
    };
    let steps = plan(&lock, &layout, &opts).unwrap();
    let report = |p: lg_engine::install::InstallProgress| {
        if p.line.is_none() {
            println!("[{}/{}] {}", p.step + 1, p.steps, p.label);
        }
    };
    let test = run(&steps, &layout, false, &CancellationToken::new(), &report)
        .await
        .unwrap()
        .unwrap();
    println!("self-test: {test:?}");
    assert!(test.matmul_ok);
    assert!(layout.is_installed());
    let constraints = std::fs::read_to_string(layout.constraints()).unwrap();
    assert!(constraints.contains("torch=="), "{constraints}");
    // Python itself lives inside the app folder.
    assert!(app.join("engines/python").exists());
}
