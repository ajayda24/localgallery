// App shell: navigation, live status bar, toasts.

import { useEffect } from "react";
import { Boxes, Cog, Images, ListOrdered, Plug, Sparkles, Workflow } from "lucide-react";
import { useStore, type Page } from "./store";
import { Bar } from "./components/ui";
import { Create } from "./pages/Create";
import { Gallery } from "./pages/Gallery";
import { Queue } from "./pages/Queue";
import { Models } from "./pages/Models";
import { Tools } from "./pages/Tools";
import { Settings } from "./pages/Settings";

const NAV: { page: Page; label: string; icon: typeof Sparkles }[] = [
  { page: "create", label: "Create", icon: Sparkles },
  { page: "gallery", label: "Gallery", icon: Images },
  { page: "queue", label: "Queue", icon: ListOrdered },
  { page: "models", label: "Models", icon: Boxes },
  { page: "tools", label: "Workflows", icon: Workflow },
  { page: "settings", label: "Settings", icon: Cog },
];

export function App() {
  const { page, go, jobs, progress, toasts, system, gauge, connected, settings } = useStore();
  const running = jobs.find((j) => j.status === "running");
  const queued = jobs.filter((j) => j.status === "queued").length;
  const p = running ? progress[running.id] : undefined;

  useEffect(() => {
    const theme = settings?.theme ?? "system";
    const light = theme === "light" || (theme === "system" && window.matchMedia("(prefers-color-scheme: light)").matches);
    document.documentElement.classList.toggle("light", light);
    document.documentElement.classList.toggle("dark", !light);
  }, [settings?.theme]);

  return (
    <div className="flex h-full">
      <nav className="flex w-[76px] shrink-0 flex-col items-center gap-1 border-r line py-3" aria-label="Main">
        <div className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-accent-400 to-accent-700 text-white font-bold" title="LocalGallery">LG</div>
        {NAV.map(({ page: pg, label, icon: Icon }) => (
          <button key={pg} onClick={() => go(pg)} aria-current={page === pg ? "page" : undefined} data-nav={pg} className={`relative flex w-16 flex-col items-center gap-1 rounded-xl py-2 text-[11px] cursor-pointer transition-colors ${page === pg ? "bg-accent-500/15 text-[var(--accent-text)]" : "muted hover:text-[var(--text)] hover:bg-[var(--panel-2)]"}`}>
            <Icon size={20} />
            {label}
            {pg === "queue" && (queued || running) ? <span className="absolute right-2 top-1 rounded-full bg-accent-500 px-1.5 text-[10px] text-white">{queued + (running ? 1 : 0)}</span> : null}
          </button>
        ))}
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-11 shrink-0 items-center gap-4 border-b line px-4 text-xs">
          <span className={`inline-flex items-center gap-1.5 ${system?.engine.state === "ready" ? "text-emerald-400" : "muted"}`} data-testid="engine-state">
            <Plug size={13} /> {engineLabel(system?.engine.state)}
          </span>
          {!connected ? <span className="text-amber-400">Reconnecting…</span> : null}
          {running ? (
            <button className="flex min-w-0 max-w-sm flex-1 items-center gap-2 cursor-pointer" onClick={() => go("queue")}>
              <span className="whitespace-nowrap">Creating{queued ? ` (+${queued})` : ""}</span>
              <Bar value={p?.overall ?? 0.02} />
            </button>
          ) : <span className="flex-1" />}
          {gauge && gauge.vramTotal > 0 ? <Gauge label="GPU" used={gauge.vramUsed} total={gauge.vramTotal} warn={gauge.shared !== null && gauge.shared > 0.5} /> : null}
          {gauge ? <Gauge label="RAM" used={gauge.ramUsed} total={gauge.ramTotal} /> : null}
        </header>
        <main className="min-h-0 flex-1">
          {page === "create" && <Create />}
          {page === "gallery" && <Gallery />}
          {page === "queue" && <Queue />}
          {page === "models" && <Models />}
          {page === "tools" && <Tools />}
          {page === "settings" && <Settings />}
        </main>
      </div>
      <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex w-96 flex-col gap-2" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`pointer-events-auto panel px-4 py-3 text-sm shadow-xl ${t.kind === "error" ? "border-red-500/50" : t.kind === "success" ? "border-emerald-500/50" : ""}`} role={t.kind === "error" ? "alert" : "status"}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}

function engineLabel(s?: string) {
  return { ready: "Engine ready", starting: "Engine starting…", installing: "Installing engine…", not_installed: "Engine not installed", stopped: "Engine idle", busy: "Engine busy", error: "Engine error" }[s ?? ""] ?? "Connecting…";
}

function Gauge({ label, used, total, warn }: { label: string; used: number; total: number; warn?: boolean }) {
  const f = total ? used / total : 0;
  return (
    <span className="flex items-center gap-1.5" title={`${label}: ${used.toFixed(1)} of ${total.toFixed(1)} GB${warn ? " (spilling into shared memory)" : ""}`}>
      <span className="muted">{label}</span>
      <span className="h-1.5 w-16 overflow-hidden rounded-full bg-zinc-500/25">
        <span className={`block h-full ${warn ? "bg-red-500" : f > 0.9 ? "bg-amber-400" : "bg-emerald-400"}`} style={{ width: `${Math.min(100, f * 100)}%` }} />
      </span>
      <span className="tabular-nums muted">{used.toFixed(1)}/{total.toFixed(0)}</span>
    </span>
  );
}
