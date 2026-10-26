# BUILD_PLAN.md — Local Image & Video Studio (working title: "LocalGen")

> Executor: Claude Code, working inside this repo. Owner: solo developer, personal use.
> Status: plan v1 (Oct 2026). Update checkboxes and `docs/decisions.md` as you go.

## 0. How to work (read first)

**Kickoff prompt for the owner to paste:**
"Read BUILD_PLAN.md completely. Do Milestone 0 only. Stop at the decision gate and report the results table. Do not start Milestone 1."

**Working agreement**
1. One milestone at a time. Each ends with its acceptance checks passing and a short demo note in `docs/progress.md`.
2. **Never trust memory for upstream tools.** Flags, endpoints and release names for stable-diffusion.cpp (sd.cpp) and ComfyUI change often. Run `--help`, read the repo docs, and record what you found in `docs/spikes.md` before coding against it. Pin versions in `engines.lock.json`.
3. **Ask the owner before:** downloading anything over ~1 GB, installing system software (Visual Studio Build Tools, CUDA Toolkit, Rust, Node), or touching anything outside this repo folder.
4. **Do not copy or modify ComfyUI source.** Run it only as a separate process over HTTP/WebSocket. (ComfyUI is GPL-3.0; this keeps the boundary clean. The owner should confirm licensing before any public release.)
5. Windows 11 only. Use PowerShell for scripts. Test with paths containing spaces and non-ASCII characters (some native engine builds mishandle them).
6. Keep a decision log: any deviation from this plan gets a dated entry in `docs/decisions.md` with the reason.
7. Small commits. Quality gates before every commit: `cargo fmt`, `cargo clippy -D warnings`, `cargo test`, `npm run lint`, `npm run typecheck`, `npm test`.

## 1. Goal and constraints (locked)

**Goal:** a Windows-only, portable desktop app that generates images and video locally with open models. It combines ComfyUI-class capability (model-specific controls, masks, regions, LoRA, video, imported workflows) with a clean beginner-friendly UI, and gets the most out of the owner's hardware.

**Owner's machine (design target)**
- GPU: NVIDIA RTX 5060, 8 GB VRAM (Blackwell, compute capability sm_120)
- RAM: 32 GB; Storage: 1 TB NVMe
- OS: Windows 11 x64
- NPU: unknown — see Milestone 8 (conditional)

**Constraints**
- **Portable:** the folder is the app. Delete the folder = uninstall. No registry writes, no `%APPDATA%` writes, all state in `./data`.
- **Small base:** app shell under ~30 MB (excluding the OS WebView2 runtime). Engines and models download on demand.
- **Not locked to GGUF:** support safetensors, ckpt/pth and GGUF as the engines allow.
- **Beginner-friendly by default, power underneath:** Simple → Studio → Graph.
- **Memory-smart:** use VRAM, pinned RAM, RAM and NVMe deliberately (see §3.6). Large models must run, even if slowly.

**Non-goals (for now):** other OSes, mobile, cloud/API generation, model training, multi-user, auto-updating the app exe, AMD/Intel GPU paths (unless Milestone 0 forces Vulkan), NPU (until Milestone 8 is unlocked).

## 2. Model targets for 8 GB VRAM / 32 GB RAM

| Tier | Models | Expectation |
|---|---|---|
| A — comfortable | SD 1.5, SDXL (+ Turbo/Lightning variants), Z-Image Turbo (quantized), FLUX.2 klein 4B (quantized/fp8) | resident or light offload, interactive speeds |
| B — works with offload | FLUX.1 schnell/dev (GGUF Q4/Q5 or fp8), Qwen-Image (quantized), Chroma | RAM offload, minutes per image |
| C — video | LTX-2.3 distilled (Q4 GGUF), Wan 2.2 TI2V-5B (Q4 GGUF), Wan 2.2 14B two-pass GGUF with a speed-up LoRA | short low-res clips; 32 GB RAM is tight for 14B video, so expect NVMe streaming and long runs |

