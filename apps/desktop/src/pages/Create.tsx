// The main page: pick a model, fill its inputs, create. Studio canvas when the model takes images, masks or boxes.

import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, Cpu, Gauge, Sparkles, Wand2 } from "lucide-react";
import { api, fileUrl } from "../api";
import { Canvas, hasCanvas } from "../components/Canvas";
import { InputWidget } from "../components/inputs";
import { Badge, Bar, Button, Field, Modal, Switch, Tabs } from "../components/ui";
import { applyPreset, changedValues, formatSeconds, inputsFor, missing, opLabel, sortedOps } from "../lib/form";
import { currentModel, currentValues, useStore } from "../store";
import type { Level, ModelDetail, ModelSummary, Plan } from "../types";

export function Create() {
  const model = useStore(currentModel);
  const op = useStore((s) => s.op);
  if (!model || !op) return <div className="p-8 muted">Loading models…</div>;
  return <CreateFor model={model} op={op} />;
}

function CreateFor({ model, op }: { model: ModelDetail; op: string }) {
  const values = useStore(currentValues);
  const { setValue, setValues, setOp, level, setLevel, batch, setBatch, toast, resetValues } = useStore();
  const [view, setView] = useState<"result" | "canvas">(hasCanvas(model, op) ? "canvas" : "result");
  const [busy, setBusy] = useState(false);
  const [memory, setMemory] = useState<{ plan: string; precision: string; vae_tiled: string }>({ plan: "auto", precision: "", vae_tiled: "auto" });
  const [overrides, setOverrides] = useState("");
  useEffect(() => setView(hasCanvas(model, op) ? "canvas" : "result"), [model.id, op]); // eslint-disable-line react-hooks/exhaustive-deps

  const shown = inputsFor(model, op, level);
  const lacking = missing(model, op, values);
  const spec = useMemo(() => {
    let ov: Record<string, unknown> = {};
    try {
      ov = overrides.trim() ? JSON.parse(overrides) : {};
    } catch {
      /* shown as an error below */
    }
    return {
      model: model.id,
      op,
      values: changedValues(model, op, values),
      batch,
      memory: {
        plan: memory.plan === "auto" ? null : memory.plan,
        precision: memory.precision || null,
        vae_tiled: memory.vae_tiled === "auto" ? null : memory.vae_tiled === "on",
      },
      overrides: ov,
    };
  }, [model, op, values, batch, memory, overrides]);

  const submit = async () => {
    if (lacking.length) {
      toast("error", `Please fill in: ${lacking.join(", ")}`);
      return;
    }
    setBusy(true);
    try {
      await api.createJob(spec);
      setView("result");
    } catch (e) {
      toast("error", (e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  // Ctrl+Enter creates.
  const submitRef = useRef(submit);
  submitRef.current = submit;
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") submitRef.current();
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  return (
    <div className="flex h-full min-h-0">
      <aside className="w-[400px] shrink-0 border-r line flex flex-col min-h-0">
        <div className="p-4 space-y-3 border-b line">
          <ModelPicker current={model} />
          {model.ops.length > 1 ? (
            <div className="flex flex-wrap gap-1.5" role="tablist" aria-label="What to do">
              {sortedOps(model.ops, model.main_op).map((o) => (
                <button key={o} role="tab" aria-selected={o === op} onClick={() => setOp(o)} className={`rounded-lg border px-2.5 py-1 text-xs cursor-pointer ${o === op ? "border-accent-500 bg-accent-500/15 text-[var(--accent-text)]" : "line panel-2 muted hover:text-[var(--text)]"}`}>
                  {opLabel(o)}
                </button>
              ))}
            </div>
          ) : null}
        </div>
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {model.presets.length ? (
            <div className="flex flex-wrap gap-1.5 items-center">
              <span className="text-xs muted mr-1">Presets</span>
              {model.presets.map((p) => (
                <button key={p.name} title={p.description} onClick={() => setValues(applyPreset(values, p))} className="rounded-full border line panel-2 px-2.5 py-0.5 text-xs cursor-pointer hover:border-accent-500">
                  {p.name}
                </button>
              ))}
            </div>
          ) : null}
          {shown.map((i) => (
            <InputWidget key={i.id} spec={i} value={values[i.id]} onChange={(v) => setValue(i.id, v)} model={model} />
          ))}
          {level === "ultra" ? (
            <UltraPanel memory={memory} setMemory={setMemory} overrides={overrides} setOverrides={setOverrides} model={model} op={op} />
          ) : null}
          <button className="text-xs muted underline cursor-pointer" onClick={resetValues}>Reset to defaults</button>
        </div>
        <div className="border-t line p-4 space-y-3">
          <div className="flex items-center justify-between">
            <Tabs<Level> value={level} onChange={setLevel} items={[{ value: "simple", label: "Simple" }, { value: "advanced", label: "Advanced" }, { value: "ultra", label: "Ultra" }]} />
            <label className="flex items-center gap-2 text-xs muted" title="Run several times with consecutive seeds">
              Runs
              <input type="number" min={1} max={64} value={batch} onChange={(e) => setBatch(Math.max(1, Math.min(64, Number(e.target.value))))} className="w-16 py-1" aria-label="Runs" />
            </label>
          </div>
          <PlanCard spec={spec} model={model} />
          <Button variant="primary" size="lg" className="w-full" onClick={submit} disabled={busy || Boolean(model.unavailable)} data-testid="create">
            <Sparkles size={18} /> {model.media === "video" ? "Create video" : "Create"}
            <span className="ml-1 text-xs font-normal opacity-70">Ctrl+Enter</span>
          </Button>
          {model.unavailable ? <p className="text-xs text-red-400">{model.unavailable}</p> : lacking.length ? <p className="text-xs muted">Needs: {lacking.join(", ")}</p> : null}
        </div>
      </aside>
      <section className="flex-1 min-w-0 flex flex-col p-4 gap-3">
        <div className="flex items-center gap-2">
          {hasCanvas(model, op) ? <Tabs value={view} onChange={setView} items={[{ value: "canvas", label: "Studio canvas" }, { value: "result", label: "Results" }]} /> : <h2 className="font-semibold">Results</h2>}
          <div className="ml-auto text-xs muted truncate">{model.description}</div>
        </div>
        <div className="flex-1 min-h-0">{view === "canvas" ? <Canvas key={`${model.id}:${op}`} model={model} op={op} values={values} setValue={setValue} /> : <Results model={model} />}</div>
      </section>
    </div>
  );
}

function ModelPicker({ current }: { current: ModelDetail }) {
  const models = useStore((s) => s.models);
  const selectModel = useStore((s) => s.selectModel);
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const groups = useMemo(() => {
    const f = (m: ModelSummary) => !q || `${m.display_name} ${m.tags.join(" ")} ${m.family}`.toLowerCase().includes(q.toLowerCase());
    return [
      { title: "Models", items: models.filter((m) => m.origin === "builtin" && f(m)) },
      { title: "Your tools", items: models.filter((m) => (m.origin === "tool" || m.origin === "user") && !m.id.startsWith("bp-") && f(m)) },
      { title: "Official ComfyUI workflows", items: models.filter((m) => m.id.startsWith("bp-") && f(m)) },
    ].filter((g) => g.items.length);
  }, [models, q]);
  return (
    <>
      <button onClick={() => setOpen(true)} className="w-full panel-2 border line rounded-xl p-3 text-left cursor-pointer hover:border-accent-500" data-testid="model-picker">
        <div className="flex items-center gap-2">
          <Wand2 size={16} className="text-accent-400" />
          <span className="font-semibold truncate">{current.display_name}</span>
          <ChevronDown size={16} className="ml-auto muted" />
        </div>
        <div className="mt-1.5 flex flex-wrap gap-1">
          <FitBadge m={current} />
          {!current.installed ? <Badge color="gray">needs download</Badge> : null}
          <Badge color="gray">{current.media}</Badge>
        </div>
      </button>
      {open ? (
        <Modal title="Choose a model or tool" onClose={() => setOpen(false)} wide>
          <input autoFocus placeholder="Search models and tools…" value={q} onChange={(e) => setQ(e.target.value)} className="mb-4" />
          {groups.map((g) => (
            <div key={g.title} className="mb-5">
              <h3 className="text-xs uppercase tracking-wide muted mb-2">{g.title} · {g.items.length}</h3>
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-2">
                {g.items.map((m) => (
                  <button key={m.id} data-model={m.id} onClick={() => { selectModel(m.id); setOpen(false); }} className={`panel-2 border rounded-xl p-3 text-left cursor-pointer hover:border-accent-500 ${m.id === current.id ? "border-accent-500" : "line"}`}>
                    <div className="flex items-center gap-2">
                      <span className="font-medium truncate">{m.display_name}</span>
                      <span className="ml-auto"><FitBadge m={m} /></span>
                    </div>
                    <p className="mt-1 text-xs muted line-clamp-2">{m.description || m.tags.join(" · ")}</p>
                    <div className="mt-2 flex flex-wrap gap-1">
                      {m.unavailable ? <Badge color="red" title={m.unavailable}>unavailable</Badge> : m.installed ? <Badge color="green">ready</Badge> : <Badge color="gray">needs download</Badge>}
                      {sortedOps(m.ops, m.main_op).map((o) => <Badge key={o} color="gray">{opLabel(o)}</Badge>)}
                    </div>
                  </button>
                ))}
              </div>
            </div>
          ))}
        </Modal>
      ) : null}
    </>
  );
}

export function FitBadge({ m }: { m: ModelSummary }) {
  if (!m.fit) return null;
  const label = m.fit.sizes_known === false
    ? "size unknown"
    : { resident: "fits your GPU", offload: "uses RAM too", stream: "streams from disk", too_big: "too big" }[m.fit.fit];
  return <Badge color={m.fit.badge} title={[...m.fit.explain, ...m.fit.warnings].join("\n")}><Gauge size={11} /> {label}</Badge>;
}

/** Live "how will this run" estimate, refreshed as settings change. */
function PlanCard({ spec, model }: { spec: { model: string; op: string; values: Record<string, unknown> } & Record<string, unknown>; model: ModelDetail }) {
  const [plan, setPlan] = useState<Plan | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const key = JSON.stringify(spec);
  useEffect(() => {
    const t = setTimeout(() => {
      api.plan(spec as never).then((r) => {
        if (r.ok && r.plan) {
          setPlan(r.plan);
          setErr(null);
        } else setErr(r.error ?? null);
      }).catch(() => {});
    }, 250);
    return () => clearTimeout(t);
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!plan) return err ? <p className="text-xs muted">{err}</p> : null;
  const unknown = plan.sizes_known === false;
  const badge = (unknown ? "gray" : { resident: "green", offload: "yellow", stream: "red", too_big: "red" }[plan.fit]) as "green";
  return (
    <div className="rounded-lg panel-2 border line p-2.5 text-xs" data-testid="plan">
      <button className="flex w-full items-center gap-2 cursor-pointer" onClick={() => setOpen(!open)}>
        <Cpu size={14} className="muted" />
        <Badge color={badge}>{unknown ? "Size unknown" : { resident: "Runs on GPU", offload: "GPU + RAM", stream: "Streams from disk", too_big: "Too big" }[plan.fit]}</Badge>
        {plan.precision ? <Badge color="gray">{plan.precision}</Badge> : null}
        {plan.estimated_seconds ? <span className="muted">≈ {formatSeconds(plan.estimated_seconds)}</span> : null}
        <span className="ml-auto muted">{open ? "Hide" : "Why?"}</span>
      </button>
      {open ? (
        <ul className="mt-2 space-y-1 list-disc pl-4">
          {plan.explain.map((x, i) => <li key={i}>{x}</li>)}
          {plan.warnings.map((x, i) => <li key={`w${i}`} className="text-amber-400">{x}</li>)}
          {plan.launch_flags.length ? <li className="font-mono muted">{plan.launch_flags.join(" ")}</li> : null}
        </ul>
      ) : plan.warnings.length ? <p className="mt-1.5 text-amber-400">{plan.warnings[0]}</p> : null}
      {err ? <p className="mt-1 text-red-400">{err}</p> : null}
      {model.unavailable ? null : null}
    </div>
  );
}

function UltraPanel({ memory, setMemory, overrides, setOverrides, model, op }: {
  memory: { plan: string; precision: string; vae_tiled: string };
  setMemory: (m: { plan: string; precision: string; vae_tiled: string }) => void;
  overrides: string;
  setOverrides: (s: string) => void;
  model: ModelDetail;
  op: string;
}) {
  let bad = false;
  try {
    if (overrides.trim()) JSON.parse(overrides);
  } catch {
    bad = true;
  }
  const precisions = [...new Set(model.files.flatMap((f) => f.variants.map((v) => v.precision)))];
  return (
    <div className="rounded-xl border border-accent-500/30 p-3 space-y-3" data-testid="ultra">
      <h3 className="text-xs uppercase tracking-wide text-[var(--accent-text)]">Engine & memory</h3>
      <Field label="Memory plan" help="Auto picks the fastest plan that fits. Force one to compare.">
        <select value={memory.plan} onChange={(e) => setMemory({ ...memory, plan: e.target.value })}>
          <option value="auto">Auto</option>
          <option value="resident">Keep everything on the GPU</option>
          <option value="offload">Stream weights from RAM</option>
          <option value="stream">Stream weights from disk</option>
        </select>
      </Field>
      {precisions.length > 1 ? (
        <Field label="Weights">
          <select value={memory.precision} onChange={(e) => setMemory({ ...memory, precision: e.target.value })}>
            <option value="">Auto</option>
            {precisions.map((p) => <option key={p} value={p}>{p}</option>)}
          </select>
        </Field>
      ) : null}
      <Field label="Tiled decode" help="Decodes in tiles to save memory on large images and videos.">
        <select value={memory.vae_tiled} onChange={(e) => setMemory({ ...memory, vae_tiled: e.target.value })}>
          <option value="auto">Auto</option>
          <option value="on">On</option>
          <option value="off">Off</option>
        </select>
      </Field>
      <Field label="Node overrides (JSON)" help='Set any node input directly: {"3.denoise": 0.8}. Node ids are shown in Tools > Graph.'>
        <textarea rows={3} className={`font-mono text-xs ${bad ? "border-red-500" : ""}`} value={overrides} onChange={(e) => setOverrides(e.target.value)} placeholder="{}" />
      </Field>
      <p className="text-[11px] muted">Operation: {op} · Model id: {model.id}</p>
    </div>
  );
}

function Results({ model }: { model: ModelDetail }) {
  const jobs = useStore((s) => s.jobs);
  const progress = useStore((s) => s.progress);
  const preview = useStore((s) => s.preview);
  const outputs = useStore((s) => s.outputs);
  const selectOutput = useStore((s) => s.selectOutput);
  const go = useStore((s) => s.go);
  const running = jobs.find((j) => j.status === "running");
  const queued = jobs.filter((j) => j.status === "queued").length;
  const mine = outputs.filter((o) => o.model === model.id);
  const latest = mine[0];
  const p = running ? progress[running.id] : undefined;
  const pv = running ? preview[running.id] : undefined;
  return (
    <div className="flex h-full flex-col gap-3">
      <div className="relative flex-1 min-h-0 rounded-xl checker flex items-center justify-center overflow-hidden" data-testid="result">
        {running && pv ? (
          <img src={pv} alt="Live preview" className="max-h-full max-w-full object-contain [image-rendering:auto] opacity-90" />
        ) : latest ? (
          <Media o={latest} big />
        ) : (
          <p className="muted">Your results appear here.</p>
        )}
        {running ? (
          <div className="absolute inset-x-4 bottom-4 panel p-3 space-y-1.5" data-testid="progress">
            <div className="flex items-center justify-between text-xs">
              <span>{running.spec.model === model.id ? "Creating…" : `Running ${running.spec.model}`} {p?.node && !/^\d+$/.test(p.node) ? `· ${p.node}` : ""}</span>
              <span className="muted tabular-nums">
                {p && p.total ? `step ${p.step}/${p.total}` : ""} {p?.eta ? `· ${formatSeconds(p.eta)} left` : ""} {queued ? `· ${queued} queued` : ""}
              </span>
            </div>
            <Bar value={p?.overall ?? 0.02} />
          </div>
        ) : null}
      </div>
      {mine.length ? (
        <div className="flex gap-2 overflow-x-auto pb-1" data-testid="recent">
          {mine.slice(0, 24).map((o) => (
            <button key={o.id} onClick={() => { selectOutput(o.id); go("gallery"); }} className="h-20 w-20 shrink-0 overflow-hidden rounded-lg border line cursor-pointer hover:border-accent-500">
              <Media o={o} />
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

export function Media({ o, big }: { o: { media: string; path: string; thumb: string | null; prompt?: string }; big?: boolean }) {
  if (o.media === "video" && !o.path.endsWith(".gif")) {
    return <video src={fileUrl(o.path)} className={big ? "max-h-full max-w-full" : "h-full w-full object-cover"} controls={big} autoPlay={big} loop muted playsInline preload="metadata" />;
  }
  if (o.media === "audio") return <audio src={fileUrl(o.path)} controls className="w-full" />;
  const src = big || !o.thumb ? fileUrl(o.path) : fileUrl(o.thumb);
  return <img src={src} alt={o.prompt ?? ""} loading="lazy" className={big ? "max-h-full max-w-full object-contain" : "h-full w-full object-cover"} />;
}

export { Switch };
