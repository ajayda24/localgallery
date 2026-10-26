# Spikes: what was checked against the real tools

The original plan asked for upstream facts to be checked, not remembered. This page lists what was checked, where, and how.

**Build machine:** Linux container, 4 CPUs, 15 GB RAM, no GPU. Hugging Face and download.pytorch.org were blocked there; PyPI, npm, crates.io and GitHub worked.

**Windows:** checked on GitHub Actions `windows-latest` (see `.github/workflows/ci.yml`).

**The owner's RTX 5060:** nothing has run on it yet. `scripts/smoke.ps1` and `scripts/bench.ps1` are the checks for it.

## ComfyUI (pinned commit `9fe7ef76`, 2026-10-23, version 0.42)

| Fact | How it was checked |
|---|---|
| Python 3.13 + PyTorch cu130 is the official portable combination | README and the portable build scripts at the pinned commit |
| Memory flags: `--reserve-vram`, `--async-offload`, `--fast-disk`, `--disable-pinned-memory`, `--cache-ram`, `--fast fp16_accumulation\|fp8_matrix_mult\|autotune`, `--cpu` | `comfy/cli_args.py` |
| `--base-directory` needs `custom_nodes/` to exist, or ComfyUI exits | Ran it; the supervisor now creates the folder |
| WS binary preview frames: 4-byte event type, then 4-byte image type | `server.py`, then real previews |
| List-valued widgets must be sent as `{"__value__": [...]}` | Real `/prompt` rejections |
| Required inputs must be sent even when they have defaults | Real `/prompt` rejections |
| Autogrow inputs use dotted keys (`images.image_1`); DynamicCombo sub-inputs use `name.sub` (`format.codec`) | `comfy_api/latest/_io.py`, then real validation |
| `/prompt` answers 200 with `node_errors` for a partly valid graph | Real server; the client treats it as a rejection |
| Native nodes for each mechanism: `CreateBoundingBoxes`, `BuildJsonPromptIdeogram`, `Flux3LayoutPrompt`, `TextEncodeFibo2`, `ConditioningSetAreaPercentage`, `ReferenceLatent`, `ImageCompositeMasked`, `SaveVideo` | Captured `/object_info` (683 nodes) in `tests/fixtures/object_info.json` |
| FIBO 2: `fibo2` CLIP type (Qwen3-VL 4B), Flux2 latents, JSON caption keys | `comfy/supported_models.py`, `comfy/text_encoders/fibo2.py`, `comfy_extras/nodes_fibo2.py` |
| SaveVideo writes mp4 (h264) through PyAV with no separate FFmpeg | A real run producing an mp4 |
| 117 blueprints; each flattens to a valid API graph | `cargo test -p lg-workflow --test blueprints` with `LG_COMFY_DIR` (pure Rust), plus real validation |

## Run for real on CPU ComfyUI (container)

These ran against real ComfyUI on CPU, with no model weights available:
- **Managed install:** uv, Python 3.13 and torch into a portable folder whose path has spaces and "ü". The self-test passed and torch versions were frozen into `constraints.txt` (`real_install`).
- **Supervisor:** starts and stops ComfyUI, with logs and port selection (`real_comfy`).
- **Built-in operations:** all 13 operations of the 6 built-in manifests (SD/SDXL ×4, Z-Image Turbo, Ideogram 4 ×2, FLUX.2 Klein ×2, Wan 2.2 ×2, FIBO 2 ×2) pass `/prompt` validation with every mechanism filled in. Only the model files are reported missing (`real_builtin`).
- **Blueprint tools:** 113 of 113 runnable official blueprint tools pass the same check (`real_blueprints`).
- **Full stack:** the core, queue and API against real ComfyUI (`lg-server` `real_e2e`). That run covered:
  - a core-node graph producing real images;
  - Ideogram's JSON chain producing the expected JSON;
  - an mp4 written;
  - inpaint changing zero pixels outside the mask.

## Run for real on Windows (GitHub Actions `windows-latest`, CI run 6)
- **Portable build:** produces a 28.1 MB folder.
- **Headless job:** the core ran a full job from `D:\a\_temp\Local Gallery ü test\` and wrote nothing outside the folder.
- **Desktop window:** WebView2 opened the UI. A second launch handed over to the first window and exited, and the core started exactly once.
- **Window-phase writes:** only the WebView2 runtime's and Windows' own keys appeared (see decisions).
- **Playwright on Windows:** 8/8 against the release core.
- **Managed engine install, through the packaged app:**
  - uv, Python 3.13, the pinned ComfyUI archive and CPU PyTorch installed into the app folder; ComfyUI started.
  - Built-in manifests: all 13 operations pass real validation.
  - Blueprint tools: 113 of 113 runnable ones pass (2 fully accepted, 111 rejected only for missing model files; 3 paid-API tools skipped).
  - The full-stack test on the real engine passed.
- **Bugs this found and fixed:**
  - `\\?\` verbatim paths hid the disk from the planner and leaked into ComfyUI paths.
  - Monitor samples stalled behind `typeperf`.
  - Startup waited on a slow PowerShell NPU query.
  - The client read ComfyUI's history before it was stored.

## Desktop shell

- Tauri 2.12.5 builds on Linux (webkit2gtk) and Windows (WebView2).
- Under Xvfb, the window opens the real UI and authenticates with the launch token. The WebView profile, logs and database all land inside the app folder.
- The single-instance plugin needs D-Bus on Linux and is checked on Windows in CI.

## Not checkable here (needs the RTX 5060)

- **Speed:** real generation speed, and how much the cu130 build and fp8/NVFP4 paths gain on sm_120.
- **Memory:** VRAM use, shared-memory spill behaviour, and whether the planner's fit levels match reality on 8 GB.
- **Quality:** image quality per model and format, and whether video length or quality holds up on 8 GB.
- **CUDA install size and time** on a home connection.
