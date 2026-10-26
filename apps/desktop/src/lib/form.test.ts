import { describe, expect, it } from "vitest";
import { changedValues, defaults, inputsFor, missing, roundSize, optionLabel } from "./form";
import type { ModelDetail } from "../types";

const model = {
  inputs: [
    { id: "prompt", kind: "text", label: "Prompt", required: true },
    { id: "image", kind: "image", label: "Image", ops: ["img2img"] },
    { id: "steps", kind: "int", label: "Steps", level: "advanced", default: 20, min: 1, max: 50 },
    { id: "dtype", kind: "enum", label: "W", level: "ultra", default: "default", options: ["default", "fp8"] },
    { id: "size", kind: "size", label: "Size", default: { w: 1024, h: 1024 } },
  ],
  manifest: { ops: { img2img: { requires: ["image"] } } },
} as unknown as ModelDetail;

describe("form", () => {
  it("filters by op and level", () => {
    expect(inputsFor(model, "txt2img", "simple").map((i) => i.id)).toEqual(["prompt", "size"]);
    expect(inputsFor(model, "img2img", "advanced").map((i) => i.id)).toEqual(["prompt", "image", "steps", "size"]);
  });
  it("sends only changed values, so planner defaults still apply", () => {
    const v = { ...defaults(model), prompt: "cat", steps: 30 };
    expect(changedValues(model, "txt2img", v)).toEqual({ prompt: "cat", steps: 30 });
  });
  it("reports required inputs", () => {
    expect(missing(model, "img2img", defaults(model))).toEqual(["Prompt", "Image"]);
  });
  it("rounds sizes", () => {
    expect(roundSize(1000, 770, 16)).toEqual({ w: 1008, h: 768 });
    expect(roundSize(3, 3, 8)).toEqual({ w: 8, h: 8 });
  });
  it("labels options", () => {
    expect(optionLabel({ value: { w: 832, h: 480 }, label: "Wide" })).toBe("Wide");
    expect(optionLabel({ value: { w: 832, h: 480 } })).toBe("832 × 480");
  });
});
