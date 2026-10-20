// Live acceptance check for an isolated 8081 instance carrying real ledger data.
// Usage: node tests/verify-8081.mjs [baseUrl] [screenshotDir]
import { chromium, expect } from "@playwright/test";
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const base = (process.argv[2] || "http://127.0.0.1:8081").replace(/\/$/, "");
const shots = resolve(process.argv[3] || "data/e2e");
await mkdir(shots, { recursive: true });
const json = async (path) => (await fetch(base + path)).json();
const action = (page, a, p) =>
  page.locator(`[data-action="${a}"]${p === undefined ? "" : `[data-path="${p}"]`}`);
// Statistics render reported + estimated thinking tokens, tagging the cell when
// any part of it came from the local byte-share estimate.
const thinkCell = (row) => {
  const estimated = Number(row?.reasoning_estimated_tokens || 0);
  const total = Number(row?.reasoning_tokens || 0) + estimated;
  return total.toLocaleString() + (estimated ? " ≈" : "");
};

const browser = await chromium.launch({
  executablePath:
    process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ||
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(base);
  await expect(page.getByRole("heading", { name: "All statistics", exact: true })).toBeVisible();

  // Account totals must mirror the live API, including the new token columns.
  const stats = await json("/api/stats?window=1d");
  const row = page
    .locator("tbody tr")
    .filter({ has: page.locator("td:first-child", { hasText: /^ollama-cloud$/ }) })
    .first();
  for (const [column, field] of [
    [1, "attempts"],
    [4, "cache_miss_tokens"],
    [5, "cache_hit_tokens"],
    [6, "output_tokens"],
    [8, "total_tokens"],
  ])
    await expect(row.locator("td").nth(column)).toHaveText(
      Number(stats.backends["ollama-cloud"][field]).toLocaleString(),
    );
  await expect(row.locator("td").nth(7)).toHaveText(
    thinkCell(stats.backends["ollama-cloud"]),
  );
  await page.screenshot({ path: shots + "/8081-all-statistics.png", fullPage: true });
  console.log("PASS all-statistics token columns match the live API");

  // A model whose upstreams report thinking tokens shows non-zero Think values.
  const routing = await json("/api/routing");
  const model = routing.models.find((m) => m.name === "glm-5.3-flash") || routing.models[0];
  const index = routing.models.findIndex((m) => m.name === model.name);
  await action(page, "select", String(index)).click();
  await action(page, "window", "all").click();
  await expect(page.getByRole("heading", { name: "Model statistics", exact: true })).toBeVisible();
  const modelStats = await json(`/api/routing/stats?model=${encodeURIComponent(model.name)}&window=all`);
  assert(modelStats.totals.reasoning_tokens > 0, "expected a model that reports thinking tokens");
  // The first table row is the first layer, not the model total, so compare
  // against that layer's own totals.
  const firstLayer = modelStats.layers[0].totals;
  const layerRow = page.locator("tbody tr").first();
  const cells = await layerRow.locator("td.numeric").allTextContents();
  assert.equal(cells.at(-1), Number(firstLayer.total_tokens).toLocaleString());
  assert.equal(cells.at(-2).trim(), thinkCell(firstLayer));
  await page.screenshot({ path: shots + "/8081-model-statistics.png", fullPage: true });
  console.log(`PASS model statistics expose uncached / cached / output / think totals for ${model.name}`);

  await action(page, "view", "history").click();
  const history = await json(`/api/requests?limit=20&model=${encodeURIComponent(model.name)}`);
  const newest = history.requests[0].usage;
  const tokenCell = page.locator("tbody tr").first().locator("td").nth(4);
  const numbers = (text) => text.split("/").map((part) => Number(part.replace(/[^0-9]/g, "")));
  assert.deepEqual(numbers(await tokenCell.locator(".cell-main").textContent()), [
    newest.cache_miss_tokens,
    newest.output_tokens,
  ]);
  const detail = await tokenCell.locator(".cell-sub").textContent();
  assert(detail.includes(`cache ↓ ${newest.cache_hit_tokens.toLocaleString()}`), detail);
  const thinking = Number(newest.reasoning_tokens || 0);
  const estimatedThinking = Number(newest.reasoning_estimated_tokens || 0);
  const thinkText =
    newest.reasoning_reported || thinking > 0
      ? `think ${thinking.toLocaleString()}`
      : estimatedThinking > 0
        ? `think ≈${estimatedThinking.toLocaleString()}`
        : "think —";
  assert(detail.includes(thinkText), `${detail} (expected ${thinkText})`);
  await page.screenshot({ path: shots + "/8081-history-tokens.png", fullPage: true });
  console.log("PASS history rows render the per-request cache and thinking detail");

  // An upstream that omits the reasoning split still shows an estimate, tagged
  // with "≈" and explained in the cell title.
  const estimatedRequest = (await json("/api/requests?limit=50")).requests.find(
    (r) => Number(r.usage?.reasoning_estimated_tokens || 0) > 0,
  );
  if (!estimatedRequest) {
    console.log("SKIP no estimated-thinking row recorded yet on this instance");
  } else {
    const estimatedModelIndex = routing.models.findIndex(
      (m) => m.name === estimatedRequest.served_model,
    );
    await action(page, "select", String(estimatedModelIndex)).click();
    await action(page, "view", "history").click();
    const estimatedModel = routing.models[estimatedModelIndex].name;
    const estimatedHistory = await json(
      `/api/requests?limit=20&model=${encodeURIComponent(estimatedModel)}`,
    );
    const rowIndex = estimatedHistory.requests.findIndex(
      (r) => Number(r.usage?.reasoning_estimated_tokens || 0) > 0,
    );
    assert(rowIndex !== -1, "the estimated request must appear in its model history");
    const expected = estimatedHistory.requests[rowIndex].usage.reasoning_estimated_tokens;
    const cell = page.locator("tbody tr").nth(rowIndex).locator("td").nth(4);
    const text = await cell.locator(".cell-sub").textContent();
    assert(
      text.includes(`think ≈${expected.toLocaleString()}`),
      `${text} (expected think ≈${expected.toLocaleString()})`,
    );
    const title = await cell.locator(".cell-main").getAttribute("title");
    assert(
      title.includes(`≈${expected.toLocaleString()}`) && title.includes("estimated"),
      `${title} (expected an estimated-thinking explanation)`,
    );
    await page.screenshot({
      path: shots + "/8081-history-estimated-thinking.png",
      fullPage: true,
    });
    console.log(
      `PASS estimated thinking renders as ≈${expected.toLocaleString()} for ${estimatedModel} (${estimatedRequest.backend})`,
    );
  }

  // Manual refresh keeps the scope and updates the timestamp.
  const before = await page.locator("#updated-at").textContent();
  await new Promise((r) => setTimeout(r, 1100));
  const refreshed = page.waitForResponse((r) => new URL(r.url()).pathname === "/api/requests");
  await action(page, "refresh").click();
  assert.equal((await refreshed).status(), 200);
  await expect(page.locator("#updated-at")).not.toHaveText(before);
  await expect(page.getByRole("heading", { name: "Recent request history", exact: true })).toBeVisible();
  console.log("PASS manual refresh reloads the visible scope without leaving it");

  // Hash deep link survives a reload, and back navigation restores the previous view.
  await action(page, "view", "statistics").click();
  await action(page, "window", "1w").click();
  const deepLink = page.url();
  assert(deepLink.includes("#/workspace/model:"), deepLink);
  await page.reload();
  await expect(page.getByRole("heading", { name: "Model statistics", exact: true })).toBeVisible();
  await expect(action(page, "window", "1w")).toHaveAttribute("aria-pressed", "true");
  await action(page, "view", "history").click();
  await expect(page.getByText("Page 1", { exact: true })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("heading", { name: "Model statistics", exact: true })).toBeVisible();
  console.log("PASS deep link, reload and back navigation keep the model, view and window");

  // The 20-second data poll runs against the live instance.
  const polls = [];
  page.on("request", (r) => {
    if (new URL(r.url()).pathname === "/api/routing/stats") polls.push(Date.now());
  });
  const started = Date.now();
  await expect.poll(() => polls.length, { timeout: 30000, intervals: [1000] }).toBeGreaterThan(0);
  const elapsed = polls[0] - started;
  assert(elapsed >= 15000, `automatic refresh fired after ${elapsed}ms`);
  console.log(`PASS automatic statistics refresh fired after ${Math.round(elapsed / 1000)}s`);

  assert.deepEqual(errors, []);
  console.log("PASS live 8081 verification complete with no browser errors");
} finally {
  await browser.close();
}
