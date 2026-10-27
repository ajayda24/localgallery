// Models and tools: what fits this PC, what is downloaded, what is downloading.

import { useEffect, useMemo, useState } from "react";
import { Download, ExternalLink, FolderSearch, Lock, Pause, Play, Search, Trash2 } from "lucide-react";
import { api } from "../api";
import { Badge, Bar, Button, Empty, Tabs } from "../components/ui";
import { formatBytes, opLabel, sortedOps } from "../lib/form";
import { useStore } from "../store";
import type { HubFile, HubRepo, LibraryFile, ModelSummary } from "../types";
import { FitBadge } from "./Create";

export function Models() {
  const [tab, setTab] = useState<"models" | "official" | "hub" | "files" | "downloads">("models");
  const downloads = useStore((s) => s.downloads);
  const active = downloads.filter((d) => d.state === "running" || d.state === "queued").length;
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b line p-4">
        <Tabs value={tab} onChange={setTab} items={[
          { value: "models", label: "Models" },
          { value: "official", label: "Official workflows" },
          { value: "hub", label: "Hugging Face" },
          { value: "files", label: "Files on disk" },
          { value: "downloads", label: active ? `Downloads (${active})` : "Downloads" },
        ]} />
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {tab === "models" ? <ModelGrid filter={(m) => !m.id.startsWith("bp-")} /> : null}
        {tab === "official" ? <ModelGrid filter={(m) => m.id.startsWith("bp-")} search /> : null}
        {tab === "hub" ? <Hub /> : null}
        {tab === "files" ? <Files /> : null}
        {tab === "downloads" ? <Downloads /> : null}
      </div>
    </div>
  );
}

