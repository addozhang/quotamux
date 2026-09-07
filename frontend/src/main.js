const app = document.querySelector("#app");
const esc = (v) =>
  String(v ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
const state = {
  live: null,
  draft: null,
  revision: null,
  selected: "all",
  view: "statistics",
  accounts: false,
  editing: false,
  dirty: false,
  busy: false,
  message: "",
  preview: null,
  at: "",
  stats: null,
  history: null,
  totals: null,
  alerts: null,
  window: "1d",
  metric: "calls",
  cursor: null,
  cursors: [],
  nextCursor: null,
  childGeneration: 0,
  loading: false,
};
const browseActions = new Set([
  "enable-edit",
  "disable-edit",
  "accounts",
  "workspace",
  "select",
  "view",
  "window",
  "metric",
  "older",
  "newer",
  "preview",
]);
const protocols = ["openai-chat", "openai-responses", "anthropic-messages"];
const adapters = [
  "opencode-go",
  "deepseek-official",
  "kimi-official",
  "kimi-code",
  "zhipu-coding-plan",
  "custom-chat-completions",
  "custom-responses",
  "custom-anthropic",
  "ollama-cloud",
];
const get = (p) => p.split(".").reduce((v, k) => v?.[k], state.draft);
function set(p, v) {
  const keys = p.split(".");
  const last = keys.pop();
  keys.reduce((v, k) => v[k], state.draft)[last] = v;
  state.dirty = true;
  status();
}
const button = (action, label, path = "", extra = "") =>
  `<button type="button" data-action="${action}" data-path="${esc(path)}" ${extra}>${esc(label)}</button>`;
function field(path, label, kind = "text", choices = null) {
  const value = get(path);
  return `<label><span>${esc(label)}</span>${choices ? `<select data-field="${path}">${[...new Set([...(value ? [value] : []), ...choices])].map((v) => `<option value="${esc(v)}" ${value === v ? "selected" : ""}>${esc(v || "None")}</option>`).join("")}</select>` : `<input data-field="${path}" data-kind="${kind}" type="${kind === "number" ? "number" : kind === "password" ? "password" : "text"}" ${kind === "number" ? 'min="0" step="any"' : ""} value="${esc(kind === "list" ? (value || []).join(", ") : value)}" autocomplete="${kind === "password" ? "new-password" : "off"}">`}</label>`;
}
function choices(path, label, values) {
  return `<fieldset class="choices"><legend>${esc(label)}</legend>${values.map(([v, title]) => `<label><input type="checkbox" data-choice="${path}" value="${v}" ${(get(path) || []).includes(v) ? "checked" : ""}>${esc(title)}</label>`).join("")}</fieldset>`;
}
function refs(path) {
  const t = get(path),
    backend = state.draft.backends.find((b) => b.id === t.backend);
  return `<div class="target-editor">${field(path + ".backend", "Account", "text", ["", ...state.draft.backends.map((b) => b.id)])}${field(path + ".credential", "Credential", "text", ["", ...(backend?.credentials || []).map((c) => c.id)])}${field(path + ".model", "Upstream model", "text", ["", ...(backend?.models || []).map((m) => m.name)])}${button("remove", "Remove target", path)}</div>`;
}
function render() {
  if (!state.draft) {
    app.innerHTML =
      '<main><h1>Routing</h1><p role="status">Loading workspace…</p></main>';
    return;
  }
  const model = state.draft.models[state.selected];
  app.innerHTML = `<header><strong>QuotaMux</strong><span>Routing workspace</span><div class="header-actions">${button("workspace", "Workspace", "", `aria-current="${!state.accounts ? "page" : "false"}"`)}${button("accounts", "Accounts", "", `aria-current="${state.accounts ? "page" : "false"}"`)}${button(state.editing ? "disable-edit" : "enable-edit", state.editing ? "Disable edit" : "Enable edit")}</div></header><div id="notice" role="status" aria-live="polite">${esc(state.message)}</div><div id="migration-notice" role="status">${migrationMessage()}</div>${state.accounts ? accounts() : `<div class="workspace"><aside class="model-rail"><h2>Models</h2>${button("select", "All", "all", `aria-current="${state.selected === "all" ? "page" : "false"}"`)}${state.draft.models.map((m, i) => button("select", m.name || "New model", String(i), `aria-current="${i === state.selected ? "page" : "false"}"`)).join("")}${state.editing ? button("add-model", "+ Add model") : ""}<p class="muted">${state.live?.now ? "Live status " + esc(new Date(state.live.now).toLocaleTimeString()) : ""}</p></aside><main><nav class="child-nav" aria-label="Model views">${(state.selected === "all" ? ["statistics", "history"] : ["routing", "statistics", "history"]).map((v) => button("view", v[0].toUpperCase() + v.slice(1), v, `aria-pressed="${state.view === v}"`)).join("")}</nav>${state.view === "routing" ? (model ? modelEditor(model) : "<h1>Add your first model</h1>") : childView()}</main></div>`}${state.editing ? `<footer><span id="draft-status">${state.dirty ? "Unsaved changes" : "All changes saved"}</span><span>Save validates upstreams and immediately updates routing.</span>${button("discard", "Discard", "", state.busy ? "disabled" : "")}${button("save", state.busy ? "Validating and saving…" : "Save", "", `data-testid="save-workspace" ${state.busy || !state.editing ? "disabled" : ""}`)}</footer>` : ""}`;
  if (state.busy)
    app
      .querySelectorAll("input,select,button")
      .forEach((n) => (n.disabled = true));
  if (!state.editing) {
    app
      .querySelectorAll("[data-field],[data-choice]")
      .forEach((node) => (node.disabled = true));
    app.querySelectorAll("[data-action]").forEach((node) => {
      if (!browseActions.has(node.dataset.action)) node.disabled = true;
    });
  }
  updateLive();
}
function modelEditor(m) {
  if (!state.editing) return `<div class="heading"><div><p class="eyebrow">Public model</p><h1>${esc(m.name || "New model")}</h1></div></div><div class="effective" id="effective-order"></div>`;
  const p = `models.${state.selected}`;
  return `<div class="heading"><div><p class="eyebrow">Public model</p><h1>${esc(m.name || "New model")}</h1></div>${button("remove", "Delete model", p)}</div><div class="row">${field(p + ".name", "Model name")}${field(p + ".aliases", "Aliases (comma separated)", "list")}</div>${choices(
    p + ".protocols",
    "Accepted protocols",
    protocols.map((v) => [v, v]),
  )}<div class="effective" id="effective-order"></div><section><div class="heading"><h2>Routing layers</h2>${button("add-layer", "+ Add layer", p + ".layers")}</div><p class="muted">Default priority runs top to bottom. A full target is skipped so another target or layer can serve the request.</p>${m.layers
    .map((l, i) => {
      const q = `${p}.layers.${i}`;
      return `<article class="layer-card"><div class="row">${field(q + ".name", "Layer name")}${field(q + ".strategy", "Selection strategy", "text", ["random", "prompt-prefix-affinity"])}<div class="buttons">${button("up", "Move up", q, i === 0 ? "disabled" : "")}${button("down", "Move down", q, i === m.layers.length - 1 ? "disabled" : "")}${button("remove", "Delete layer", q)}</div></div>${l.targets.map((t, j) => `<div class="target">${refs(`${q}.targets.${j}`)}<div class="target-live" data-live-target="${esc([l.name, t.backend, t.credential, t.model].join("|"))}"></div></div>`).join("")}${button("add-target", "+ Add target", q + ".targets")}</article>`;
    })
    .join(
      "",
    )}</section><section><div class="heading"><h2>Time windows</h2>${button("add-schedule", "+ Add time window", p + ".schedules")}</div>${field(p + ".timezone", "Timezone (IANA name)")}<p class="muted">No matching window uses default priority. Start inclusive, end exclusive; overnight windows belong to their starting day.</p>${(
    m.schedules || []
  )
    .map((s, i) => {
      const q = `${p}.schedules.${i}`;
      return `<article><div class="row">${field(q + ".name", "Window name")}${field(q + ".start", "Start (HH:MM or HH:MM:SS)")}${field(q + ".end", "End (HH:MM or HH:MM:SS)")}</div>${choices(
        q + ".days",
        "Starting days",
        [
          [1, "Mon"],
          [2, "Tue"],
          [3, "Wed"],
          [4, "Thu"],
          [5, "Fri"],
          [6, "Sat"],
          [7, "Sun"],
        ],
      )}${scheduleOrder(q, m)}${button("remove", "Delete window", q)}</article>`;
    })
    .join(
      "",
    )}<div class="preview"><h3>Preview saved policy</h3><p class="muted">Read-only preview uses the currently saved rules, not unsaved edits.</p><label><span>Instant (RFC3339 with timezone)</span><input id="preview-at" value="${esc(state.at)}" placeholder="2026-09-07T12:00:00Z"></label>${button("preview", "Preview saved order")}<div id="preview-result">${state.preview ? esc(state.preview) : ""}</div></div></section>`;
}
function accounts() {
  return `<main class="accounts-page" aria-label="Accounts"><div class="heading"><h1>Accounts and shared capacity</h1></div><p class="muted">Credentials stay blank when loaded. Leave an existing key blank to preserve it.</p>${state.draft.backends
    .map((b, i) => {
      const p = `backends.${i}`;
      return `<article><h3>${esc(b.id || "New account")}</h3><div class="row">${field(p + ".id", "Account ID")}${field(p + ".adapter", "Adapter", "text", adapters)}</div>${b.adapter.startsWith("custom-") ? field(p + ".endpoint", "Endpoint URL (blank preserves existing)") : '<p class="muted">Official endpoint managed by adapter</p>'}<h4>Credentials</h4>${b.credentials
        .map((c, j) => {
          const q = `${p}.credentials.${j}`;
          return `<div class="credential"><div class="row">${field(q + ".id", "Credential ID")}${field(q + ".api_key", "API key (blank preserves existing)", "password")}${field(q + ".pool", "Shared pool", "text", ["", ...(state.draft.capacity_pools || []).map((p) => p.id)])}</div>${button("remove", "Delete credential", q)}</div>`;
        })
        .join(
          "",
        )}${button("add-credential", "+ Add credential", p + ".credentials")}<h4>Upstream models</h4>${b.models
        .map((m, j) => {
          const q = `${p}.models.${j}`;
          return `<fieldset><legend>Upstream model</legend>${field(q + ".name", "Upstream model name")}${choices(
            q + ".protocols",
            "Protocols (empty uses adapter defaults)",
            protocols.map((v) => [v, v]),
          )}${priceEditor(q,b.id,m)}${button("remove", "Delete upstream model", q)}</fieldset>`;
        })
        .join(
          "",
        )}${button("add-upstream", "+ Add upstream model", p + ".models")}${button("remove", "Delete account", p)}</article>`;
    })
    .join(
      "",
    )}${button("add-backend", "+ Add account")}<h3>Shared capacity pools</h3><p class="muted">Assign credentials above. No pool means no local concurrency limit.</p>${(state.draft.capacity_pools || []).map((p, i) => `<article><div class="row">${field(`capacity_pools.${i}.id`, "Pool ID")}${field(`capacity_pools.${i}.max_in_flight`, "Maximum concurrent requests", "number")}${button("remove", "Delete pool", `capacity_pools.${i}`)}</div></article>`).join("")}${button("add-pool", "+ Add pool")}${advanced()}</main>`;
}
const windows = [
  ["1h", "1 hour"],
  ["1d", "1 day"],
  ["1w", "1 week"],
  ["1m", "1 month"],
  ["all", "All time"],
];
const metrics = [
  ["calls", "Calls"],
  ["total_tokens", "Total tokens"],
  ["input_tokens", "Input tokens"],
  ["output_tokens", "Output tokens"],
];
function scheduleOrder(path, model) {
  const order = get(path + ".order");
  return `<fieldset><legend>Layer priority for this window</legend>${order
    .map(
      (name, index) =>
        `<div class="row">${field(
          `${path}.order.${index}`,
          `Priority ${index + 1}`,
          "text",
          model.layers.map((layer) => layer.name),
        )}${button("up", "Move priority up", `${path}.order.${index}`, index === 0 ? "disabled" : "")}${button("down", "Move priority down", `${path}.order.${index}`, index === order.length - 1 ? "disabled" : "")}</div>`,
    )
    .join("")}</fieldset>`;
}
function priceFields(path) {
  return `<div class="row">${field(path+'.cache_hit_input_usd_per_million','Cached input USD / million','number')}${field(path+'.cache_miss_input_usd_per_million','Uncached input USD / million','number')}${field(path+'.output_usd_per_million','Output USD / million','number')}</div>`;
}
function activePriceText(row) {
  if(!row?.rates)return 'No published usage rates configured';
  const rates=row.rates;
  return `${row.active_window || 'Base rates'} · cached input ${rates.cache_hit_input_usd_per_million}, uncached input ${rates.cache_miss_input_usd_per_million}, output ${rates.output_usd_per_million} USD / million tokens`;
}
function priceEditor(path, backend, model) {
  return `<h4>Published usage rates (USD per million tokens)</h4><p class="muted" data-price-backend="${esc(backend)}" data-price-model="${esc(model.name)}">${esc(activePriceText(state.live.pricing?.find(row=>row.backend===backend&&row.model===model.name)))}</p><p class="muted">Usage value is an estimate, not an actual cash bill. Subscription requests may consume quota.</p>${field(path+'.pricing_note','Billing context (subscription, points or quota)')}${field(path+'.pricing_source','Rate reference URL or source')}${model.pricing?`${priceFields(path+'.pricing')}${button('remove-pricing','Remove published rates',path)}${button('add-price-window','Add price window',path)}`:button('add-pricing','Add pricing',path)}${model.pricing_schedule?`<div>${field(path+'.pricing_schedule.timezone','Price timezone (IANA name)')}${model.pricing_schedule.windows.map((window,index)=>{const q=`${path}.pricing_schedule.windows.${index}`;return `<fieldset><legend>Price window</legend><div class="row">${field(q+'.name','Price window name')}${field(q+'.start','Price start (HH:MM or HH:MM:SS)')}${field(q+'.end','Price end (HH:MM or HH:MM:SS)')}</div>${choices(q+'.days','Price starting days',[[1,'Mon'],[2,'Tue'],[3,'Wed'],[4,'Thu'],[5,'Fri'],[6,'Sat'],[7,'Sun']])}${priceFields(q+'.pricing')}${button('remove','Delete price window',q)}</fieldset>`;}).join('')}</div>`:''}`;
}
function advanced() {
  const tuning = {
    "server.max_inference_body_bytes": "Maximum request body (bytes)",
    "server.timeouts.upstream_connect_ms": "Upstream connection timeout (ms)",
    "server.timeouts.upstream_read_ms": "Upstream response timeout (ms)",
    "server.timeouts.upstream_stream_read_ms": "Stream inactivity timeout (ms)",
    "server.timeouts.upstream_total_ms": "Total upstream timeout (ms)",
    "server.timeouts.downstream_sse_heartbeat_ms":
      "Stream heartbeat interval (ms)",
    "server.timeouts.route_probe_wait_ms": "Route recovery wait (ms)",
    "affinity.checkpoint_bytes": "Prefix checkpoint size (bytes)",
    "affinity.max_checkpoints_per_path": "Maximum checkpoints per request",
    "affinity.max_candidates_per_prefix": "Maximum candidates per prefix",
    "affinity.max_leases": "Maximum affinity leases",
    "affinity.success_ttl_ms": "Successful affinity lifetime (ms)",
  };
  return `<details><summary>Advanced runtime settings</summary><p class="muted">These settings apply on Save. Listener and data location require a restart.</p><div class="row"><label>Listener<input readonly value="${esc(state.draft.server.listen)}"></label><label>Data directory<input readonly value="${esc(state.draft.server.data_dir)}"></label></div><div class="row">${Object.entries(
    tuning,
  )
    .map(([path, label]) => field(path, label, "number"))
    .join("")}</div></details>`;
}
function childView() {
  const model = state.draft.models[state.selected];
  if (
    state.selected !== "all" &&
    (!model || !state.live.models.some((row) => row.name === model.name))
  )
    return "<h1>No saved data</h1><p>Save this model to view its statistics and history.</p>";
  if (state.loading) return '<p role="status">Loading selected scope…</p>';
  if (state.view === "statistics" && state.selected === "all")
    return globalStatistics();
  if (state.view === "history") {
    return `<h1>Recent request history</h1><div class="buttons">${button("newer", "Newer", "", state.cursors.length ? "" : "disabled")}<span>Page ${state.cursors.length + 1}</span>${button("older", "Older", "", state.nextCursor ? "" : "disabled")}</div><div class="table-scroll"><table><thead><tr><th>Started</th><th>Model</th><th>Layer</th><th>Account / key</th><th>Status</th><th>Tokens</th></tr></thead><tbody>${(state.history?.requests || []).map((r) => `<tr><td>${esc(new Date(r.started_at_ms).toLocaleString())}</td><td>${esc(r.served_model)}</td><td>${esc(r.route_layer)}</td><td>${esc(r.backend)} / ${esc(r.credential)}</td><td>${esc(r.status)}</td><td>${esc(r.usage?.total_tokens || 0)}</td></tr>`).join("")}</tbody></table></div>`;
  }
  const value = (totals) =>
    Number(totals?.[state.metric] || 0).toLocaleString();
  const targetRow = (target, layer, historical = false) =>
    `<tr><td>${historical ? "Historical target" : "Target"}</td><td>${esc(layer)}</td><td>${esc(target.backend)}</td><td>${esc(target.credential)}</td><td>${esc(target.upstream_model || target.model)}</td><td>${value(target.totals)}</td></tr>`;
  return `<h1>Model statistics</h1><div class="buttons">${windows.map(([id, label]) => button("window", label, id, `aria-pressed="${id === state.window}"`)).join("")}</div><div class="buttons">${metrics.map(([id, label]) => button("metric", label, id, `aria-pressed="${id === state.metric}"`)).join("")}</div><p>Model total: ${value(state.stats?.totals)}</p><div class="table-scroll"><table><thead><tr><th>Entry</th><th>Layer</th><th>Account</th><th>Key</th><th>Upstream model</th><th>${esc(metrics.find(([id]) => id === state.metric)?.[1])}</th></tr></thead><tbody>${(state.stats?.layers || []).map((layer) => `<tr><th>Layer total</th><td>${esc(layer.name)}</td><td></td><td></td><td></td><td>${value(layer.totals)}</td></tr>${(layer.targets || []).map((target) => targetRow(target, layer.name)).join("")}`).join("")}${(state.stats?.historical_targets || []).map((target) => targetRow(target, target.layer_name || target.layer_index, true)).join("")}${state.stats?.unattributed?.calls ? `<tr><td>No final target</td><td colspan="4">No final layer or target recorded for these requests.</td><td>${value(state.stats.unattributed)}</td></tr>` : ""}</tbody></table></div>`;
}
function globalStatistics() {
  return `<h1>All statistics</h1><div class="buttons">${windows.map(([id, label]) => button("window", label, id, `aria-pressed="${id === state.window}"`)).join("")}</div><h2>Account totals</h2><div class="table-scroll"><table><thead><tr><th>Account</th><th>Attempts</th><th>Successes</th><th>Errors</th><th>Cache hit</th><th>Usage value estimate (USD)</th></tr></thead><tbody>${Object.entries(
    state.totals?.backends || {},
  )
    .map(([id, row]) => {
      const total =
        Number(row.cache_hit_tokens || 0) + Number(row.cache_miss_tokens || 0);
      return `<tr><td>${esc(id)}</td><td>${row.attempts || 0}</td><td>${row.successes || 0}</td><td>${row.errors || 0}</td><td>${row.cache_reported_attempts ? (total ? (100 * row.cache_hit_tokens) / total : 0).toFixed(1) + "%" : "—"}</td><td>${Number(row.cost_usd || 0).toFixed(6)}</td></tr>`;
    })
    .join(
      "",
    )}</tbody></table></div><h2>Active alerts</h2><p class="muted">Current account and credential alerts, independent of the statistics time range.</p><div class="table-scroll"><table><thead><tr><th>Account</th><th>Key</th><th>Class</th><th>Last seen</th><th>Next probe</th></tr></thead><tbody>${(
    state.alerts?.alerts || []
  )
    .filter((a) => a.active)
    .map(
      (a) =>
        `<tr><td>${esc(a.backend)}</td><td>${esc(a.credential)}</td><td>${esc(a.class)}</td><td>${esc(new Date(a.last_seen_ms).toLocaleString())}</td><td>${a.next_probe_at_ms ? esc(new Date(a.next_probe_at_ms).toLocaleString()) : "—"}</td></tr>`,
    )
    .join("")}</tbody></table></div>`;
}
function updateLive() {
  document.querySelectorAll("[data-price-backend]").forEach(node=>node.textContent=activePriceText(state.live?.pricing?.find(row=>row.backend===node.dataset.priceBackend&&row.model===node.dataset.priceModel)));
  const m = state.draft?.models[state.selected];
  const live = state.live?.models.find((row) => row.name === m?.name);
  const order = document.querySelector("#effective-order");
  if (order)
    order.textContent = live
      ? `Live priority: ${live.effective_order.join(" → ")} · ${live.active_schedule || "Default order"}`
      : "New model — not saved yet";
  document.querySelectorAll("[data-live-target]").forEach((node) => {
    const [layer, backend, credential, model] =
      node.dataset.liveTarget.split("|");
    const t = live?.layers
      .find((l) => l.name === layer)
      ?.targets.find(
        (t) =>
          t.backend === backend &&
          t.credential === credential &&
          t.upstream_model === model,
      );
    node.textContent = t
      ? `${t.circuit?.mode || "Unknown health"} · ${t.pool ? `Pool ${t.pool}: ${t.in_flight} / ${t.max_in_flight} in flight` : "No local capacity limit"}`
      : "Not in active routing";
  });
}
function migrationMessage() {
  return state.live?.migration?.persisted === false
    ? "Active routing loaded. Make the configuration directory writable to save the migration."
    : "";
}
function status() {
  const n = document.querySelector("#draft-status");
  if (n) n.textContent = state.dirty ? "Unsaved changes" : "All changes saved";
  const notice = document.querySelector("#notice");
  if (notice) notice.textContent = state.message;
  const migration = document.querySelector("#migration-notice");
  if (migration) migration.textContent = migrationMessage();
}
async function request(path, options = {}) {
  const response = await fetch(path, {
    cache: "no-store",
    ...options,
    headers: {
      ...options.headers,
    },
  });
  const data = await response.json();
  if (!response.ok)
    throw Error(
      `${response.status === 409 ? "Conflict: " : ""}${data.error || response.statusText}`,
    );
  return data;
}
function adopt(data) {
  const selectedName = state.draft?.models[state.selected]?.name;
  const wasAll = state.selected === "all";
  resetChildren();
  state.preview = null;
  state.live = data;
  state.draft = structuredClone(data.document);
  state.draft.capacity_pools ||= [];
  state.draft.models.forEach((m) => {
    m.timezone ||= "UTC";
    m.schedules ||= [];
  });
  state.revision = data.revision;
  const matched = state.draft.models.findIndex(model => model.name === selectedName);
  state.selected = wasAll || !state.draft.models.length ? "all" : matched >= 0 ? matched : Math.min(state.selected, Math.max(0, state.draft.models.length - 1));
  state.dirty = false;
  if (state.selected === 'all' && state.view === 'routing') state.view = 'statistics';
  if (!state.accounts && state.view !== 'routing') queueMicrotask(() => children());
}
async function refresh() {
  try {
    const data = await request("/api/routing");
    state.live = data;
    if (
      !state.draft ||
      (!state.dirty && !state.busy && data.revision !== state.revision)
    ) {
      adopt(data);
      render();
    } else {
      if (state.dirty && data.revision !== state.revision && !state.busy) {
        state.message =
          "Routing changed elsewhere. Your draft is retained; use Discard to load the latest saved routing.";
      }
      updateLive();
      status();
    }
  } catch (e) {
    state.message = e.message;
    if (!state.draft)
      app.innerHTML = `<main><h1>Routing unavailable</h1><p>${esc(e.message)}</p></main>`;
    else status();
  }
}
function resetChildren() {
  state.childGeneration++;
  state.cursor = null;
  state.cursors = [];
  state.nextCursor = null;
  state.stats = null;
  state.totals = null;
  state.alerts = null;
  state.history = null;
  state.loading = false;
}
async function children() {
  if (state.accounts || state.view === "routing") return;
  const generation = ++state.childGeneration;
  const selected = state.selected,
    view = state.view,
    window = state.window,
    cursor = state.cursor;
  const model = state.draft.models[selected];
  if (
    selected !== "all" &&
    (!model || !state.live.models.some((row) => row.name === model.name))
  ) {
    render();
    return;
  }
  state.loading = true;
  render();
  try {
    let result;
    if (view === "statistics")
      result =
        selected === "all"
          ? await Promise.all([request(`/api/stats?window=${window}`), request("/api/status")])
          : await request(
              `/api/routing/stats?model=${encodeURIComponent(model.name)}&window=${window}`,
            );
    if (view === "history")
      result = await request(
        "/api/requests?limit=20" +
          (selected === "all"
            ? ""
            : "&model=" + encodeURIComponent(model.name)) +
          (cursor ? "&before=" + encodeURIComponent(cursor) : ""),
      );
    if (generation !== state.childGeneration) return;
    state.loading = false;
    if (view === "statistics") {
      if (selected === "all") [state.totals, state.alerts] = result;
      else state.stats = result;
    }
    if (view === "history") {
      state.history = result;
      state.nextCursor = result.next_cursor;
    }
    render();
  } catch (error) {
    if (generation !== state.childGeneration) return;
    state.loading = false;
    state.message = error.message;
    render();
  }
}
app.addEventListener("input", (event) => {
  const n = event.target;
  if (n.id === "preview-at") {
    state.at = n.value;
    return;
  }
  if (!state.editing || state.busy) return;
  if (n.dataset.field?.match(/^models\.\d+\.layers\.\d+\.name$/)) {
    const old = get(n.dataset.field);
    const model = get(n.dataset.field.split(".").slice(0, 2).join("."));
    model.schedules.forEach(
      (schedule) =>
        (schedule.order = schedule.order.map((name) =>
          name === old ? n.value : name,
        )),
    );
  }
  if (n.dataset.field) {
    set(
      n.dataset.field,
      n.dataset.kind === "list"
        ? n.value
            .split(",")
            .map((v) => v.trim())
            .filter(Boolean)
        : n.dataset.kind === "number"
          ? Number(n.value)
          : n.value,
    );
  }
  if (n.dataset.choice) {
    const numeric = n.dataset.choice.endsWith(".days");
    const v = numeric ? Number(n.value) : n.value;
    const values = new Set(get(n.dataset.choice) || []);
    n.checked ? values.add(v) : values.delete(v);
    set(n.dataset.choice, [...values]);
  }
});
app.addEventListener("change", (event) => {
  if (!state.editing || state.busy) return;
  const path = event.target.dataset.field;
  if (path?.endsWith(".backend")) {
    const target = get(path.slice(0, -8));
    target.credential = "";
    target.model = "";
    render();
  }
  if (path?.endsWith(".adapter")) {
    const backend = get(path.slice(0, -8));
    delete backend.endpoint;
    const adapterProtocols = {
      "opencode-go": [],
      "custom-chat-completions": ["openai-chat"],
      "custom-responses": ["openai-responses"],
      "custom-anthropic": ["anthropic-messages"],
      "kimi-code": ["openai-chat", "anthropic-messages"],
      "kimi-official": ["openai-chat"],
      "ollama-cloud": ["openai-chat"],
    };
    backend.models.forEach(
      (model) =>
        (model.protocols = [
          ...(adapterProtocols[backend.adapter] || protocols),
        ]),
    );
    render();
  }
  if (path?.match(/\.order\.\d+$/)) {
    const keys = path.split(".");
    const index = Number(keys.pop());
    const order = get(keys.join("."));
    const names = state.draft.models[state.selected].layers.map(
      (layer) => layer.name,
    );
    const missing = names.find((name) => !order.includes(name));
    const duplicate = order.findIndex(
      (name, i) => i !== index && name === order[index],
    );
    if (duplicate >= 0 && missing) order[duplicate] = missing;
    render();
  }
  if (path?.endsWith(".name") || path?.endsWith(".id")) {
    // Refresh references in place so a blur cannot replace the clicked control.
    app.querySelectorAll('select[data-field]').forEach(select => {
      const reference = select.dataset.field;
      let values;
      if (reference.endsWith('.pool')) values = ['', ...state.draft.capacity_pools.map(pool => pool.id)];
      else if (reference.endsWith('.backend')) values = ['', ...state.draft.backends.map(backend => backend.id)];
      else if (/\.targets\.\d+\.(credential|model)$/.test(reference)) {
        const target = get(reference.split('.').slice(0, -1).join('.'));
        const backend = state.draft.backends.find(row => row.id === target.backend);
        values = reference.endsWith('.credential') ? ['', ...(backend?.credentials || []).map(row => row.id)] : ['', ...(backend?.models || []).map(row => row.name)];
      } else if (/\.order\.\d+$/.test(reference)) values = get(reference.split('.').slice(0, 2).join('.')).layers.map(layer => layer.name);
      if (values) {
        const current = get(reference);
        select.innerHTML = [...new Set([...(current ? [current] : []), ...values])].map(value => `<option value="${esc(value)}" ${value === current ? 'selected' : ''}>${esc(value || 'None')}</option>`).join('');
      }
    });
    state.draft.models.forEach((model, index) => {
      const rail = app.querySelector(`[data-action="select"][data-path="${index}"]`);
      if (rail) rail.textContent = model.name || 'New model';
    });
    if (!state.accounts && state.view === 'routing') {
      const heading = app.querySelector('main h1');
      if (heading) heading.textContent = state.draft.models[state.selected]?.name || 'New model';
    }
  }
});
function confirmRemoval(path, item) {
  const collection = path.split(".").at(-2);
  const type = collection === "models"
    ? (path.startsWith("backends.") ? "upstream model" : "model")
    : ({ layers: "layer", targets: "target", schedules: "time window", backends: "account", credentials: "credential", windows: "price window", capacity_pools: "shared capacity pool" })[collection] || "item";
  const name = item?.name || item?.id || (collection === "targets" ? [item.backend, item.credential, item.model].filter(Boolean).join(" / ") : "") || "Unnamed";
  return confirm(`Delete ${type} "${name}"? This changes only the draft until Save.`);
}
app.addEventListener("click", async (event) => {
  const n = event.target.closest("[data-action]");
  if (!n || n.disabled || state.busy) return;
  const a = n.dataset.action,
    p = n.dataset.path;
  if ((!state.editing) && !browseActions.has(a)) return;
  try {
    if (a === "enable-edit") {
      state.editing = true;
      render();
      return;
    }
    if (a === "disable-edit") {
      if (state.dirty) {
        if (!confirm("Discard unsaved changes and disable editing?")) return;
        adopt(await request("/api/routing"));
      }
      state.editing = false;
      render();
      return;
    }
    if (a === "window") {
      state.window = p;
      await children();
      return;
    }
    if (a === "metric") {
      state.metric = p;
      render();
      return;
    }
    if (a === "older") {
      if (state.nextCursor) {
        state.cursors.push(state.cursor);
        state.cursor = state.nextCursor;
        await children();
      }
      return;
    }
    if (a === "newer") {
      if (state.cursors.length) {
        state.cursor = state.cursors.pop();
        await children();
      }
      return;
    }
    if (a === "accounts" || a === "workspace") {
      const accountsPage = a === "accounts";
      if (state.accounts === accountsPage) return;
      resetChildren();
      state.accounts = accountsPage;
      render();
      if (!state.accounts) await children();
      return;
    }
    if (a === "select") {
      resetChildren();
      state.selected = p === "all" ? "all" : Number(p);
      state.view = "statistics";
      state.window = "1d";
      state.metric = "calls";
      state.preview = null;
      render();
      if (state.view !== "routing") await children();
      return;
    }
    if (a === "view") {
      if (state.selected === "all" && p === "routing") return;
      resetChildren();
      state.view = p;
      render();
      if (p !== "routing") await children();
      return;
    }
    if (a === "discard") {
      if (state.dirty && !confirm("Discard all unsaved changes?")) return;
      adopt(await request("/api/routing"));
      state.message = "Draft discarded.";
      render();
      return;
    }
    if (a === "save") {
      if (!confirm("Save all pending changes, including accounts, prices and shared capacity? After validation, the saved configuration applies immediately to new requests.")) return;
      state.busy = true;
      state.message = "Validating upstreams and saving…";
      render();
      const doc = structuredClone(state.draft);
      doc.backends.forEach((b) =>
        b.credentials.forEach((c) => {
          if (!c.pool) delete c.pool;
        }),
      );
      try {
        adopt(
          await request("/api/routing", {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ revision: state.revision, document: doc }),
          }),
        );
        state.message = "Saved. New requests use this routing.";
      } finally {
        state.busy = false;
        render();
      }
      return;
    }
    if (a === "preview") {
      const generation = state.childGeneration;
      const selectedName = state.draft.models[state.selected]?.name;
      const view = state.view;
      const instant = state.at;
      const data = await request("/api/routing?at=" + encodeURIComponent(instant));
      if (generation !== state.childGeneration || view !== state.view || selectedName !== state.draft.models[state.selected]?.name || instant !== state.at) return;
      const model = data.models.find(model => model.name === selectedName);
      const pairs = new Set((model?.layers || []).flatMap(layer => layer.targets.map(target => JSON.stringify([target.backend, target.upstream_model]))));
      state.preview = model ? `${model.active_schedule || "Default order"}: ${model.effective_order.join(" → ")}` : "Model not yet saved";
      state.preview += "\n" + (data.pricing || []).filter(row => pairs.has(JSON.stringify([row.backend, row.model]))).map(row => `${row.backend}/${row.model}: ${activePriceText(row)}`).join("\n");
      const result = document.querySelector('#preview-result');
      if (result) result.textContent = state.preview;
      return;
    }
    if (["remove", "up", "down"].includes(a)) {
      const keys = p.split("."),
        i = Number(keys.pop()),
        array = get(keys.join("."));
      if (!Array.isArray(array) || i < 0 || i >= array.length) return;
      if (a === "remove") {
        if (!confirmRemoval(p, array[i])) return;
        if (p.match(/^models\.\d+\.layers\.\d+$/)) {
          const model = get(keys.slice(0, 2).join("."));
          model.schedules.forEach(
            (schedule) =>
              (schedule.order = schedule.order.filter(
                (name) => name !== array[i].name,
              )),
          );
        }
        array.splice(i, 1);
        if (/^models\.\d+$/.test(p)) {
          resetChildren();
          state.selected = array.length ? Math.min(i, array.length - 1) : "all";
          state.view = "statistics";
          state.window = "1d";
          state.metric = "calls";
          state.preview = null;
        }
      } else {
        const j = i + (a === "up" ? -1 : 1);
        if (j >= 0 && j < array.length)
          [array[i], array[j]] = [array[j], array[i]];
      }
      state.selected = state.selected === "all" ? "all" : Math.min(
        state.selected,
        Math.max(0, state.draft.models.length - 1),
      );
    }
    const defaults = {
      "add-model": {
        name: "",
        aliases: [],
        protocols: ["openai-chat"],
        layers: [],
        timezone: "UTC",
        schedules: [],
      },
      "add-layer": { name: "", strategy: "random", targets: [] },
      "add-target": { backend: "", credential: "", model: "" },
      "add-backend": {
        id: "",
        adapter: "custom-chat-completions",
        endpoint: "",
        credentials: [],
        models: [],
      },
      "add-credential": { id: "", api_key: "" },
      "add-upstream": { name: "", protocols: ["openai-chat"] },
      "add-pool": { id: "", max_in_flight: 1 },
      "add-schedule": {
        name: "",
        days: [1, 2, 3, 4, 5, 6, 7],
        start: "09:00",
        end: "17:00",
        order:
          state.draft.models[state.selected]?.layers.map((l) => l.name) || [],
      },
    };
    if (defaults[a]) {
      const array =
        a === "add-model"
          ? state.draft.models
          : a === "add-backend"
            ? state.draft.backends
            : a === "add-pool"
              ? state.draft.capacity_pools
              : get(p);
      if (!Array.isArray(array)) return;
      array.push(defaults[a]);
      if (a === "add-layer") {
        get(p.split(".").slice(0, 2).join(".")).schedules.forEach((schedule) =>
          schedule.order.push(""),
        );
      }
      if (a === "add-model") {resetChildren();state.selected = array.length - 1;state.view="routing";}
    }
    if (a === "add-pricing")
      get(p).pricing = {
        cache_hit_input_usd_per_million: 0,
        cache_miss_input_usd_per_million: 0,
        output_usd_per_million: 0,
      };
    if (a === "remove-pricing") {
      const model = get(p);
      if (!model || !confirm(`Remove published rates and all price windows for upstream model "${model.name || "Unnamed"}"? This changes only the draft until Save.`)) return;
      delete model.pricing;
      delete model.pricing_schedule;
    }
    if (a === "add-price-window") {
      const model=get(p);
      model.pricing_schedule ||= {timezone:'UTC',windows:[]};
      model.pricing_schedule.windows.push({name:'',days:[1,2,3,4,5,6,7],start:'09:00',end:'17:00',pricing:structuredClone(model.pricing)});
    }
    state.dirty = true;
    render();
    if (a === "remove" && /^models\.\d+$/.test(p)) await children();
  } catch (e) {
    state.message = e.message;
    status();
  }
});
render();
refresh();
setInterval(refresh, 5000);
