# LocalGallery build plan (revised)

The owner's original plan is in `BUILD_PLAN.original.md`. This revision is what was built. Deviations and their reasons are in `decisions.md`, and status is in `progress.md`.

## Goal
A Windows-only, portable desktop app that makes images and video locally. It uses the whole PC (GPU, pinned RAM, RAM, NVMe), runs newly released models on day 0, and exposes each model's own input mechanisms. Those mechanisms include JSON prompts, bounding boxes, regions, masks, reference images and first/last frames. The UI is simple by default, with every setting that matters one click away.

Design target: an RTX 5060 8 GB (Blackwell, sm_120) with 32 GB RAM, NVMe and Windows 11.

## Architecture
```
LocalGallery.exe (Tauri 2: one WebView2 window, profile in data/webview)
 └─ lg-server, in-process (axum on 127.0.0.1, per-launch token)  ◄── React UI over HTTP/WS
     ├ lg-hwprobe    GPU/RAM/disk/NPU probe, live monitor, shared-GPU-memory spill
     ├ lg-modelscan  safetensors/GGUF header detection (ComfyUI's signatures)
     ├ lg-manifests  model and tool manifests, hot reload, blueprint tools
     ├ lg-workflow   templates, expander, validation, import, subgraph flattening
     ├ lg-planner    fit level, file variant, precision, launch flags, OOM ladder
     ├ lg-downloader resumable, SHA-256, Hugging Face token only to huggingface.co
     ├ lg-store      SQLite: jobs, outputs, assets, benchmarks, settings
     └ lg-engine     ComfyUI client + supervisor (Job Object) + managed installer
          └─ ComfyUI (separate process; GPL boundary), Python 3.13, torch cu130
```
`localgallery-core.exe` is the same core without a window, for scripts and benchmarks. `lg-fakecomfy` implements the ComfyUI protocol for tests.

## Portable layout
```
LocalGallery\
  LocalGallery.exe  localgallery-core.exe  engines.lock.json  portable.txt  README.txt
  ui\  manifests\  tools\ (smoke.ps1, bench.ps1)
  models\<ComfyUI folders>\      model files (plus any extra folders set in Settings)
  engines\full\ComfyUI, venv, constraints.txt   engines\python   engines\tools\uv
  data\  localgallery.db  settings.json  outputs\  thumbs\  assets\  logs\  webview\  cache\  tmp\
```
Nothing is written outside this folder. CI checks this on every push.

## Adding a model (no code)
1. Write `manifests/models/<id>.json`. It contains:
   - files (role, folder, variants with precision, size and URL);
   - memory profile;
   - ops, each pointing to a workflow template;
   - inputs from the kind catalogue, each with a level (`simple`, `advanced`, `ultra`), help text, defaults and presets.
2. Write the ComfyUI API-format templates in `manifests/workflows/<family>/<op>.json`, with placeholders:
   - `{{prompt}}`, `{{size.w}}`, `{{file.vae}}`;
   - `{{caption|json}}`, `{{image|asset}}`, `{{palette|split}}`, `{{boxes|bbox_state:size}}`, and so on.
3. Repeated structures (LoRA stacks, regions, reference chains) use `expand` entries instead of code.
4. Check it with `cargo test -p lg-manifests` (against the captured node set). For real ComfyUI, run `LG_REAL_COMFY_URL=… cargo test -p lg-engine --test real_builtin -- --ignored`.

Alternatives: drop the files in `data/user-manifests/` (hot reload), or import any ComfyUI workflow and publish it as a tool from the Workflows page. Official ComfyUI blueprints appear as tools automatically.

## Memory planner
- **Inputs:** model files (sizes, precisions), the job's resolution and frames, hardware, settings (VRAM headroom, RAM reserve, policy) and measured history.
- **Fit levels:**
  - *resident:* everything fits in VRAM;
  - *offload:* weights partly in RAM, with `--async-offload`;
  - *stream:* from NVMe, with `--fast-disk` and a warning;
  - *too_big:* refused, with the reason.
- **Policies:**
  - *Balanced:* the best format that stays fast;
  - *Quality:* the highest precision that fits in RAM;
  - *Speed:* NVFP4/fp8 on Blackwell, with `--fast`.
- **Out of memory:** a job retries with fp8 storage, then tiled VAE, then a lighter offload level.
- **Low RAM:** pinned memory is turned off.

## Quality gates
- Rust: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test --workspace`.
- UI: `npm run lint`, `typecheck`, `test`, and Playwright `e2e` (real core, fake engine, real node set).
- Opt-in real-engine tests: `real_install`, `real_comfy`, `real_builtin`, `real_blueprints`, `real_e2e`.
- CI: the Linux job; the Windows job (build, tests, portable zip, portable check, Playwright); and the Windows-engine job (real install through the packaged app plus the real-engine tests).

## Out of scope for now
- sd.cpp Lite engine.
- NPU.
- Model training.
- Other OSes.
- Auto-updating the app.
