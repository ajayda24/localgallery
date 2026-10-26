// App state. Events from the core update it; pages read slices of it.

import { create } from "zustand";
import { api } from "./api";
import type { CoreEvent, Download, Job, Level, ModelDetail, ModelSummary, Output, Settings, SystemInfo } from "./types";
import { defaults, PREFERRED_START } from "./lib/form";

export type Page = "create" | "gallery" | "queue" | "models" | "tools" | "settings";

export interface Progress { step: number; total: number; overall: number; node: string | null; eta: number | null }
export interface Gauge { vramUsed: number; vramTotal: number; ramUsed: number; ramTotal: number; shared: number | null; util: number | null }
export interface Toast { id: number; kind: "info" | "error" | "success"; text: string }

interface State {
  page: Page;
  system: SystemInfo | null;
  settings: Settings | null;
  models: ModelSummary[];
  details: Record<string, ModelDetail>;
  modelId: string | null;
  op: string | null;
  level: Level;
  /** Form values per model (shared across that model's ops). */
  values: Record<string, Record<string, unknown>>;
  batch: number;
  jobs: Job[];
  outputs: Output[];
  progress: Record<string, Progress>;
  preview: Record<string, string>;
  downloads: Download[];
  gauge: Gauge | null;
  logs: string[];
  toasts: Toast[];
  connected: boolean;
  selectedOutput: string | null;

  go: (p: Page) => void;
  refresh: () => Promise<void>;
  loadModel: (id: string) => Promise<ModelDetail | null>;
  selectModel: (id: string, op?: string) => Promise<void>;
  setOp: (op: string) => void;
  setLevel: (l: Level) => void;
  setValue: (id: string, v: unknown) => void;
  setValues: (v: Record<string, unknown>) => void;
  resetValues: () => void;
  setBatch: (n: number) => void;
  toast: (kind: Toast["kind"], text: string) => void;
  handle: (e: CoreEvent) => void;
  refreshOutputs: () => Promise<void>;
  selectOutput: (id: string | null) => void;
}

let toastId = 0;

