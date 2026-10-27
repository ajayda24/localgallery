# Decisions

Changes from `BUILD_PLAN.original.md`, with the reason for each. Newest last.

## 2026-10-23: ComfyUI is the primary engine; sd.cpp is not in this delivery
The original plan made stable-diffusion.cpp the default and ComfyUI optional. The owner's goal is day-0 support for new models and their special inputs (JSON prompts, boxes, reference images, first/last frames). Those land in ComfyUI first, usually as native nodes on release day. sd.cpp would only add a smaller install for a few older families. The engine boundary (`lg-engine` client plus supervisor, and the `JobSpec` IR) leaves room for a Lite adapter later; it is not built.

## 2026-10-23: The core is a local HTTP/WS service; the Tauri shell is only the window
The UI talks to `lg-server` (axum on 127.0.0.1) instead of Tauri commands. As a result:
- The same core runs headless (`localgallery-core.exe`) for scripts and benchmarks.
- The real UI and core can be tested end to end with Playwright on Linux CI.
- A native UI could replace the WebView later without touching the core.

Security:
- A random per-launch token, traded once from the URL for an HttpOnly SameSite=Strict cookie.
- A Host-header check (DNS rebinding), and CSP headers on the UI.
- The window gets no Tauri commands at all.

Performance: the engine (PyTorch/CUDA) does the heavy work. The core is native Rust. The WebView runs with background networking, sync and component updates off, and is suspended when minimised.

## 2026-10-23: The app is named LocalGallery
At the owner's request (the plan used "LocalGen").

## 2026-10-23: New models are data, not code
A model is a manifest (`manifests/models/*.json`) plus ComfyUI workflow templates. The manifest declares its inputs from a fixed catalogue of input kinds, and each kind has one generic UI widget and one binding rule. The catalogue: text, negative_text, structured_prompt, regions, mask, image, image_list, first_frame, last_frame, video, audio, control_image, lora_stack, size, seed, int, float, bool, enum, model_file, motion, raw.

Templates use `{{placeholders}}` with filters. A repeat-chain expander covers LoRA stacks, regional prompts and reference-image chains without code. Manifests in `data/user-manifests` are hot-reloaded, and any ComfyUI workflow can be imported and published as a tool.

## 2026-10-23: Official ComfyUI blueprints become ready-made tools
The pinned ComfyUI ships 117 official blueprints (subgraph workflows). The core flattens them and turns each into a tool manifest:
- exposed fields come from `proxyWidgets`;
- downloadable files come from `properties.models`;
- optional uploads are found by simulation.

113 of the 116 that run locally pass real ComfyUI validation. The other 3 use a paid API node and are skipped. New official workflows appear after an engine update with no app release.

## 2026-10-23: Engine install
- **Python and PyTorch:** Python 3.13 with PyTorch from the cu130 index, matching ComfyUI's own portable build at the pinned commit. Torch versions are frozen into `engines/full/constraints.txt` after install, rather than pinned to versions that could not be verified.
- **No xformers:** it is never installed (Blackwell breakage).
- **ffmpeg:** no separate download, because SaveVideo writes mp4 through PyAV's bundled FFmpeg.
- **Paths with spaces:** uv runs with its working directory in `engines/full` and a relative constraints path, because uv splits `-c` paths at spaces.

## 2026-10-23: Memory flags follow the pinned ComfyUI, not the original plan's guesses
The planner uses ComfyUI's actual flags: `--reserve-vram`, `--async-offload`, `--fast-disk` (NVMe streaming), `--disable-pinned-memory` (low RAM), `--cpu`, plus `weight_dtype` fp8 storage and tiled VAE per job. Its fit levels are resident, offload, stream and too_big.

On out-of-memory, a job is retried up to three times, each time with a lighter plan. Measured runs, including Windows shared-GPU-memory spill, feed later estimates.

## 2026-10-24: The Tauri shell starts the core inside `setup`
On Windows the single-instance check runs when the plugin initialises. Starting the core afterwards means a second launch only focuses the first window and never opens the database or the engine.

## 2026-10-24: `lg-imageops` removed
Masked edits are composited inside the workflow (`ImageCompositeMasked`). A test against real ComfyUI proves that zero pixels change outside the mask, so a separate Rust image crate had nothing to do. Thumbnails use the `image` crate in `lg-server`.

## 2026-10-25: FIBO 2 ships with file pickers instead of download links
FIBO 2 is supported natively by the pinned ComfyUI (TextEncodeFibo2, the `fibo2` CLIP type and the Flux2 latent space). Its Hugging Face file names could not be verified from the build machine. Rather than invent URLs, the manifest asks for the diffusion model and text encoder through `model_file` pickers; only the shared FLUX.2 VAE downloads automatically. The caption fields come from the encoder's null caption in `comfy/text_encoders/fibo2.py`.

