// The UI end to end against the real core (fake engine). Screenshots go to test-results/screens.

import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
import { deflateSync } from "node:zlib";

const repo = resolve(here, "../../..");
const shot = (page: Page, name: string) => page.screenshot({ path: `test-results/screens/${name}.png` });

async function open(page: Page) {
  await page.goto("/?token=e2e");
  await expect(page.getByTestId("model-picker")).toBeVisible();
}

async function pickModel(page: Page, id: string) {
  await page.getByTestId("model-picker").click();
  await page.locator(`[data-model="${id}"]`).click();
  await expect(page.getByTestId("model-picker")).toBeVisible();
}

/** A small solid PNG, built by hand so the test needs no image libraries. */
function png(w: number, h: number, rgb: [number, number, number]): Buffer {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (b: Buffer) => {
    let c = 0xffffffff;
    for (const x of b) c = crcTable[(c ^ x) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, data: Buffer) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => rgb).flat())]);
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

test("create an image, see it in the gallery, reuse its settings", async ({ page }) => {
  await open(page);
  await pickModel(page, "z-image-turbo");
  await shot(page, "01-create");
  await page.locator('[data-input="prompt"]').fill("a lighthouse on a cliff at dusk");
  await page.getByTestId("create").click();
  await expect(page.getByTestId("progress")).toBeVisible();
  await shot(page, "02-progress");
  await expect(page.getByTestId("recent").locator("img").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("progress")).toBeHidden({ timeout: 30_000 });
  await shot(page, "03-result");

  await page.locator('[data-nav="gallery"]').click();
  const item = page.getByTestId("gallery").locator("[data-output]").first();
  await expect(item).toBeVisible();
  await item.click();
  await expect(page.getByRole("dialog")).toContainText("a lighthouse on a cliff at dusk");
  await page.getByRole("dialog").getByRole("button", { name: "Favourite", exact: true }).click();
  await shot(page, "04-viewer");
  await page.getByRole("dialog").getByRole("button", { name: "Reuse settings" }).click();
  await expect(page.locator('[data-input="prompt"]')).toHaveValue("a lighthouse on a cliff at dusk");
});

test("studio: draw regional boxes on the canvas and run them", async ({ page, request }) => {
  await open(page);
  await pickModel(page, "sd-checkpoint");
  await page.getByRole("tab", { name: "Advanced" }).click();
  await page.locator('[data-input="prompt"]').fill("a harbour");
  const stage = page.getByTestId("canvas").locator("canvas").last();
  const b = (await stage.boundingBox())!;
  await page.getByRole("button", { name: /Box/ }).click();
  await page.mouse.move(b.x + b.width * 0.1, b.y + b.height * 0.5);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width * 0.45, b.y + b.height * 0.85, { steps: 5 });
  await page.mouse.up();
  await expect(page.locator("[data-region-prompt='0']")).toBeVisible();
  await page.locator("[data-region-prompt='0']").fill("a red boat");
  await shot(page, "05-studio-regions");
  await page.getByTestId("create").click();
  // The box reached the job (wait for the job itself: the progress bar may not have appeared yet).
  const regionJob = async () => {
    const jobs = await (await request.get("/api/jobs", { headers: { authorization: "Bearer e2e" } })).json();
    return jobs.find((j: any) => j.spec.model === "sd-checkpoint" && j.status === "done" && j.spec.values.regions?.length);
  };
  await expect.poll(regionJob, { timeout: 30_000 }).toBeTruthy();
  const job = await regionJob();
  expect(job.spec.values.regions[0].prompt).toBe("a red boat");
  expect(job.spec.values.regions[0].x).toBeGreaterThan(0.05);
});

