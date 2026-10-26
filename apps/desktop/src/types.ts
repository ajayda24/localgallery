// Mirrors of the core's JSON types (crates/lg-types).

export type InputKind =
  | "text" | "negative_text" | "structured_prompt" | "regions" | "mask" | "image" | "image_list"
  | "first_frame" | "last_frame" | "video" | "audio" | "control_image" | "lora_stack" | "size"
  | "seed" | "int" | "float" | "bool" | "enum" | "model_file" | "motion" | "raw";

export type Level = "simple" | "advanced" | "ultra";

export interface OptionItem { value: unknown; label?: string }

export interface InputSpec {
  id: string;
  kind: InputKind;
  label: string;
  help?: string;
  level?: Level;
  group?: string | null;
  default?: unknown;
  required?: boolean;
  ops?: string[];
  min?: number | null;
  max?: number | null;
  step?: number | null;
  options?: unknown[];
  schema?: Record<string, unknown> | null;
  extra?: Record<string, unknown>;
}

export interface Preset { name: string; description?: string; values: Record<string, unknown> }

export interface FitInfo { fit: Fit; badge: "green" | "yellow" | "red"; explain: string[]; warnings: string[]; precision?: string | null }
export type Fit = "resident" | "offload" | "stream" | "too_big";

export interface FileView {
  role: string; folder: string; ops: string[]; optional: boolean; present: string[];
  variants: { name: string; precision: string; size_gb: number; downloadable: boolean }[];
}

export interface ModelSummary {
  id: string;
  display_name: string;
  family: string;
  media: "image" | "video" | "audio";
  description: string;
  tags: string[];
  license: { name: string; commercial: boolean; url?: string | null };
  origin: "builtin" | "user" | "tool";
  ops: string[];
  main_op: string;
  problems: string[];
  unavailable: string | null;
  files: FileView[];
  installed: boolean;
  size_gb: number;
  fit: FitInfo | null;
}

export interface ModelDetail extends ModelSummary {
  inputs: InputSpec[];
  presets: Preset[];
  choices: Record<string, string[]>;
  manifest: Record<string, any>;
}

export interface Region {
  x: number; y: number; w: number; h: number;
  prompt: string; type?: "obj" | "text"; text?: string; weight?: number; palette?: string[];
}

export interface JobSpec {
  version?: number;
  model: string;
  op: string;
  values: Record<string, unknown>;
  batch?: number;
  memory?: { plan?: string | null; precision?: string | null; vae_tiled?: boolean | null };
  overrides?: Record<string, unknown>;
}

export type JobStatus = "queued" | "running" | "done" | "failed" | "cancelled";

export interface Job {
  id: string;
  spec: JobSpec;
  status: JobStatus;
  position: number;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  error: string | null;
  outputs: string[];
  metrics: { seconds: number; peak_vram_gb?: number | null; peak_ram_gb?: number | null; shared_gpu_gb?: number | null; plan: string; precision?: string | null } | null;
}

export interface Output {
  id: string; job_id: string; path: string; thumb: string | null; media: "image" | "video" | "audio";
  width: number | null; height: number | null; model: string; op: string; prompt: string;
  seed: number | null; created_at: number; favorite: boolean;
}

export interface Plan {
  fit: Fit; variants: [string, string][]; precision: string | null; launch_flags: string[]; vae_tiled: boolean;
  value_overrides: Record<string, unknown>; vram_budget_gb: number; weights_gb: number; downloads: [string, number][];
  estimated_vram_gb: number; estimated_seconds: number | null; explain: string[]; warnings: string[];
}

export interface GpuInfo { index: number; name: string; vendor: string; vram_total_gb: number; vram_free_gb: number; compute_capability: [number, number] | null; driver?: string | null; cuda_driver_version?: string | null }

export interface Hardware {
  os: string; cpu: string; cpu_threads: number; ram_total_gb: number; ram_free_gb: number; gpus: GpuInfo[]; npu: string;
  disk: { path: string; free_gb: number; total_gb: number; kind: string } | null; laptop: boolean | null;
}

export type EngineState = "not_installed" | "installing" | "stopped" | "starting" | "ready" | "busy" | "error";

export interface SystemInfo {
  version: string;
  hardware: Hardware;
  engine: { state: EngineState; detail: string; mode: { kind: "managed" | "external" | "fake"; installed?: boolean; url?: string }; flags: string[] };
  root: string;
  running_job: string | null;
  manifest_errors: { path: string; error: string }[];
}

export interface Settings {
  planner: { vram_headroom_gb: number; ram_reserve_gb: number; policy: "balanced" | "quality" | "speed"; fast_math: boolean };
  auto_download: boolean;
  hf_token: string;
  hf_token_set?: boolean;
  extra_model_dirs: string[];
  group_by_model: boolean;
  previews: boolean;
  theme: string;
  ui_level: string;
}

export interface Download { id: string; name: string; url: string; dest: string; received: number; total: number | null; state: string; error: string | null }

export interface LibraryFile {
  folder: string; name: string; root: string; size_bytes: number;
  info: { kind: string; arch: string | null; precision: string | null; format: string; parameters: number } | null;
}

export interface Benchmark { model: string; op: string; precision: string | null; plan: string; width: number; height: number; frames: number; steps: number; seconds: number; peak_vram_gb: number | null; peak_ram_gb: number | null; shared_gpu_gb: number | null; created_at: number }

export type CoreEvent =
  | { type: "job_updated"; job: Job }
  | { type: "progress"; job_id: string; step: number; total: number; overall: number; node: string | null; eta_seconds: number | null }
  | { type: "preview"; job_id: string; data: string }
  | { type: "output_added"; output: Output }
  | { type: "log"; source: string; line: string }
  | { type: "engine_status"; status: EngineState; detail: string }
  | { type: "download"; id: string; name: string; received: number; total: number | null; state: string; error: string | null }
  | { type: "manifests_changed" }
  | { type: "hardware"; vram_used_gb: number; vram_total_gb: number; ram_used_gb: number; ram_total_gb: number; shared_gpu_gb: number | null; gpu_util: number | null }
  | { type: "resync" };
