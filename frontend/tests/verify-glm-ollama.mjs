// Read-only production check for the GLM + Ollama Cloud configuration change.
// Usage: node tests/verify-glm-ollama.mjs [baseUrl] [screenshotDir]
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const base = (process.argv[2] || "http://127.0.0.1:8080").replace(/\/$/, "");
const shots = resolve(process.argv[3] || "data/e2e-glm-ollama");
await mkdir(shots, { recursive: true });

const browser = await chromium.launch({
  executablePath:
    process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ||
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
const errors = [];
const writes = [];
page.on("pageerror", (error) => errors.push(error.message));
page.on("request", (request) => {
  if (request.method() !== "GET") writes.push(`${request.method()} ${request.url()}`);
});
const evidence = {};

await page.goto(base);
await page.getByRole("heading", { name: "All statistics", exact: true }).waitFor();

// Accounts: the Ollama Cloud tab must expose both GLM models and their notes.
await page.locator('[data-action="accounts"]').click();
const tabs = page.locator('[data-action="account-tab"]');
const count = await tabs.count();
let ollamaTab = null;
for (let i = 0; i < count; i += 1) {
  const tab = tabs.nth(i);
  const label = (await tab.innerText()).trim();
  if (/ollama/i.test(label)) ollamaTab = tab;
}
assert.ok(ollamaTab, "an Ollama Cloud account tab exists");
await ollamaTab.click();
const panel = page.locator("#account-panel");
const panelText = await panel.innerText();
evidence.ollamaAccountPanel = panelText;
for (const needle of [
  "glm-5.3-flash",
  "glm-5.3",
  "no peak window",
  "0.03",
  "0.15",
  "0.5",
  "0.26",
  "1.4",
  "4.4",
])
  assert.ok(panelText.includes(needle), `Ollama account panel shows ${needle}`);
await page.screenshot({ path: shots + "/production-accounts-ollama.png", fullPage: true });
console.log("PASS ollama-cloud account lists both GLM models with their verified rates");

// Routing: both GLM routes must show the three layers in the approved order.
await page.locator('[data-action="workspace"]').click();
const modelButtons = page.locator('[data-action="select"]');
const modelCount = await modelButtons.count();
evidence.routing = {};
for (const model of ["glm-5.3-flash", "glm-5.3"]) {
  let found = null;
  for (let i = 0; i < modelCount; i += 1) {
    const button = modelButtons.nth(i);
    if ((await button.innerText()).trim() === model) found = button;
  }
  assert.ok(found, `model rail lists ${model}`);
  await found.click();
  await page.locator('[data-action="view"][data-path="routing"]').click();
  const text = await page.locator("main").innerText();
  evidence.routing[model] = text;
  // The UI displays zhipu-coding-plan as "GLM Coding Plan".
  const shown = text.replace(/GLM Coding Plan/g, "zhipu-coding-plan");
  const zhipu = shown.indexOf("zhipu-coding-plan");
  const go = shown.indexOf("opencode-go-plan");
  const ollama = shown.indexOf("ollama-cloud");
  assert.ok(zhipu >= 0 && go >= 0 && ollama >= 0, `${model} shows all three layers`);
  assert.ok(zhipu < go && go < ollama, `${model} keeps zhipu-coding-plan → opencode-go-plan → ollama-cloud`);
  const order = text.match(/Live priority: ([^·]+)/)?.[1]?.trim();
  evidence.livePriority = evidence.livePriority || {};
  evidence.livePriority[model] = order;
  assert.equal(
    order,
    "GLM Coding Plan → opencode-go-plan → ollama-cloud",
    `${model} live priority display`,
  );
}
await page.screenshot({ path: shots + "/production-routing-glm-5.3.png", fullPage: true });
console.log("PASS both GLM routes keep GLM Coding Plan first, then Go, then Ollama Cloud");

assert.deepEqual(writes, [], "the check issued no configuration writes");
assert.deepEqual(errors, [], "no page errors");
await writeFile(shots + "/ui-evidence.json", JSON.stringify(evidence, null, 2));
await browser.close();
console.log("PASS read-only browser check completed with no writes and no page errors");
