import { defineConfig } from "@playwright/test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));

// The real core binary, with the fake engine serving the real ComfyUI node set,
// on a fresh portable folder whose name has spaces and non-ASCII characters.
// Set LG_COMFY_DIR to a ComfyUI checkout to also get its official workflows as tools.
const repo = resolve(here, "../..");
const root = join(mkdtempSync(join(tmpdir(), "lg-e2e-")), "Local Gallery ü");
const exe = process.env.LG_CORE_EXE ?? join(repo, "target/debug/localgallery-core" + (process.platform === "win32" ? ".exe" : ""));
export const PORT = 7891;

export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  workers: 1,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    viewport: { width: 1440, height: 900 },
    launchOptions: process.env.LG_CHROMIUM ? { executablePath: process.env.LG_CHROMIUM } : {},
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: `"${exe}" --root "${root}" --engine fake --fake-object-info "${join(repo, "tests/fixtures/object_info.json")}" --fake-step-ms 30 --manifests "${join(repo, "manifests")}" --ui "${join(here, "dist")}" --port ${PORT} --token e2e`,
    url: `http://127.0.0.1:${PORT}/api/health`,
    reuseExistingServer: false,
    timeout: 60_000,
    env: { ...process.env, RUST_LOG: "warn" } as Record<string, string>,
  },
});
