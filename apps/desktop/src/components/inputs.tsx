// One widget per input kind. Every model's form is built from these.

import { useRef, useState } from "react";
import { Dice5, ImagePlus, Lock, Plus, Trash2, Unlock, X } from "lucide-react";
import { api, fileUrl } from "../api";
import type { InputSpec, ModelDetail, Region } from "../types";
import { optionLabel, optionValue, roundSize, same } from "../lib/form";
import { useStore } from "../store";
import { Button, Field, Switch } from "./ui";

export interface WidgetProps {
  spec: InputSpec;
  value: unknown;
  onChange: (v: unknown) => void;
  model: ModelDetail;
}

export function InputWidget(p: WidgetProps) {
  const { spec } = p;
  const label = spec.label + (spec.required ? " *" : "");
  switch (spec.kind) {
    case "text":
    case "negative_text":
      return (
        <Field label={label} help={spec.help}>
          <textarea data-input={spec.id} rows={spec.kind === "text" ? 4 : 2} value={String(p.value ?? "")} placeholder={spec.kind === "text" ? "Describe what you want…" : "What to avoid"} onChange={(e) => p.onChange(e.target.value)} />
        </Field>
      );
    case "raw":
      return (
        <Field label={label} help={spec.help}>
          <input data-input={spec.id} value={String(p.value ?? "")} onChange={(e) => p.onChange(e.target.value)} />
        </Field>
      );
    case "int":
    case "float":
      return <NumberInput {...p} label={label} />;
    case "seed":
      return <SeedInput {...p} label={label} />;
    case "bool":
      return (
        <div className="flex items-center justify-between gap-3">
          <div>
            <div className="text-[13px] font-medium">{label}</div>
            {spec.help ? <div className="text-xs muted">{spec.help}</div> : null}
          </div>
          <Switch checked={Boolean(p.value)} onChange={p.onChange} label={spec.label} />
        </div>
      );
    case "enum":
    case "motion":
      return (
        <Field label={label} help={spec.help}>
          <select data-input={spec.id} value={JSON.stringify(p.value)} onChange={(e) => p.onChange(JSON.parse(e.target.value))}>
            {(spec.options ?? []).map((o) => (
              <option key={JSON.stringify(optionValue(o))} value={JSON.stringify(optionValue(o))}>
                {optionLabel(o)}
              </option>
            ))}
          </select>
        </Field>
      );
    case "model_file": {
      const choices = p.model.choices?.[spec.id] ?? [];
      const all = choices.includes(String(p.value)) || !p.value ? choices : [String(p.value), ...choices];
      return (
        <Field label={label} help={spec.help ?? (choices.length ? undefined : "No files in this folder yet")}>
          <select data-input={spec.id} value={String(p.value ?? "")} onChange={(e) => p.onChange(e.target.value)}>
            {all.map((c) => (
              <option key={c} value={c}>
                {c}
                {choices.includes(c) ? "" : " (not downloaded)"}
              </option>
            ))}
          </select>
        </Field>
      );
    }
    case "size":
      return <SizeInput {...p} label={label} />;
    case "image":
    case "mask":
    case "first_frame":
    case "last_frame":
    case "control_image":
    case "video":
    case "audio":
      return <AssetInput {...p} label={label} />;
    case "image_list":
      return <ImageListInput {...p} label={label} />;
    case "lora_stack":
      return <LoraStack {...p} label={label} />;
    case "regions":
      return <RegionsList {...p} label={label} />;
    case "structured_prompt":
      return <StructuredPrompt {...p} label={label} />;
    default:
      return null;
  }
}

