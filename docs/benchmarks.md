# Benchmarks

No numbers yet. The builder had no GPU, and fake-engine timings say nothing about real speed, so none are recorded here.

## How to fill this in (on the RTX 5060)

1. Close LocalGallery.
2. From the app folder, run `powershell -ExecutionPolicy Bypass -File tools\smoke.ps1`. It installs the engine (asking first), downloads Z-Image Turbo (asking first, with the size), checks CUDA, and renders the same image cold and then warm.
3. Run `powershell -ExecutionPolicy Bypass -File tools\bench.ps1` for every downloaded image model × sizes × policies. For example: `-Sizes 1024x1024,1536x1536 -Runs 5`.
4. Paste the summary table and the JSON/CSV paths from `data\bench\` below.

Each run is also saved to the app's benchmark history. The planner then uses the measured time and peak memory for ETAs and for spotting shared-memory spill.

## Results

| Date | GPU / driver | Model | Size | Policy | Format | Warm s | Peak VRAM GB | Peak RAM GB | Spill GB |
|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | |
