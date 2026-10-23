// Engine, performance, folders, downloads, interface, hardware and benchmarks.

import { useEffect, useState } from "react";
import { FolderPlus, Play, Square, Trash2 } from "lucide-react";
import { api } from "../api";
import { Badge, Button, Field, Switch } from "../components/ui";
import { formatSeconds } from "../lib/form";
import { useStore } from "../store";
import type { Benchmark, Settings as S } from "../types";

export function Settings() {
  const { settings, system, toast, logs } = useStore();
  const [draft, setDraft] = useState<S | null>(settings);
  const [token, setToken] = useState("");
  const [dir, setDir] = useState("");
  const [bench, setBench] = useState<Benchmark[]>([]);
  const [engineLog, setEngineLog] = useState<string[]>([]);
  useEffect(() => setDraft(settings), [settings]);
  useEffect(() => {
    api.benchmarks().then(setBench).catch(() => {});
    api.logs().then(setEngineLog).catch(() => {});
  }, []);
  if (!draft || !system) return null;
  const save = async (patch: Partial<S> & { clear_hf_token?: boolean }) => {
    try {
      const s = await api.saveSettings(patch);
      useStore.setState({ settings: s });
      toast("success", "Saved");
    } catch (e) {
      toast("error", (e as Error).message);
    }
  };
  const p = draft.planner;
  const e = system.engine;
  const gpu = system.hardware.gpus[0];
  return (
    <div className="h-full overflow-y-auto p-6">
      <div className="mx-auto max-w-4xl space-y-5">
        <Section title="Image engine" testid="engine">
          <div className="flex flex-wrap items-center gap-2">
            <Badge color={e.state === "ready" ? "green" : e.state === "error" ? "red" : e.state === "installing" || e.state === "starting" ? "accent" : "gray"}>{e.state.replace("_", " ")}</Badge>
            <span className="text-sm">{e.mode.kind === "managed" ? "Built-in ComfyUI" : e.mode.kind === "external" ? `Your ComfyUI at ${e.mode.url}` : "Test engine (no GPU work)"}</span>
            <span className="text-xs muted">{e.detail}</span>
          </div>
          {e.mode.kind === "managed" && e.state === "not_installed" ? (
            <div className="rounded-lg panel-2 p-3 space-y-2">
              <p className="text-sm">The engine (ComfyUI with PyTorch) is downloaded into this app folder. It needs about 6 GB of disk and a few minutes.</p>
              <div className="flex gap-2">
                <Button variant="primary" onClick={() => api.installEngine("cuda").catch((x) => toast("error", x.message))}>Install for NVIDIA GPU</Button>
                <Button onClick={() => api.installEngine("cpu").catch((x) => toast("error", x.message))}>Install CPU-only</Button>
              </div>
            </div>
          ) : null}
          {e.state === "installing" ? <Button onClick={() => api.cancelInstall()}>Cancel install</Button> : null}
          <div className="flex gap-2">
            <Button onClick={() => api.startEngine().catch((x) => toast("error", x.message))} disabled={e.state === "not_installed" || e.state === "installing"}><Play size={14} /> Start now</Button>
            <Button onClick={() => api.stopEngine()}><Square size={14} /> Stop</Button>
          </div>
          {e.flags.length ? <p className="font-mono text-xs muted break-all">{e.flags.join(" ")}</p> : null}
          <details>
            <summary className="cursor-pointer text-xs muted">Engine log</summary>
            <pre className="mt-2 max-h-64 overflow-auto rounded-lg panel-2 p-2 text-[11px] whitespace-pre-wrap">{[...engineLog, ...logs].slice(-300).join("\n") || "No output yet."}</pre>
          </details>
        </Section>

        <Section title="Performance">
          <Field label="Keep free on the GPU" help="Memory left for Windows, the browser and games. Lower = faster, but other apps may stutter." right={<span className="text-xs tabular-nums muted">{p.vram_headroom_gb.toFixed(1)} GB</span>}>
            <input type="range" min={0.3} max={4} step={0.1} value={p.vram_headroom_gb} onChange={(x) => setDraft({ ...draft, planner: { ...p, vram_headroom_gb: Number(x.target.value) } })} />
          </Field>
          <Field label="Keep free in RAM" right={<span className="text-xs tabular-nums muted">{p.ram_reserve_gb.toFixed(0)} GB</span>}>
            <input type="range" min={2} max={16} step={1} value={p.ram_reserve_gb} onChange={(x) => setDraft({ ...draft, planner: { ...p, ram_reserve_gb: Number(x.target.value) } })} />
          </Field>
          <Field label="Priority" help="Balanced: best quality that runs at full speed. Speed: smallest fast format your GPU supports (fp8 / NVFP4 on RTX 50). Quality: highest precision that fits in RAM.">
            <select value={p.policy} onChange={(x) => setDraft({ ...draft, planner: { ...p, policy: x.target.value as S["planner"]["policy"] } })}>
              <option value="balanced">Balanced</option>
              <option value="speed">Speed</option>
              <option value="quality">Quality</option>
            </select>
          </Field>
          <Row label="Fast math" help="ComfyUI's --fast optimisations (fp16 accumulation, fp8 matrix math). A little faster; results can change slightly."><Switch checked={p.fast_math} onChange={(v) => setDraft({ ...draft, planner: { ...p, fast_math: v } })} /></Row>
          <Row label="Group queued jobs by model" help="Avoids reloading models between jobs."><Switch checked={draft.group_by_model} onChange={(v) => setDraft({ ...draft, group_by_model: v })} /></Row>
          <Row label="Live previews" help="Shows the picture forming while it is made."><Switch checked={draft.previews} onChange={(v) => setDraft({ ...draft, previews: v })} /></Row>
          <Button variant="primary" onClick={() => save({ planner: draft.planner, group_by_model: draft.group_by_model, previews: draft.previews })}>Save</Button>
        </Section>

        <Section title="Model folders">
          <p className="text-sm muted">Models live in <code>{system.root}/models</code>. Add folders you already have (for example an existing ComfyUI): files are used in place, never copied.</p>
          {draft.extra_model_dirs.map((d) => (
            <div key={d} className="flex items-center gap-2 text-sm">
              <code className="flex-1 truncate">{d}</code>
              <Button size="sm" variant="ghost" onClick={() => save({ extra_model_dirs: draft.extra_model_dirs.filter((x) => x !== d) })} aria-label="Remove folder"><Trash2 size={14} /></Button>
            </div>
          ))}
          <div className="flex gap-2">
            <input placeholder="D:\\ComfyUI or D:\\Models" value={dir} onChange={(x) => setDir(x.target.value)} />
            <Button onClick={() => { if (dir.trim()) { save({ extra_model_dirs: [...draft.extra_model_dirs, dir.trim()] }); setDir(""); } }}><FolderPlus size={14} /> Add</Button>
          </div>
        </Section>

        <Section title="Downloads">
          <Row label="Download missing files automatically" help="When a job needs a model you do not have yet."><Switch checked={draft.auto_download} onChange={(v) => save({ auto_download: v })} /></Row>
          <Field label="Hugging Face token" help={settings?.hf_token_set ? "A token is saved. It is only sent to huggingface.co." : "Only needed for gated models. Saved in this app folder, only sent to huggingface.co."}>
            <div className="flex gap-2">
              <input type="password" placeholder={settings?.hf_token_set ? "••••••••" : "hf_…"} value={token} onChange={(x) => setToken(x.target.value)} />
              <Button onClick={() => { save({ hf_token: token }); setToken(""); }} disabled={!token}>Save</Button>
              {settings?.hf_token_set ? <Button variant="ghost" onClick={() => save({ clear_hf_token: true })}>Remove</Button> : null}
            </div>
          </Field>
        </Section>

        <Section title="Interface">
          <Field label="Theme">
            <select value={draft.theme} onChange={(x) => save({ theme: x.target.value })}>
              <option value="system">Follow Windows</option>
              <option value="dark">Dark</option>
              <option value="light">Light</option>
            </select>
          </Field>
        </Section>

        <Section title="This PC">
          <dl className="grid grid-cols-[160px_1fr] gap-y-1 text-sm">
            <dt className="muted">GPU</dt><dd>{gpu ? `${gpu.name}, ${gpu.vram_total_gb.toFixed(1)} GB` : "None found"}{gpu?.compute_capability ? ` · sm_${gpu.compute_capability.join("")}` : ""}{gpu?.cuda_driver_version ? ` · CUDA ${gpu.cuda_driver_version}` : ""}</dd>
            <dt className="muted">Memory</dt><dd>{system.hardware.ram_total_gb.toFixed(0)} GB RAM</dd>
            <dt className="muted">Processor</dt><dd>{system.hardware.cpu} ({system.hardware.cpu_threads} threads)</dd>
            <dt className="muted">Disk</dt><dd>{system.hardware.disk ? `${system.hardware.disk.free_gb.toFixed(0)} GB free of ${system.hardware.disk.total_gb.toFixed(0)} GB (${system.hardware.disk.kind})` : "-"}</dd>
            <dt className="muted">NPU</dt><dd>{system.hardware.npu}</dd>
            <dt className="muted">System</dt><dd>{system.hardware.os}{system.hardware.laptop ? " · laptop" : ""}</dd>
            <dt className="muted">App folder</dt><dd className="font-mono text-xs break-all">{system.root}</dd>
            <dt className="muted">Version</dt><dd>{system.version}</dd>
          </dl>
        </Section>

        <Section title="Benchmarks" testid="benchmarks">
          <p className="text-xs muted">Every run is measured. The planner uses these numbers for time estimates and to avoid memory spills.</p>
          {bench.length ? (
            <table className="w-full text-xs">
              <thead className="muted text-left"><tr><th>Model</th><th>Op</th><th>Size</th><th>Steps</th><th>Plan</th><th className="text-right">Time</th><th className="text-right">Peak VRAM</th><th className="text-right">Spill</th></tr></thead>
              <tbody>
                {bench.slice(0, 60).map((b, i) => (
                  <tr key={i} className="border-t line">
                    <td className="py-1">{b.model}</td><td>{b.op}</td><td className="tabular-nums">{b.width}×{b.height}{b.frames > 1 ? `×${b.frames}f` : ""}</td><td>{b.steps}</td><td>{b.plan}{b.precision ? ` · ${b.precision}` : ""}</td>
                    <td className="text-right tabular-nums">{formatSeconds(b.seconds)}</td><td className="text-right tabular-nums">{b.peak_vram_gb ? `${b.peak_vram_gb.toFixed(1)} GB` : "-"}</td><td className="text-right tabular-nums">{b.shared_gpu_gb ? `${b.shared_gpu_gb.toFixed(1)} GB` : "-"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : <p className="text-sm muted">No runs yet.</p>}
        </Section>
      </div>
    </div>
  );
}

function Section({ title, children, testid }: { title: string; children: React.ReactNode; testid?: string }) {
  return (
    <section className="panel p-5 space-y-3" data-testid={testid}>
      <h2 className="font-semibold">{title}</h2>
      {children}
    </section>
  );
}

function Row({ label, help, children }: { label: string; help?: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <div>
        <div className="text-[13px] font-medium">{label}</div>
        {help ? <div className="text-xs muted">{help}</div> : null}
      </div>
      {children}
    </div>
  );
}