function ModelGrid({ filter, search }: { filter: (m: ModelSummary) => boolean; search?: boolean }) {
  const { models, selectModel, go, toast } = useStore();
  const [q, setQ] = useState("");
  const list = useMemo(() => models.filter(filter).filter((m) => !q || `${m.display_name} ${m.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase())), [models, filter, q]);
  const official = models.filter((m) => m.id.startsWith("bp-")).length;
  if (!list.length && !q) {
    return search ? <Empty title="No official workflows yet">They appear when the image engine starts: ComfyUI ships ready-made workflows for each model family.</Empty> : <Empty title="No models">Check the manifests folder.</Empty>;
  }
  return (
    <div className="space-y-3">
      {search ? <input placeholder={`Search ${official} official workflows…`} value={q} onChange={(e) => setQ(e.target.value)} className="max-w-md" /> : null}
      <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(320px,1fr))]">
        {list.map((m) => {
          const missing = m.files.filter((f) => !f.optional && !f.present.length);
          const size = m.files.reduce((a, f) => a + (f.present.length ? 0 : f.variants[0]?.size_gb ?? 0), 0);
          return (
            <div key={m.id} className="panel p-4 flex flex-col gap-2" data-model-card={m.id}>
              <div className="flex items-start gap-2">
                <div className="min-w-0">
                  <h3 className="font-semibold truncate">{m.display_name}</h3>
                  <p className="text-xs muted line-clamp-2">{m.description}</p>
                </div>
                <span className="ml-auto shrink-0"><FitBadge m={m} /></span>
              </div>
              <div className="flex flex-wrap gap-1">
                {sortedOps(m.ops, m.main_op).map((o) => <Badge key={o}>{opLabel(o)}</Badge>)}
                {m.license?.name ? <Badge color={m.license.commercial ? "gray" : "yellow"} title={m.license.url ?? ""}>{m.license.commercial ? m.license.name : "non-commercial"}</Badge> : null}
              </div>
              {m.fit?.explain?.[0] ? <p className="text-xs muted">{m.fit.explain[0]}</p> : null}
              {m.unavailable ? <p className="text-xs text-red-400">{m.unavailable}</p> : null}
              {m.problems.length ? <p className="text-xs text-red-400">{m.problems.join("; ")}</p> : null}
              <div className="mt-auto space-y-1">
                {m.files.map((f) => (
                  <div key={`${f.role}${f.ops.join()}`} className="flex items-center gap-2 text-xs">
                    <span className={`h-1.5 w-1.5 rounded-full ${f.present.length ? "bg-emerald-400" : "bg-zinc-500"}`} />
                    <span className="truncate muted">{f.present[0] ?? f.variants[0]?.name}</span>
                    <span className="ml-auto muted whitespace-nowrap">{f.variants[0]?.size_gb ? formatBytes(f.variants[0].size_gb * 1024 ** 3) : ""}</span>
                  </div>
                ))}
              </div>
              <div className="flex gap-2 pt-1">
                <Button variant="primary" size="sm" onClick={async () => { await selectModel(m.id); go("create"); }}>Open</Button>
                {missing.length && missing.every((f) => f.variants.some((v) => v.downloadable)) ? (
                  <Button size="sm" onClick={() => api.download({ model: m.id }).then((r) => toast("info", `Downloading ${r.started.length} file(s)`)).catch((e) => toast("error", e.message))}>
                    <Download size={14} /> Download{size > 0 ? ` ${size.toFixed(1)} GB` : ""}
                  </Button>
                ) : null}
                {m.origin === "tool" && !m.id.startsWith("bp-") ? (
                  <Button size="sm" variant="ghost" onClick={() => api.deleteModel(m.id).catch((e) => toast("error", e.message))} aria-label="Delete tool"><Trash2 size={14} /></Button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Files() {
  const { selectModel, go } = useStore();
  const [files, setFiles] = useState<LibraryFile[]>([]);
  const load = () => api.library().then(setFiles);
  useEffect(() => { load(); }, []);
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <Button onClick={async () => { await api.rescan(); load(); }}><FolderSearch size={14} /> Rescan folders</Button>
        <p className="text-xs muted">Put model files in the app's <code>models</code> folder (or add your own folders in Settings). Files are identified from their headers.</p>
      </div>
      {files.length ? (
        <table className="w-full text-sm">
          <thead className="text-xs muted text-left"><tr><th className="py-1">File</th><th>Folder</th><th>Detected</th><th>Precision</th><th>Used by</th><th className="text-right">Size</th></tr></thead>
          <tbody>
            {files.map((f) => (
              <tr key={`${f.root}/${f.folder}/${f.name}`} className="border-t line">
                <td className="py-1.5 pr-2 break-all">{f.name}</td>
                <td className="muted">{f.folder}</td>
                <td>{f.info ? `${f.info.kind}${f.info.arch ? ` · ${f.info.arch}` : ""}` : <span className="muted">unknown</span>}</td>
                <td className="muted">{f.info?.precision ?? ""}</td>
                <td>
                  <span className="flex flex-wrap gap-1">
                    {f.used_by.slice(0, 3).map((u) => (
                      <button key={u.id} className="cursor-pointer" onClick={async () => { await selectModel(u.id); go("create"); }}>
                        <Badge color="accent">{u.tool ? "workflow: " : ""}{u.name}</Badge>
                      </button>
                    ))}
                    {f.used_by.length > 3 ? <Badge>+{f.used_by.length - 3} more</Badge> : null}
                  </span>
                </td>
                <td className="text-right tabular-nums muted">{formatBytes(f.size_bytes)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : <Empty title="No model files yet">Download a model from the Models tab, or copy files into the models folder.</Empty>}
    </div>
  );
}

function Downloads() {
  const { downloads, toast } = useStore();
  if (!downloads.length) return <Empty title="No downloads">Model downloads appear here. They resume if interrupted.</Empty>;
  return (
    <div className="panel divide-y divide-[var(--line)]">
      {downloads.map((d) => (
        <div key={d.id} className="p-3 space-y-1.5" data-download={d.id}>
          <div className="flex items-center gap-2 text-sm">
            <span className="font-medium truncate">{d.name}</span>
            <Badge color={d.state === "done" ? "green" : d.state === "failed" ? "red" : d.state === "running" ? "accent" : "gray"}>{d.state}</Badge>
            <span className="ml-auto text-xs muted tabular-nums">{formatBytes(d.received)}{d.total ? ` / ${formatBytes(d.total)}` : ""}</span>
            {d.state === "running" || d.state === "queued" ? <Button size="sm" variant="ghost" onClick={() => api.pauseDownload(d.id)} aria-label="Pause"><Pause size={14} /></Button> : null}
            {d.state === "paused" || d.state === "failed" ? <Button size="sm" variant="ghost" onClick={() => toast("info", "Start the model's download again to resume.")} aria-label="Resume"><Play size={14} /></Button> : null}
            <Button size="sm" variant="ghost" onClick={async () => { await api.removeDownload(d.id); useStore.setState({ downloads: await api.downloads() }); }} aria-label="Remove"><Trash2 size={14} /></Button>
          </div>
          {d.total ? <Bar value={d.received / d.total} /> : null}
          {d.error ? <p className="text-xs text-red-400">{d.error}</p> : null}
        </div>
      ))}
    </div>
  );
}

// ComfyUI model folders a downloaded file can go into (mirrors the core's list).
const FOLDERS = ["diffusion_models", "checkpoints", "text_encoders", "vae", "loras", "controlnet", "upscale_models", "clip_vision", "model_patches", "latent_upscale_models", "embeddings"];

function readPref(key: string, fallback: string) {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}
function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage can be unavailable; the default is fine.
  }
}

/** Browse a Hugging Face author's model repos (Comfy-Org by default) and download files. */
function Hub() {
  const { toast, models, selectModel, go } = useStore();
  const [author, setAuthor] = useState(() => readPref("lg.hub.author", "Comfy-Org"));
  const [q, setQ] = useState("");
  const [repos, setRepos] = useState<HubRepo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [repo, setRepo] = useState<string | null>(null);

  useEffect(() => {
    const t = setTimeout(() => {
      setError(null);
      api.hubRepos(q.trim(), author.trim())
        .then((r) => setRepos(r.repos))
        .catch((e) => { setRepos([]); setError(e.message); });
    }, 300);
    return () => clearTimeout(t);
  }, [q, author]);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(260px,340px)_1fr]" data-testid="hub">
      <div className="space-y-2 min-w-0">
        <div className="flex gap-2">
          <label className="relative flex-1">
            <Search size={14} className="absolute left-2.5 top-2.5 muted" />
            <input aria-label="Search repos" placeholder="Search models…" value={q} onChange={(e) => setQ(e.target.value)} className="pl-8" />
          </label>
          <input aria-label="Author" className="w-32" value={author} onChange={(e) => { setAuthor(e.target.value); writePref("lg.hub.author", e.target.value); }} title="Hugging Face user or organisation" />
        </div>
        {error ? <p className="text-xs text-red-400">{error}</p> : null}
        {repos === null ? <p className="text-xs muted">Loading…</p> : null}
        {repos?.length === 0 && !error ? <p className="text-xs muted">No repos found.</p> : null}
        <div className="space-y-1 max-h-[70vh] overflow-y-auto pr-1">
          {repos?.map((r) => (
            <button key={r.id} data-repo={r.id} onClick={() => setRepo(r.id)}
              className={`w-full text-left rounded-lg px-3 py-2 cursor-pointer transition-colors ${repo === r.id ? "bg-accent-500/15 text-[var(--accent-text)]" : "hover:bg-[var(--panel-2)]"}`}>
              <div className="flex items-center gap-1.5 text-sm font-medium truncate">{r.gated ? <Lock size={12} aria-label="gated" /> : null}{r.name}</div>
              <div className="text-xs muted">{[r.updated ? new Date(r.updated).toLocaleDateString() : null, r.downloads != null ? `${r.downloads.toLocaleString()} downloads` : null].filter(Boolean).join(" · ")}</div>
            </button>
          ))}
        </div>
      </div>
      <div className="min-w-0">
        {repo ? <HubRepoFiles repo={repo} toast={toast} models={models} open={async (id) => { await selectModel(id); go("create"); }} />
          : <Empty title="Pick a repo">Files are shown with their real size and the models folder they go in. Downloads resume if interrupted.</Empty>}
      </div>
    </div>
  );
}

function HubRepoFiles({ repo, toast, models, open }: { repo: string; toast: (kind: "info" | "error" | "success", text: string) => void; models: ModelSummary[]; open: (id: string) => void }) {
  const downloads = useStore((s) => s.downloads);
  const [data, setData] = useState<{ files: HubFile[]; other_files: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [folders, setFolders] = useState<Record<string, string>>({});
  // Reload when the model list changes (a download finished, a file appeared on disk).
  useEffect(() => {
    let live = true;
    setError(null);
    api.hubFiles(repo).then((d) => live && setData(d)).catch((e) => { if (live) { setData(null); setError(e.message); } });
    return () => { live = false; };
  }, [repo, models]);

  if (error) return <div className="panel p-4 text-sm text-red-400">{error}</div>;
  if (!data) return <p className="text-xs muted">Loading files…</p>;
  const total = data.files.reduce((a, f) => a + (f.present ? 0 : f.size_bytes), 0);
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <h3 className="font-semibold truncate">{repo}</h3>
        <a className="muted hover:text-[var(--text)]" href={`https://huggingface.co/${repo}`} rel="noreferrer" aria-label="Open on Hugging Face"><ExternalLink size={14} /></a>
        <span className="ml-auto text-xs muted">{data.files.length} model file(s){total ? `, ${formatBytes(total)} not downloaded` : ""}{data.other_files ? ` · ${data.other_files} other file(s) hidden` : ""}</span>
      </div>
      {data.files.length ? (
        <div className="panel divide-y divide-[var(--line)]">
          {data.files.map((f) => {
            const folder = f.folder ?? folders[f.path] ?? "";
            const dl = downloads.find((d) => d.url === f.url);
            const active = dl && (dl.state === "running" || dl.state === "queued");
            return (
              <div key={f.path} className="p-3 space-y-1.5" data-hub-file={f.name}>
                <div className="flex items-center gap-2 text-sm min-w-0">
                  <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${f.present ? "bg-emerald-400" : "bg-zinc-500"}`} />
                  <span className="font-medium truncate" title={f.path}>{f.name}</span>
                  <span className="ml-auto shrink-0 text-xs muted tabular-nums">{f.size_bytes ? formatBytes(f.size_bytes) : ""}</span>
                </div>
                <div className="flex flex-wrap items-center gap-1.5 text-xs">
                  {f.folder ? <Badge>{f.folder}</Badge> : (
                    <select aria-label={`Folder for ${f.name}`} className="w-44 py-0.5 text-xs" value={folder} onChange={(e) => setFolders({ ...folders, [f.path]: e.target.value })}>
                      <option value="">Choose a folder…</option>
                      {FOLDERS.map((x) => <option key={x} value={x}>{x}</option>)}
                    </select>
                  )}
                  {f.used_by.slice(0, 4).map((u) => <button key={u.id} onClick={() => open(u.id)} className="cursor-pointer"><Badge color="accent">{u.tool ? "workflow: " : ""}{u.name}</Badge></button>)}
                  {f.used_by.length > 4 ? <Badge>+{f.used_by.length - 4} more</Badge> : null}
                  {!f.used_by.length ? <span className="muted" title="Downloading works, but a model, an official workflow or an imported workflow has to use the file before you can create with it.">not used by a model yet</span> : null}
                  <span className="ml-auto" />
                  {f.present ? <span className="text-emerald-400">on disk</span> : active ? (
                    <span className="muted tabular-nums">{dl.total ? `${Math.round((dl.received / dl.total) * 100)}%` : "starting…"}</span>
                  ) : (
                    <Button size="sm" disabled={!folder} data-hub-download={f.name}
                      onClick={() => api.download({ url: f.url, folder, name: f.name }).then(() => toast("info", `Downloading ${f.name}`)).catch((e) => toast("error", e.message))}>
                      <Download size={14} /> {dl?.state === "failed" ? "Retry" : dl?.state === "paused" ? "Resume" : "Download"}
                    </Button>
                  )}
                </div>
                {active && dl.total ? <Bar value={dl.received / dl.total} /> : null}
                {dl?.state === "failed" && dl.error ? <p className="text-xs text-red-400">{dl.error}</p> : null}
              </div>
            );
          })}
        </div>
      ) : <Empty title="No model files">This repo has no .safetensors or .gguf files.</Empty>}
    </div>
  );
}