Notes: FLUX.2 klein 9B and FLUX.2 dev weights are non-commercial licensed — fine for personal use, but surface the license in each model's manifest. Wan 2.2 14B image-to-video loads two experts (high-noise and low-noise), so they should not both be resident at once.

## 3. Architecture

```
┌──────────────────────── Tauri 2 shell (Rust) ────────────────────────┐
│  UI (React+TS)  ◄──commands/events──►  Core services                 │
│  Simple | Studio canvas | Graph        ├ hwprobe   ├ modelscan       │
│                                        ├ planner   ├ downloader      │
│                                        ├ jobqueue  └ store (SQLite)  │
│                                        └ engine supervisor           │
└───────────────────────┬──────────────────────────────┬───────────────┘
                        │ Engine adapter contract       │
              ┌─────────▼────────┐          ┌──────────▼──────────┐     (M8, conditional)
              │ Lite: sd.cpp     │          │ Full: ComfyUI       │     ┌──────────────┐
              │ sidecar process  │          │ headless sidecar    │     │ NPU: OpenVINO│
              └──────────────────┘          └─────────────────────┘     └──────────────┘
```

### 3.1 Process model
Each engine is a **separate child process** the app supervises (Windows Job Object so children die with the app; `CREATE_NO_WINDOW`; stdout/stderr captured to `data/logs`). Engines bind to `127.0.0.1` only, on a random free port.

### 3.2 Engine adapter contract (Rust trait)
```
probe() -> EngineInfo {name, version, backends, supported_ops, supported_formats}
start(config) / stop()
submit(job: JobSpec) -> JobId
events(job_id) -> stream { Progress{step,total,eta}, Preview{png}, Log{line}, Done{outputs}, Error{kind,msg} }
cancel(job_id)
```
- **Lite adapter:** translates `JobSpec` → sd.cpp invocation. Spike (M0) decides between server mode (`sd-server`) and per-job CLI mode (`sd-cli`). Support both behind the same trait if feasible; CLI mode is the fallback.
- **Full adapter:** translates `JobSpec` → ComfyUI `workflow_api` JSON from per-family templates, queues via `POST /prompt`, tracks via WebSocket `/ws`, fetches results via `/history` and `/view`. Also supports **raw passthrough** of an imported workflow.

### 3.3 `JobSpec` (engine-neutral IR, versioned)
```json
{
  "version": 1,
  "op": "txt2img",
  "model": {"id": "sdxl-base", "files": {"diffusion": "models/checkpoints/x.safetensors"}},
  "prompt": "...", "negative": "...",
  "params": {"seed": 42, "steps": 25, "cfg": 5.5, "sampler": "euler", "width": 1024, "height": 1024},
  "loras": [{"file": "models/loras/a.safetensors", "weight": 0.8}],
  "inputs": {"image": null, "mask": null, "refs": []},
  "regions": [],
  "memory": {"plan": "auto"},
  "output": {"format": "png", "dir": "data/outputs"}
}
```
Ops: `txt2img, img2img, inpaint, outpaint, edit, txt2vid, img2vid, upscale`. Keep the IR small; add fields only when two features need them.