test("studio: inpaint with an uploaded image and a painted mask", async ({ page }) => {
  await open(page);
  await pickModel(page, "sd-checkpoint");
  await page.getByRole("tab", { name: "Inpaint" }).click();
  await page.locator('[data-input="prompt"]').fill("a cat on the sofa");
  await page.locator('input[data-input="image"]').setInputFiles({ name: "room.png", mimeType: "image/png", buffer: png(320, 240, [40, 120, 200]) });
  await expect(page.getByText("Chosen. Drop another to replace.").first()).toBeVisible();
  await expect(page.getByRole("button", { name: /Mask/ })).toHaveAttribute("aria-pressed", "true");
  const stage = page.getByTestId("canvas").locator("canvas").last();
  const b = (await stage.boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.4, b.y + b.height * 0.4);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width * 0.6, b.y + b.height * 0.6, { steps: 8 });
  await page.mouse.up();
  // The painted mask is uploaded and filled in automatically.
  await expect(page.getByText("Needs:")).toBeHidden({ timeout: 5000 });
  await shot(page, "06-studio-inpaint");
  await page.getByTestId("create").click();
  await page.getByRole("tab", { name: "Results" }).click();
  await expect(page.getByTestId("result").locator("img")).toBeVisible({ timeout: 30_000 });
});

test("ultra settings explain the memory plan", async ({ page }) => {
  await open(page);
  await pickModel(page, "wan22-14b");
  await page.getByRole("tab", { name: "Ultra" }).click();
  await expect(page.getByTestId("ultra")).toBeVisible();
  await page.getByTestId("plan").getByText("Why?").click();
  await expect(page.getByTestId("plan").locator("li").first()).toBeVisible();
  await shot(page, "07-ultra");
});

test("queue: cancel a running job", async ({ page }) => {
  await open(page);
  await pickModel(page, "sd-checkpoint");
  await page.getByRole("tab", { name: "Advanced" }).click();
  await page.locator('[data-input="prompt"]').fill("slow one");
  await page.locator('[data-input="steps"]').fill("150");
  await page.getByTestId("create").click();
  await page.locator('[data-nav="queue"]').click();
  await expect(page.getByTestId("queue").getByText("running")).toBeVisible();
  await shot(page, "08-queue");
  await page.getByRole("button", { name: "Cancel" }).first().click();
  await expect(page.getByTestId("queue").getByText("cancelled").first()).toBeVisible({ timeout: 15_000 });
});

test("models page shows fit, files and downloads", async ({ page }) => {
  await open(page);
  await page.locator('[data-nav="models"]').click();
  for (const id of ["sd-checkpoint", "z-image-turbo", "ideogram-v4", "flux2-klein-4b", "wan22-14b"]) {
    await expect(page.locator(`[data-model-card="${id}"]`)).toBeVisible();
  }
  await shot(page, "09-models");
  await page.getByRole("tab", { name: "Files on disk" }).click();
  await shot(page, "10-files");
});

test("import an official blueprint and run it as a tool", async ({ page }) => {
  await open(page);
  await page.locator('[data-nav="tools"]').click();
  const bp = readFileSync(resolve(repo, "tests/fixtures/blueprints/Text to Image (Z-Image-Turbo).json"), "utf8");
  await page.getByTestId("workflow-json").fill(bp);
  await page.getByTestId("import").click();
  await expect(page.getByText("This workflow already says which settings matter")).toBeVisible();
  await page.getByRole("tab", { name: "Graph" }).click();
  await expect(page.getByTestId("graph").locator(".react-flow__node").first()).toBeVisible();
  await shot(page, "11-workflow-graph");
  await page.getByRole("tab", { name: "Settings to show" }).click();
  await page.getByTestId("publish").click();
  await expect(page.getByTestId("model-picker")).toContainText("Text to Image (Z-Image-Turbo)");
  await page.locator('[data-input="prompt"]').fill("a tool run");
  await page.getByTestId("create").click();
  await expect(page.getByTestId("recent").locator("img").first()).toBeVisible({ timeout: 30_000 });
});

test("settings persist", async ({ page }) => {
  await open(page);
  await page.locator('[data-nav="settings"]').click();
  await expect(page.getByTestId("engine")).toBeVisible();
  const slider = page.locator('input[type="range"]').first();
  await slider.fill("2");
  await page.getByRole("button", { name: "Save" }).first().click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  await shot(page, "12-settings");
  await page.reload();
  await page.locator('[data-nav="settings"]').click();
  await expect(page.getByText("2.0 GB")).toBeVisible();
});
