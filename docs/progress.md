# Progress

Each milestone ends with its checks passing. Gates on every commit: `cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace`, and `npm run lint`, `typecheck`, `test` and `e2e`.

## M1: Foundation (done)
- Rust workspace with core types (`JobSpec`, manifest, events, hardware/plan).
- Workflow binder, SQLite store and resumable downloader (with SHA-256 checks).
- `lg-fakecomfy`: a full ComfyUI protocol fake that validates graphs against a real `/object_info`.

## M2: ComfyUI engine (done)
- HTTP/WS client: queue, progress, previews, cancel, `/free`, OOM detection.
- Supervisor with a Windows Job Object.
- Managed portable install with uv (Python 3.13, cu130 torch, frozen constraints, no xformers).
- Verified against real ComfyUI 0.42 on CPU, from paths with spaces and non-ASCII characters.

## M3: Manifests and input system (done)
- Input-kind catalogue, `{{placeholder}}` templates with filters, and the repeat-chain expander.
- Hot-reloaded registry with precedence builtin < generated < tool < user.
- Workflow import and publish-as-tool.
- Subgraph flattening; the 117 official blueprints become tools (113/113 runnable pass real validation).
- Built-in manifests: SD/SDXL (txt2img, img2img, inpaint, outpaint, regions, LoRA), Z-Image Turbo, Ideogram 4 (boxes to JSON layout), FLUX.2 Klein 4B (edit with reference chains), Wan 2.2 14B (txt2vid, img2vid with two-expert sampling), FIBO 2 (structured JSON caption, edit).

## M4: Library, hardware, planner, downloads (done)
- `lg-hwprobe`: nvidia-smi, RAM, disk, laptop, NPU, and a shared-GPU-memory counter.
- `lg-modelscan`: safetensors/GGUF header detection using ComfyUI's model signatures.
- `lg-planner`: fit levels, precision choice per policy, launch flags, fp8 storage, tiled VAE, OOM escalation, and learning from history.
- Benchmark recorder.

## M5–M7: UI (done)
- React app with Create (Simple/Advanced/Ultra), Studio canvas (mask brush, region boxes, undo/redo), Gallery with compare and sidecar recipes, Queue with live previews, Models (fit badges, downloads), Workflows (import, publish, graph view), and Settings.
- Playwright suite against the real core and the fake engine: 8/8 tests.
- The real core on real ComfyUI produced zero changed pixels outside the inpaint mask.

## M8: Portable packaging (done, Windows CI pending its first green run)
- **Tauri 2 shell:** the core runs in-process with a single-instance window. The WebView2 profile lives in `data/webview`, with trimmed browser args and suspend-on-minimise.
- **`scripts/build-portable.ps1`** produces the zip and a sha256.
- **`portable-check.ps1`:** runs the app from a path with spaces and non-ASCII characters, then diffs `%APPDATA%`, `%LOCALAPPDATA%` and `HKCU\Software`. A negative test (a stray write) makes it fail.
- **`engine-check.ps1`:** a real engine install through the packaged app on Windows, followed by the real-engine tests.
- **`smoke.ps1` and `bench.ps1`** for the owner's PC, shipped in `tools\` inside the zip.
- **CI:** the Linux, Windows and Windows-engine jobs.

## Not done
- sd.cpp "Lite" engine (deferred; see decisions).
- NPU use (the owner has not confirmed an NPU).
- Real GPU results (see `benchmarks.md`).
