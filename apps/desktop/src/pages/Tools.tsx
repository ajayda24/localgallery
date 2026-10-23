// Bring any ComfyUI workflow in, choose what to expose, and use it like a built-in model.

import { useMemo, useRef, useState } from "react";
import { Background, Controls, MiniMap, ReactFlow, type Edge, type Node } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { FileJson, Upload } from "lucide-react";
import { api, type Candidate, type ImportResult } from "../api";
import { Badge, Button, Empty, Field, Tabs } from "../components/ui";
import { useStore } from "../store";

const KINDS = ["text", "negative_text", "seed", "int", "float", "bool", "enum", "image", "mask", "video", "audio", "model_file", "raw", "regions", "structured_prompt"];

export function Tools() {
  const [text, setText] = useState("");
  const [res, setRes] = useState<ImportResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<"fields" | "graph">("fields");
  const { toast, selectModel, go } = useStore();
  const file = useRef<HTMLInputElement>(null);
  const run = async (src: string) => {
    setBusy(true);
    try {
      const wf = JSON.parse(src);
      setRes(await api.importWorkflow(wf));
    } catch (e) {
      toast("error", (e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="h-full overflow-y-auto p-6 space-y-4">
      <div className="panel p-4 space-y-3">
        <h2 className="font-semibold">Import a ComfyUI workflow</h2>
        <p className="text-sm muted">Paste or open any workflow (the normal editor format, an API export, or an official blueprint). LocalGallery converts it, checks every node against your engine, and lets you pick which settings to show. New models work as soon as ComfyUI supports them.</p>
        <textarea rows={5} className="font-mono text-xs" placeholder='{"nodes": [...], "links": [...]}' value={text} onChange={(e) => setText(e.target.value)} data-testid="workflow-json" />
        <div className="flex gap-2">
          <Button variant="primary" disabled={!text.trim() || busy} onClick={() => run(text)} data-testid="import"><FileJson size={15} /> Import</Button>
          <Button onClick={() => file.current?.click()}><Upload size={15} /> Open file…</Button>
          <input ref={file} type="file" accept=".json,application/json" className="hidden" onChange={async (e) => { const f = e.target.files?.[0]; if (f) { const t = await f.text(); setText(t); run(t); } }} />
        </div>
      </div>
      {res ? (
        <div className="panel p-4 space-y-4">
          <div className="flex items-center gap-3">
            <h2 className="font-semibold">{res.title ?? "Imported workflow"}</h2>
            <Badge>{Object.keys(res.graph).length} nodes</Badge>
            {res.issues.length ? <Badge color="yellow">{res.issues.length} notes</Badge> : <Badge color="green">all nodes available</Badge>}
            <div className="ml-auto"><Tabs value={tab} onChange={setTab} items={[{ value: "fields", label: "Settings to show" }, { value: "graph", label: "Graph" }]} /></div>
          </div>
          {res.issues.length ? <ul className="text-xs text-amber-400 list-disc pl-5">{res.issues.slice(0, 8).map((i, k) => <li key={k}>{i}</li>)}</ul> : null}
          {res.models.length ? <p className="text-xs muted">Uses: {res.models.map((m) => m.name).join(", ")}</p> : null}
          {tab === "graph" ? <GraphView graph={res.graph} /> : res.suggested_tool ? (
            <Suggested res={res} onDone={async (id) => { await selectModel(id); go("create"); }} />
          ) : (
            <Picker res={res} onDone={async (id) => { await selectModel(id); go("create"); }} />
          )}
        </div>
      ) : null}
    </div>
  );
}

function Suggested({ res, onDone }: { res: ImportResult; onDone: (id: string) => void }) {
  const tool = res.suggested_tool as { display_name: string; inputs: { id: string; label: string; kind: string }[] };
  const [name, setName] = useState(tool.display_name);
  const toast = useStore((s) => s.toast);
  return (
    <div className="space-y-3">
      <p className="text-sm">This workflow already says which settings matter. It becomes a tool with:</p>
      <div className="flex flex-wrap gap-1.5">{tool.inputs.map((i) => <Badge key={i.id} color="accent">{i.label} · {i.kind}</Badge>)}</div>
      <Field label="Tool name"><input value={name} onChange={(e) => setName(e.target.value)} /></Field>
      <Button variant="primary" data-testid="publish" onClick={async () => {
        try {
          const r = await api.publishTool({ manifest: { ...tool, display_name: name, id: `tool-${name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "")}` } });
          toast("success", "Tool saved");
          onDone(r.id);
        } catch (e) {
          toast("error", (e as Error).message);
        }
      }}>Save as tool</Button>
    </div>
  );
}

function Picker({ res, onDone }: { res: ImportResult; onDone: (id: string) => void }) {
  const [name, setName] = useState(res.title ?? "My tool");
  const [chosen, setChosen] = useState<Record<string, { on: boolean; label: string; kind: string; level: string }>>(() => {
    const m: Record<string, { on: boolean; label: string; kind: string; level: string }> = {};
    for (const c of res.candidates) m[`${c.node}.${c.input}`] = { on: c.suggested, label: c.label, kind: c.kind, level: c.suggested ? "simple" : "advanced" };
    return m;
  });
  const toast = useStore((s) => s.toast);
  const byNode = useMemo(() => {
    const g = new Map<string, Candidate[]>();
    for (const c of res.candidates) g.set(c.node, [...(g.get(c.node) ?? []), c]);
    return [...g.entries()];
  }, [res]);
  const publish = async () => {
    const fields = res.candidates
      .filter((c) => chosen[`${c.node}.${c.input}`]?.on)
      .map((c, i) => {
        const s = chosen[`${c.node}.${c.input}`];
        const id = `${s.label.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_|_$/g, "") || "field"}_${i}`;
        return { id, label: s.label, kind: s.kind, level: s.level, targets: [{ node: c.node, input: c.input }] };
      });
    try {
      const r = await api.publishTool({ graph: res.graph, fields, meta: { name } });
      toast("success", "Tool saved");
      onDone(r.id);
    } catch (e) {
      toast("error", (e as Error).message);
    }
  };
  return (
    <div className="space-y-3">
      <Field label="Tool name"><input value={name} onChange={(e) => setName(e.target.value)} /></Field>
      <div className="grid gap-3 md:grid-cols-2">
        {byNode.map(([node, cs]) => (
          <div key={node} className="rounded-lg border line p-3 space-y-2">
            <div className="text-xs muted">#{node} · {cs[0].title}</div>
            {cs.map((c) => {
              const k = `${c.node}.${c.input}`;
              const s = chosen[k];
              return (
                <div key={k} className="flex items-center gap-2">
                  <input type="checkbox" checked={s.on} onChange={(e) => setChosen({ ...chosen, [k]: { ...s, on: e.target.checked } })} aria-label={`Show ${c.input}`} />
                  <input className="flex-1 py-1 text-xs" value={s.label} onChange={(e) => setChosen({ ...chosen, [k]: { ...s, label: e.target.value } })} />
                  <select className="w-28 py-1 text-xs" value={s.kind} onChange={(e) => setChosen({ ...chosen, [k]: { ...s, kind: e.target.value } })}>{KINDS.map((x) => <option key={x}>{x}</option>)}</select>
                  <select className="w-24 py-1 text-xs" value={s.level} onChange={(e) => setChosen({ ...chosen, [k]: { ...s, level: e.target.value } })}><option>simple</option><option>advanced</option><option>ultra</option></select>
                </div>
              );
            })}
          </div>
        ))}
      </div>
      <Button variant="primary" onClick={publish} data-testid="publish">Save as tool</Button>
    </div>
  );
}

/** Read-only node graph with a simple layered layout. */
export function GraphView({ graph }: { graph: ImportResult["graph"] }) {
  const { nodes, edges } = useMemo(() => {
    const ids = Object.keys(graph);
    const depth: Record<string, number> = {};
    const d = (id: string, seen = new Set<string>()): number => {
      if (depth[id] !== undefined) return depth[id];
      if (seen.has(id)) return 0;
      seen.add(id);
      let m = 0;
      for (const v of Object.values(graph[id]?.inputs ?? {})) if (Array.isArray(v) && typeof v[0] === "string" && graph[v[0]]) m = Math.max(m, d(v[0], seen) + 1);
      return (depth[id] = m);
    };
    ids.forEach((id) => d(id));
    const rows: Record<number, number> = {};
    const nodes: Node[] = ids.map((id) => {
      const col = depth[id];
      const row = (rows[col] = (rows[col] ?? -1) + 1);
      const n = graph[id];
      const widgets = Object.entries(n.inputs).filter(([, v]) => !(Array.isArray(v) && typeof v[0] === "string")).slice(0, 4);
      return {
        id,
        position: { x: col * 260, y: row * 140 },
        data: { label: (<div className="text-left"><div className="font-semibold text-[11px]">{n._meta?.title ?? n.class_type}</div><div className="text-[10px] opacity-60">#{id} {n.class_type}</div>{widgets.map(([k, v]) => <div key={k} className="text-[10px] truncate opacity-80">{k}: {JSON.stringify(v).slice(0, 28)}</div>)}</div>) },
        style: { width: 220, fontSize: 11, background: "var(--panel-2)", color: "var(--text)", border: "1px solid var(--line)", borderRadius: 10 },
      };
    });
    const edges: Edge[] = [];
    for (const id of ids) for (const [k, v] of Object.entries(graph[id].inputs)) if (Array.isArray(v) && typeof v[0] === "string" && graph[v[0]]) edges.push({ id: `${v[0]}-${id}-${k}`, source: v[0], target: id, label: k, style: { stroke: "#7c5cff" }, labelStyle: { fontSize: 9, fill: "#9aa1b2" }, labelBgStyle: { fill: "transparent" } });
    return { nodes, edges };
  }, [graph]);
  if (!nodes.length) return <Empty title="Empty graph" />;
  return (
    <div className="h-[520px] rounded-xl border line overflow-hidden" data-testid="graph">
      <ReactFlow nodes={nodes} edges={edges} fitView nodesDraggable nodesConnectable={false} proOptions={{ hideAttribution: true }} colorMode="dark">
        <Background />
        <MiniMap pannable zoomable />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}