### 3.4 Capability manifests (data, not code)
One JSON file per model family/variant under `manifests/models/`. The UI is generated from it. Validate with a JSON Schema in CI. Example:
```json
{
  "id": "flux1-schnell-gguf",
  "family": "flux1",
  "display_name": "FLUX.1 schnell (GGUF)",
  "ops": ["txt2img"],
  "controls": {
    "steps": {"min": 1, "max": 8, "default": 4},
    "cfg": null,
    "negative_prompt": false,
    "sampler": ["euler"],
    "resolution": {"multiple_of": 16, "max_pixels": 1048576, "presets": [[1024,1024],[832,1216]]},
    "lora": true
  },
  "files": {
    "diffusion": {"formats": ["gguf", "safetensors"], "size_gb": 6.8},
    "text_encoders": [{"id": "t5xxl", "size_gb": 4.9}, {"id": "clip_l", "size_gb": 0.25}],
    "vae": {"size_gb": 0.33}
  },
  "memory": {"weights_gb": {"diffusion": 6.8, "te": 5.2, "vae": 0.33}, "activation_gb_at_1mp": 1.2},
  "license": {"name": "Apache-2.0", "commercial": true},
  "engines": {"lite": "supported", "full": "supported"},
  "ui": {"simple": ["prompt", "size", "seed"], "advanced": ["steps", "sampler", "lora"]}
}
```
Sizes above are illustrative; fill real numbers from the model files during M2.

### 3.5 Model library
Scan configured folders (and optionally an existing ComfyUI `models/` tree). Detect architecture by reading the **safetensors header** (JSON at file start) or **GGUF metadata**, never by loading weights. Match to a manifest by heuristics with a manual override. Never copy model files unless the owner asks.

### 3.6 Memory planner (core differentiator)
Inputs: hardware probe (VRAM total/free, RAM total/free, disk speed class), manifest memory profile, requested resolution/frames. Output: a **plan** (engine flags or workflow settings) plus a **plain-language explanation** shown to the user.

Rules v1 (tune with benchmarks):
1. Reserve VRAM headroom for Windows and the desktop (start with ~1 GB; make configurable).
2. If diffusion weights + activations fit in `VRAM − headroom`: keep resident.
3. Text encoders larger than ~1 GB: encode, then evict (CPU/RAM) before denoising.
4. Doesn't fit: stream/offload weights from RAM. If RAM can hold everything with an OS reserve (start ~8 GB), use RAM; otherwise stream from NVMe (mmap/disk params) and warn about speed.
5. Always enable flash attention when the engine supports it.
6. **Avoid Windows "shared GPU memory" spill** (it silently kills speed). Monitor dedicated vs shared usage during runs; if spill is detected, reduce resolution/batch or escalate the offload level.
7. Keep VAE on the GPU; VAE-on-CPU is extremely slow (one sd.cpp benchmark measured ~600 s vs ~115 s).
8. Log every run's peak VRAM/RAM/time to SQLite. The planner prefers measured history over estimates for the same (model, resolution, plan).

Candidate sd.cpp memory flags to verify in M0 (do not assume): `--diffusion-fa`, `--offload-to-cpu`, `--params-backend` (per-module `cpu`/`disk`, e.g. `diffusion=disk,te=cpu,vae=cpu`), `--vae-on-cpu`. A layer-streaming mode exists as a proposed upstream PR; check whether it is merged before relying on it.

### 3.7 Portable layout
```
LocalGen/
├─ localgen.exe            # Tauri shell
├─ portable.txt            # marker (always portable; keep for clarity)
├─ engines/
│  ├─ lite/                # sd.cpp binaries (downloaded on demand)
│  ├─ full/                # embedded Python + ComfyUI + venv (optional)
│  └─ tools/               # ffmpeg etc. (on demand)
├─ models/                  # or configured external folders
├─ data/                    # db, settings, logs, outputs, thumbnails, webview2 profile
└─ engines.lock.json        # pinned versions + hashes
```
- WebView2 user data folder lives in `data/`. For child processes set cache/temp env vars (e.g., `HF_HOME`, `TORCH_HOME`, `TEMP`) into `data/`.
- WebView2: Windows 10/11 ship it; offer an optional "fixed runtime" build (~180 MB bigger) only for machines without it. In dev, use the system runtime (fixed runtime is known to be problematic in `tauri dev`).

