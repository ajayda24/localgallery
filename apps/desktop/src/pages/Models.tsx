// Models and tools: what fits this PC, what is downloaded, what is downloading.

import { useEffect, useMemo, useState } from "react";
import { Download, FolderSearch, Pause, Play, Trash2 } from "lucide-react";
import { api } from "../api";
import { Badge, Bar, Button, Empty, Tabs } from "../components/ui";
import { formatBytes, opLabel, sortedOps } from "../lib/form";
import { useStore } from "../store";
import type { LibraryFile, ModelSummary } from "../types";
import { FitBadge } from "./Create";

export function Models() {
  const [tab, setTab] = useState<"models" | "official" | "files" | "downloads">("models");
  const downloads = useStore((s) => s.downloads);
  const active = downloads.filter((d) => d.state === "running" || d.state === "queued").length;
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b line p-4">
        <Tabs value={tab} onChange={setTab} items={[
          { value: "models", label: "Models" },
          { value: "official", label: "Official workflows" },
          { value: "files", label: "Files on disk" },
          { value: "downloads", label: active ? `Downloads (${active})` : "Downloads" },
        ]} />
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {tab === "models" ? <ModelGrid filter={(m) => !m.id.startsWith("bp-")} /> : null}
        {tab === "official" ? <ModelGrid filter={(m) => m.id.startsWith("bp-")} search /> : null}
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
                    <span className="ml-auto muted whitespace-nowrap">{f.variants[0]?.size_gb ? `${f.variants[0].size_gb.toFixed(1)} GB` : ""}</span>
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
          <thead className="text-xs muted text-left"><tr><th className="py-1">File</th><th>Folder</th><th>Detected</th><th>Precision</th><th className="text-right">Size</th></tr></thead>
          <tbody>
            {files.map((f) => (
              <tr key={`${f.root}/${f.folder}/${f.name}`} className="border-t line">
                <td className="py-1.5 pr-2 break-all">{f.name}</td>
                <td className="muted">{f.folder}</td>
                <td>{f.info ? `${f.info.kind}${f.info.arch ? ` · ${f.info.arch}` : ""}` : <span className="muted">unknown</span>}</td>
                <td className="muted">{f.info?.precision ?? ""}</td>
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
