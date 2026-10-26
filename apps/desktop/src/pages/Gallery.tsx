// Everything created: search, filter, view, compare, reuse.

import { useEffect, useMemo, useState } from "react";
import { Copy, Heart, ImageUp, RotateCcw, Trash2 } from "lucide-react";
import { api } from "../api";
import { assetUrls } from "../components/inputs";
import { Badge, Button, Empty, Modal } from "../components/ui";
import { inputsFor, opLabel } from "../lib/form";
import { useStore } from "../store";
import type { Output } from "../types";
import { Media } from "./Create";

export function Gallery() {
  const { outputs, models, selectedOutput, selectOutput, refreshOutputs } = useStore();
  const [q, setQ] = useState("");
  const [model, setModel] = useState("");
  const [media, setMedia] = useState("");
  const [fav, setFav] = useState(false);
  const [compare, setCompare] = useState<string[]>([]);
  const [results, setResults] = useState<Output[] | null>(null);
  useEffect(() => {
    if (!q && !model && !media && !fav) {
      setResults(null);
      return;
    }
    const t = setTimeout(() => api.outputs({ search: q, model, media, favorites: fav, limit: 500 }).then(setResults), 200);
    return () => clearTimeout(t);
  }, [q, model, media, fav, outputs.length]);
  const list = results ?? outputs;
  const usedModels = useMemo(() => [...new Set(outputs.map((o) => o.model))], [outputs]);
  const selected = list.find((o) => o.id === selectedOutput) ?? outputs.find((o) => o.id === selectedOutput);
  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b line p-4">
        <input placeholder="Search prompts…" value={q} onChange={(e) => setQ(e.target.value)} className="max-w-xs" aria-label="Search" />
        <select value={model} onChange={(e) => setModel(e.target.value)} className="max-w-[220px]" aria-label="Model">
          <option value="">All models</option>
          {usedModels.map((m) => <option key={m} value={m}>{models.find((x) => x.id === m)?.display_name ?? m}</option>)}
        </select>
        <select value={media} onChange={(e) => setMedia(e.target.value)} className="max-w-[140px]" aria-label="Type">
          <option value="">Images & videos</option>
          <option value="image">Images</option>
          <option value="video">Videos</option>
          <option value="audio">Audio</option>
        </select>
        <Button variant={fav ? "primary" : "default"} onClick={() => setFav(!fav)}><Heart size={14} /> Favourites</Button>
        <div className="ml-auto flex items-center gap-2 text-xs muted">
          {compare.length ? <span>{compare.length}/2 selected to compare</span> : <span>{list.length} items</span>}
          {compare.length === 2 ? null : compare.length ? <Button size="sm" variant="ghost" onClick={() => setCompare([])}>Clear</Button> : null}
        </div>
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {list.length === 0 ? (
          <Empty title="Nothing here yet">Create something and it shows up here, with every setting saved next to the file.</Empty>
        ) : (
          <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(180px,1fr))]" data-testid="gallery">
            {list.map((o) => (
              <div key={o.id} className={`group relative aspect-square overflow-hidden rounded-xl border cursor-pointer ${compare.includes(o.id) ? "border-accent-500" : "line"}`} onClick={(e) => (e.shiftKey ? setCompare((c) => (c.includes(o.id) ? c.filter((x) => x !== o.id) : [...c, o.id].slice(-2))) : selectOutput(o.id))} data-output={o.id}>
                <Media o={o} />
                <div className="pointer-events-none absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent p-2 opacity-0 transition-opacity group-hover:opacity-100">
                  <p className="text-xs text-white line-clamp-2">{o.prompt || opLabel(o.op)}</p>
                </div>
                {o.favorite ? <Heart size={14} className="absolute right-2 top-2 fill-rose-500 text-rose-500" /> : null}
                {o.media === "video" ? <span className="absolute left-2 top-2"><Badge color="accent">video</Badge></span> : null}
              </div>
            ))}
          </div>
        )}
        <p className="mt-3 text-xs muted">Tip: Shift+click two items to compare them.</p>
      </div>
      {selected ? <Viewer o={selected} onClose={() => selectOutput(null)} onChanged={refreshOutputs} /> : null}
      {compare.length === 2 ? <Compare a={list.find((o) => o.id === compare[0])!} b={list.find((o) => o.id === compare[1])!} onClose={() => setCompare([])} /> : null}
    </div>
  );
}

