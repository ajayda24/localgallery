// Thin client for the core's HTTP API. The session cookie is set when the app window opens.

import type { Benchmark, Download, HubFile, HubRepo, Job, JobSpec, LibraryFile, ModelDetail, ModelSummary, Output, Plan, Settings, SystemInfo } from "./types";

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin", headers: {} };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)["content-type"] = "application/json";
  }
  const r = await fetch(path, init);
  const text = await r.text();
  const data = text ? JSON.parse(text) : null;
  if (!r.ok) throw new ApiError(r.status, (data && data.error) || `${r.status} ${r.statusText}`);
  return data as T;
}

export const api = {
  system: () => call<SystemInfo>("GET", "/api/system"),
  settings: () => call<Settings>("GET", "/api/settings"),
  saveSettings: (patch: Partial<Settings> & { clear_hf_token?: boolean }) => call<Settings>("PUT", "/api/settings", patch),
  models: () => call<ModelSummary[]>("GET", "/api/models"),
  model: (id: string) => call<ModelDetail>("GET", `/api/models/${encodeURIComponent(id)}`),
  deleteModel: (id: string) => call("DELETE", `/api/models/${encodeURIComponent(id)}`),
  plan: (spec: JobSpec) => call<{ ok: boolean; plan?: Plan; values?: Record<string, unknown>; error?: string }>("POST", "/api/plan", spec),
  jobs: () => call<Job[]>("GET", "/api/jobs"),
  createJob: (spec: JobSpec) => call<Job>("POST", "/api/jobs", spec),
  cancelJob: (id: string) => call("POST", `/api/jobs/${id}/cancel`, {}),
  retryJob: (id: string) => call<Job>("POST", `/api/jobs/${id}/retry`, {}),
  deleteJob: (id: string) => call("DELETE", `/api/jobs/${id}`),
  reorder: (ids: string[]) => call<Job[]>("POST", "/api/jobs/reorder", { ids }),
  outputs: (q: { search?: string; model?: string; media?: string; favorites?: boolean; limit?: number } = {}) => {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== "" && v !== false) p.set(k, String(v));
    return call<Output[]>("GET", `/api/outputs?${p}`);
  },
  output: (id: string) => call<{ output: Output; sidecar: any }>("GET", `/api/outputs/${id}`),
  deleteOutput: (id: string) => call("DELETE", `/api/outputs/${id}`),
  favorite: (id: string, favorite: boolean) => call("POST", `/api/outputs/${id}/favorite`, { favorite }),
  outputAsAsset: (id: string) => call<Asset>("POST", `/api/outputs/${id}/as-asset`, {}),
  library: () => call<LibraryFile[]>("GET", "/api/library"),
  hubRepos: (q: string, author: string) => call<{ repos: HubRepo[] }>("GET", `/api/hub/repos?${new URLSearchParams({ q, author })}`),
  hubFiles: (repo: string) => call<{ repo: string; files: HubFile[]; other_files: number }>("GET", `/api/hub/files?${new URLSearchParams({ repo })}`),
  rescan: () => call("POST", "/api/library/rescan", {}),
  downloads: () => call<Download[]>("GET", "/api/downloads"),
  download: (body: { model?: string; op?: string; url?: string; folder?: string; name?: string }) => call<{ started: string[] }>("POST", "/api/downloads", body),
  pauseDownload: (id: string) => call("POST", `/api/downloads/${id}/pause`, {}),
  removeDownload: (id: string) => call("DELETE", `/api/downloads/${id}`),
  installEngine: (profile: "cuda" | "cpu") => call("POST", "/api/engine/install", { profile }),
  cancelInstall: () => call("POST", "/api/engine/cancel-install", {}),
  startEngine: () => call("POST", "/api/engine/start", {}),
  stopEngine: () => call("POST", "/api/engine/stop", {}),
  importWorkflow: (workflow: unknown) => call<ImportResult>("POST", "/api/workflows/import", { workflow }),
  publishTool: (body: { manifest?: unknown; graph?: unknown; fields?: unknown; meta?: unknown }) => call<{ id: string }>("POST", "/api/tools", body),
  benchmarks: (model?: string) => call<Benchmark[]>("GET", `/api/benchmarks${model ? `?model=${encodeURIComponent(model)}` : ""}`),
  logs: () => call<string[]>("GET", "/api/logs"),
  async upload(file: Blob, name: string): Promise<Asset> {
    const r = await fetch(`/api/assets?name=${encodeURIComponent(name)}`, { method: "POST", credentials: "same-origin", body: file });
    const data = await r.json();
    if (!r.ok) throw new ApiError(r.status, data.error ?? "upload failed");
    return data as Asset;
  },
};

export interface Asset { id: string; url: string; width: number | null; height: number | null; name: string }

export interface Candidate { node: string; class_type: string; title: string; input: string; value: unknown; kind: string; label: string; suggested: boolean }

export interface ImportResult {
  graph: Record<string, { class_type: string; inputs: Record<string, unknown>; _meta?: { title?: string } }>;
  candidates: Candidate[];
  exposed: [string, [string, string][]][];
  title: string | null;
  description: string | null;
  models: { name: string; url: string | null; directory: string }[];
  issues: string[];
  suggested_tool: unknown | null;
}

export const fileUrl = (rel: string | null | undefined) => (rel ? `/files/${rel.split("/").map(encodeURIComponent).join("/")}` : "");