## 4. Tech stack (decided)
- **Shell/backend:** Tauri 2, Rust workspace (crates: `core`, `hwprobe`, `modelscan`, `planner`, `downloader`, `engine-lite`, `engine-comfy`, `store`).
- **Frontend:** React + TypeScript + Vite + Tailwind; Zustand for state; **Konva** for the Studio canvas (layers, masks, boxes); **React Flow** for Graph view.
- **Storage:** SQLite (jobs, images, benchmarks, settings); JSON sidecar per output image with full parameters.
- **Hardware probe:** NVML (via crate) or `nvidia-smi` parsing as fallback; `sysinfo` for RAM; DXGI/perf counters for shared-GPU-memory monitoring (investigate in M2).
- **Full engine environment:** `uv` to create the venv; pinned Python, PyTorch (CUDA 12.8 or newer wheels — required for Blackwell), pinned ComfyUI commit, `constraints.txt` that protects torch.

## 5. Repo layout
```
/apps/desktop/{src (React), src-tauri (Rust)}
/crates/{core,hwprobe,modelscan,planner,downloader,engine-lite,engine-comfy,store}
/manifests/{models,recipes,schema}
/scripts/        # PowerShell: setup-dev, fetch-engines, build-portable, smoke
/docs/           # BUILD_PLAN.md copy, spikes.md, decisions.md, progress.md, benchmarks.md
/tests/{fixtures,integration}
engines.lock.json
```

## 6. Milestones

Size: S ≈ a few hours, M ≈ a day or two, L ≈ several days (rough, depends on how much debugging engines need).

### M0 — Environment and spikes (size M) — **decision gate**
Goal: replace assumptions with measurements on the owner's PC. No app code beyond throwaway scripts.

- [ ] **S1 Toolchain check** (`scripts/setup-dev.ps1`): report versions of `rustc`, `cargo`, Node, `git`, `nvidia-smi` (driver must support CUDA 12.8+), WebView2 presence. List what's missing and ask before installing.
- [ ] **S2 sd.cpp on the 5060:** download the latest Windows CUDA 12 prebuilt release; run `--help` (save output). Test SD 1.5 and SDXL txt2img. **If it errors with a missing-kernel/arch message or crashes:** try the Vulkan build; if that is unacceptable, build from source with CUDA 12.8+ and `CMAKE_CUDA_ARCHITECTURES=120` (ask before installing the CUDA Toolkit/MSVC). Then test Z-Image Turbo (Q4) and FLUX.2 klein 4B. Record per run: seconds, peak VRAM (sample `nvidia-smi --query-gpu=memory.used --format=csv -l 1`), peak RAM, flags used. Try the memory flags from §3.6.
- [ ] **S3 sd-server vs sd-cli:** read the server docs/`--help`, enumerate endpoints, send one generation request, observe progress reporting and cancel behavior. Decide server mode vs CLI mode for the Lite adapter.
- [ ] **S4 Full engine feasibility:** create `engines/full` by hand: Python version per ComfyUI's current README, PyTorch from the CUDA 12.8+ index. Smoke test: `torch.cuda.get_device_capability()` must be `(12, 0)` and a CUDA matmul must succeed. Install ComfyUI (pinned commit), **do not install xformers** (it can force a PyTorch downgrade that breaks Blackwell); use `--use-pytorch-cross-attention` if attention errors appear; avoid SageAttention by default (reported black outputs on some models). Launch headless on loopback with a custom port, `--base-directory`, `--output-directory`, and `--extra-model-paths-config`. Submit one `workflow_api.json` via `/prompt`, follow `/ws`, fetch `/object_info` and save it as a fixture. Confirm Dynamic VRAM is active (log lines mention dynamic VRAM loading). If out-of-memory occurs, try `--disable-pinned-memory` first.
- [ ] **S5 Tauri hello-world + portable proof:** `npm create tauri-app`, run in dev, build a release exe, and prove WebView2 user data can live in `./data`. Verify no writes to `%APPDATA%`/registry (diff before/after, or Process Monitor).
- [ ] **S6 Video smoke (time-boxed):** one LTX-2.3 or Wan 2.2 5B clip through sd.cpp at the smallest sane settings; record time and memory. Do not tune.