function Viewer({ o, onClose, onChanged }: { o: Output; onClose: () => void; onChanged: () => void }) {
  const [sidecar, setSidecar] = useState<any>(null);
  const { selectModel, setValues, go, toast, details } = useStore();
  useEffect(() => {
    api.output(o.id).then((r) => setSidecar(r.sidecar)).catch(() => setSidecar(null));
  }, [o.id]);
  const reuse = async () => {
    await selectModel(o.model, o.op);
    if (sidecar?.values) setValues(sidecar.values);
    go("create");
    onClose();
  };
  const useAsInput = async () => {
    const current = useStore.getState();
    const m = current.modelId ? details[current.modelId] : null;
    if (!m || !current.op) return;
    const target = inputsFor(m, current.op).find((i) => ["image", "first_frame", "control_image"].includes(i.kind)) ?? (m.ops.includes("img2img") ? inputsFor(m, "img2img").find((i) => i.kind === "image") : undefined);
    if (!target) {
      toast("info", "The current model does not take an input image. Pick an edit or image-to-image model first.");
      return;
    }
    const a = await api.outputAsAsset(o.id);
    assetUrls.set(a.id, a.url);
    if (!inputsFor(m, current.op).includes(target) && m.ops.includes("img2img")) current.setOp("img2img");
    current.setValue(target.id, a.id);
    go("create");
    onClose();
  };
  return (
    <Modal title={o.prompt ? o.prompt.slice(0, 80) : opLabel(o.op)} onClose={onClose} wide>
      <div className="grid gap-5 lg:grid-cols-[1fr_320px]">
        <div className="flex max-h-[70vh] items-center justify-center rounded-xl checker overflow-hidden">
          <Media o={o} big />
        </div>
        <div className="space-y-3 text-sm">
          <div className="flex flex-wrap gap-2">
            <Button onClick={async () => { await api.favorite(o.id, !o.favorite); onChanged(); }}><Heart size={14} className={o.favorite ? "fill-rose-500 text-rose-500" : ""} /> {o.favorite ? "Unfavourite" : "Favourite"}</Button>
            <Button onClick={reuse}><RotateCcw size={14} /> Reuse settings</Button>
            {o.media === "image" ? <Button onClick={useAsInput}><ImageUp size={14} /> Use as input</Button> : null}
            <Button onClick={() => { navigator.clipboard?.writeText(o.prompt); toast("success", "Prompt copied"); }}><Copy size={14} /> Copy prompt</Button>
            <Button variant="danger" onClick={async () => { await api.deleteOutput(o.id); onChanged(); onClose(); }}><Trash2 size={14} /> Delete</Button>
          </div>
          <dl className="grid grid-cols-[110px_1fr] gap-x-2 gap-y-1 text-xs">
            <dt className="muted">Model</dt><dd>{sidecar?.model_name ?? o.model}</dd>
            <dt className="muted">Operation</dt><dd>{opLabel(o.op)}</dd>
            <dt className="muted">Size</dt><dd>{o.width} × {o.height}</dd>
            <dt className="muted">Seed</dt><dd className="tabular-nums">{o.seed ?? "-"}</dd>
            <dt className="muted">Created</dt><dd>{new Date(o.created_at).toLocaleString()}</dd>
            {sidecar?.plan ? (<><dt className="muted">Ran as</dt><dd>{sidecar.plan.fit}{sidecar.plan.precision ? `, ${sidecar.plan.precision}` : ""}</dd></>) : null}
            <dt className="muted">File</dt><dd className="break-all font-mono">{o.path}</dd>
          </dl>
          {sidecar?.values ? (
            <details>
              <summary className="cursor-pointer text-xs muted">All settings</summary>
              <pre className="mt-2 max-h-72 overflow-auto rounded-lg panel-2 p-2 text-[11px]">{JSON.stringify(sidecar.values, null, 2)}</pre>
            </details>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}

function Compare({ a, b, onClose }: { a: Output; b: Output; onClose: () => void }) {
  const [x, setX] = useState(50);
  return (
    <Modal title="Compare" onClose={onClose} wide>
      <div className="relative mx-auto aspect-square max-h-[70vh] overflow-hidden rounded-xl checker select-none" data-testid="compare">
        <div className="absolute inset-0 flex items-center justify-center"><Media o={b} big /></div>
        <div className="absolute inset-0 flex items-center justify-center" style={{ clipPath: `inset(0 ${100 - x}% 0 0)` }}><Media o={a} big /></div>
        <div className="absolute inset-y-0 w-0.5 bg-white/80" style={{ left: `${x}%` }} />
      </div>
      <input type="range" min={0} max={100} value={x} onChange={(e) => setX(Number(e.target.value))} className="mt-3 w-full" aria-label="Compare slider" />
      <div className="mt-2 grid grid-cols-2 gap-4 text-xs muted">
        <p>Left: {a.prompt}</p>
        <p>Right: {b.prompt}</p>
      </div>
    </Modal>
  );
}
