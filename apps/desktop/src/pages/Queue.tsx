// What is running, what is waiting, what happened.

import { ArrowDown, ArrowUp, RotateCw, Trash2, X } from "lucide-react";
import { api } from "../api";
import { Badge, Bar, Button, Empty } from "../components/ui";
import { formatSeconds, opLabel } from "../lib/form";
import { useStore } from "../store";
import type { Job } from "../types";

export function Queue() {
  const { jobs, progress, models, toast } = useStore();
  const name = (id: string) => models.find((m) => m.id === id)?.display_name ?? id;
  const running = jobs.filter((j) => j.status === "running");
  const queued = jobs.filter((j) => j.status === "queued").sort((a, b) => a.position - b.position);
  const done = jobs.filter((j) => !["running", "queued"].includes(j.status)).sort((a, b) => (b.finished_at ?? 0) - (a.finished_at ?? 0));
  const move = async (i: number, d: number) => {
    const ids = queued.map((j) => j.id);
    const [x] = ids.splice(i, 1);
    ids.splice(Math.max(0, Math.min(ids.length, i + d)), 0, x);
    useStore.setState({ jobs: await api.reorder(ids) });
  };
  const act = (f: () => Promise<unknown>) => f().catch((e) => toast("error", String(e.message)));
  const prompt = (j: Job) => String(j.spec.values?.prompt ?? Object.values(j.spec.values ?? {}).find((v) => typeof v === "string") ?? "");
  if (!jobs.length) return <Empty title="The queue is empty">Jobs you create wait here and run one at a time on your GPU.</Empty>;
  return (
    <div className="h-full overflow-y-auto p-6 space-y-6" data-testid="queue">
      {running.map((j) => {
        const p = progress[j.id];
        return (
          <div key={j.id} className="panel p-4 space-y-2" data-job={j.id}>
            <div className="flex items-center gap-2">
              <Badge color="accent">running</Badge>
              <span className="font-medium">{name(j.spec.model)}</span>
              <span className="muted text-xs">{opLabel(j.spec.op)}</span>
              <Button size="sm" variant="danger" className="ml-auto" onClick={() => act(() => api.cancelJob(j.id))}><X size={14} /> Cancel</Button>
            </div>
            <p className="text-sm muted truncate">{prompt(j)}</p>
            <Bar value={p?.overall ?? 0.02} />
            <p className="text-xs muted tabular-nums">{p?.total ? `Step ${p.step} of ${p.total}` : p?.node && !/^\d+$/.test(p.node) ? p.node : "Starting…"} {p?.eta ? `· about ${formatSeconds(p.eta)} left` : ""}</p>
          </div>
        );
      })}
      {queued.length ? (
        <section>
          <h3 className="mb-2 text-xs uppercase tracking-wide muted">Waiting · {queued.length}</h3>
          <div className="panel divide-y divide-[var(--line)]">
            {queued.map((j, i) => (
              <div key={j.id} className="flex items-center gap-3 p-3" data-job={j.id}>
                <span className="w-6 text-center text-xs muted tabular-nums">{i + 1}</span>
                <div className="min-w-0 flex-1">
                  <div className="text-sm"><span className="font-medium">{name(j.spec.model)}</span> <span className="muted text-xs">{opLabel(j.spec.op)}{(j.spec.batch ?? 1) > 1 ? ` · ×${j.spec.batch}` : ""}</span></div>
                  <div className="truncate text-xs muted">{prompt(j)}</div>
                </div>
                <Button size="sm" variant="ghost" disabled={i === 0} onClick={() => move(i, -1)} aria-label="Move up"><ArrowUp size={14} /></Button>
                <Button size="sm" variant="ghost" disabled={i === queued.length - 1} onClick={() => move(i, 1)} aria-label="Move down"><ArrowDown size={14} /></Button>
                <Button size="sm" variant="ghost" onClick={() => act(() => api.cancelJob(j.id))} aria-label="Cancel"><X size={14} /></Button>
              </div>
            ))}
          </div>
        </section>
      ) : null}
      {done.length ? (
        <section>
          <h3 className="mb-2 text-xs uppercase tracking-wide muted">History</h3>
          <div className="panel divide-y divide-[var(--line)]">
            {done.map((j) => (
              <div key={j.id} className="flex items-center gap-3 p-3" data-job={j.id}>
                <Badge color={j.status === "done" ? "green" : j.status === "failed" ? "red" : "gray"}>{j.status}</Badge>
                <div className="min-w-0 flex-1">
                  <div className="text-sm"><span className="font-medium">{name(j.spec.model)}</span> <span className="muted text-xs">{opLabel(j.spec.op)}</span></div>
                  <div className="truncate text-xs muted">{j.error ?? prompt(j)}</div>
                </div>
                <span className="text-xs muted tabular-nums whitespace-nowrap">
                  {j.metrics ? `${formatSeconds(j.metrics.seconds)}${j.metrics.peak_vram_gb ? ` · ${j.metrics.peak_vram_gb.toFixed(1)} GB VRAM` : ""}` : ""}
                </span>
                <Button size="sm" variant="ghost" onClick={() => act(() => api.retryJob(j.id))} aria-label="Run again" title="Run again"><RotateCw size={14} /></Button>
                <Button size="sm" variant="ghost" onClick={() => act(async () => { await api.deleteJob(j.id); useStore.setState({ jobs: await api.jobs() }); })} aria-label="Remove"><Trash2 size={14} /></Button>
              </div>
            ))}
          </div>
        </section>
      ) : null}
    </div>
  );
}
