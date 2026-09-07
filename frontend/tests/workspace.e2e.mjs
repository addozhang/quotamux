import { chromium, expect } from "@playwright/test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
const root = resolve(import.meta.dirname, "../..");
const artifacts = resolve(root, "data/e2e");
await mkdir(artifacts, { recursive: true });
const dir = await mkdtemp(artifacts + "/workspace-");
const base = "http://127.0.0.1:8081",
  key = "workspace-fake-key";
let child,
  browser,
  logs = "",
  requests = 0;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mock = createServer(async (req, res) => {
  requests++;
  let body = "";
  for await (const part of req) body += part;
  if (req.headers.authorization !== `Bearer ${key}`) {
    res.writeHead(401, { "Content-Type": "application/json" });
    res.end('{"error":{"message":"invalid key"}}');
    return;
  }
  const data = JSON.parse(body);
  if (data.stream) {
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    res.end(
      'data: {"id":"test","choices":[{"index":0,"delta":{"content":"OK"},"finish_reason":null}]}\n\ndata: {"id":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n',
    );
  } else {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        id: "test",
        object: "chat.completion",
        model: data.model,
        choices: [
          {
            index: 0,
            message: { role: "assistant", content: "OK" },
            finish_reason: "stop",
          },
        ],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      }),
    );
  }
});
const mockHost = process.env.QUOTAMUX_E2E_UPSTREAM_HOST || "127.0.0.1";
const field = (page, p) => page.locator(`[data-field="${p}"]`);
const action = (page, a, p) =>
  page.locator(
    `[data-action="${a}"]${p === undefined ? "" : `[data-path="${p}"]`}`,
  );
