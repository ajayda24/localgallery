// Studio canvas: paint masks and draw region boxes on top of the input image.
// Shapes are kept in image pixels; regions are sent as fractions of the image.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Image as KImage, Layer, Line, Rect, Stage, Text, Transformer } from "react-konva";
import type Konva from "konva";
import { Brush, Eraser, MousePointer2, Redo2, Square, Trash2, Undo2 } from "lucide-react";
import { api } from "../api";
import type { ModelDetail, Region } from "../types";
import { inputsFor } from "../lib/form";
import { assetUrls } from "./inputs";
import { Button } from "./ui";

type Tool = "select" | "box" | "brush" | "eraser";
interface Stroke { points: number[]; size: number; erase: boolean }
interface Snapshot { strokes: Stroke[]; regions: Region[] }

function useImage(url: string | undefined) {
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  useEffect(() => {
    if (!url) {
      setImg(null);
      return;
    }
    const i = new window.Image();
    i.onload = () => setImg(i);
    i.src = url;
  }, [url]);
  return img;
}

export function Canvas({ model, op, values, setValue }: { model: ModelDetail; op: string; values: Record<string, unknown>; setValue: (id: string, v: unknown) => void }) {
  const inputs = inputsFor(model, op);
  const baseInput = inputs.find((i) => ["image", "first_frame", "control_image"].includes(i.kind));
  const maskInput = inputs.find((i) => i.kind === "mask");
  const regionInput = inputs.find((i) => i.kind === "regions");
  const sizeInput = inputs.find((i) => i.kind === "size");
  const baseId = baseInput ? String(values[baseInput.id] ?? "") : "";
  const img = useImage(baseId ? assetUrls.get(baseId) : undefined);
  const size = (sizeInput ? (values[sizeInput.id] as { w: number; h: number }) : null) ?? { w: 1024, h: 1024 };
  const W = img?.naturalWidth ?? size.w;
  const H = img?.naturalHeight ?? size.h;

  // Painting a mask is the point of ops that have one; otherwise boxes.
  const [tool, setTool] = useState<Tool>(maskInput ? "brush" : regionInput ? "box" : "select");
  const [brush, setBrush] = useState(48);
  const [strokes, setStrokes] = useState<Stroke[]>([]);
  const regions = useMemo(() => (regionInput ? ((values[regionInput.id] as Region[]) ?? []) : []), [regionInput, values]);
  const setRegions = useCallback((r: Region[]) => regionInput && setValue(regionInput.id, r), [regionInput, setValue]);
  const [past, setPast] = useState<Snapshot[]>([]);
  const [future, setFuture] = useState<Snapshot[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [drag, setDrag] = useState<{ x: number; y: number; x2: number; y2: number } | null>(null);
  const drawing = useRef(false);

  // Fit the canvas to its container.
  const box = useRef<HTMLDivElement>(null);
  const [avail, setAvail] = useState({ w: 800, h: 600 });
  useEffect(() => {
    if (!box.current) return;
    const ro = new ResizeObserver(([e]) => setAvail({ w: e.contentRect.width, h: e.contentRect.height }));
    ro.observe(box.current);
    return () => ro.disconnect();
  }, []);
  const scale = Math.min(avail.w / W, avail.h / H, 4) || 1;

  const snapshot = (): Snapshot => ({ strokes: structuredClone(strokes), regions: structuredClone(regions) });
  const commit = () => {
    setPast((p) => [...p.slice(-49), snapshot()]);
    setFuture([]);
  };
  const undo = useCallback(() => {
    setPast((p) => {
      if (!p.length) return p;
      const prev = p[p.length - 1];
      setFuture((f) => [{ strokes, regions }, ...f]);
      setStrokes(prev.strokes);
      setRegions(prev.regions);
      return p.slice(0, -1);
    });
  }, [strokes, regions, setRegions]);
  const redo = useCallback(() => {
    setFuture((f) => {
      if (!f.length) return f;
      const next = f[0];
      setPast((p) => [...p, { strokes, regions }]);
      setStrokes(next.strokes);
      setRegions(next.regions);
      return f.slice(1);
    });
  }, [strokes, regions, setRegions]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "TEXTAREA" || (e.target as HTMLElement)?.tagName === "INPUT") return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) redo();
        else undo();
      }
      if (e.key === "Delete" && selected !== null) {
        commit();
        setRegions(regions.filter((_, i) => i !== selected));
        setSelected(null);
      }
      if (e.key === "b") setTool("brush");
      if (e.key === "e") setTool("eraser");
      if (e.key === "r") setTool("box");
      if (e.key === "v") setTool("select");
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  });

  // Render the mask at full image resolution and upload it after a pause in painting.
  const uploadTimer = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (!maskInput) return;
    window.clearTimeout(uploadTimer.current);
    if (!strokes.length) return;
    uploadTimer.current = window.setTimeout(async () => {
      const c = document.createElement("canvas");
      c.width = W;
      c.height = H;
      const g = c.getContext("2d")!;
      g.fillStyle = "#000";
      g.fillRect(0, 0, W, H);
      g.lineCap = "round";
      g.lineJoin = "round";
      for (const s of strokes) {
        g.strokeStyle = s.erase ? "#000" : "#fff";
        g.lineWidth = s.size;
        g.beginPath();
        for (let i = 0; i < s.points.length; i += 2) (i ? g.lineTo : g.moveTo).call(g, s.points[i], s.points[i + 1]);
        if (s.points.length === 2) g.lineTo(s.points[0] + 0.01, s.points[1]);
        g.stroke();
      }
      const blob: Blob = await new Promise((r) => c.toBlob((b) => r(b!), "image/png"));
      const a = await api.upload(blob, "mask.png");
      assetUrls.set(a.id, a.url);
      setValue(maskInput.id, a.id);
    }, 400);
  }, [strokes, maskInput, W, H, setValue]);

  const pos = (e: Konva.KonvaEventObject<MouseEvent | TouchEvent>) => {
    const p = e.target.getStage()!.getPointerPosition()!;
    return { x: Math.max(0, Math.min(W, p.x / scale)), y: Math.max(0, Math.min(H, p.y / scale)) };
  };
  const down = (e: Konva.KonvaEventObject<MouseEvent | TouchEvent>) => {
    const p = pos(e);
    if ((tool === "brush" || tool === "eraser") && maskInput) {
      commit();
      drawing.current = true;
      setStrokes((s) => [...s, { points: [p.x, p.y], size: brush, erase: tool === "eraser" }]);
    } else if (tool === "box" && regionInput) {
      setDrag({ x: p.x, y: p.y, x2: p.x, y2: p.y });
    } else if (e.target === e.target.getStage() || e.target.name() === "bg") {
      setSelected(null);
    }
  };
  const move = (e: Konva.KonvaEventObject<MouseEvent | TouchEvent>) => {
    const p = pos(e);
    if (drawing.current) {
      setStrokes((s) => {
        const last = s[s.length - 1];
        return [...s.slice(0, -1), { ...last, points: [...last.points, p.x, p.y] }];
      });
    } else if (drag) setDrag({ ...drag, x2: p.x, y2: p.y });
  };
  const up = () => {
    drawing.current = false;
    if (drag) {
      const x = Math.min(drag.x, drag.x2), y = Math.min(drag.y, drag.y2);
      const w = Math.abs(drag.x2 - drag.x), h = Math.abs(drag.y2 - drag.y);
      if (w > 8 && h > 8) {
        commit();
        setRegions([...regions, { x: x / W, y: y / H, w: w / W, h: h / H, prompt: "", type: "obj", weight: 1 }]);
        setSelected(regions.length);
      }
      setDrag(null);
    }
  };

  const tr = useRef<Konva.Transformer>(null);
  const rects = useRef<(Konva.Rect | null)[]>([]);
  useEffect(() => {
    if (!tr.current) return;
    const node = selected !== null ? rects.current[selected] : null;
    tr.current.nodes(node && tool === "select" ? [node] : []);
  }, [selected, tool, regions]);

  const updateRegion = (i: number, node: Konva.Rect) => {
    const sx = node.scaleX(), sy = node.scaleY();
    node.scaleX(1);
    node.scaleY(1);
    commit();
    setRegions(regions.map((r, j) => (j === i ? { ...r, x: node.x() / W, y: node.y() / H, w: (node.width() * sx) / W, h: (node.height() * sy) / H } : r)));
  };

  const colors = ["#7c5cff", "#22c55e", "#f59e0b", "#06b6d4", "#ec4899", "#ef4444", "#84cc16", "#3b82f6"];
  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex flex-wrap items-center gap-1.5" role="toolbar" aria-label="Canvas tools">
        {regionInput || maskInput ? <ToolBtn on={tool === "select"} onClick={() => setTool("select")} title="Select and move (V)"><MousePointer2 size={15} /></ToolBtn> : null}
        {regionInput ? <ToolBtn on={tool === "box"} onClick={() => setTool("box")} title="Draw a box (R)"><Square size={15} /> Box</ToolBtn> : null}
        {maskInput ? (
          <>
            <ToolBtn on={tool === "brush"} onClick={() => setTool("brush")} title="Paint the area to change (B)"><Brush size={15} /> Mask</ToolBtn>
            <ToolBtn on={tool === "eraser"} onClick={() => setTool("eraser")} title="Erase mask (E)"><Eraser size={15} /></ToolBtn>
            <input aria-label="Brush size" type="range" min={4} max={256} value={brush} onChange={(e) => setBrush(Number(e.target.value))} className="w-28" />
            <span className="text-xs muted w-10 tabular-nums">{brush}px</span>
            <Button size="sm" variant="ghost" onClick={() => { commit(); setStrokes([]); setValue(maskInput.id, ""); }} title="Clear mask"><Trash2 size={14} /></Button>
          </>
        ) : null}
        <div className="ml-auto flex gap-1">
          <Button size="sm" variant="ghost" onClick={undo} disabled={!past.length} title="Undo (Ctrl+Z)" aria-label="Undo"><Undo2 size={15} /></Button>
          <Button size="sm" variant="ghost" onClick={redo} disabled={!future.length} title="Redo (Ctrl+Shift+Z)" aria-label="Redo"><Redo2 size={15} /></Button>
        </div>
      </div>
      <div ref={box} className="relative flex-1 min-h-[280px] flex items-center justify-center overflow-hidden rounded-xl checker" data-testid="canvas">
        <Stage width={W * scale} height={H * scale} scaleX={scale} scaleY={scale} onMouseDown={down} onMouseMove={move} onMouseUp={up} onMouseLeave={up} onTouchStart={down} onTouchMove={move} onTouchEnd={up} style={{ cursor: tool === "select" ? "default" : "crosshair" }}>
          <Layer listening={tool === "select" || tool === "box"}>
            {img ? <KImage image={img} width={W} height={H} name="bg" /> : <Rect width={W} height={H} fill="rgba(127,127,127,0.10)" name="bg" />}
            {!img && baseInput ? <Text x={16} y={16} text="Add an image on the left to start" fontSize={Math.max(16, W / 40)} fill="#9aa1b2" /> : null}
          </Layer>
          {maskInput ? (
            <Layer opacity={0.5} listening={false}>
              {strokes.map((s, i) => (
                <Line key={i} points={s.points.length === 2 ? [...s.points, s.points[0] + 0.01, s.points[1]] : s.points} stroke={s.erase ? "#000" : "#ff3b6b"} strokeWidth={s.size} lineCap="round" lineJoin="round" globalCompositeOperation={s.erase ? "destination-out" : "source-over"} />
              ))}
            </Layer>
          ) : null}
          {regionInput ? (
            <Layer>
              {regions.map((r, i) => (
                <Rect
                  key={i}
                  ref={(n) => {
                    rects.current[i] = n;
                  }}
                  x={r.x * W}
                  y={r.y * H}
                  width={r.w * W}
                  height={r.h * H}
                  stroke={colors[i % colors.length]}
                  strokeWidth={3 / scale}
                  fill={`${colors[i % colors.length]}22`}
                  draggable={tool === "select"}
                  onClick={() => setSelected(i)}
                  onTap={() => setSelected(i)}
                  onDragEnd={(e) => updateRegion(i, e.target as Konva.Rect)}
                  onTransformEnd={(e) => updateRegion(i, e.target as Konva.Rect)}
                />
              ))}
              {regions.map((r, i) => (
                <Text key={`t${i}`} x={r.x * W + 6 / scale} y={r.y * H + 6 / scale} text={`${i + 1}${r.prompt ? ` ${r.prompt.slice(0, 40)}` : ""}${r.type === "text" ? ` "${r.text ?? ""}"` : ""}`} fontSize={14 / scale} fill="#fff" listening={false} shadowColor="#000" shadowBlur={4} />
              ))}
              {drag ? <Rect x={Math.min(drag.x, drag.x2)} y={Math.min(drag.y, drag.y2)} width={Math.abs(drag.x2 - drag.x)} height={Math.abs(drag.y2 - drag.y)} stroke="#fff" dash={[6 / scale, 4 / scale]} strokeWidth={2 / scale} /> : null}
              <Transformer ref={tr} rotateEnabled={false} keepRatio={false} />
            </Layer>
          ) : null}
        </Stage>
      </div>
      <p className="text-xs muted">
        {W} × {H}px
        {regionInput ? " · Draw boxes, then describe them on the left. Delete removes the selected box." : ""}
        {maskInput ? " · Paint over what should change." : ""}
      </p>
    </div>
  );
}

function ToolBtn({ on, children, ...p }: { on: boolean; onClick: () => void; title: string; children: React.ReactNode }) {
  return (
    <button {...p} aria-pressed={on} className={`inline-flex items-center gap-1.5 rounded-lg border px-2.5 py-1 text-xs cursor-pointer ${on ? "border-accent-500 bg-accent-500/15 text-[var(--accent-text)]" : "line panel-2"}`}>
      {children}
    </button>
  );
}

export function hasCanvas(model: ModelDetail, op: string) {
  return inputsFor(model, op).some((i) => ["regions", "mask", "image", "first_frame", "control_image"].includes(i.kind));
}
