// Small UI primitives used across pages.

import type { ButtonHTMLAttributes, ReactNode } from "react";

export function Button({ variant = "default", size = "md", className = "", ...p }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "ghost" | "danger"; size?: "sm" | "md" | "lg" }) {
  const v = {
    default: "panel-2 border line hover:border-[var(--muted)]",
    primary: "bg-accent-500 hover:bg-accent-600 text-white border border-transparent shadow-sm shadow-accent-700/30",
    ghost: "border border-transparent hover:bg-[var(--panel-2)]",
    danger: "border border-red-500/40 text-red-400 hover:bg-red-500/10",
  }[variant];
  const s = { sm: "px-2.5 py-1 text-xs", md: "px-3.5 py-1.5", lg: "px-5 py-2.5 text-base font-semibold" }[size];
  return <button {...p} className={`inline-flex items-center justify-center gap-1.5 rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer ${v} ${s} ${className}`} />;
}

export function Badge({ color = "gray", children, title }: { color?: "green" | "yellow" | "red" | "gray" | "accent"; children: ReactNode; title?: string }) {
  const c = {
    green: "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
    yellow: "bg-amber-500/15 text-amber-400 border-amber-500/30",
    red: "bg-red-500/15 text-red-400 border-red-500/30",
    gray: "bg-zinc-500/10 muted border-zinc-500/25",
    accent: "bg-accent-500/15 text-[var(--accent-text)] border-accent-500/30",
  }[color];
  return (
    <span title={title} className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium whitespace-nowrap ${c}`}>
      {children}
    </span>
  );
}

export function Field({ label, help, children, right }: { label: string; help?: string; children: ReactNode; right?: ReactNode }) {
  return (
    <label className="block space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[13px] font-medium">{label}</span>
        {right}
      </div>
      {children}
      {help ? <p className="text-xs muted leading-snug">{help}</p> : null}
    </label>
  );
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: string }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} onClick={() => onChange(!checked)} className={`relative h-5 w-9 rounded-full transition-colors cursor-pointer ${checked ? "bg-accent-500" : "bg-zinc-500/40"}`}>
      <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all ${checked ? "left-4.5" : "left-0.5"}`} />
    </button>
  );
}

export function Tabs<T extends string>({ value, onChange, items }: { value: T; onChange: (v: T) => void; items: { value: T; label: string }[] }) {
  return (
    <div className="inline-flex rounded-lg panel-2 border line p-0.5" role="tablist">
      {items.map((it) => (
        <button key={it.value} role="tab" aria-selected={value === it.value} onClick={() => onChange(it.value)} className={`px-3 py-1 rounded-md text-[13px] whitespace-nowrap cursor-pointer transition-colors ${value === it.value ? "bg-[var(--panel)] shadow-sm font-medium" : "muted hover:text-[var(--text)]"}`}>
          {it.label}
        </button>
      ))}
    </div>
  );
}

export function Modal({ title, onClose, children, wide }: { title: string; onClose: () => void; children: ReactNode; wide?: boolean }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6" onClick={onClose}>
      <div className={`panel w-full ${wide ? "max-w-6xl" : "max-w-2xl"} max-h-[90vh] overflow-auto shadow-2xl`} onClick={(e) => e.stopPropagation()} role="dialog" aria-label={title}>
        <div className="flex items-center justify-between border-b line px-5 py-3 sticky top-0 bg-[var(--panel)] z-10">
          <h2 className="text-base font-semibold">{title}</h2>
          <Button variant="ghost" size="sm" onClick={onClose} aria-label="Close">✕</Button>
        </div>
        <div className="p-5">{children}</div>
      </div>
    </div>
  );
}

export function Bar({ value, className = "" }: { value: number; className?: string }) {
  return (
    <div className={`h-1.5 w-full overflow-hidden rounded-full bg-zinc-500/20 ${className}`}>
      <div className="h-full rounded-full bg-accent-500 transition-[width] duration-300" style={{ width: `${Math.max(0, Math.min(100, value * 100))}%` }} />
    </div>
  );
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-16 text-center">
      <p className="text-base font-medium">{title}</p>
      <div className="muted text-sm max-w-md">{children}</div>
    </div>
  );
}