**Deliverable:** `docs/spikes.md` with a results table and these decisions:
(a) Lite build source: prebuilt CUDA / self-built sm_120 / Vulkan. (b) server vs CLI mode. (c) Full engine pins (Python, torch, ComfyUI commit) written to `engines.lock.json`. (d) baseline benchmark numbers in `docs/benchmarks.md`.
**Gate:** stop and report. The owner approves before M1.

### M1 — Walking skeleton (size L)
Goal: one thin vertical slice, portable, end to end.
- [ ] Scaffold Tauri 2 + React/TS/Vite/Tailwind and the Rust workspace.
- [ ] Portable mode: all state in `./data`; WebView2 profile in `./data`; env vars for children.
- [ ] Engine supervisor: spawn, health check, restart policy, Job Object kill-on-close, log capture to file and a UI log pane.
- [ ] Downloader: resumable (HTTP range), SHA-256 verify, pause/cancel, progress events; used for engine archive and model files; optional Hugging Face token stored in `data/` and never logged.
- [ ] Lite adapter v0: `txt2img` only; progress parsing; cancel; outputs to gallery.
- [ ] SQLite store; JSON sidecar per image (prompt, seed, model, flags, timings, peak memory).
- [ ] UI v0: model picker (two hard-coded models), prompt, seed, steps, size, Generate/Cancel, progress, viewer, gallery.

**Acceptance**
- Copy the folder to a path with spaces on another location; it still works.
- Generates a 1024×1024 SDXL image. Cancel mid-run returns VRAM to baseline (verify with `nvidia-smi`). Closing the app kills engine processes.
- After a full session, no files exist outside the app folder and no new `HKCU\Software` keys for the app.
- Shell size recorded in `docs/progress.md`.

### M2 — Library, manifests, planner v1 (size L)
- [ ] `hwprobe` (GPU/VRAM/RAM/disk; NPU detection returns "none/unknown" for now).
- [ ] `modelscan`: safetensors header + GGUF metadata parsing; architecture detection; manifest matching; manual override UI; configurable extra model folders (including an existing ComfyUI models tree).
- [ ] Manifest JSON Schema + loader + CI validation. Write manifests for: SD1.5, SDXL, Z-Image Turbo, FLUX.1 schnell/dev, FLUX.2 klein 4B (+ Qwen-Image if the Lite engine supports it per M0).
- [ ] Dynamic UI renderer driven by manifests: Simple vs Advanced panels; hidden controls when the model has none (e.g., no CFG/negative prompt).
- [ ] Planner v1 (rules in §3.6) with an `explain` output and a CLI dry-run (`planner explain --model <id> --res 1024`).
- [ ] Benchmark recorder + "Benchmarks" page; "fits your PC" badges (green/yellow/red) in a curated Model Hub with sizes and license notes.

**Acceptance:** table-driven planner tests (e.g., 8 GB/32 GB: SDXL → resident; FLUX.1 dev Q4 → RAM offload; a 24 GB model on 32 GB RAM → disk streaming with a warning). Switching models visibly changes the controls with zero model-specific UI code.

### M3 — Editing and Studio canvas (size L)
- [ ] `img2img`, `inpaint`, `outpaint`, LoRA stack (weights UI), and edit models that the Lite engine supports (confirm via M0 `--help`; mark unsupported ops as unavailable in manifests).
- [ ] ControlNet / reference inputs only where the engine supports them; otherwise greyed out with a "needs Full engine" hint.
- [ ] Studio canvas (Konva): layers, brush/eraser mask, rectangle boxes, zoom/pan, undo/redo.
- [ ] **Regions feature (honest version):** boxes with per-box prompts run as sequential inpaint/composite passes. Label it "region edit" in the UI; it is not attention-level regional prompting.
- [ ] Always composite inpaint results back through the mask (feathered) so unmasked pixels remain identical to the source.
- [ ] Upscaling: investigate a small standalone upscaler binary or the engine's own option; decide in `docs/decisions.md`.