export const useStore = create<State>((set, get) => ({
  page: "create",
  system: null,
  settings: null,
  models: [],
  details: {},
  modelId: null,
  op: null,
  level: (localStorage.getItem("lg.level") as Level) || "simple",
  values: {},
  batch: 1,
  jobs: [],
  outputs: [],
  progress: {},
  preview: {},
  downloads: [],
  gauge: null,
  logs: [],
  toasts: [],
  connected: false,
  selectedOutput: null,

  go: (page) => set({ page }),

  refresh: async () => {
    const [system, settings, models, jobs, outputs, downloads] = await Promise.all([
      api.system(), api.settings(), api.models(), api.jobs(), api.outputs({ limit: 300 }), api.downloads(),
    ]);
    set({ system, settings, models, jobs, outputs, downloads });
    const s = get();
    // Refresh the open model's details (files may have arrived).
    if (s.modelId) await s.loadModel(s.modelId);
    if (!s.modelId && models.length) {
      const remembered = localStorage.getItem("lg.model");
      const usable = (m: ModelSummary) => !m.unavailable;
      const first =
        models.find((m) => m.id === remembered && usable(m)) ??
        PREFERRED_START.map((id) => models.find((m) => m.id === id && m.installed && usable(m))).find(Boolean) ??
        models.find((m) => m.installed && usable(m) && m.origin === "builtin") ??
        PREFERRED_START.map((id) => models.find((m) => m.id === id)).find(Boolean) ??
        models[0];
      await get().selectModel(first.id);
    }
  },

  loadModel: async (id) => {
    try {
      const d = await api.model(id);
      set((s) => ({ details: { ...s.details, [id]: d } }));
      return d;
    } catch {
      return null;
    }
  },

  selectModel: async (id, op) => {
    const d = get().details[id] ?? (await get().loadModel(id));
    if (!d) return;
    set((s) => ({
      modelId: id,
      op: op && d.ops.includes(op) ? op : d.main_op,
      values: s.values[id] ? s.values : { ...s.values, [id]: defaults(d) },
    }));
    localStorage.setItem("lg.model", id);
  },

  setOp: (op) => set({ op }),
  setLevel: (level) => {
    localStorage.setItem("lg.level", level);
    set({ level });
  },
  setValue: (id, v) =>
    set((s) => {
      if (!s.modelId) return {};
      return { values: { ...s.values, [s.modelId]: { ...s.values[s.modelId], [id]: v } } };
    }),
  setValues: (v) =>
    set((s) => {
      if (!s.modelId) return {};
      return { values: { ...s.values, [s.modelId]: { ...s.values[s.modelId], ...v } } };
    }),
  resetValues: () =>
    set((s) => {
      const d = s.modelId ? s.details[s.modelId] : null;
      if (!d || !s.modelId) return {};
      return { values: { ...s.values, [s.modelId]: defaults(d) } };
    }),
  setBatch: (batch) => set({ batch }),

  toast: (kind, text) => {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
    setTimeout(() => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })), kind === "error" ? 8000 : 4000);
  },

  refreshOutputs: async () => set({ outputs: await api.outputs({ limit: 300 }) }),
  selectOutput: (selectedOutput) => set({ selectedOutput }),

  handle: (e) => {
    switch (e.type) {
      case "job_updated":
        set((s) => {
          const jobs = s.jobs.some((j) => j.id === e.job.id) ? s.jobs.map((j) => (j.id === e.job.id ? e.job : j)) : [...s.jobs, e.job];
          const done = e.job.status !== "queued" && e.job.status !== "running";
          const progress = { ...s.progress };
          if (done) delete progress[e.job.id];
          return { jobs: jobs.sort((a, b) => a.position - b.position), progress };
        });
        if (e.job.status === "failed" && e.job.error) get().toast("error", e.job.error);
        if (e.job.status !== "running") api.system().then((system) => set({ system })).catch(() => {});
        break;
      case "progress":
        set((s) => ({ progress: { ...s.progress, [e.job_id]: { step: e.step, total: e.total, overall: e.overall, node: e.node, eta: e.eta_seconds } } }));
        break;
      case "preview":
        set((s) => ({ preview: { ...s.preview, [e.job_id]: e.data } }));
        break;
      case "output_added":
        set((s) => ({ outputs: [e.output, ...s.outputs.filter((o) => o.id !== e.output.id)] }));
        break;
      case "log":
        set((s) => ({ logs: [...s.logs.slice(-499), `[${e.source}] ${e.line}`] }));
        break;
      case "engine_status":
        set((s) => (s.system ? { system: { ...s.system, engine: { ...s.system.engine, state: e.status, detail: e.detail } } } : {}));
        break;
      case "download":
        set((s) => {
          const d: Download = { id: e.id, name: e.name, url: "", dest: "", received: e.received, total: e.total, state: e.state, error: e.error };
          const exists = s.downloads.some((x) => x.id === e.id);
          return { downloads: exists ? s.downloads.map((x) => (x.id === e.id ? { ...x, ...d, url: x.url, dest: x.dest } : x)) : [...s.downloads, d] };
        });
        if (e.state === "done" || e.state === "failed") api.models().then((models) => set({ models })).catch(() => {});
        break;
      case "manifests_changed":
        api.models().then((models) => set({ models })).catch(() => {});
        if (get().modelId) get().loadModel(get().modelId!);
        break;
      case "hardware":
        set({ gauge: { vramUsed: e.vram_used_gb, vramTotal: e.vram_total_gb, ramUsed: e.ram_used_gb, ramTotal: e.ram_total_gb, shared: e.shared_gpu_gb, util: e.gpu_util } });
        break;
      case "resync":
        get().refresh().catch(() => {});
        break;
    }
  },
}));

/** Keep a WebSocket to the core open, reconnecting with backoff. */
export function connectEvents() {
  let delay = 500;
  const open = () => {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/api/events`);
    ws.onopen = () => {
      delay = 500;
      useStore.setState({ connected: true });
      useStore.getState().refresh().catch(() => {});
    };
    ws.onmessage = (m) => {
      try {
        useStore.getState().handle(JSON.parse(m.data));
      } catch {
        /* ignore malformed */
      }
    };
    ws.onclose = () => {
      useStore.setState({ connected: false });
      setTimeout(open, delay);
      delay = Math.min(delay * 2, 8000);
    };
  };
  open();
}

export const currentModel = (s: State) => (s.modelId ? s.details[s.modelId] ?? null : null);
export const currentValues = (s: State) => (s.modelId ? s.values[s.modelId] ?? {} : {});