async function enableEditing(page) {
  await page.goto(base);
  await expect(field(page, "models.0.name")).toBeDisabled();
  await expect(action(page, "add-model")).toBeDisabled();
  await action(page, "accounts").first().click();
  await expect(action(page, "add-backend")).toBeDisabled();
  await page
    .getByRole("button", { name: "Close accounts", exact: true })
    .last()
    .click();
  await action(page, "enable-edit").click();
  await expect(field(page, "models.0.name")).toBeEnabled();
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.name")).toBeDisabled();
  await action(page, "enable-edit").click();
  const original = await field(page, "models.0.aliases").inputValue();
  await field(page, "models.0.aliases").fill("pending-edit");
  page.once('dialog', dialog=>dialog.dismiss());
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.aliases")).toHaveValue("pending-edit");
  await expect(field(page, "models.0.aliases")).toBeEnabled();
  page.once('dialog', dialog=>dialog.accept());
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.aliases")).toHaveValue(original);
  await expect(field(page, "models.0.aliases")).toBeDisabled();
  await action(page, "enable-edit").click();
  await expect(page.getByTestId("save-workspace")).toBeEnabled();
}
async function save(page, status = 200) {
  const response = page.waitForResponse(
    (r) =>
      r.request().method() === "PUT" &&
      new URL(r.url()).pathname === "/api/routing",
  );
  await page.getByTestId("save-workspace").click();
  const result = await response;
  assert.equal(
    result.status(),
    status,
    result.status() === status ? undefined : await result.text(),
  );
  await expect(page.getByTestId("save-workspace")).toBeEnabled();
  if (status === 200)
    await expect(page.locator("#notice")).toContainText("Saved.");
}
async function physicalClick(page, locator) {
  await locator.scrollIntoViewIfNeeded();
  const box = await locator.boundingBox();
  assert(box, 'click target must be visible');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();
}
async function infer(model = "public") {
  const r = await fetch(base + "/v1/chat/completions", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "x-relay-include-metadata": "1",
    },
    body: JSON.stringify({
      model,
      messages: [{ role: "user", content: "OK" }],
    }),
  });
  assert.equal(r.status, 200);
  await r.json();
  return r;
}
try {
  const guard = createServer();
  await new Promise((r, j) => {
    guard.on("error", j);
    guard.listen(8081, "127.0.0.1", r);
  });
  await new Promise((r) => guard.close(r));
  await new Promise((r) => mock.listen(0, "127.0.0.1", r));
  await writeFile(
    dir + "/config.toml",
    `config_version=3\n[server]\nlisten="127.0.0.1:8081"\ndata_dir="${dir}/data"\n[[backends]]\nid="mock"\nadapter="custom-chat-completions"\nendpoint="http://${mockHost}:${mock.address().port}/chat"\n[[backends.credentials]]\nid="primary"\napi_key="${key}"\n[[backends.models]]\nname="upstream"\nprotocols=["openai-chat"]\n[[models]]\nname="public"\nprotocols=["openai-chat"]\n[[models.layers]]\nname="first"\nstrategy="random"\ntargets=[{backend="mock",credential="primary",model="upstream"}]\n`,
    { mode: 0o600 },
  );
  child = spawn(
    process.env.QUOTAMUX_E2E_BINARY || root + "/target/debug/quotamux",
    ["--config", dir + "/config.toml"],
  );
  child.stdout.on("data", (b) => (logs += b));
  child.stderr.on("data", (b) => (logs += b));
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) throw Error(logs);
    try {
      if ((await fetch(base + "/api/routing")).ok) break;
    } catch {}
    await sleep(100);
  }
  browser = await chromium.launch({
    executablePath:
      process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ||
      "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  });
  const page = await browser.newPage({
    viewport: { width: 1280, height: 900 },
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await enableEditing(page);
  await expect(
    page.getByRole("heading", { name: "Routing layers", exact: true }),
  ).toBeVisible();
  assert.equal(
    await page
      .getByRole("button", { name: "Configuration", exact: true })
      .count(),
    0,
  );
  await field(page, "models.0.aliases").fill("draft-alias");
  await sleep(5300);
  await expect(field(page, "models.0.aliases")).toHaveValue("draft-alias");
  await action(page, "accounts").first().click();
  await action(page, "add-backend").click();
  await field(page, "backends.1.id").fill("extra-account");
  await physicalClick(page, action(page,"add-credential","backends.1.credentials"));
  await expect(field(page,"backends.1.credentials.0.id")).toBeVisible();
  await field(page, "backends.1.endpoint").fill(
    `http://${mockHost}:${mock.address().port}/chat`,
  );
  await field(page, "backends.1.credentials.0.id").fill("extra");
  await field(page, "backends.1.credentials.0.id").blur();
  await field(page, "backends.1.credentials.0.api_key").fill(key);
  await action(page, "add-upstream", "backends.1.models").click();
  await field(page, "backends.1.models.0.name").fill("upstream");
  await field(page, "backends.1.models.0.name").blur();
  await save(page);
  await action(page, "remove", "backends.1").click();
  await save(page);
  await action(page, "add-pool").click();
  await field(page, "capacity_pools.0.id").fill("shared");
  await field(page, "capacity_pools.0.id").blur();
  await field(page, "capacity_pools.0.max_in_flight").fill("2");
  await field(page, "backends.0.credentials.0.pool").selectOption("shared");
  await action(page, "add-credential", "backends.0.credentials").click();
  await field(page, "backends.0.credentials.1.id").fill("second");
  await field(page, "backends.0.credentials.1.id").blur();
  await field(page, "backends.0.credentials.1.api_key").fill(key);
  await field(page, "backends.0.credentials.1.pool").selectOption("shared");
  await action(page, "add-upstream", "backends.0.models").click();
  await field(page, "backends.0.models.1.name").fill("upstream-two");
  await field(page, "backends.0.models.1.name").blur();
  await action(page, "add-pricing", "backends.0.models.1").click();
  await field(page, "backends.0.models.1.pricing.output_usd_per_million").fill(
    "1",
  );
  await field(page,'backends.0.models.1.pricing_note').fill('Subscription quota usage value');
  await field(page,'backends.0.models.1.pricing_source').fill('https://example.test/rates');
  await action(page,'add-price-window','backends.0.models.1').click();
  await field(page,'backends.0.models.1.pricing_schedule.windows.0.name').fill('peak');
  await field(page,'backends.0.models.1.pricing_schedule.windows.0.name').blur();
  await field(page,'backends.0.models.1.pricing_schedule.windows.0.pricing.output_usd_per_million').fill('9');
  await page.getByText("Advanced runtime settings", { exact: true }).click();
  await expect(
    field(page, "server.timeouts.upstream_connect_ms"),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Close accounts", exact: true })
    .last()
    .click();
  await action(page, "add-layer", "models.0.layers").click();
  await field(page, "models.0.layers.1.name").fill("second");
  await physicalClick(page, action(page, "add-target", "models.0.layers.1.targets"));
  await expect(field(page,"models.0.layers.1.targets.0.backend")).toBeVisible();
  await field(page, "models.0.layers.1.targets.0.backend").selectOption("mock");
  await field(page, "models.0.layers.1.targets.0.credential").selectOption(
    "second",
  );
  await field(page, "models.0.layers.1.targets.0.model").selectOption(
    "upstream-two",
  );
  await action(page, "add-schedule", "models.0.schedules").click();
  await field(page, "models.0.schedules.0.name").fill("daytime");
  await field(page, "models.0.schedules.0.name").blur();
  await field(page, "models.0.schedules.0.order.0").selectOption("second");
  await expect(field(page, "models.0.schedules.0.order.1")).toHaveValue(
    "first",
  );
  await save(page);
  assert.equal(
    (await infer("draft-alias")).headers.get("x-relay-upstream-model") !== null,
    true,
  );
  await page.locator("#preview-at").fill("2026-09-07T10:00:00Z");
  await action(page, "preview").click();
  await expect(page.locator("#preview-result")).toContainText(
    "daytime: second → first",
  );
  await field(page, "models.0.layers.1.name").fill("renamed");
  await field(page, "models.0.layers.1.name").blur();
  await expect(field(page, "models.0.schedules.0.order.0")).toHaveValue(
    "renamed",
  );
  await save(page);
  console.log(
    "PASS unified edits, account credentials/pools/models/pricing, schedule selectors and preview, dirty polling",
  );
  const priced=await(await fetch(base+'/api/routing')).json();
  const pricedModel=priced.document.backends[0].models[1];
  assert.equal(pricedModel.pricing_note,'Subscription quota usage value');
  assert.equal(pricedModel.pricing_source,'https://example.test/rates');
  for(const [at,rate,active] of [['2026-09-07T08:59:59Z',1,null],['2026-09-07T09:00:00Z',9,'peak'],['2026-09-07T17:00:00Z',1,null]]){
    const preview=await(await fetch(base+'/api/routing?at='+encodeURIComponent(at))).json();
    const price=preview.pricing.find(row=>row.backend==='mock'&&row.model==='upstream-two');
    assert.equal(price.rates.output_usd_per_million,rate);assert.equal(price.active_window,active);
  }
  await action(page,'accounts').first().click();
  await action(page,'remove-pricing','backends.0.models.1').click();
  await save(page);
  const removed=(await(await fetch(base+'/api/routing')).json()).document.backends[0].models[1];
  assert(!removed.pricing_schedule);assert(!removed.pricing);assert.equal(removed.pricing_note,'Subscription quota usage value');
  await page.getByRole('button',{name:'Close accounts',exact:true}).last().click();
  const before = await readFile(dir + "/config.toml", "utf8");
  await action(page, "accounts").first().click();
  await field(page, "backends.0.credentials.0.api_key").fill("invalid-key");
  await save(page, 422);
  assert.equal(await readFile(dir + "/config.toml", "utf8"), before);
  await expect(field(page, "backends.0.credentials.0.api_key")).toHaveValue(
    "invalid-key",
  );
  await field(page, "backends.0.credentials.0.api_key").fill("");
  await page
    .getByRole("button", { name: "Close accounts", exact: true })
    .last()
    .click();
  const clean = await browser.newPage();
  await enableEditing(clean);
  const stale = await browser.newPage();
  await enableEditing(stale);
  await field(stale, "models.0.aliases").fill("stale-edit");
  await field(page, "models.0.aliases").fill("fresh-edit");
  await save(page);
  await sleep(5300);
  await expect(field(clean, "models.0.aliases")).toHaveValue("fresh-edit");
  await expect(stale.locator("#notice")).toContainText(
    "Routing changed elsewhere",
  );
  await clean.close();
  await save(stale, 409);
  await expect(field(stale, "models.0.aliases")).toHaveValue("stale-edit");
  await stale.close();
  await action(page, "add-model").click();
  await field(page, "models.1.name").fill("second-public");
  await physicalClick(page, action(page, "add-layer", "models.1.layers"));
  await expect(field(page,"models.1.layers.0.name")).toBeVisible();
  await field(page, "models.1.layers.0.name").fill("route");
  await field(page, "models.1.layers.0.name").blur();
  await action(page, "add-target", "models.1.layers.0.targets").click();
  await field(page, "models.1.layers.0.targets.0.backend").selectOption("mock");
  await field(page, "models.1.layers.0.targets.0.credential").selectOption(
    "primary",
  );
  await field(page, "models.1.layers.0.targets.0.model").selectOption(
    "upstream",
  );
  await action(page, "select", "0").click();
  await action(page, "select", "1").click();
  await expect(field(page, "models.1.name")).toHaveValue("second-public");
  await save(page);
  await page.locator('#preview-at').fill('2026-09-07T10:00:00Z');
  await action(page,'preview').click();
  await expect(page.locator('#preview-result')).toContainText('mock/upstream:');
  await expect(page.locator('#preview-result')).not.toContainText('mock/upstream-two:');
  await page.route('**/api/routing?at=*',async route=>{await sleep(500);await route.continue();});
  await action(page,'preview').click();
  await action(page,'select','0').click();
  await sleep(700);
  await expect(page.locator('#preview-result')).toHaveText('');
  await page.unroute('**/api/routing?at=*');
  for (let i = 0; i < 25; i++) {
    await infer("second-public");
    await infer("fresh-edit");
  }
  await action(page, "select", "0").click();
  await action(page, "view", "statistics").click();
  await expect(
    page.getByRole("heading", { name: "Model statistics", exact: true }),
  ).toBeVisible();
  await action(page, "window", "all").click();
  await action(page, "metric", "output_tokens").click();
  await expect(
    page.getByRole("heading", { name: "Account totals" }),
  ).toHaveCount(0);
  await action(page, "select", "all").click();
  await expect(action(page, "view", "routing")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Account totals" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Active alerts" }),
  ).toBeVisible();
  await action(page, "view", "history").click();
  await expect(page.locator("tbody")).toContainText("public");
  await expect(page.locator("tbody")).toContainText("second-public");
  await action(page, "select", "0").click();
  await expect(page.getByText("Page 1", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).not.toContainText("second-public");
  await expect(action(page, "older")).toBeEnabled();
  await action(page, "older").click();
  await expect(page.getByText("Page 2", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).not.toContainText("second-public");
  await action(page, "select", "1").click();
  await expect(page.getByText("Page 1", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).toContainText("second-public");
  await page.route("**/api/requests?**", async (route) => {
    if (new URL(route.request().url()).searchParams.get("model") === "public")
      await sleep(500);
    await route.continue();
  });
  await action(page, "select", "0").click();
  await action(page, "select", "1").click();
  await expect(page.locator("tbody")).toContainText("second-public");
  await sleep(700);
  assert(
    (await page.locator("tbody tr td:nth-child(2)").allTextContents()).every(
      (name) => name === "second-public",
    ),
  );
  await page.unroute("**/api/requests?**");
  const external=await(await fetch(base+'/api/routing')).json();
  external.document.models.reverse();
  const reordered=await fetch(base+'/api/routing',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({revision:external.revision,document:external.document})});
  assert.equal(reordered.status,200);
  await sleep(5300);
  await expect(action(page,'select','0')).toHaveAttribute('aria-current','page');
  assert((await page.locator('tbody tr td:nth-child(2)').allTextContents()).every(name=>name==='second-public'));
  await expect(page.locator('tbody')).toContainText('second-public');
  await action(page, "view", "routing").click();
  await action(page, "remove", "models.0").click();
  await save(page);
  await page.screenshot({
    path: artifacts + "/routing-workspace-desktop.png",
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  assert(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
    "mobile page overflow",
  );
  await page.screenshot({
    path: artifacts + "/routing-workspace-mobile.png",
    fullPage: true,
  });
  assert.deepEqual(errors, []);
  console.log(
    "PASS invalid-key persistence protection, stale writer, model CRUD, stats/history, mobile and console",
  );
} finally {
  await browser?.close();
  if (child && child.exitCode === null) {
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await exited;
  }
  mock.closeAllConnections();
  await new Promise((r) => mock.close(r));
  await writeFile(artifacts + "/workspace-gateway.log", logs);
  await rm(dir, { recursive: true, force: true });
}