**Acceptance:** an inpaint test fixture shows zero pixel difference outside the mask (after compositing). Undo/redo is reliable across brush strokes.

### M4 — Video on Lite (size L)
- [ ] LTX-2.3 distilled and Wan 2.2 5B first; Wan 2.2 14B two-pass later if feasible within 32 GB RAM.
- [ ] UI: duration/fps/resolution with an **ETA from benchmark history**; frame preview if the engine provides it; queue with cancel.
- [ ] mp4 export via ffmpeg downloaded on demand into `engines/tools`.
- [ ] Safe mode: cap frames/resolution to what the planner predicts will fit; warn when ETA exceeds ~15 minutes.

**Acceptance:** a short 480p clip completes without shared-GPU-memory spill or a system freeze; the result plays; time/memory logged to benchmarks.

### M5 — Full engine (ComfyUI) (size L)
- [ ] Managed install: `uv` venv in `engines/full`, versions from `engines.lock.json`, `constraints.txt` pinning torch and excluding xformers. Show download sizes (PyTorch CUDA wheel is multi-GB) and require explicit opt-in.
- [ ] Supervisor launches ComfyUI headless on loopback with `--base-directory`, `--output-directory`, and `--extra-model-paths-config` pointing at the app's `models/`.
- [ ] WebSocket client and `JobSpec → workflow_api` templates per manifest.
- [ ] **Import** any `workflow_api.json`; fetch `/object_info` and generate a form from node input specs (INT/FLOAT/STRING/COMBO/IMAGE…).
- [ ] **"Publish as tool":** the user picks which node inputs become simple form fields; save as a recipe.
- [ ] Graph view (React Flow): read-only first, editable later.
- [ ] Custom node manager: clone at a pinned commit; strip `torch`/`xformers` lines from node requirements before install; list installed nodes.
- [ ] GGUF models in the Full engine typically need a GGUF loader custom node; pin one and document it.

**Acceptance:** run a community FLUX workflow and a Wan 2.2 image-to-video workflow via the app on 8 GB/32 GB; logs show dynamic VRAM loading; document times and any RAM pressure (32 GB is tight for video, so keep a "move models to NVMe / fast-disk" option available).

### M6 — Recipes and polish (size M)
- [ ] Recipes: saved multi-step pipelines (e.g., txt2img → upscale → img2vid) runnable on either engine through the IR.
- [ ] Batch runs, queue reorder, history search, compare view, per-model presets, shortcuts, theme.
- [ ] Optional prompt helper using a small local LLM sidecar (llama.cpp). Skip if it complicates memory budgeting.

### M7 — Portable packaging and hardening (size M)
- [ ] `scripts/build-portable.ps1` → zip. Variants: **Lite** (uses OS WebView2) and optional **Fixed-runtime** (+~180 MB).
- [ ] First-run wizard: hardware probe → recommended engine/model → guided download.
- [ ] Integrity check (hash verification of engines), "Repair engine", crash recovery, log export.
- [ ] Document antivirus false-positive guidance for unsigned binaries and embedded Python.

**Acceptance:** unzip on a second path/drive; first run works; delete folder leaves no traces.

### M8 — NPU backend (conditional; size L)
Only start if `hwprobe` detects an NPU **and** the owner confirms it's wanted. Scope: an OpenVINO GenAI (Intel) or vendor-specific adapter for **small/turbo models** and side tasks (prompt helper, upscaler). Supported pipelines can split stages across devices (e.g., text encoder CPU, UNet NPU, VAE GPU), but FLUX is documented as unsupported on the NPU and NPU diffusion is often slower than the integrated GPU on AMD. Do not promise speedups on large models.

