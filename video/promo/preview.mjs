// Save single frames at the given times: node preview.mjs <lang> <t1> <t2> ...
import { createRequire } from "node:module";
import { spawn } from "node:child_process";
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_CORE || "playwright-core");
const [lang, ...times] = process.argv.slice(2);
const server = spawn("python3", ["-m", "http.server", "8799", "--bind", "127.0.0.1"], { stdio: "ignore" });
await new Promise((r) => setTimeout(r, 800));
const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
await page.goto(`http://127.0.0.1:8799/index.html?lang=${lang}`);
await page.evaluate(() => window.ready);
for (const t of times) {
  await page.evaluate(async (time) => { window.seek(time); await Promise.all([...document.images].filter((i) => !i.complete).map((i) => new Promise((d) => { i.onload = i.onerror = d; }))); }, Number(t));
  await page.screenshot({ path: `out/preview-${lang}-${String(t).padStart(5, "0")}.jpg`, type: "jpeg", quality: 85 });
}
await browser.close(); server.kill();
