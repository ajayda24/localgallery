// A stand-in for the parts of the Hugging Face API the app uses, for the browser tests:
// repo search, recursive file listings, and resolve URLs (HEAD for sizes, GET for downloads).
import { createServer } from "node:http";

const PORT = Number(process.env.MOCK_HUB_PORT ?? 7892);
const GiB = 1024 ** 3;

const repos = [
  { id: "Comfy-Org/z_image_turbo", lastModified: "2026-10-01T00:00:00.000Z", downloads: 182000, likes: 410, gated: false },
  { id: "Comfy-Org/MOSS-Image", lastModified: "2026-10-20T00:00:00.000Z", downloads: 950, likes: 31, gated: false },
  { id: "Comfy-Org/flux2-dev", lastModified: "2026-09-02T00:00:00.000Z", downloads: 52000, likes: 120, gated: false },
];

// Files of the z_image_turbo repo; the VAE is small enough to really download.
const VAE_BYTES = 4096;
const trees = {
  "Comfy-Org/z_image_turbo": [
    { type: "file", path: "README.md", size: 1200 },
    { type: "file", path: "split_files/diffusion_models/z_image_turbo_bf16.safetensors", size: 135, lfs: { size: Math.round(11.46 * GiB) } },
    { type: "file", path: "split_files/text_encoders/qwen_3_4b.safetensors", size: 135, lfs: { size: Math.round(7.49 * GiB) } },
    { type: "file", path: "split_files/vae/ae.safetensors", size: VAE_BYTES },
    { type: "file", path: "extra/z_image_turbo-Q4_K_M.gguf", size: Math.round(3.9 * GiB) },
  ],
};

// Sizes the app looks up for the built-in models (only Z-Image here: others stay unknown).
function sizeOf(path) {
  for (const files of Object.values(trees)) {
    const f = files.find((x) => path.endsWith(x.path));
    if (f) return f.lfs?.size ?? f.size;
  }
  return null;
}

createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const json = (v, code = 200) => { res.writeHead(code, { "content-type": "application/json" }); res.end(JSON.stringify(v)); };
  if (url.pathname === "/api/models") {
    const q = (url.searchParams.get("search") ?? "").toLowerCase();
    return json(repos.filter((r) => r.id.toLowerCase().includes(q)));
  }
  const tree = url.pathname.match(/^\/api\/models\/([^/]+\/[^/]+)\/tree\/main$/);
  if (tree) return trees[tree[1]] ? json(trees[tree[1]]) : json({ error: "not found" }, 404);
  const resolve = url.pathname.match(/^\/([^/]+\/[^/]+)\/resolve\/main\/(.+)$/);
  if (resolve) {
    const path = decodeURIComponent(resolve[2]);
    const size = sizeOf(path);
    if (size == null) { res.writeHead(404); return res.end(); }
    if (req.method === "HEAD") { res.writeHead(302, { "x-linked-size": String(size), location: "/cdn/blob" }); return res.end(); }
    if (!path.endsWith("ae.safetensors")) { res.writeHead(403); return res.end(); }
    res.writeHead(200, { "content-length": String(VAE_BYTES), "content-type": "application/octet-stream" });
    return res.end(Buffer.alloc(VAE_BYTES, 7));
  }
  if (url.pathname === "/health") return json({ ok: true });
  res.writeHead(404);
  res.end();
}).listen(PORT, "127.0.0.1");