## 7. Testing and verification
- **Unit:** planner (table-driven), manifest schema validation, safetensors/GGUF header parsers (fixtures of header-only files), `JobSpec` → engine-arg/workflow translation (golden files).
- **Integration without a GPU:** a fake engine server implementing the adapter protocol so CI/dev can test supervisor, queue, cancel, and event streaming.
- **GPU smoke (`scripts/smoke.ps1`, manual on the 5060):** fixed-seed generations for SDXL, Z-Image Turbo, FLUX.2 klein 4B, and one video clip. Automatic checks: output exists, not all-black, pixel variance above a threshold, peak VRAM/RAM recorded, no shared-memory spill.
- **Portability test:** run from a path with spaces; diff `%APPDATA%`, `%LOCALAPPDATA%`, and `HKCU\Software` before/after.
- **Regression benchmarks:** keep `docs/benchmarks.md` updated when engine versions change.

## 8. Risk register

| Risk | Mitigation |
|---|---|
| Blackwell (sm_120) needs CUDA 12.8+ builds; older CUDA/PyTorch wheels fail with kernel/arch errors | M0 S2/S4 verify; pin cu128-or-newer PyTorch; startup self-test; fall back to Vulkan for Lite if needed |
| xformers/custom-node requirements silently replace PyTorch | `constraints.txt`, strip torch/xformers from node requirements, post-install torch version assertion |
| SageAttention black outputs on some models | off by default; black-image check in smoke tests |
| Windows shared-GPU-memory spill kills speed | monitor, escalate offload, warn user |
| Pinned memory causing OOM | planner option + setting; try disabling first when OOM on templates that should fit |
| 32 GB RAM tight for 14B video | NVMe streaming mode, smaller video models first, honest ETAs |
| sd.cpp flags/APIs drift | pin version in `engines.lock.json`, adapter tests, re-run smoke on upgrade |
| Lite lacks ComfyUI-only features | clear "needs Full engine" hints; Full engine is opt-in |
| GPL (ComfyUI) / model licenses | separate process boundary; license shown per manifest; owner reviews before any distribution |
| Unsigned exe and embedded Python flagged by antivirus | documented exclusions; no self-modifying updater |
| WebView2 differences | system runtime in dev; optional fixed-runtime build for release |

## 9. Open questions for the owner
1. Does the PC have an NPU (Core Ultra / Ryzen AI / Snapdragon)? Controls whether M8 ever starts.
2. Desktop or laptop? (Both RTX 5060 variants have 8 GB; laptops add power/thermal limits that affect long video runs.)
3. App name (replace "LocalGen")?
4. Priority order after M2: editing (M3), video (M4), or Full engine (M5)? Default is the order above.
5. Existing ComfyUI install or model folders to reuse?

## 10. References (verified during planning)
- stable-diffusion.cpp: https://github.com/leejet/stable-diffusion.cpp
- sd.cpp VRAM offload / layer-streaming PR: https://github.com/leejet/stable-diffusion.cpp/pull/1477
- ComfyUI startup flags: https://docs.comfy.org/development/comfyui-server/startup-flags
- ComfyUI API (`/prompt`, `/ws`, `/object_info`): https://dev.to/methodox/devlog-20250710-comfyui-api-1mi0 and https://docs.runcomfy.com/serverless/workflow-files
- Dynamic VRAM: https://blog.comfy.org/p/dynamic-vram-in-comfyui-saving-local
- Blackwell needs PyTorch with CUDA 12.8+: https://discuss.pytorch.org/t/pytorch-support-for-sm120/216099?page=3
- Blackwell/xformers and attention pitfalls: https://lilting.ch/en/articles/comfyui-blackwell-gpu-compatibility
- Tauri Windows WebView2 options and fixed-runtime size: https://v1.tauri.app/v1/guides/building/windows
- Portable Tauri pattern (data folder, lite vs fixed-runtime zips): https://github.com/thejacedev/Noteriv/releases/tag/v2.0.2
- Tauri sidecar path convention: https://github.com/tamp-build/tamp-tauri
