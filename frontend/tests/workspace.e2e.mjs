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
const heldRequests = [];
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
  if (data.messages?.[0]?.content === "HOLD") await new Promise(resolve => heldRequests.push(resolve));
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
  await expect(page.getByRole("heading", {name:"All statistics", exact:true})).toBeVisible();
  await expect(action(page, "window", "1d")).toHaveAttribute("aria-pressed", "true");
  await expect(action(page, "add-model")).toHaveCount(0);
  await expect(page.locator("footer")).toHaveCount(0);
  await page.screenshot({path:artifacts+"/all-statistics-default.png",fullPage:true});
  await action(page, "accounts").click();
  await expect(page.locator(".accounts-page")).toBeVisible();
  await expect(page.locator(".model-rail, .child-nav, .drawer-backdrop")).toHaveCount(0);
  await expect(action(page, "add-backend")).toHaveCount(0);
  await expect(page.locator('.accounts-page input,.accounts-page select')).toHaveCount(0);
  await expect(action(page, 'account-tab', 'backend:0')).toHaveAttribute('aria-selected','true');
  await expect(action(page,'account-tab','pools')).toHaveCount(0);
  await expect(action(page,'account-tab','runtime')).toHaveCount(0);
  await action(page,'settings').click();
  await expect(page.getByRole('heading',{name:'Settings',exact:true})).toBeVisible();
  await expect(page.locator('main input,main select,main [data-action]')).toHaveCount(0);
  await expect(page.locator('main')).toContainText('Upstream connection timeout (ms)');
  await expect(page.locator('.account-tabs,.model-rail,.child-nav')).toHaveCount(0);
  await action(page, "workspace").click();
  await action(page, "select", "0").click();
  await expect(page.getByRole("heading", {name:"Model statistics", exact:true})).toBeVisible();
  await expect(action(page, "window", "1d")).toHaveAttribute("aria-pressed", "true");
  await action(page, "view", "routing").click();
  await expect(page.locator("#effective-order")).toContainText("Live priority:");
  await expect(page.locator("main input, main select, main [data-action]")).toHaveCount(3); // only child navigation
  await expect(field(page, "models.0.name")).toHaveCount(0);
  await page.screenshot({path:artifacts+"/routing-readonly.png",fullPage:true});
  await action(page, "enable-edit").click();
  await expect(field(page, "models.0.name")).toBeEnabled();
  await expect(action(page, "add-model")).toBeVisible();
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.name")).toHaveCount(0);
  await action(page, "enable-edit").click();
  const original = await field(page, "models.0.aliases").inputValue();
  await field(page, "models.0.aliases").fill("pending-edit");
  page.once('dialog', dialog=>dialog.dismiss());
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.aliases")).toHaveValue("pending-edit");
  await expect(field(page, "models.0.aliases")).toBeEnabled();
  page.once('dialog', dialog=>dialog.accept());
  await action(page, "disable-edit").click();
  await expect(field(page, "models.0.aliases")).toHaveCount(0);
  await action(page, "enable-edit").click();
  await expect(field(page, "models.0.aliases")).toHaveValue(original);
  await expect(page.getByTestId("save-workspace")).toBeEnabled();
}
async function save(page, status = 200) {
  const response = page.waitForResponse(
    (r) =>
      r.request().method() === "PUT" &&
      new URL(r.url()).pathname === "/api/routing",
  );
  page.once("dialog", dialog => dialog.accept());
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
async function auditConfirmations(browser) {
  const page = await browser.newPage();
  await enableEditing(page);
  let puts = 0;
  page.on('request', request => {
    if (request.method() === 'PUT') puts++;
  });
  const baseline = await (await fetch(base + '/api/routing')).json();
  await field(page, 'models.0.aliases').fill('cancel-save');
  let saveDialog = '';
  page.once('dialog', async dialog => {saveDialog = dialog.message(); await dialog.dismiss();});
  await page.getByTestId('save-workspace').click();
  assert.match(saveDialog, /immediately/);
  await expect(field(page, 'models.0.aliases')).toHaveValue('cancel-save');
  await expect(page.locator('#draft-status')).toHaveText('Unsaved changes');
  assert.equal(puts, 0);
  assert.equal((await (await fetch(base + '/api/routing')).json()).revision, baseline.revision);
  page.once('dialog', dialog => dialog.dismiss());
  await action(page, 'discard').click();
  await expect(field(page, 'models.0.aliases')).toHaveValue('cancel-save');
  page.once('dialog', dialog => dialog.accept());
  await action(page, 'discard').click();
  await expect(page.locator('#draft-status')).toHaveText('All changes saved');

  const snapshot = () => page.locator('main').evaluate(main => ({
    fields: [...main.querySelectorAll('input,select')].map(n => [n.dataset.field || n.dataset.choice || n.id, n.value, n.checked]),
    actions: [...main.querySelectorAll('[data-action]')].map(n => [n.dataset.action, n.dataset.path]),
  }));
  const cases = [
    ['workspace', 'remove', 'models.0', 'model'],
    ['workspace', 'remove', 'models.0.layers.0', 'layer'],
    ['workspace', 'remove', 'models.0.layers.0.targets.0', 'target'],
    ['workspace', 'remove', 'models.0.schedules.0', 'time window'],
    ['accounts', 'remove', 'backends.0.credentials.0', 'credential'],
    ['accounts', 'remove', 'backends.0.models.1', 'upstream model'],
    ['accounts', 'remove', 'backends.0.models.1.pricing_schedule.windows.0', 'price window'],
    ['accounts', 'remove', 'capacity_pools.0', 'shared capacity pool'],
    ['accounts', 'remove-pricing', 'backends.0.models.1', 'published rates'],
  ];
  for (const [scope, kind, path, label] of cases) {
    await action(page, scope).click();
    if (scope === 'workspace') {
      await action(page, 'select', '0').click();
      await action(page, 'view', 'routing').click();
    } else {
      await action(page, 'account-tab', 'backend:0').click();
    }
    const before = await snapshot();
    let message = '';
    page.once('dialog', async dialog => {message = dialog.message(); await dialog.dismiss();});
    await action(page, kind, path).click();
    assert(message.includes(label), `confirmation identifies ${label}: ${message}`);
    assert(message.includes('draft until Save'), 'delete explains when it takes effect');
    assert.deepEqual(await snapshot(), before, `${label}: cancel preserves draft`);
    await expect(page.locator('#draft-status')).toHaveText('All changes saved');
    page.once('dialog', dialog => dialog.accept());
    await action(page, kind, path).click();
    await expect(page.locator('#draft-status')).toHaveText('Unsaved changes');
    assert.notDeepEqual(await snapshot(), before, `${label}: confirmed deletion changes draft`);
    assert.equal(puts, 0, 'draft deletion must not issue a write');
    assert.equal((await (await fetch(base + '/api/routing')).json()).revision, baseline.revision);
    page.once('dialog', dialog => dialog.accept());
    await action(page, 'discard').click();
    await expect(page.locator('#draft-status')).toHaveText('All changes saved');
  }
  await page.close();
  console.log('PASS Save/Discard cancellation and 9 delete kinds cancel/confirm without premature writes; unused account covered separately');
}
async function auditAccountDependencies(browser) {
  const baseline = await (await fetch(base+'/api/routing')).json();
  const bytes = await readFile(dir+'/config.toml');
  const invalid = structuredClone(baseline.document);
  const retained = structuredClone(invalid.backends[0]);
  retained.id = 'unreferenced-account';
  retained.credentials.forEach(credential => { credential.api_key = key; });
  invalid.backends.splice(0,1);
  invalid.backends.push(retained);
  const rejected = await fetch(base+'/api/routing',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({revision:baseline.revision,document:invalid})});
  const rejection = await rejected.text();
  assert.equal(rejected.status,422,rejection);
  assert.match(rejection,/references missing backend mock/);
  const unchanged = await (await fetch(base+'/api/routing')).json();
  assert.equal(unchanged.revision,baseline.revision);
  assert.deepEqual(unchanged.document,baseline.document);
  assert.deepEqual(await readFile(dir+'/config.toml'),bytes,'invalid account removal cannot persist');
  const page = await browser.newPage({viewport:{width:1280,height:900}});
  await enableEditing(page);
  let puts=0,dialogs=0;
  page.on('request',request=>{if(request.method()==='PUT') puts++;});
  page.on('dialog',async dialog=>{dialogs++;await dialog.dismiss();});
  // Add a new draft model targeting the same account. Existing lower-priority
  // layers and this unsaved model must all participate in the dependency check.
  await action(page,'add-model').click();
  const index=baseline.document.models.length;
  await field(page,`models.${index}.name`).fill('draft-dependent');
  await field(page,`models.${index}.name`).blur();
  await action(page,'add-layer',`models.${index}.layers`).click();
  await field(page,`models.${index}.layers.0.name`).fill('draft-lower-layer');
  await field(page,`models.${index}.layers.0.name`).blur();
  await action(page,'add-target',`models.${index}.layers.0.targets`).click();
  await field(page,`models.${index}.layers.0.targets.0.backend`).selectOption(baseline.document.backends[0].id);
  await action(page,'accounts').click();
  await action(page,'account-tab','backend:0').click();
  const snapshot=()=>page.locator('main').evaluate(main=>[...main.querySelectorAll('input,select')].map(n=>[n.dataset.field,n.value,n.checked]));
  const before=await snapshot();
  for(const viewport of [{width:1280,height:900},{width:390,height:844}]) {
    await page.setViewportSize(viewport);
    await page.evaluate(()=>window.scrollTo(0,0));
    const box=await action(page,'remove','backends.0').boundingBox();
    assert(box && box.y>=0 && box.y+box.height<=viewport.height,'Delete account visible near heading without scrolling forms');
    await physicalClick(page,action(page,'remove','backends.0'));
    await expect(page.locator('#notice')).toContainText(`Cannot delete account "${baseline.document.backends[0].id}"`);
    await expect(page.locator('#notice')).toContainText('draft-dependent / draft-lower-layer');
    for(const model of baseline.document.models) for(const layer of model.layers) {
      if(layer.targets.some(target=>target.backend===baseline.document.backends[0].id)) await expect(page.locator('#notice')).toContainText(`${model.name} / ${layer.name}`);
    }
    await expect(page.locator('#notice')).toContainText('Remove or replace these routing targets first.');
    assert.deepEqual(await snapshot(),before,'blocked removal retains draft');
    await expect(page.locator('#draft-status')).toHaveText('Unsaved changes');
    assert.equal(dialogs,0,'referenced removal is blocked before confirmation');
    assert.equal(puts,0);
  }
  await action(page,'workspace').click();
  await action(page,'select',String(index)).click();
  await action(page,'view','routing').click();
  await expect(field(page,`models.${index}.name`)).toHaveValue('draft-dependent');
  assert.equal((await(await fetch(base+'/api/routing')).json()).revision,baseline.revision);
  assert.deepEqual(await readFile(dir+'/config.toml'),bytes);
  await page.close();
  console.log('PASS account dependency guard covers all layers and unsaved models, preserves drafts, desktop/mobile physical click; API rejects dangling targets atomically');
}
async function inspectReadonlyAccounts(browser) {
  const page=await browser.newPage({viewport:{width:1280,height:900}});
  await page.goto(base);
  await action(page,'accounts').click();
  await expect(page.locator('.accounts-page')).toBeVisible();
  await expect(page.locator('main input,main select,main [data-action="add-backend"],main [data-action="remove"]')).toHaveCount(0);
  await expect(page.locator('main')).toContainText('upstream-two');
  await expect(page.locator('main')).toContainText('Subscription quota usage value');
  await expect(page.locator('main')).toContainText('peak');
  await expect(page.locator('main')).toContainText('UTC');
  await expect(page.locator('main')).not.toContainText(key);
  await page.screenshot({path:artifacts+'/account-readonly-desktop.png',fullPage:true});
  for(const scope of ['accounts','settings']) {
    await action(page,scope).click();
    await expect(page.locator('main input,main select,main textarea')).toHaveCount(0);
    await page.setViewportSize({width:390,height:844});
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth), `${scope} readonly mobile overflow`);
    await page.screenshot({path:artifacts+`/${scope}-readonly-mobile.png`,fullPage:true});
  }
  await action(page,'accounts').click();
  await action(page,'account-tab','backend:0').click();
  await action(page,'enable-edit').click();
  await expect(field(page,'backends.0.models.1.pricing_note')).toHaveValue('Subscription quota usage value');
  await action(page,'disable-edit').click();
  await expect(page.locator('main input,main select')).toHaveCount(0);
  await page.close();
  console.log('PASS account-local capacity and independent Settings: readonly information, mobile, and edit toggle');
}
async function physicalClick(page, locator) {
  await locator.evaluate(node => node.scrollIntoView({block:"center", inline:"nearest", behavior:"instant"}));
  const box = await locator.boundingBox();
  assert(box, 'click target must be visible');
  assert(await locator.evaluate(node => {const r=node.getBoundingClientRect(); return node.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2));}), 'physical click must hit its control');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();
}
async function inspectSettingsAndCapacity(browser) {
  const page = await browser.newPage();
  await page.goto(base);
  await action(page, 'settings').click();
  await action(page, 'enable-edit').click();
  const baseline = await (await fetch(base + '/api/routing')).json();
  const bodyLimit = 'server.max_inference_body_bytes';
  await expect(field(page, 'backends.0.id')).toHaveCount(0);
  await expect(field(page, 'server.listen')).toHaveCount(0);
  await field(page, bodyLimit).fill('256');
  await action(page, 'accounts').click();
  await expect(field(page, bodyLimit)).toHaveCount(0);
  await action(page, 'settings').click();
  await expect(field(page, bodyLimit)).toHaveValue('256');
  page.once('dialog', dialog => dialog.dismiss());
  await action(page, 'save').click();
  assert.equal((await (await fetch(base+'/api/routing')).json()).revision, baseline.revision);
  await save(page);
  await expect(page.getByRole('heading', {name:'Settings',exact:true})).toBeVisible();
  const largeRequest = () => fetch(base + '/v1/chat/completions', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({model:'public',messages:[{role:'user',content:'X'.repeat(1024)}]})});
  const limited = await largeRequest();
  assert.equal(limited.status,413,'Settings Save applies the request-body limit without restarting');
  await limited.text();
  await page.reload();
  await action(page,'settings').click();
  await expect(page.locator('main input,main select')).toHaveCount(0);
  await expect(page.locator('main')).toContainText('256');
  await action(page,'enable-edit').click();
  await field(page,bodyLimit).fill(String(baseline.document.server.max_inference_body_bytes));
  await save(page);
  const restored = await largeRequest();assert.equal(restored.status,200);await restored.json();

  await action(page,'accounts').click();
  await action(page,'add-backend').click();
  await field(page,'backends.1.id').fill('shared-account');
  await physicalClick(page,action(page,'add-credential','backends.1.credentials'));
  await field(page,'backends.1.credentials.0.id').fill('shared-key');
  await field(page,'backends.1.credentials.0.pool').selectOption('shared');
  await expect(field(page,'capacity_pools.0.max_in_flight')).toHaveValue('2');
  await field(page,'capacity_pools.0.id').fill('shared-renamed');
  await physicalClick(page,action(page,'settings'));
  await expect(page.getByRole('heading',{name:'Settings',exact:true})).toBeVisible();
  await action(page,'accounts').click();
  await expect(field(page,'backends.1.credentials.0.pool')).toHaveValue('shared-renamed');
  await action(page,'account-tab','backend:0').click();
  await expect(field(page,'backends.0.credentials.0.pool')).toHaveValue('shared-renamed');
  await expect(field(page,'backends.0.credentials.1.pool')).toHaveValue('shared-renamed');
  await expect(page.locator('main')).toContainText('shared-account');
  await field(page,'capacity_pools.0.id').fill('');
  await physicalClick(page,action(page,'settings'));
  await action(page,'accounts').click();
  await expect(field(page,'capacity_pools.0.id')).toHaveValue('');
  await field(page,'capacity_pools.0.id').fill('shared-renamed');
  await field(page,'capacity_pools.0.id').blur();
  await field(page,'backends.0.credentials.1.pool').selectOption('');
  await field(page,'backends.0.credentials.1.pool').selectOption('shared-renamed');
  await field(page,'capacity_pools.0.id').fill('shared-again');
  await field(page,'capacity_pools.0.id').blur();
  await expect(field(page,'backends.0.credentials.1.pool')).toHaveValue('shared-again');
  let deletion='';
  page.once('dialog',async dialog=>{deletion=dialog.message();await dialog.dismiss();});
  await action(page,'remove','capacity_pools.0').click();
  assert.match(deletion,/shared-account/);
  await expect(field(page,'backends.0.credentials.0.pool')).toHaveValue('shared-again');
  page.once('dialog',dialog=>dialog.accept());
  await action(page,'remove','capacity_pools.0').click();
  await expect(field(page,'backends.0.credentials.0.pool')).toHaveValue('');
  await action(page,'account-tab','backend:1').click();
  await expect(field(page,'backends.1.credentials.0.pool')).toHaveValue('');
  page.once('dialog',dialog=>dialog.accept());await action(page,'discard').click();
  const after = await (await fetch(base+'/api/routing')).json();
  assert.deepEqual(after.document,baseline.document,'Cross-account capacity edits remain draft-only');
  await action(page,'workspace').click();
  while (await action(page,'select','0').count()) {
    await action(page,'select','0').click();
    await action(page,'view','routing').click();
    page.once('dialog',dialog=>dialog.accept());
    await action(page,'remove','models.0').click();
  }
  await action(page,'accounts').click();
  page.once('dialog',dialog=>dialog.accept());await action(page,'remove','backends.0').click();
  await expect(action(page,'account-tab')).toHaveCount(0);
  await expect(action(page,'add-backend')).toBeVisible();
  await action(page,'settings').click();
  await expect(field(page,bodyLimit)).toBeVisible();
  page.once('dialog',dialog=>dialog.accept());await action(page,'discard').click();
  await page.close();
  console.log('PASS Settings confirmed Save has immediate HTTP effect; shared capacity rename/delete preserves all account bindings; empty Accounts remains navigable');
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
  await action(page, "account-tab", "backend:0").click();
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
  await action(page, 'account-tab', 'backend:0').click();
  await expect(field(page, 'backends.1.id')).toHaveCount(0);
  await action(page, 'account-tab', 'backend:1').click();
  await expect(field(page, 'backends.1.credentials.0.api_key')).toHaveValue(key);
  await action(page, "add-upstream", "backends.1.models").click();
  await field(page, "backends.1.models.0.name").fill("upstream");
  await field(page, "backends.1.models.0.name").blur();
  await save(page);
  await expect(action(page, "account-tab", "backend:1")).toHaveAttribute("aria-selected", "true");
  const unusedBaseline = await (await fetch(base+'/api/routing')).json();
  let unusedPuts = 0;
  const countUnusedPut = request => { if(request.method()==='PUT') unusedPuts++; };
  page.on('request',countUnusedPut);
  page.once('dialog',dialog=>dialog.dismiss());
  await physicalClick(page,action(page,'remove','backends.1'));
  await expect(action(page,'account-tab','backend:1')).toHaveAttribute('aria-selected','true');
  await expect(field(page,'backends.1.id')).toHaveValue('extra-account');
  await expect(page.locator('#draft-status')).toHaveText('All changes saved');
  assert.equal(unusedPuts,0);
  page.once("dialog", dialog => dialog.accept());
  await physicalClick(page, action(page, "remove", "backends.1"));
  assert.equal(unusedPuts,0,'confirmed unused account deletion remains draft-only');
  assert.equal((await(await fetch(base+'/api/routing')).json()).revision,unusedBaseline.revision);
  page.off('request',countUnusedPut);
  await expect(action(page, "account-tab", "backend:0")).toHaveAttribute("aria-selected", "true");
  await save(page);
  await action(page, "add-pool", "backends.0.credentials.0").click();
  await field(page, "capacity_pools.0.id").fill("shared");
  await field(page, "capacity_pools.0.id").blur();
  await field(page, "capacity_pools.0.max_in_flight").fill("2");
  await expect(field(page, "backends.0.credentials.0.pool")).toHaveValue("shared");
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
  await action(page, "settings").click();
  await expect(
    field(page, "server.timeouts.upstream_connect_ms"),
  ).toBeVisible();
  await action(page, "workspace").click();
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
  await inspectReadonlyAccounts(browser);
  await auditConfirmations(browser);
  await auditAccountDependencies(browser);
  await inspectSettingsAndCapacity(browser);
  // The independent Settings page has saved and restored the shared document.
  await action(page,'discard').click();
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
  await action(page,'account-tab','backend:0').click();
  page.once('dialog', dialog => dialog.accept());
  await action(page,'remove-pricing','backends.0.models.1').click();
  await save(page);
  const removed=(await(await fetch(base+'/api/routing')).json()).document.backends[0].models[1];
  assert(!removed.pricing_schedule);assert(!removed.pricing);assert.equal(removed.pricing_note,'Subscription quota usage value');
  await action(page, "workspace").click();
  const before = await readFile(dir + "/config.toml", "utf8");
  await action(page, "accounts").first().click();
  await action(page, "account-tab", "backend:0").click();
  await field(page, "backends.0.credentials.0.api_key").fill("invalid-key");
  await save(page, 422);
  assert.equal(await readFile(dir + "/config.toml", "utf8"), before);
  await expect(field(page, "backends.0.credentials.0.api_key")).toHaveValue(
    "invalid-key",
  );
  await field(page, "backends.0.credentials.0.api_key").fill("");
  await action(page, "workspace").click();
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
  await expect(page.getByRole("heading", {name:"No saved data", exact:true})).toBeVisible();
  await action(page, "view", "routing").click();
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
  await expect(page.locator('#preview-result')).toHaveCount(0);
  await action(page, 'view', 'routing').click();
  await expect(page.locator('#preview-result')).toHaveText('');
  await page.unroute('**/api/routing?at=*');
  for (let i = 0; i < 25; i++) {
    await infer("second-public");
    await infer("fresh-edit");
  }
  const inferenceBody = (content) => ({method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({model:'second-public', messages:[{role:'user',content}]})});
  const poolPage=await browser.newPage();
  await poolPage.goto(base);
  await action(poolPage,'accounts').click();
  await action(poolPage,'account-tab','backend:0').click();
  const occupancy=poolPage.locator('[data-live-pool="shared"]');
  await expect(occupancy).toContainText('0 / 2');
  const held = [fetch(base+'/v1/chat/completions', inferenceBody('HOLD')), fetch(base+'/v1/chat/completions', inferenceBody('HOLD'))];
  try {
    for (let i=0; i<100 && heldRequests.length<2; i++) await sleep(50);
    assert.equal(heldRequests.length, 2, 'both shared pool slots are occupied');
    await expect(occupancy).toContainText('2 / 2',{timeout:10000});
    const blocked = await fetch(base+'/v1/chat/completions', inferenceBody('OK'));
    assert.equal(blocked.status, 503);
    await blocked.json();
  } finally {
    heldRequests.splice(0).forEach(release => release());
    for (const response of await Promise.all(held)) {assert.equal(response.status,200); await response.json();}
  }
  await expect(occupancy).toContainText('0 / 2',{timeout:10000});
  await poolPage.close();
  await action(page, 'select', '1').click();
  await action(page, 'window', 'all').click();
  const noTarget = page.locator('tbody tr').filter({hasText:'No final target'});
  await expect(noTarget).toHaveCount(1);
  await expect(noTarget.locator('td').last()).toHaveText('1');
  await action(page, 'metric', 'output_tokens').click();
  await expect(noTarget.locator('td').last()).toHaveText('0');
  console.log('PASS actual full-capacity rejection remains attributed to its model, with No final target shown only when needed');
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
  await expect(page.getByRole("heading", {name:"Active alerts", exact:true})).toHaveCount(0);
  await expect(page.getByText("No final target", {exact:true})).toHaveCount(0);
  await action(page, "select", "all").click();
  await expect(action(page, "window", "1d")).toHaveAttribute("aria-pressed", "true");
  for (const window of ['1h', '1d', '1w', '1m', 'all']) {
    const response = page.waitForResponse(r => new URL(r.url()).pathname === '/api/stats' && new URL(r.url()).searchParams.get('window') === window);
    await action(page, 'window', window).click();
    const stats = await (await response).json();
    assert.equal(stats.window.id.toLowerCase(), window);
    await expect(action(page, 'window', window)).toHaveAttribute('aria-pressed', 'true');
    const row = page.locator('tbody tr').filter({has: page.locator('td:first-child', {hasText:/^mock$/})}).first();
    await expect(row.locator('td').nth(1)).toHaveText(String(stats.backends.mock.attempts));
  }
  await expect(action(page, "view", "routing")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Account totals" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Active alerts" }),
  ).toBeVisible();
  await page.route('**/api/stats?**', async route => {await sleep(500); await route.continue();});
  await action(page, 'window', '1d').click();
  await action(page, 'accounts').click();
  await sleep(700);
  await expect(page.locator('.accounts-page')).toBeVisible();
  await expect(page.getByRole('heading', {name:'All statistics', exact:true})).toHaveCount(0);
  await page.unroute('**/api/stats?**');
  await action(page,'workspace').click();
  await expect(action(page,'window','1d')).toHaveAttribute('aria-pressed','true');
  await page.route('**/api/stats?**', async route => {await sleep(500); await route.continue();});
  await action(page,'window','all').click();
  await action(page,'settings').click();
  await sleep(700);
  await expect(page.getByRole('heading',{name:'Settings',exact:true})).toBeVisible();
  await expect(page.getByRole('heading',{name:'All statistics',exact:true})).toHaveCount(0);
  await page.unroute('**/api/stats?**');
  await action(page, 'workspace').click();
  await expect(action(page, 'window', 'all')).toHaveAttribute('aria-pressed', 'true');
  await action(page, "view", "history").click();
  await expect(page.locator("tbody")).toContainText("public");
  await expect(page.locator("tbody")).toContainText("second-public");
  await action(page, "select", "0").click();
  await expect(action(page, "view", "statistics")).toHaveAttribute("aria-pressed", "true");
  await action(page, "view", "history").click();
  await expect(page.getByText("Page 1", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).not.toContainText("second-public");
  await expect(action(page, "older")).toBeEnabled();
  await action(page, "older").click();
  await expect(page.getByText("Page 2", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).not.toContainText("second-public");
  await action(page, "select", "1").click();
  await action(page, "view", "history").click();
  await expect(page.getByText("Page 1", { exact: true })).toBeVisible();
  await expect(page.locator("tbody")).toContainText("second-public");
  await page.route("**/api/requests?**", async (route) => {
    if (new URL(route.request().url()).searchParams.get("model") === "public")
      await sleep(500);
    await route.continue();
  });
  await action(page, "select", "0").click();
  await action(page, "view", "history").click();
  await action(page, "select", "1").click();
  await action(page, "view", "history").click();
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
  page.once("dialog", dialog => dialog.accept());
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
  await action(page, 'accounts').click();
  await expect(page.locator('.accounts-page')).toBeVisible();
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'mobile Accounts overflow');
  await page.screenshot({path: artifacts + '/accounts-mobile.png', fullPage:true});
  await page.setViewportSize({width:1280, height:900});
  await page.screenshot({path: artifacts + '/accounts-desktop.png', fullPage:true});
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