function NumberInput({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const v = typeof value === "number" ? value : Number(value ?? 0);
  const isInt = spec.kind === "int";
  const step = spec.step ?? (isInt ? 1 : 0.05);
  const hasRange = spec.min !== null && spec.min !== undefined && spec.max !== null && spec.max !== undefined;
  const set = (n: number) => onChange(isInt ? Math.round(n) : Math.round(n / step) * step);
  return (
    <Field label={label} help={spec.help} right={<span className="text-xs tabular-nums muted">{isInt ? v : v.toFixed(2)}</span>}>
      <div className="flex items-center gap-3">
        {hasRange ? <input aria-label={spec.label} type="range" min={spec.min!} max={spec.max!} step={step} value={v} onChange={(e) => set(Number(e.target.value))} className="flex-1" /> : null}
        <input data-input={spec.id} type="number" className={hasRange ? "w-24" : ""} min={spec.min ?? undefined} max={spec.max ?? undefined} step={step} value={v} onChange={(e) => set(Number(e.target.value))} />
      </div>
    </Field>
  );
}

function SeedInput({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const v = Number(value ?? -1);
  const random = v < 0;
  return (
    <Field label={label} help={random ? "A new random seed each time." : "Same seed + same settings = same picture."}>
      <div className="flex gap-2">
        <input data-input={spec.id} type="number" value={random ? "" : v} placeholder="Random" onChange={(e) => onChange(e.target.value === "" ? -1 : Math.max(0, Math.floor(Number(e.target.value))))} />
        <Button title="New random seed" onClick={() => onChange(Math.floor(Math.random() * 2 ** 31))}>
          <Dice5 size={15} />
        </Button>
        <Button title={random ? "Keep a fixed seed" : "Use a random seed"} onClick={() => onChange(random ? Math.floor(Math.random() * 2 ** 31) : -1)}>
          {random ? <Unlock size={15} /> : <Lock size={15} />}
        </Button>
      </div>
    </Field>
  );
}

function SizeInput({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const v = (value as { w: number; h: number }) ?? { w: 1024, h: 1024 };
  const multiple = Number(spec.extra?.multiple_of ?? 8);
  const maxPx = Number(spec.extra?.max_pixels ?? 0);
  const [lock, setLock] = useState(false);
  const over = maxPx > 0 && v.w * v.h > maxPx;
  const set = (w: number, h: number) => onChange(roundSize(w, h, multiple));
  return (
    <Field label={label} help={over ? "Larger than this model supports." : `${((v.w * v.h) / 1e6).toFixed(2)} MP, multiples of ${multiple}`}>
      <div className="flex flex-wrap gap-1.5">
        {(spec.options ?? []).map((o) => {
          const ov = optionValue(o) as { w: number; h: number };
          return (
            <button key={optionLabel(o)} type="button" onClick={() => onChange(ov)} className={`rounded-md border px-2 py-1 text-xs cursor-pointer ${same(ov, v) ? "border-accent-500 bg-accent-500/15 text-[var(--accent-text)]" : "line panel-2 muted hover:text-[var(--text)]"}`}>
              {optionLabel(o)}
            </button>
          );
        })}
      </div>
      <div className="mt-2 flex items-center gap-2">
        <input aria-label="Width" data-input={`${spec.id}.w`} type="number" step={multiple} value={v.w} onChange={(e) => set(Number(e.target.value), lock ? Math.round((Number(e.target.value) * v.h) / v.w) : v.h)} />
        <span className="muted">×</span>
        <input aria-label="Height" data-input={`${spec.id}.h`} type="number" step={multiple} value={v.h} onChange={(e) => set(lock ? Math.round((Number(e.target.value) * v.w) / v.h) : v.w, Number(e.target.value))} />
        <Button title="Keep aspect ratio" onClick={() => setLock(!lock)}>{lock ? <Lock size={14} /> : <Unlock size={14} />}</Button>
        <Button title="Swap" onClick={() => onChange({ w: v.h, h: v.w })}>⇄</Button>
      </div>
    </Field>
  );
}

/** Cache of asset id -> preview URL so pickers can show what was chosen. */
export const assetUrls = new Map<string, string>();

export function AssetInput({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const ref = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const toast = useStore((s) => s.toast);
  const accept = spec.kind === "video" ? "video/*" : spec.kind === "audio" ? "audio/*" : "image/*";
  const id = String(value ?? "");
  const url = assetUrls.get(id);
  const pick = async (f: File | undefined) => {
    if (!f) return;
    setBusy(true);
    try {
      const a = await api.upload(f, f.name);
      assetUrls.set(a.id, a.url);
      onChange(a.id);
    } catch (e) {
      toast("error", String((e as Error).message));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Field label={label} help={spec.help}>
      <div
        className="flex items-center gap-3 rounded-lg border border-dashed line panel-2 p-2"
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          e.preventDefault();
          pick(e.dataTransfer.files[0]);
        }}
      >
        <div className="h-16 w-16 shrink-0 overflow-hidden rounded-md checker flex items-center justify-center">
          {id && url && accept === "image/*" ? <img src={url} alt="" className="h-full w-full object-cover" /> : id ? <span className="text-[10px] muted">ready</span> : <ImagePlus size={20} className="muted" />}
        </div>
        <div className="flex-1 text-xs muted">{busy ? "Uploading…" : id ? "Chosen. Drop another to replace." : "Drop a file here, or"}</div>
        <input ref={ref} data-input={spec.id} type="file" accept={accept} className="hidden" onChange={(e) => pick(e.target.files?.[0])} />
        <Button size="sm" onClick={() => ref.current?.click()}>Browse</Button>
        {id ? (
          <Button size="sm" variant="ghost" title="Clear" onClick={() => onChange("")}>
            <X size={14} />
          </Button>
        ) : null}
      </div>
    </Field>
  );
}

function ImageListInput({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const ids = (Array.isArray(value) ? value : []) as string[];
  const max = Number(spec.extra?.max_items ?? 8);
  const ref = useRef<HTMLInputElement>(null);
  const add = async (files: FileList | null) => {
    if (!files) return;
    const next = [...ids];
    for (const f of Array.from(files).slice(0, max - ids.length)) {
      const a = await api.upload(f, f.name);
      assetUrls.set(a.id, a.url);
      next.push(a.id);
    }
    onChange(next);
  };
  return (
    <Field label={label} help={spec.help}>
      <div className="flex flex-wrap gap-2" onDragOver={(e) => e.preventDefault()} onDrop={(e) => { e.preventDefault(); add(e.dataTransfer.files); }}>
        {ids.map((id, i) => (
          <div key={id} className="relative h-16 w-16 overflow-hidden rounded-md checker">
            {assetUrls.get(id) ? <img src={assetUrls.get(id)} alt="" className="h-full w-full object-cover" /> : null}
            <button className="absolute right-0.5 top-0.5 rounded bg-black/60 p-0.5 text-white cursor-pointer" onClick={() => onChange(ids.filter((_, j) => j !== i))} aria-label="Remove">
              <X size={12} />
            </button>
          </div>
        ))}
        {ids.length < max ? (
          <button className="h-16 w-16 rounded-md border border-dashed line flex items-center justify-center muted cursor-pointer" onClick={() => ref.current?.click()} aria-label="Add images">
            <Plus size={18} />
          </button>
        ) : null}
        <input ref={ref} data-input={spec.id} type="file" accept="image/*" multiple className="hidden" onChange={(e) => add(e.target.files)} />
      </div>
    </Field>
  );
}

function LoraStack({ spec, value, onChange, model, label }: WidgetProps & { label: string }) {
  const items = (Array.isArray(value) ? value : []) as { file: string; strength: number }[];
  const choices = model.choices?.[spec.id] ?? [];
  const set = (i: number, patch: Partial<{ file: string; strength: number }>) => onChange(items.map((it, j) => (j === i ? { ...it, ...patch } : it)));
  return (
    <Field label={label} help={choices.length ? spec.help : "Put LoRA files in the models/loras folder (or download them) to use them here."}>
      <div className="space-y-2">
        {items.map((it, i) => (
          <div key={i} className="flex items-center gap-2">
            <select value={it.file} onChange={(e) => set(i, { file: e.target.value })} className="flex-1 min-w-0">
              {[it.file, ...choices.filter((c) => c !== it.file)].filter(Boolean).map((c) => (
                <option key={c} value={c}>{c}</option>
              ))}
            </select>
            <input type="number" step={0.05} min={-2} max={2} value={it.strength} onChange={(e) => set(i, { strength: Number(e.target.value) })} className="w-20" aria-label="Strength" />
            <Button size="sm" variant="ghost" onClick={() => onChange(items.filter((_, j) => j !== i))} aria-label="Remove LoRA"><Trash2 size={14} /></Button>
          </div>
        ))}
        <Button size="sm" disabled={!choices.length && !items.length && false} onClick={() => onChange([...items, { file: choices[0] ?? "", strength: 1 }])}>
          <Plus size={14} /> Add LoRA
        </Button>
      </div>
    </Field>
  );
}

function RegionsList({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const items = (Array.isArray(value) ? value : []) as Region[];
  const textBoxes = Boolean(spec.extra?.text_boxes);
  const set = (i: number, patch: Partial<Region>) => onChange(items.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <Field label={label} help={spec.help ?? "Draw boxes on the canvas in Studio, then describe each one."}>
      <div className="space-y-2" data-input={spec.id}>
        {items.length === 0 ? <p className="text-xs muted">No boxes yet. Use the box tool on the canvas.</p> : null}
        {items.map((r, i) => (
          <div key={i} className="rounded-lg border line p-2 space-y-1.5">
            <div className="flex items-center gap-2 text-xs muted">
              <span className="inline-flex h-5 w-5 items-center justify-center rounded bg-accent-500/20 text-[var(--accent-text)] font-semibold">{i + 1}</span>
              <span className="tabular-nums">{Math.round(r.x * 100)}%, {Math.round(r.y * 100)}% · {Math.round(r.w * 100)}×{Math.round(r.h * 100)}%</span>
              {textBoxes ? (
                <select value={r.type ?? "obj"} onChange={(e) => set(i, { type: e.target.value as Region["type"] })} className="ml-auto w-auto py-0.5 text-xs">
                  <option value="obj">Object</option>
                  <option value="text">Text</option>
                </select>
              ) : null}
              <button className="ml-auto cursor-pointer" onClick={() => onChange(items.filter((_, j) => j !== i))} aria-label="Remove box"><Trash2 size={13} /></button>
            </div>
            <input placeholder="What is in this box?" value={r.prompt} onChange={(e) => set(i, { prompt: e.target.value })} data-region-prompt={i} />
            {r.type === "text" ? <input placeholder="Text to render" value={r.text ?? ""} onChange={(e) => set(i, { text: e.target.value })} /> : null}
          </div>
        ))}
      </div>
    </Field>
  );
}

/** JSON prompt editor: a form generated from the input's JSON Schema, with a raw JSON view. */
function StructuredPrompt({ spec, value, onChange, label }: WidgetProps & { label: string }) {
  const [raw, setRaw] = useState(false);
  const [text, setText] = useState(() => JSON.stringify(value ?? {}, null, 2));
  const [err, setErr] = useState<string | null>(null);
  const schema = (spec.schema ?? {}) as { properties?: Record<string, any> };
  const obj = (value && typeof value === "object" ? value : {}) as Record<string, unknown>;
  const props = Object.entries(schema.properties ?? {});
  return (
    <Field label={label} help={spec.help} right={<Button size="sm" variant="ghost" onClick={() => { setText(JSON.stringify(value ?? {}, null, 2)); setRaw(!raw); }}>{raw ? "Form" : "JSON"}</Button>}>
      {raw || props.length === 0 ? (
        <>
          <textarea data-input={spec.id} rows={8} className="font-mono text-xs" value={text} onChange={(e) => {
            setText(e.target.value);
            try {
              onChange(JSON.parse(e.target.value || "{}"));
              setErr(null);
            } catch (x) {
              setErr((x as Error).message);
            }
          }} />
          {err ? <p className="text-xs text-red-400">{err}</p> : null}
        </>
      ) : (
        <div className="space-y-2">
          {props.map(([k, p]) => (
            <SchemaField key={k} name={k} schema={p} value={obj[k]} onChange={(v) => onChange({ ...obj, [k]: v })} />
          ))}
        </div>
      )}
    </Field>
  );
}

function SchemaField({ name, schema, value, onChange }: { name: string; schema: any; value: unknown; onChange: (v: unknown) => void }) {
  const title = schema.title ?? name.replace(/_/g, " ");
  if (schema.enum) {
    return (
      <Field label={title} help={schema.description}>
        <select value={String(value ?? schema.default ?? "")} onChange={(e) => onChange(e.target.value)}>
          {schema.enum.map((o: string) => <option key={o} value={o}>{o}</option>)}
        </select>
      </Field>
    );
  }
  if (schema.type === "number" || schema.type === "integer") {
    return <Field label={title} help={schema.description}><input type="number" value={Number(value ?? schema.default ?? 0)} onChange={(e) => onChange(Number(e.target.value))} /></Field>;
  }
  if (schema.type === "boolean") {
    return <div className="flex items-center justify-between"><span className="text-[13px]">{title}</span><Switch checked={Boolean(value)} onChange={onChange} /></div>;
  }
  if (schema.type === "array") {
    const arr = Array.isArray(value) ? (value as string[]) : [];
    return <Field label={title} help={schema.description ?? "One per line"}><textarea rows={2} value={arr.join("\n")} onChange={(e) => onChange(e.target.value.split("\n").filter(Boolean))} /></Field>;
  }
  if (schema.type === "object" && schema.properties) {
    const o = (value && typeof value === "object" ? value : {}) as Record<string, unknown>;
    return (
      <fieldset className="rounded-lg border line p-2 space-y-2">
        <legend className="px-1 text-xs muted">{title}</legend>
        {Object.entries(schema.properties).map(([k, p]) => <SchemaField key={k} name={k} schema={p} value={o[k]} onChange={(v) => onChange({ ...o, [k]: v })} />)}
      </fieldset>
    );
  }
  return <Field label={title} help={schema.description}><input value={String(value ?? "")} onChange={(e) => onChange(e.target.value)} /></Field>;
}

export function assetPreview(id: string) {
  return assetUrls.get(id) ?? "";
}

export { fileUrl };
