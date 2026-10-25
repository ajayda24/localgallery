# LocalGallery

A portable Windows app for making images and video on your own PC. It runs ComfyUI underneath, and adds a simple UI on top that adapts to each model's own controls.

- **Simple first.** Type a prompt, pick a size, press Create. Advanced and Ultra levels reveal every setting that matters: sampler, steps, weight format, memory policy, raw graph overrides.
- **Each model's own mechanisms**, from one generic set of widgets:
  - JSON/structured prompts (form or raw JSON);
  - bounding boxes and regional prompts drawn on the canvas;
  - inpaint/outpaint masks;
  - reference images;
  - first frame for video;
  - LoRA stacks and model-file pickers.
- **New models without an app update.** A model is a JSON manifest plus a ComfyUI workflow template. You can also import any ComfyUI workflow and publish it as a tool. ComfyUI's official workflows (113 of them in the pinned version) appear as ready-made tools.
- **Uses the whole PC.** A memory planner picks the file format (bf16, fp8, NVFP4 on RTX 50, GGUF) and how to spread the model across VRAM, pinned RAM, RAM and NVMe, from your hardware and from measured runs. A job that runs out of memory retries with a lighter plan. The status bar warns when Windows starts spilling into shared GPU memory.
- **Portable.** The folder is the app. Settings, gallery, database, logs, models, the engine and the window's browser profile all stay inside it. Nothing goes to the registry or `%APPDATA%`; to uninstall, delete the folder.

Built-in models: Stable Diffusion / SDXL checkpoints (txt2img, img2img, inpaint, outpaint, regions, LoRA), Z-Image Turbo, Ideogram 4 (box layouts), FLUX.2 Klein 4B (edit with reference images), FIBO 2 (structured JSON captions, edit) and Wan 2.2 14B (text-to-video, image-to-video). Many more families arrive through the official workflow tools.

## Using it
1. Download `LocalGallery-<version>-win64.zip` from the CI artifacts (or build it, below) and unzip it anywhere, including paths with spaces.
2. Run `LocalGallery.exe`. On first use, Settings › Engine installs ComfyUI with Python and PyTorch (about 3 GB for NVIDIA) into `engines\`.
3. Open Models, pick one that is marked as fitting your PC, and press Download. Files go to `models\`. You can also copy your own `.safetensors`/`.gguf` files into the subfolders; they are recognised from their headers.
4. Create.

Optional: add your Hugging Face token in Settings for gated models (it is only ever sent to huggingface.co). You can also point Settings at an existing ComfyUI models folder.

To check it on your PC, close the app and run, from the app folder:
```
powershell -ExecutionPolicy Bypass -File tools\smoke.ps1   # engine + first image: time, VRAM, RAM, spill, black-image check
powershell -ExecutionPolicy Bypass -File tools\bench.ps1   # timings for every downloaded model, sizes and policies
```

## Building
Requirements: Windows 11, Rust (rustup), Node.js 24, Git. Run `pwsh scripts/setup-dev.ps1` once to check them, then:
```
pwsh scripts/build-portable.ps1          # dist\LocalGallery\ and dist\LocalGallery-<version>-win64.zip
```
For development:
```
cargo run -p lg-server --bin localgallery-core -- --root .\dev-root --engine fake --manifests manifests --ui apps\desktop\dist --port 7890 --dev
cd apps\desktop; npm run dev             # UI with hot reload
```
`--engine` takes `managed` (the app's own ComfyUI), `fake` (no GPU needed), or the URL of a ComfyUI you already run.

## Tests
```
cargo test --workspace                   # core, workflow binding, planner, store, downloader, fake-engine e2e
cd apps\desktop; npm run lint; npm run typecheck; npm test; npm run e2e
```
The real-engine tests are opt-in and take a running ComfyUI (see `docs/progress.md`). CI runs on Linux and Windows. On Windows it also checks that the portable build writes nothing outside its folder, and installs the real engine through the packaged app.

## Docs
- `docs/BUILD_PLAN.md`: architecture, adding a model, the planner.
- `docs/decisions.md`: why things differ from the original plan.
- `docs/spikes.md`: what was verified against the real tools.
- `docs/progress.md`: milestone status.
- `docs/benchmarks.md`: GPU results (to be filled in on real hardware).

ComfyUI is GPL-3.0 and runs as a separate process; LocalGallery does not include or modify its source.