## 2026-10-25: Manual "Start engine" passes `--cpu` without an NVIDIA GPU
The planner already did this for jobs. The manual start button did not, so ComfyUI would fail on a CPU-only PyTorch.

## 2026-10-25: What "portable" means with a WebView2 window
The Windows CI check diffs `%APPDATA%`, `%LOCALAPPDATA%` and `HKCU\Software` around each phase:
- **Headless core:** must add nothing at all, and doesn't.
- **Desktop window:** Microsoft's WebView2 runtime writes its own bookkeeping, even with the user data folder inside the app:
  - `HKCU\Software\Microsoft\EdgeWebView\*` (version beacon, preference MACs, stability metrics);
  - its updater's `EdgeUpdate\ClientState` key for the runtime;
  - Windows' camera/microphone/location consent store entries;
  - TokenBroker cache files.

These belong to shared Windows components, not to LocalGallery, and every WebView2 app produces them. The check allows exactly these during the window phase. Anything else, or anything naming the app, fails. The README says so rather than claiming zero registry writes.

## 2026-10-26: The portable check recognises Windows setting up its own Store apps
The first release build failed the portable check. During the window phase, Windows registered its Windows Security and VCLibs packages for the runner's fresh user profile:
- package keys and AppContainer SIDs;
- `AppX…` ProgIds and the `windowsdefender:` protocol;
- Storage Sense folders.

The headless-core phase on the same runner was clean.

These writes are recognised by identifiers that only packaged apps can produce: Microsoft's package publisher id `8wekyb3d8bbwe`, `S-1-15-2-` AppContainer SIDs, `AppX` ProgIds and `AppData\Local\Packages\Microsoft.*`. LocalGallery is unpackaged and registers no package. Anything naming the app, and anything else unknown, still fails.

## 2026-10-27: Model sizes come from Hugging Face, and unknown means unknown
Manifests leave `size_gb` at 0 when the size wasn't verified, and cards showed "0.0 GB" and a green fit. The core now learns sizes with HEAD requests (Hugging Face reports `x-linked-size` on the redirect), eight at a time, cached in `data/cache/file-sizes.json`. The token goes only to Hugging Face hosts. `--hf-endpoint`/`LG_HF_ENDPOINT` points this at a mirror, or `off`. When the main file's size is still unknown, the planner picks a conservative offload plan, prints no numbers, and the badge reads "size unknown".

## 2026-10-27: A Hugging Face browser, not automatic model support
Models › Hugging Face lists an organisation's repos and files through the Hub API, maps `split_files/<folder>/` to the ComfyUI folder, and downloads into it. It does not turn a repo into a model: a new family needs nodes and a workflow, which only a manifest or an imported workflow provides. Each file says which models or workflows use it, so a download that nothing can run is visible as such.

## 2026-10-27: GGUF through ComfyUI-GGUF, swapped in at graph-build time
ComfyUI core cannot load GGUF. We pin city96/ComfyUI-GGUF (Apache-2.0, commit `6ea2651e`) in `engines.lock.json` under `custom_nodes`. It installs into `engines/full/base/custom_nodes` (ComfyUI runs with `--base-directory full/base`) with its requirements under the same constraints as ComfyUI. Existing engines get it on next start; if that fails, the engine starts without it and logs why.

Workflows stay as they are. After the files are chosen, `gguf_loaders` replaces `UNETLoader` and the `CLIPLoader` family with their GGUF versions wherever the file is `.gguf`, dropping inputs those nodes don't have (`weight_dtype`). A GGUF checkpoint is refused with an explanation, because the format carries no text encoder or VAE.

Local files become variants by architecture, not name: manifests list `arch` (scanner names, e.g. `lumina2`, `wan2.1`) and optionally `name_contains` (case and separators ignored, to tell Wan's high- and low-noise experts apart). GGUF files carry `general.architecture`, which the scanner maps to the same names. Imported workflows guess `arch` from the file names they use. The planner prefers on-disk variants and honours a per-role choice from Ultra.

Verified here: graph rewriting, scanning, planning and the job path with the fake engine serving the add-on's node definitions (`tests/fixtures/object_info.gguf.json`, written from the pinned source, not captured). Verified on Windows CI: the add-on installs, its nodes load in the real engine, and they list the app's model folders. Not verified: inference from a real GGUF, because no GGUF weights were available on the build machine.
