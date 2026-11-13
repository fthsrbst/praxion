// Step the composition frame by frame in headless Chrome and save JPEG frames.
// Usage: node render.mjs <lang> <captions 0|1> <frames dir> <duration s> <fps>
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { createRequire } from "node:module";

const [lang, captions, framesDir, durationText, fpsText] = process.argv.slice(2);
const root = new URL(".", import.meta.url).pathname;
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_CORE || "playwright-core");

const types = { ".html": "text/html", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".js": "text/javascript" };
const server = createServer(async (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, "http://x").pathname)).replace(/^\/+/, "") || "index.html";
  try {
    const body = await readFile(join(root, path));
    response.writeHead(200, { "Content-Type": types[extname(path)] || "application/octet-stream" });
    response.end(body);
  } catch {
    response.writeHead(404); response.end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const port = server.address().port;

const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
await page.goto(`http://127.0.0.1:${port}/index.html?lang=${lang}&captions=${captions}`);
await page.evaluate(() => window.ready);
const fps = Number(fpsText), total = Math.round(Number(durationText) * fps);
for (let frame = 0; frame < total; frame++) {
  await page.evaluate(async (t) => {
    window.seek(t);
    // Wait for every image the frame switched to (footage frames) before the shot.
    await Promise.all([...document.images].filter((img) => !img.complete).map((img) => new Promise((done) => { img.onload = img.onerror = done; })));
  }, frame / fps);
  await page.screenshot({ path: join(framesDir, String(frame + 1).padStart(5, "0") + ".jpg"), type: "jpeg", quality: 92 });
  if (frame % 150 === 0) console.log(`frame ${frame}/${total}`);
}
await browser.close();
server.close();
