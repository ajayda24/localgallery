// Pure form logic, shared by every model: no model-specific code anywhere.

import type { InputKind, InputSpec, Level, ModelDetail, OptionItem, Preset } from "../types";

const LEVELS: Level[] = ["simple", "advanced", "ultra"];

export const ASSET_KINDS: InputKind[] = ["mask", "image", "first_frame", "last_frame", "video", "audio", "control_image"];
export const isAsset = (k: InputKind) => ASSET_KINDS.includes(k);

/** Inputs that apply to `op`, at or below `level`. */
export function inputsFor(model: Pick<ModelDetail, "inputs">, op: string, level: Level = "ultra"): InputSpec[] {
  const max = LEVELS.indexOf(level);
  return model.inputs.filter(
    (i) => (!i.ops || i.ops.length === 0 || i.ops.includes(op)) && LEVELS.indexOf(i.level ?? "simple") <= max,
  );
}

export function emptyValue(kind: InputKind): unknown {
  switch (kind) {
    case "regions":
    case "image_list":
    case "lora_stack":
      return [];
    case "structured_prompt":
      return {};
    case "bool":
      return false;
    case "seed":
      return -1;
    case "int":
    case "float":
      return 0;
    case "size":
      return { w: 1024, h: 1024 };
    default:
      return "";
  }
}

export function defaultValue(i: InputSpec): unknown {
  if (i.default !== undefined && i.default !== null) return structuredClone(i.default);
  if ((i.kind === "enum" || i.kind === "motion") && i.options?.length) return optionValue(i.options[0]);
  if (i.kind === "int" || i.kind === "float") return i.min ?? 0;
  return emptyValue(i.kind);
}

export function defaults(model: Pick<ModelDetail, "inputs">): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const i of model.inputs) out[i.id] = defaultValue(i);
  return out;
}

export function optionValue(o: unknown): unknown {
  return o && typeof o === "object" && "value" in (o as OptionItem) ? (o as OptionItem).value : o;
}

export function optionLabel(o: unknown): string {
  if (o && typeof o === "object") {
    const it = o as OptionItem;
    if (it.label) return it.label;
    if ("value" in it) return labelOf(it.value);
  }
  return labelOf(o);
}

function labelOf(v: unknown): string {
  if (v && typeof v === "object" && "w" in (v as any)) return `${(v as any).w} × ${(v as any).h}`;
  return String(v);
}

export function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Round a size to the model's grid, keeping it at least one step. */
export function roundSize(w: number, h: number, multiple = 8): { w: number; h: number } {
  const r = (x: number) => Math.max(multiple, Math.round(x / multiple) * multiple);
  return { w: r(w), h: r(h) };
}

/**
 * The values to send: only what differs from the defaults (so the planner may still
 * tune defaults), plus anything required, and never empty uploads.
 */
export function changedValues(model: ModelDetail, op: string, values: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const i of inputsFor(model, op)) {
    const v = values[i.id];
    if (v === undefined) continue;
    if (isAsset(i.kind) && (v === "" || v === null)) continue;
    if (i.required || !same(v, defaultValue(i))) out[i.id] = v;
  }
  return out;
}

/** Problems to show before submitting (the core checks again). */
export function missing(model: ModelDetail, op: string, values: Record<string, unknown>): string[] {
  const reqOp: string[] = model.manifest?.ops?.[op]?.requires ?? [];
  const out: string[] = [];
  for (const i of inputsFor(model, op)) {
    const needed = i.required || reqOp.includes(i.id);
    const v = values[i.id];
    const empty = v === undefined || v === null || v === "" || (Array.isArray(v) && v.length === 0 && i.kind !== "regions" && i.kind !== "lora_stack");
    if (needed && empty) out.push(i.label);
  }
  return out;
}

export function applyPreset(values: Record<string, unknown>, p: Preset): Record<string, unknown> {
  return { ...values, ...structuredClone(p.values) };
}

/** Group inputs for display: explicit groups, else by level. */
export function groups(inputs: InputSpec[]): { title: string; inputs: InputSpec[] }[] {
  const map = new Map<string, InputSpec[]>();
  for (const i of inputs) {
    const g = i.group ?? "";
    if (!map.has(g)) map.set(g, []);
    map.get(g)!.push(i);
  }
  return [...map.entries()].map(([title, inputs]) => ({ title, inputs }));
}

export function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
}

export function formatSeconds(s: number | null | undefined): string {
  if (s === null || s === undefined || !isFinite(s)) return "";
  if (s < 60) return `${Math.round(s)} s`;
  const m = Math.floor(s / 60);
  return `${m} min ${Math.round(s - m * 60)} s`;
}

export const opLabel = (op: string) =>
  ({
    txt2img: "Text to image",
    img2img: "Image to image",
    inpaint: "Inpaint",
    outpaint: "Extend",
    edit: "Edit",
    layout: "Layout",
    txt2vid: "Text to video",
    img2vid: "Image to video",
    run: "Run",
  })[op] ?? op;

const OP_ORDER = ["txt2img", "txt2vid", "img2img", "edit", "inpaint", "outpaint", "layout", "img2vid", "run"];

/** Ops in a stable, friendly order with the model's main op first. */
export function sortedOps(ops: string[], main: string): string[] {
  const rank = (o: string) => (o === main ? -1 : OP_ORDER.indexOf(o) < 0 ? 99 : OP_ORDER.indexOf(o));
  return [...ops].sort((a, b) => rank(a) - rank(b));
}

/** The model to open first: fast, good defaults, ready to run. */
export const PREFERRED_START = ["z-image-turbo", "sd-checkpoint", "flux2-klein-4b"];
