// End-to-end test of the web app in a real headless browser, with no npm packages: it drives
// an installed Chrome or Edge over the DevTools protocol (Node 22's built-in WebSocket).
//
//   node web/tests/e2e.mjs            (serves the repo itself on a free port)
//   node web/tests/e2e.mjs --url http://localhost:8000/web/
//
// Screenshots go to web/tests/screenshots/. Needs web/pkg (scripts/build-wasm.*) first.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { get as nodeHttpGet } from "node:http";
// a GET over node:http, the body read to its end (Node's fetch has crashed on Windows CI in
// undici, `assert(!this.paused)`, on a response whose body was left unread)
function httpGet(u) {
  return new Promise((resolve, reject) => {
    const q = nodeHttpGet(u, (r) => {
      let body = "";
      r.setEncoding("utf8");
      r.on("data", (c) => (body += c));
      r.on("end", () => resolve({ ok: r.statusCode >= 200 && r.statusCode < 300, json: () => JSON.parse(body) }));
      r.on("error", reject);
    });
    q.on("error", reject);
    q.setTimeout(5000, () => q.destroy(new Error("timeout")));
  });
}
const httpOk = (u) => (u.startsWith("http:") ? httpGet(u) : fetch(u).then(async (r) => (await r.arrayBuffer(), r))).then((r) => r.ok);


const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const shots = join(here, "screenshots");
mkdirSync(shots, { recursive: true });

const argUrl = process.argv.includes("--url") ? process.argv[process.argv.indexOf("--url") + 1] : null;
const children = [];
const cleanup = () => children.forEach((c) => { try { c.kill(); } catch {} });
process.on("exit", cleanup);
// (Windows, Node 22: Chrome gone with the DevTools socket open can trip an assertion inside
// undici's WebSocket; once closing, that one is not a test failure)
let closing = false;
process.on("uncaughtException", (e) => {
  if (closing && e?.code === "ERR_ASSERTION" && /undici/.test(e.stack || "")) return;
  console.error(e);
  process.exit(1);
});

function freePort() {
  return new Promise((resolve) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => resolve(p)); });
  });
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(fn, what, timeoutMs = 30000) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < timeoutMs) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(200);
  }
  // (what the engine was doing then: its calls not answered yet, and how long they waited)
  const busy = await js("JSON.stringify({ inFlight: window.__marcam?.inFlight?.(), lock: window.__marcam?.simLock?.busy, status: document.getElementById('status')?.textContent })").catch(() => "");
  throw new Error(`timed out waiting for ${what} (${last})${busy ? ` ${busy}` : ""}`);
}

function findBrowser() {
  const cands = process.platform === "win32"
    ? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"]
    : ["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"];
  const found = cands.find((p) => existsSync(p));
  if (!found) throw new Error("no Chrome/Edge/Chromium found");
  return found;
}

// ------------------------------------------------------------------ CDP client
class Cdp {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.waiters = new Map(); this.events = [];
    // page dialogs (confirm, prompt, beforeunload) are answered here: OK by default, the
    // prompt's own default text; set dialogAnswer = false to decline
    this.dialogs = []; this.dialogAnswer = true;
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.method === "Page.javascriptDialogOpening") {
        this.dialogs.push(msg.params);
        this.send("Page.handleJavaScriptDialog", { accept: this.dialogAnswer, promptText: msg.params.defaultPrompt ?? "" }, msg.sessionId);
      }
      if (msg.id && this.waiters.has(msg.id)) {
        const { resolve, reject } = this.waiters.get(msg.id);
        this.waiters.delete(msg.id);
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
      } else if (msg.method) this.events.push(msg);
    };
  }
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("CDP connect failed")); });
    return new Cdp(ws);
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    return new Promise((resolve, reject) => this.waiters.set(id, { resolve, reject }));
  }
}

// ------------------------------------------------------------------ run
const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}

let url = argUrl;
if (!url) {
  const port = await freePort();
  const py = spawn(process.platform === "win32" ? "python" : "python3", ["-m", "http.server", String(port), "--bind", "127.0.0.1", "--directory", root], { stdio: "ignore" });
  children.push(py);
  url = `http://127.0.0.1:${port}/web/`;
  await waitFor(() => httpOk(url), "the web server", 60000);
}

const profile = mkdtempSync(join(tmpdir(), "marcam-e2e-"));
const downloads = join(profile, "downloads");
mkdirSync(downloads);
const dport = await freePort();
const browser = spawn(findBrowser(), [
  "--headless=new", `--remote-debugging-port=${dport}`, `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--disable-extensions", "--window-size=1400,900",
  // WebGPU in headless mode, where the machine has a GPU (the GPU checks fall back otherwise)
  "--enable-unsafe-webgpu", "--enable-gpu",
  // no GPU at all (CI runners): WebGL on the CPU (SwiftShader), so the 3D view still runs
  ...(process.env.MARCAM_E2E_SOFTWARE_GL ? ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] : []),
  "about:blank",
], { stdio: "ignore" });
children.push(browser);
const version = await waitFor(() => httpGet(`http://127.0.0.1:${dport}/json/version`).then((r) => r.json()), "the browser", 20000);
const cdp = await Cdp.connect(version.webSocketDebuggerUrl);
const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
const S = (m, p) => cdp.send(m, p, sessionId);
await S("Page.enable");
await S("Runtime.enable");
await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
await cdp.send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: downloads, eventsEnabled: true });

async function js(expr) {
  const r = await S("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, replMode: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result.value;
}
async function shot(name) {
  const { data } = await S("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(shots, `${name}.png`), Buffer.from(data, "base64"));
}
const state = () => js("document.body.dataset.state");
// Generate, once it can be pressed: a project just loaded may not have enabled it yet (on a
// slow machine the click came first and was lost)
async function clickGenerate() {
  try {
    await waitFor(() => js("!document.getElementById('generate').disabled && document.body.dataset.state !== 'busy'"), "Generate to be enabled", 60000);
  } catch (e) {
    const why = await js("JSON.stringify({ disabled: document.getElementById('generate').disabled, state: document.body.dataset.state, status: document.getElementById('status').textContent, lock: window.__marcam.simLock?.busy, idle: window.__marcam.planIdle?.(), playing: window.__marcam.state.machining?.busy })");
    throw new Error(`${e.message}: ${why}`);
  }
  await js("document.getElementById('generate').click(); true");
}
// console.error from the page (an error swallowed into the console is still a failure), less
// the browser's own resource noise (a favicon) and what a test causes on purpose
// the fast suite (scripts/check.sh --fast): the core sections only
const FAST = !!process.env.MARCAM_E2E_FAST;
const EXPECTED_CONSOLE = [/favicon/i, /net::ERR_/, /Failed to load resource/];
function consoleErrors() {
  return cdp.events
    .filter((e) => e.method === "Runtime.consoleAPICalled" && e.params.type === "error")
    .map((e) => e.params.args.map((a) => a.value ?? a.description ?? "").join(" "))
    .filter((t) => !EXPECTED_CONSOLE.some((r) => r.test(t)));
}
// the playback open and nothing more coming for it (no seek, no simulation running or waiting:
// a reopen after an eye change is one)
const PLAYBACK_SETTLED = "!!window.__marcam.state.machining?.open && !window.__marcam.state.machining.busy && !window.__marcam.simLock.busy && !window.__marcam.simLock.waiting && document.body.dataset.state !== 'busy'";
// Stop the simulations running by themselves (the stock after a plan): cancelled until none
// runs or waits for half a second (a slow machine takes minutes over them)
async function stopBackground() {
  const busy = "!!(window.__marcam.simRing.active || window.__marcam.simLock.busy || window.__marcam.simLock.waiting)";
  for (let k = 0; k < 120; k++) {
    if (await js(busy)) {
      await js("window.__marcam.cancelEngine(); true");
    } else {
      await sleep(500);
      if (!(await js(busy))) return;
    }
    await sleep(250);
  }
  throw new Error(`the background simulation does not stop: ${await js("JSON.stringify({ ring: window.__marcam.simRing.active, lock: window.__marcam.simLock.busy, waiting: window.__marcam.simLock.waiting, status: document.getElementById('status').textContent })")}`);
}
// The operation types the tree offers (the tree renders after a plan: waited for)
async function addTypes() {
  await waitFor(() => js("!!document.getElementById('treeAddType')"), "the tree's operation types", 30000);
  return js("[...document.getElementById('treeAddType').options].map((o) => o.value)");
}
// (Windows CI: its browser has run the engine's WebAssembly far slower than anywhere else,
// the same plans taking seconds natively and in Node on that runner and minutes in the page;
// see scripts/knob-timing.mjs, run after check.sh. A plan is waited for four times longer
// there: a plan that never ends still fails.)
const PLAN_WAIT = process.platform === "win32" && process.env.CI ? 4 : 1;
async function waitDone(what, timeoutMs) {
  timeoutMs *= PLAN_WAIT;
  try {
    return await waitFor(async () => { const s = await state(); if (s === "error") throw new Error(await js("document.getElementById('error').textContent")); return s === "done"; }, what, timeoutMs);
  } catch (e) {
    const why = await js("JSON.stringify({ state: document.body.dataset.state, status: document.getElementById('status').textContent, disabled: document.getElementById('generate').disabled, lock: window.__marcam.simLock?.busy, idle: window.__marcam.planIdle?.(), machining: document.body.dataset.machining, inFlight: window.__marcam.inFlight?.(), workerErrors: window.__marcam.workerErrors || [] })").catch(() => "?");
    throw new Error(`${e.message}: ${why}`);
  }
}
// Solids must render closed: looking down the rotary axis at either end, the middle of the
// view must show the end face, not the background through an open shell.
const VIEW_PREFIX = process.env.MARCAM_SHOT_PREFIX || "";
async function closedViews(tag) {
  const bg = [0, 0, 0];
  const out = [];
  for (const layer of ["part", "stock", "sim"]) {
    const has = await js(`window.__marcam.state.three.groups.${layer}.children.length > 0`);
    if (!has) continue;
    for (const end of ["front", "back"]) {
      const show = { path: false, axes: false, part: layer === "part", stock: layer === "stock", sim: layer === "sim" };
      await js(`window.__marcam.setView(${JSON.stringify(end)}, ${JSON.stringify(show)})`);
      await sleep(200);
      // the axis line of sight and a small ring around it must all hit the end face
      let px = [], filled = true;
      for (const [fx, fy] of [[0.5, 0.5], [0.49, 0.5], [0.51, 0.5], [0.5, 0.49], [0.5, 0.51]]) {
        const p = String(await js(`window.__marcam.pixelAt(${fx}, ${fy})`)).split(",").map(Number);
        if (!p.some((c, k) => Math.abs(c - bg[k]) > 4)) filled = false; // background is exactly black
        px = p;
      }
      await shot(`${VIEW_PREFIX}view_${tag}_${layer}_${end}`);
      out.push({ layer, end, filled, px });
    }
    await js(`window.__marcam.setView("iso", { path: false, axes: false, part: ${layer === "part"}, stock: ${layer === "stock"}, sim: ${layer === "sim"} })`);
    await sleep(200);
    await shot(`${VIEW_PREFIX}view_${tag}_${layer}_iso`);
  }
  await js(`window.__marcam.setView("iso", { path: true, axes: true, part: true, stock: true, sim: true })`);
  return out;
}

// The time estimates as shown: the status line, the summary, each file and each operation in
// the tree; with whether each is visible on screen and its value in seconds (0: none).
const SHOWN_TIMES = `(() => {
  const secs = (t) => { if (!t) return 0; let s = 0; for (const [, n, u] of String(t).matchAll(/(\\d+(?:\\.\\d+)?)\\s*(h|min|s)\\b/g)) s += Number(n) * (u === "h" ? 3600 : u === "min" ? 60 : 1); return s; };
  const vis = (e) => !!e && e.checkVisibility({ opacityProperty: true, visibilityProperty: true }) && e.getClientRects().length > 0;
  const st = document.getElementById("status").textContent;
  const m = /estimated machining time (.+)$/.exec(st);
  const sum = document.getElementById("timeEst");
  const res = window.__marcam.state.result;
  return {
    status: m ? secs(m[1]) : 0,
    summary: vis(sum) ? secs(sum.querySelector("strong")?.textContent) : 0,
    files: [...document.querySelectorAll("#tree .tree-out")].map((o) => { const e = o.querySelector(".tree-outname .tree-time"); return { name: o.dataset.out, s: vis(e) ? secs(e.textContent) : 0 }; }),
    ops: [...document.querySelectorAll("#tree .tree-op")].map((r) => { const e = r.querySelector(".tree-time"); return { id: r.dataset.op, on: !r.classList.contains("off"), s: vis(e) ? secs(e.textContent) : 0, stale: !!e && getComputedStyle(e).opacity < 1 }; }),
    total: res ? res.time_total_s : 0,
    fileTotals: res ? res.programs.map((p) => p.time?.total_s || 0) : [],
    stale: document.body.classList.contains("stale"),
  };
})()`;
/** Every enabled operation, every file and the total show a non-zero time; they add up. */
function timesOk(t) {
  const ops = t.ops.filter((o) => o.on);
  const bad = [];
  if (!(t.status > 0)) bad.push("status");
  if (!(t.summary > 0)) bad.push("summary");
  if (!t.files.length || t.files.some((f) => !(f.s > 0))) bad.push(`files ${JSON.stringify(t.files)}`);
  if (!ops.length || ops.some((o) => !(o.s > 0))) bad.push(`ops ${JSON.stringify(ops)}`);
  // shown rounded: the files add up to the total within a minute each
  const fsum = t.fileTotals.reduce((a, b) => a + b, 0);
  if (Math.abs(fsum - t.total) > 1) bad.push(`files ${fsum} vs total ${t.total}`);
  if (Math.abs(t.status - t.total) > 60 + t.total * 0.01) bad.push(`status ${t.status} vs ${t.total}`);
  return bad;
}

// longer than the auto-regenerate delay (800 ms)
const AUTO_WAIT = 1500;
const golden = (name) => readFileSync(join(root, "tests", "golden", `${name}.nc`), "utf8").replace(/\r\n/g, "\n");

let failed = false;
try {
  await S("Page.navigate", { url });
  await waitFor(async () => (await state()) === "ready", "the engine to load", 30000);
  check("page loads and the WebAssembly engine starts", true, await js("document.getElementById('version').textContent"));
  // (the app's times as the engine's: the minutes rounded before the hours split off; the
  // impeller's finish showed "2 h 60 min")
  {
    const f = JSON.parse(await js("JSON.stringify([10785, 10769, 3600, 7229, 7230, 3599].map((s) => window.__marcam.fmtTime(s)))"));
    check("times: the minutes rounded before the hours (2 h 59 min 45 s is 3 h 00 min)", JSON.stringify(f) === JSON.stringify(["3 h 00 min", "2 h 59 min", "1 h 00 min", "2 h 00 min", "2 h 01 min", "59 min 59 s"]), JSON.stringify(f));
  }
  // the live updating (on by default) is tested in its own section (8v): the sections before
  // it plan when they ask (Tools > Replan all), as they were written
  const liveDefault = await js("document.getElementById('autoGen').checked");
  await js("const a = document.getElementById('autoGen'); a.checked = false; a.dispatchEvent(new Event('change')); true");
  await waitFor(async () => (await state()) !== "busy", "a startup update to end", 60000);
  // the playback's stock (off by default) on: the sections written for the machining
  // simulation play it with the stock
  const stockDefault = await js("document.getElementById('simStock').checked");
  await js("const s = document.getElementById('simStock'); s.checked = true; s.dispatchEvent(new Event('change')); true");

  // 1. knob example on the Makera Z1: byte-identical to the cut G-code
  await clickGenerate();
  await waitDone("knob generation", 60000);
  const tabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  check("two programs, both verified OK", JSON.stringify(tabs) === JSON.stringify(["knob_1_profileOK", "knob_2_knurlOK"]), tabs.join(", "));
  const lines = [];
  for (let i = 0; i < 2; i++) {
    await js(`document.querySelectorAll('#tabs button')[${i}].click()`);
    lines.push(await js("document.getElementById('lineCount').textContent"));
    const name = i ? "knob_2_knurl" : "knob_1_profile";
    const text = await js("window.__marcam.state.result.programs[window.__marcam.state.tab].gcode");
    check(`${name}: G-code view matches the golden file byte for byte`, text === golden(name), `${text.length} chars`);
    const issues = await js("document.querySelector('#summary .issues').textContent");
    check(`${name}: verifier reports 0 issues`, issues.includes("0 issues"), issues.slice(0, 60));
    const holder = await js("document.getElementById('holderCheck')?.textContent || ''");
    check(`${name}: holder/shank check clear`, holder.includes("clear"), holder);
  }
  check("line counts 21,787 / 15,146", lines.join(" / ") === "21,787 / 15,146", lines.join(" / "));

  // 2. 3D view (three.js from the CDN) and the scrubber
  await js("document.querySelectorAll('#tabs button')[0].click()");
  await waitFor(() => js("!!(window.__marcam.state.three && (window.__marcam.state.three.failed || window.__marcam.state.three.cutLines))"), "the 3D view", 30000);
  const three = await js("window.__marcam.state.three.failed ? 'failed: ' + document.getElementById('viewNote').textContent : 'ok'");
  check("3D preview renders (three.js)", three === "ok", three);
  if (three === "ok") {
    const full = await js("window.__marcam.state.three.cutLines.geometry.drawRange.count");
    await js("const s = document.getElementById('scrub'); s.value = 3000; s.dispatchEvent(new Event('input')); true");
    await sleep(300);
    const part = await js("window.__marcam.state.three.cutLines.geometry.drawRange.count");
    const label = await js("document.getElementById('scrubLabel').textContent");
    check("scrubber limits the drawn toolpath", part > 0 && part < full, `${part} of ${full} vertices; ${label}`);
    await shot("01_knob_scrubbed");
    await js("const s = document.getElementById('scrub'); s.value = s.max; s.dispatchEvent(new Event('input')); true");
  }
  await shot("02_knob_generated");

  // 3. download
  await js("document.getElementById('download').click()");
  const file = await waitFor(() => readdirSync(downloads).find((f) => f.endsWith(".nc")), "the .nc download", 60000).catch(async (e) => {
    const why = await js("JSON.stringify({ idle: window.__marcam.planIdle?.(), state: document.body.dataset.state, status: document.getElementById('status').textContent, programs: window.__marcam.state.result?.programs?.length, tab: window.__marcam.state.tab })").catch(() => "?");
    throw new Error(`${e.message}: ${why}; downloads: ${readdirSync(downloads).join(", ") || "none"}`);
  });
  const dl = readFileSync(join(downloads, file), "utf8");
  check("Download .nc saves the program", file === "knob_1_profile.nc" && dl === golden("knob_1_profile"), file);

  // 4. simulation
  await js("document.getElementById('simulate').click()");
  await waitDone("simulation", 300000);
  const simText = await js("document.getElementById('summary').innerText");
  check("simulated stock: no gouges, no rapids through stock", /deepest gouge 0\.000 mm; 0 rapid/.test(simText), simText.split("\n").pop());
  await shot("03_knob_simulated");
  const kv = await closedViews("knob");
  check("knob: part, stock and simulated stock render closed (end faces filled)",
    kv.length === 6 && kv.every((v) => v.filled), kv.map((v) => `${v.layer}/${v.end}:${v.filled ? "filled" : "OPEN " + v.px}`).join(" "));

  // 4b. solid machining simulation: playback, scrubbing, per-operation colours
  await js("document.getElementById('simOpen').click(); true");
  await waitFor(() => js("document.body.dataset.machining === 'open'"), "the machining simulation", 60000);
  const M = "window.__marcam.machining.state";
  const nseg = await js(`${M}.session.segments`);
  const ops = await js(`${M}.session.ops.map(o => o.kind).join(',')`);
  check("machining simulation opens with the job's operations", nseg > 30000 && ops === "rotary_turning,helical_knurl,flank_chamfer", `${nseg} moves; ${ops}`);
  await js("window.__marcam.setView('iso', {})");
  await sleep(300);
  await shot("08_machining_start");
  const seekTo = async (n) => {
    await js(`const s = document.getElementById('simScrub'); s.value = ${n}; s.dispatchEvent(new Event('input')); true`);
    try {
      await waitFor(() => js(`${M}.pos === ${n} && !${M}.busy`), `seeking to ${n}`, 60000);
    } catch (e) {
      const why = await js(`JSON.stringify({ pos: ${M}.pos, target: ${M}.target, wanted: ${M}.wanted, busy: ${M}.busy, stock: ${M}.stock, open: ${M}.open, segs: ${M}.session?.segments, max: document.getElementById('simScrub').max, lock: window.__marcam.simLock.busy, status: document.getElementById('status').textContent })`);
      throw new Error(`${e.message}: ${why}`);
    }
  };
  const cutCells = (tag) => js(`${M}.op.reduce((n, t) => n + (t === ${tag}), 0)`);
  const midRough = Math.round(nseg * 0.15);
  await seekTo(midRough);
  const rough1 = await cutCells(1);
  const minR = await js(`Math.min(...${M}.r)`);
  check("seeking cuts the stock (mid-roughing)", rough1 > 1000 && minR < 12.5, `${rough1} cells cut by roughing, min radius ${minR.toFixed(2)}`);
  await js("window.__marcam.setView('iso', {})");
  await sleep(300);
  await shot("09_machining_mid_roughing");
  // play advances
  await js("document.getElementById('simSpeed').value = '3000'; document.getElementById('simPlay').click(); true");
  await sleep(1500);
  await js("document.getElementById('simPlay').click(); true");
  await waitFor(() => js(`!${M}.busy`), "playback to settle", 10000);
  const played = await js(`${M}.pos`);
  check("Play advances the simulation", played > midRough + 100, `${midRough} -> ${played}`);
  // scrubbing back recomputes
  await seekTo(500);
  const back = await cutCells(1);
  check("scrubbing back recomputes the stock", back < rough1, `roughing cells ${rough1} at ${midRough}, ${back} at 500`);
  // after the knurl
  await seekTo(nseg);
  const k2 = await cutCells(2), k3 = await cutCells(3);
  check("after the knurl, knurl and chamfer cells carry their operations", k2 > 10000 && k3 > 100, `knurl ${k2} cells, chamfer ${k3}`);
  const closed = String(await js(`const { checkClosed } = await import('./meshes.js'); const g0 = ${M}.stockMesh.geometry; const c0 = checkClosed({ positions: g0.attributes.position.array, indices: g0.index.array }); c0.closed + ' ' + c0.badEdges + ' ' + c0.volume.toFixed(0)`));
  check("the simulated solid stays watertight", closed.startsWith("true"), closed);
  await js("window.__marcam.setView('iso', {})");
  await sleep(300);
  await shot("10_machining_after_knurl");
  // operation colour
  const colorAt = `(() => { const s = ${M}; const i = s.op.indexOf(2); return Array.from(s.stockMesh.geometry.attributes.color.array.slice(3 * i, 3 * i + 3)).map(c => c.toFixed(2)).join(','); })()`;
  const before = await js(colorAt);
  await js("const c = document.querySelector('#opLegend input[data-tag=\"2\"]'); c.value = '#ff0000'; c.dispatchEvent(new Event('input', { bubbles: true })); true");
  const after = await js(colorAt);
  const saved = await js("localStorage.getItem('marcam.opColors')");
  check("changing an operation's colour recolours its cuts (and is saved)", before !== after && after === "1.00,0.00,0.00" && /#ff0000/.test(saved), `${before} -> ${after}`);
  await js("window.__marcam.setView('iso', {})");
  await sleep(300);
  await shot("11_machining_recoloured");
  await js("document.getElementById('simClose').click(); true");

  // (the fast suite, MARCAM_E2E_FAST: these sections are left to the full one)
  if (!FAST) {
  // 5. LinuxCNC machine (G93)
  await js("document.getElementById('machine').value = 'linuxcnc-4axis'; document.getElementById('machine').dispatchEvent(new Event('change')); true");
  await clickGenerate();
  await waitDone("LinuxCNC generation", 60000);
  const g93 = await js("window.__marcam.state.result.programs[window.__marcam.state.tab].gcode.includes('\\nG93 ')");
  check("LinuxCNC post uses G93 inverse time", g93);

  // 5b. machine profiles: the Carvera, with its limits checked
  await js("document.getElementById('machine').value = 'makera-carvera-4axis'; document.getElementById('machine').dispatchEvent(new Event('change')); true");
  await clickGenerate();
  await waitDone("Carvera generation", 60000);
  const info = await js("document.getElementById('machineInfo').textContent");
  check("Carvera profile shows its limits", /X 360 mm/.test(info) && /ATC 6 slots/.test(info), info);
  const lim = await js("document.getElementById('limits').innerText");
  check("limit check flags the Carvera's 5 RPM rotary cap (5209 > 1800 deg/min)", /Carvera/.test(lim) && /5209 deg\/min/.test(lim) && /1800 deg\/min/.test(lim), lim.split("\n")[1] || lim);

  // 5c. tool library: import a Fusion 360 library by URL, edit a tool, use it as T1
  await js("document.querySelector('#tree [data-act=library]')?.click(); document.getElementById('libraryPanel').open = true; document.getElementById('importUrl').value = new URL('../tests/fixtures/fusion-tool-library.json', location).href; document.getElementById('importUrlBtn').click(); true");
  const imported = await waitFor(() => js("document.getElementById('importInfo').textContent"), "the library import", 10000);
  check("Fusion tool library imports from a URL", /3 tools/.test(imported) && (await js("document.getElementById('library').value")) === "fusion-tool-library", imported);
  await js("document.querySelector('#toolTable button[data-edit=\"0\"]').click(); document.getElementById('tfName').value = 'Edited Flat 3.0'; document.getElementById('tfDiameter').value = '3.0'; document.getElementById('tfSave').click(); true");
  const row = await js("document.querySelector('#toolTable tr[data-tool]').innerText");
  check("editing a tool updates the library", /Edited Flat 3\.0/.test(row) && /Ø3\b/.test(row), row.replace(/\s+/g, " "));
  await js("const s = document.querySelector('#toolTable select[data-use=\"0\"]'); s.value = '1'; s.dispatchEvent(new Event('change', { bubbles: true })); true");
  const t1 = await js("JSON.parse(document.getElementById('job').value).tools.find(t => t.number === 1)");
  check("library tool becomes the job's T1", t1.description === "Edited Flat 3.0" && t1.diameter === 3 && t1.kind === "flat", JSON.stringify(t1));
  await js("document.getElementById('machine').value = 'makera-z1-4axis'; document.getElementById('machine').dispatchEvent(new Event('change')); true");
  await clickGenerate();
  await waitDone("generation with the library tool", 60000);
  await js("document.querySelectorAll('#tabs button')[0].click()");
  const head = await js("window.__marcam.state.result.programs[window.__marcam.state.tab].gcode.split('\\n').slice(0, 3).join(' | ')");
  const issues0 = await js("document.querySelector('#summary .issues').textContent");
  check("generates with the edited tool", head.includes("(T1 Edited Flat 3.0)") && issues0.includes("0 issues"), head);
  // a tapered ball: edited in the form, then added to the job as a new tool number
  await js("document.querySelector('#toolTable button[data-edit=\"1\"]').click(); document.getElementById('tfKind').value = 'tapered_ball'; document.getElementById('tfDiameter').value = '3.175'; document.getElementById('tfTip').value = '1'; document.getElementById('tfTaper').value = '5'; document.getElementById('tfSave').click(); true");
  const tbRow = await js("document.querySelectorAll('#toolTable tr[data-tool]')[1].innerText");
  await js("const s = document.querySelector('#toolTable select[data-use=\"1\"]'); s.value = 'new'; s.dispatchEvent(new Event('change', { bubbles: true })); true");
  const tbTool = await js("JSON.parse(document.getElementById('job').value).tools.find(t => t.kind === 'tapered_ball')");
  check("a tapered ball tool is edited and joins the job", /tapered ball tip Ø1 5°/.test(tbRow) && tbTool && tbTool.tip_diameter === 1 && tbTool.taper_angle === 5 && tbTool.diameter === 3.175, `${tbRow.replace(/\s+/g, " ")} ${JSON.stringify(tbTool)}`);
  await shot("07_tool_library");
  await S("Page.reload");
  await waitFor(async () => (await state()) === "ready", "reload", 30000);
  const kept = await js("[...document.getElementById('library').options].map(o => o.value).join(',') + ' / ' + JSON.stringify(JSON.parse(localStorage.getItem('marcam.libraries')).find(l => l.id === 'fusion-tool-library').tools[0].name)");
  check("tool libraries persist across reloads", /fusion-tool-library/.test(kept) && /Edited Flat 3\.0/.test(kept), kept);
  await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  await sleep(500);

  }
  // 5d. project tree (document model 0.2)
  await js("document.getElementById('machine').value = 'makera-z1-4axis'; document.getElementById('machine').dispatchEvent(new Event('change')); document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.querySelectorAll('#tree .tree-op').length === 3"), "the project tree", 60000);
  const outs = await js("[...document.querySelectorAll('#tree .tree-out')].map(o => o.dataset.out + ':' + o.querySelectorAll('.tree-op').length).join(' ')");
  check("tree shows the knob as one setup, two files, three operations", outs === "knob_1_profile:1 knob_2_knurl:2", outs);
  const gen = async () => {
    await clickGenerate();
    await waitDone("generation", 120000);
    return js("({ lines: window.__marcam.state.result.programs.map(p => p.lines), knurl: (window.__marcam.state.result.programs.find(p => p.name === 'knob_2_knurl') || {}).gcode || '' })");
  };
  const treeAct = (op, act) => js(`document.querySelector('#tree .tree-op[data-op="${op}"] [data-act="${act}"]').click(); true`);
  const settle = () => waitFor(() => js("!!window.__marcam.state.tree.doc && document.body.dataset.state !== 'busy'"), "tree", 60000).then(() => sleep(400)).catch(async (e) => {
    throw new Error(`${e.message}: ${await js("JSON.stringify({ doc: !!window.__marcam.state.tree.doc, state: document.body.dataset.state, status: document.getElementById('status').textContent, error: document.getElementById('error').textContent, dialog: (document.getElementById('setupDialog').open && !document.getElementById('setupDialog').classList.contains('embedded')) })")}`);
  });
  // (Mar: no Apply) the operation selected: a new one not complete yet, else the project's
  const SEL = "(() => { const t = window.__marcam.state.tree; return t.draft?.op.id === t.selected ? t.draft.op : t.doc?.setups.find((x) => x.id === t.setup)?.operations.find((o) => o.id === t.selected); })()";
  /** The setup worked on made `kind` in its details (applied as it is clicked). */
  const setKind = async (kind) => {
    const done = `(() => { try { const d = JSON.parse(document.getElementById('job').value); return (d.setups.find((x) => x.id === window.__marcam.state.tree.setup) ?? (d.setups.length === 1 ? d.setups[0] : null))?.kind === '${kind}' && document.body.dataset.state !== 'busy'; } catch { return false; } })()`;
    // (a plan landing meanwhile draws the tree again and opens the form afresh, the click
    // lost with it, on a slow runner: clicked again)
    for (let k = 0; k < 3; k++) {
      await settle();
      await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
      await waitFor(() => js("!!document.querySelector('#setupHost #setupDialog[open]')"), "the setup's form in its details", 60000);
      await js(`document.querySelector('#setupHost .sd-kind[data-kind="${kind}"]').click(); true`);
      if (await waitFor(() => js(done), `the setup a ${kind}`, 30000).then(() => true, () => false)) break;
    }
    await waitFor(() => js(`(() => { try { const d = JSON.parse(document.getElementById('job').value); return (d.setups.find((x) => x.id === window.__marcam.state.tree.setup) ?? (d.setups.length === 1 ? d.setups[0] : null))?.kind === '${kind}' && document.body.dataset.state !== 'busy'; } catch { return false; } })()`), `the setup a ${kind}`, 120000).catch(async (e) => {
      // (why not: the setups' kinds, the one worked on, what was said)
      throw new Error(`${e.message}: ${await js("JSON.stringify({ kinds: (() => { try { return JSON.parse(document.getElementById('job').value).setups.map((x) => x.id + ':' + x.kind); } catch { return 'no project'; } })(), tree: window.__marcam.state.tree.setup, error: document.getElementById('error').textContent, warn: document.getElementById('sdWarn')?.textContent, form: !!document.querySelector('#setupHost #setupDialog[open]'), checked: document.querySelector('#setupHost input[name=sdKind]:checked')?.value })")}`);
    });
    await settle();
  };
  /** The setup worked on: its details form open, `fill` (a page expression) run on it, its
   *  changes applied as a person's are (a change event), until `until` holds. */
  const inDetails = async (fill, until, what) => {
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await waitFor(() => js("!!document.querySelector('#setupHost #setupDialog[open]')"), "the setup's form in its details", 60000);
    await js(`(() => { ${fill}; for (const id of ['sdMaterial', 'sdWork', 'sdLineNums', 'sdHeader', 'sdFooter']) document.getElementById(id)?.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await waitFor(() => js(until), what, 120000);
    await settle();
  };
  /** The setup worked on given its own stock of `type` (the Stock's dropdown). */
  const ownStock = async (type) => {
    await js("document.querySelector('#tree .tree-setup.active [data-act=stock]').click(); true");
    await waitFor(() => js("!!document.getElementById('stType')"), "the Stock pane", 30000);
    await js(`const t = document.getElementById('stType'); t.value = '${type}'; t.dispatchEvent(new Event('change', { bubbles: true })); true`);
    await settle();
  };
  /** + Add setup in the tree (no dialog): `kind` (vise or rotary), continuing from the setup
   *  before ("carry") or independent ("own"); added at once, its Stock shown. */
  const addSetup = async (kind = "vise", from = "carry") => {
    // (the tree's: a 0.1 job shown as a project has its setups there only)
    const count = "(window.__marcam.state.tree.doc?.setups || []).length";
    const n0 = await js(count);
    await js(`(() => { for (const [id, v] of [['treeAddKind', '${kind}'], ['treeAddFrom', '${from}']]) { const e = document.getElementById(id); if (!e) continue; e.value = v; e.dispatchEvent(new Event('change', { bubbles: true })); } document.getElementById('treeAddSetup').click(); return true; })()`);
    await waitFor(() => js(`${count} === ${n0 + 1} && document.body.dataset.state !== 'busy'`), "the setup added", 60000);
    await settle();
  };
  await treeAct("op3", "enable");
  await settle();
  const off = await gen();
  const schema = await js("JSON.parse(document.getElementById('job').value).schema_version");
  check("turning an operation off drops it from the G-code (document saved as 0.2)", schema === "0.2" && off.lines[1] < 15146 && !off.knurl.includes("front chamfer"), `knurl file ${off.lines[1]} lines; schema ${schema}`);
  await treeAct("op3", "enable");
  await settle();
  // (Mar) reordered by dragging: op3 dropped on the upper half of op2 (the arrows are gone)
  const dragOnto = (src, dst, before) => js(`(() => { const a = document.querySelector('#tree .tree-op[data-op="${src}"]'), b = document.querySelector('#tree .tree-op[data-op="${dst}"]');
    const dt = new DataTransfer(); const r = b.getBoundingClientRect(); const y = ${before} ? r.top + 2 : r.bottom - 2;
    a.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
    b.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt, clientY: y }));
    const line = b.classList.contains(${before} ? 'drop-before' : 'drop-after');
    b.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt, clientY: y }));
    a.dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt }));
    return line; })()`);
  const arrows = await js("document.querySelectorAll('#tree [data-act=\"up\"], #tree [data-act=\"down\"]').length");
  const line = await dragOnto("op3", "op2", true);
  await settle();
  const order = await js("JSON.parse(document.getElementById('job').value).setups[0].outputs.map((o) => o.operations.join(',')).join(' | ')");
  const swapped = await gen();
  const ci = swapped.knurl.indexOf("front chamfer"), ki = swapped.knurl.indexOf("knurl family");
  check("reordering by dragging (a drop line shows where; no arrows): the project's order and the G-code's", arrows === 0 && line && /op3,op2/.test(order) && ci > 0 && ki > ci, `arrows ${arrows}; line ${line}; ${order}; chamfer at ${ci}, knurl at ${ki}`);
  // undone: the old order
  await js("document.getElementById('undoSetup').click(); true");
  await settle();
  const undone = await js("JSON.parse(document.getElementById('job').value).setups[0].outputs.map((o) => o.operations.join(',')).join(' | ')");
  // and with the keyboard: Alt+Up on the selected op3
  await js("document.querySelector('#tree .tree-op[data-op=\"op3\"] [data-act=\"select\"]').click(); true");
  await sleep(200);
  await js("document.getElementById('tree').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', altKey: true, bubbles: true })); true");
  await settle();
  const keyed = await js("JSON.parse(document.getElementById('job').value).setups[0].outputs.map((o) => o.operations.join(',')).join(' | ')");
  check("Undo puts the order back; Alt+Up moves the selected operation up", /op2,op3/.test(undone) && /op3,op2/.test(keyed), `${undone} -> ${keyed}`);
  await js("document.getElementById('undoSetup').click(); true");
  await settle();
  await js("document.querySelector('#tree .tree-op[data-op=\"op3\"] [data-act=\"select\"]').click(); true");
  await settle();
  // every operation type: the create form comes fully filled and applies without problems
  // the types offered for this (rotary) setup: the 3-axis ones need a vise setup
  const typesAll = (await addTypes()).join(",");
  const allKinds = await js("window.__marcam.state.tree.types.map(t => t.kind).join(',')");
  check("a rotary setup offers every operation type (3-axis ones indexed)", String(allKinds).split(",").length === 22 && String(typesAll).split(",").length === 22, `${typesAll}`);
  const filled = [];
  const took = [];
  let autoSeen = null;
  for (const kind of String(typesAll).split(",")) {
    const t0 = Date.now();
    const before = await js("document.querySelectorAll('#tree .tree-op').length");
    await js(`document.getElementById('treeAddType').value = '${kind}'; document.getElementById('treeAdd').click(); true`);
    await waitFor(() => js(`!!document.getElementById('opForm') && ${SEL}?.type === '${kind}'`), `the ${kind} form`, 60000);
    // blank fields, but text whose registry default is "" (none: e.g. a V-carve's text)
    // (a setting the registry marks auto, empty on purpose: worked out when planned)
    // (the auto ones: there, empty, and folded away under More)
    if (kind === "rotary_rough") autoSeen = await js(`(() => { const e = document.querySelector('#opForm [name=max_bite]'); const d = e?.closest('details'); return { there: !!e, empty: e?.value === '', folded: !!d && d.id === 'opMore' && !d.open, auto: !!window.__marcam.tree.state.types.find((x) => x.kind === 'rotary_rough').params.find((p) => p.name === 'max_bite')?.auto }; })()`);
    const blanks = await js(`(() => { const ty = window.__marcam.tree.state.types.find(x => x.kind === '${kind}'); const none = new Set(ty.params.filter(p => p.default === '' || (Array.isArray(p.default) && !p.default.length) || p.auto).map(p => p.name)); return [...document.getElementById('opForm').querySelectorAll('input, select')].filter(e => e.type !== 'checkbox' && e.type !== 'file' && !['__name', '__axial'].includes(e.name) && !none.has(e.name) && e.value.trim() === '').map(e => e.name).join(','); })()`);
    // (Mar's picking rule) the types whose geometry is a placeholder until a pick: not added,
    // the one-line prompt shown instead (the next Add replaces the draft)
    if (["contour", "pocket", "drill", "chamfer", "thread_mill", "bore", "custom"].includes(kind)) {
      await js("document.getElementById('opForm').requestSubmit(); true");
      await sleep(800);
      const held = await js(`({ n: document.querySelectorAll('#tree .tree-op').length, prompt: document.getElementById('pickNone')?.textContent || '' })`);
      filled.push(`${kind}:${blanks || "none blank"}${held.n === before && (kind === "custom" || /^Nothing picked/.test(held.prompt)) ? "" : ` (added before a pick: ${JSON.stringify(held)})`}`);
      if (blanks) break;
      took.push(`${kind} ${((Date.now() - t0) / 1000).toFixed(1)} s`);
      continue;
    }
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(() => js(`document.querySelectorAll('#tree .tree-op').length === ${before + 1}`), `${kind} to be added`, 120000).catch(async (e) => {
      // (what held it: the form still open and why, the status line)
      throw new Error(`${e.message}: ${await js("JSON.stringify({ form: !!document.getElementById('opForm'), invalid: [...(document.getElementById('opForm')?.querySelectorAll(':invalid') ?? [])].map((x) => x.name), problems: document.getElementById('opForm')?.querySelector('.problems')?.textContent ?? null, state: document.body.dataset.state, status: document.getElementById('status').textContent })")}`);
    });
    const id = await js("[...document.querySelectorAll('#tree .tree-op')].pop().dataset.op");
    filled.push(`${kind}:${blanks || "none blank"}`);
    if (blanks) break;
    await treeAct(id, "delete");
    await settle();
    took.push(`${kind} ${((Date.now() - t0) / 1000).toFixed(1)} s`);
  }
  check("every operation type's new form is fully filled and applies cleanly (those needing a pick, or a program, wait for it)", filled.length === 22 && filled.every((f) => f.endsWith("none blank")), `${filled.join(" ")}; ${took.join(", ")}`);
  check("rotary roughing's max bite: marked auto, empty and folded under More in a new form (worked out when planned)", !!autoSeen && autoSeen.there && autoSeen.empty && autoSeen.folded && autoSeen.auto, JSON.stringify(autoSeen));
  // changing the tool refreshes defaults the user hasn't edited
  await js("document.getElementById('treeAddType').value = 'rotary_finish'; document.getElementById('treeAdd').click(); true");
  await waitFor(() => js(`${SEL}?.type === 'rotary_finish'`), "the finish form", 60000);
  const pat1 = await js("document.getElementById('opForm').elements.pattern.value");
  await js("const f = document.getElementById('opForm'); f.elements.feed.value = '123'; f.elements.feed.dispatchEvent(new Event('input', { bubbles: true })); f.elements.tool.value = '2'; f.elements.tool.dispatchEvent(new Event('change', { bubbles: true })); true");
  await waitFor(() => js(`${SEL}?.tool === 2 && document.getElementById('opForm').elements.pattern.value === 'along'`), "defaults for the V-bit", 60000);
  const keptFeed = await js("document.getElementById('opForm').elements.feed.value");
  check("changing the tool refreshes untouched defaults and keeps edited ones", pat1 === "around" && keptFeed === "123", `pattern around -> along with the V-bit; edited feed kept at ${keptFeed}`);
  // (no Cancel: it is in the project; deleted)
  await settle();
  await treeAct(await js("window.__marcam.state.tree.selected"), "delete");
  await settle();

  // add an operation from the registry's form
  await js("document.getElementById('treeAddType').value = 'flank_chamfer'; document.getElementById('treeAdd').click(); true");
  await waitFor(() => js("!!document.getElementById('opForm')"), "the operation form", 60000);
  // (the Advanced section: its typed edge cleared, the form says it is missing)
  await js("document.querySelector('#opAdvanced > summary').click(); true");
  await js("const f0 = document.getElementById('opForm'); f0.elements.edge.value = ''; f0.requestSubmit(); true");
  const probs = await waitFor(() => js("document.getElementById('opErrors')?.textContent || ''"), "the missing-field message", 60000);
  // (Mar: no typed coordinates) then the edge picked on the part, as a person does: a click
  // in the 3D view on the chamfer just behind the front edge (X0, Ø14.4)
  // (live picking: on while the operation is selected, no button to press)
  await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking the edge", 60000);
  await js("window.__marcam.setView('iso', {})");
  await sleep(300);
  const edgeAt = await js("(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(-0.1, -7.3, 0).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()");
  const edgeHit = await js(`await window.__marcam.tree.pickAt(${edgeAt[0]}, ${edgeAt[1]}).then((r) => r?.pickable?.label || 'nothing')`);
  await sleep(300);
  // (each pick an edit of its own: the form shows it once planned again)
  await waitFor(() => js("document.getElementById('opForm')?.elements.edge.value === '0, 7.2'"), "the edge picked", 60000).catch(() => {});
  await settle();
  const edgePicked = await js("document.getElementById('opForm').elements.edge.value");
  check("a round edge picked on the part fills the chamfer's edge (over the one typed blank)", /X0\.00/.test(edgeHit) && edgePicked === "0, 7.2", `${edgeHit}; edge ${edgePicked}`);
  await js("const f = document.getElementById('opForm'); f.elements.tip_offset.value = '0.8'; f.elements.passes.value = '0'; f.elements.feed.value = '200'; f.elements.plunge_feed.value = '60'; f.requestSubmit(); true");
  await waitFor(() => js("document.querySelectorAll('#tree .tree-op').length === 4"), "the new operation", 60000);
  await settle();
  const probsAfter = await js("document.querySelector('#tree .error')?.textContent || ''");
  const added = await gen();
  check("adding an operation: the form shows what's missing, then it plans", /missing field `edge`/.test(probs) && probsAfter === "" && added.lines[1] > 15146, `before: ${probs.slice(0, 40)}...; knurl file ${added.lines[1]} lines`);
  // delete everything, then the knurl wizard rebuilds the program
  for (const op of ["op4", "op3", "op2", "op1"]) {
    await treeAct(op, "delete");
    await settle();
  }
  const empty = await js("document.querySelectorAll('#tree .tree-op').length");
  await js("(document.getElementById('treeWizard') || (document.querySelector('#tree .tree-setup.active [data-act=setup]').click(), document.getElementById('treeWizard'))).click(); true");
  await waitFor(() => js("document.querySelectorAll('#tree .tree-op').length === 3"), "the wizard", 60000);
  const wiz = await gen();
  await js("document.querySelectorAll('#tabs button')[0].click(); true");
  const g1 = await js("window.__marcam.state.result.programs[0].gcode");
  check("knurl wizard rebuilds the knob in the tree; G-code is the cut G-code", empty === 0 && g1 === golden("knob_1_profile") && wiz.knurl === golden("knob_2_knurl"), `${empty} ops after deleting, ${wiz.lines.join(" / ")} lines`);
  // operation colour from the tree reaches the machining simulation
  await js("const c = document.querySelector('#tree .tree-op[data-op=\"op1\"] [data-act=\"color\"]'); c.value = '#00ff00'; c.dispatchEvent(new Event('change', { bubbles: true })); true");
  await settle();
  await js("document.getElementById('simOpen').click(); true");
  await waitFor(() => js("document.body.dataset.machining === 'open'"), "the machining simulation", 60000);
  await waitFor(() => js("document.querySelector('#opLegend input[data-tag=\"1\"]')?.value === '#00ff00'"), "the new colour in the playback", 30000).catch(() => {});
  const legend = await js("document.querySelector('#opLegend input[data-tag=\"1\"]').value");
  check("operation colours set in the tree colour the machining simulation", legend === "#00ff00", legend);
  await js("document.getElementById('simClose').click(); true");
  // face picking on the mesh job: finish only the taper
  await js("document.getElementById('example').value = 'knob_finish'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('knob.stl') && document.querySelectorAll('#tree .tree-op').length === 3"), "the mesh project", 60000);
  await gen();
  await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
  // (live picking: on while the operation is selected; no button)
  await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "pick mode", 60000);
  await js("window.__marcam.setView('iso', {})");
  const picked = await js(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(-7.5, 0, -9.0).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()`);
  const region = String(await js(`const pr = await window.__marcam.tree.pickAt(${picked[0]}, ${picked[1]}); pr ? pr.shape + ' ' + pr.x.map(x => x.toFixed(1)).join('..') : 'none'`));
  await shot("13_pick_faces");
  await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
  await settle();
  const fin = await gen();
  const along = await js("window.__marcam.state.result.programs.find(p => p.name === 'knob_4_finish_along').gcode");
  const xs = along.split("\n").filter((l) => l.startsWith("G1 X")).map((l) => parseFloat(l.split(" ")[1].slice(1)));
  const minX = Math.min(...xs);
  check("picking the taper face limits the finish to it", /^cone -15\.0\.\.-0\.8/.test(region) && minX >= -16.588, `picked ${region}; finish min X ${minX}`);
  await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  // (the knob loaded: a slow machine takes longer than a moment)
  await settle();
  await waitFor(() => js("(() => { try { return JSON.parse(document.getElementById('job').value).stock?.radius !== undefined; } catch { return false; } })()"), "the knob", 30000);

  // (the fast suite, MARCAM_E2E_FAST: these sections are left to the full one)
  if (!FAST) {
  // (6. the quick settings panel: removed, Mar)
  check("no quick settings panel", !(await js("!!document.getElementById('quick')")));

  // 7. errors are shown, not thrown
  await settle();
  await js("document.getElementById('error').textContent = ''; document.getElementById('job').value = '{ not json'; document.getElementById('generate').click(); true");
  const err = await waitFor(async () => { const t = await js("document.getElementById('error').textContent"); return /not valid/.test(t) ? t : null; }, "the invalid JSON error", 60000).catch(async () => js("document.getElementById('error').textContent"));
  check("invalid JSON shows an error", /not valid/.test(err), err.slice(0, 60));

  // 8. mesh example: rough + two finishes against knob.stl
  await js("document.getElementById('machine').value = 'makera-z1-4axis'; document.getElementById('example').value = 'knob_finish'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('knob.stl')"), "the example mesh", 60000);
  await clickGenerate();
  await waitDone("mesh job generation", 240000);
  const mtabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  check("mesh job: three programs, all verified OK", mtabs.length === 3 && mtabs.every((t) => t.endsWith("OK")), mtabs.join(", "));
  const gouges = [];
  for (let i = 0; i < 3; i++) {
    await js(`document.querySelectorAll('#tabs button')[${i}].click()`);
    const t = await js("document.getElementById('summary').innerText");
    gouges.push(parseFloat((t.match(/deepest ([0-9.]+) mm into the part/) || [])[1]));
  }
  check("mesh job: gouge check under 0.01 mm", gouges.every((g) => g < 0.01), gouges.join(", "));
  await js("document.querySelectorAll('#tabs button')[2].click()");
  await sleep(1500);
  await shot("04_mesh_finish");
  await js("document.getElementById('simulate').click()");
  await waitDone("mesh simulation", 240000);
  await sleep(1000);
  const msim = await js("document.getElementById('summary').innerText");
  check("mesh job simulated: no gouges", /deepest gouge 0\.0\d\d mm; 0 rapid/.test(msim), msim.split("\n").pop());
  await shot("05_mesh_simulated");
  const mv = await closedViews("mesh");
  check("mesh job: part, stock and simulated stock render closed (end faces filled)",
    mv.length === 6 && mv.every((v) => v.filled), mv.map((v) => `${v.layer}/${v.end}:${v.filled ? "filled" : "OPEN " + v.px}`).join(" "));

  // 8b. resizable panes: drag the bars, keyboard, reset, persistence, minimum sizes (the side
  // view, off by default, shown for these)
  const sideOff = await js("document.getElementById('side').classList.contains('hidden') && !document.getElementById('showSide').checked");
  check("the side view is off by default (a toggle under the 3D view)", sideOff);
  await js("document.getElementById('showSide').click(); true");
  await sleep(300);
  const rect = (id) => js(`(() => { const r = document.getElementById('${id}').getBoundingClientRect(); return [r.x, r.y, r.width, r.height].map(Math.round).join(','); })()`).then((t) => String(t).split(",").map(Number));
  // 8b. 3-axis plate on the Carvera vise: types, forms with points, generate, preview
  await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate's dome mesh", 60000);
  await settle();
  const plateMachine = await js("document.getElementById('machine').value");
  const plateTypes = (await addTypes()).join(",");
  check("vise setup: the setup's machine and only 3-axis operation types", plateMachine === "makera-carvera-4axis" && plateTypes === "face,pocket,drill,chamfer,thread_mill,bore,contour,raster3d,waterline,rough3d,engrave,vcarve,custom", `${plateMachine}; ${plateTypes}`);
  const plateOps = await js("document.querySelectorAll('#tree .tree-op').length");
  await js("document.getElementById('treeAddType').value = 'drill'; document.getElementById('treeAdd').click(); true");
  await waitFor(() => js(`${SEL}?.type === 'drill' && !!document.getElementById('opForm')`), "the drill form", 60000);
  const drillBlank = await js("[...document.getElementById('opForm').querySelectorAll('input, select')].filter(e => e.type !== 'checkbox' && e.name !== '__name' && e.value.trim() === '').map(e => e.name).join(',')");
  // (Mar: typed coordinates are an advanced option, hidden away) the drill's points not in
  // the form until Advanced is opened; the form says what to pick instead
  // (the section remembers being open, as More does: the chamfer's opened it; folded first)
  await js("(() => { const d = document.getElementById('opAdvanced'); if (d?.open) d.querySelector('summary').click(); return true; })()");
  await sleep(200);
  const folded = await js("(() => { const e = document.getElementById('opForm').elements.points; return { hidden: !e.checkVisibility(), inAdvanced: !!e.closest('#opAdvanced'), open: !!document.getElementById('opAdvanced')?.open, none: document.getElementById('pickNone')?.textContent || '' }; })()");
  check("Advanced: the drill's typed points folded away under Advanced, the form saying what to pick", folded.hidden && folded.inAdvanced && !folded.open && /Pick a hole on the part/.test(folded.none), JSON.stringify(folded));
  await js("document.querySelector('#opAdvanced > summary').click(); true");
  await js("const f = document.getElementById('opForm'); f.elements.points.value = '30 20; 33 20'; f.elements.points.dispatchEvent(new Event('input', { bubbles: true })); f.requestSubmit(); true");
  await waitFor(() => js(`document.querySelectorAll('#tree .tree-op').length === ${plateOps + 1}`), "the drill to be added", 60000);
  // (the project text written after the tree: waited for; the drill in whichever setup is
  // selected, not only the first)
  const drillOf = "(() => { const j = JSON.parse(document.getElementById('job').value); return JSON.stringify((j.setups || [j]).flatMap((s) => s.operations || []).find((o) => o.type === 'drill' && o.points?.length === 2)?.points); })()";
  await waitFor(() => js(`${drillOf} !== undefined`), "the drill in the project", 30000).catch(() => {});
  const drillPts = await js(drillOf);
  const drillsSeen = await js("(() => { try { const j = JSON.parse(document.getElementById('job').value); return JSON.stringify((j.setups || [j]).flatMap((s) => (s.operations || []).filter((o) => o.type === 'drill').map((o) => s.id + ':' + JSON.stringify(o.points)))); } catch (e) { return 'unreadable: ' + e.message; } })()");
  check("Advanced: the 3-axis form fully filled, and points typed there as 'x y; x y'", drillBlank === "" && drillPts === "[[30,20],[33,20]]", `blank: ${drillBlank || "none"}; points ${drillPts}; drills in the project ${drillsSeen}`);
  // a V-carve whose outline comes from an SVG file, through the form's file input
  const vOps = await js("document.querySelectorAll('#tree .tree-op').length");
  await js("document.getElementById('treeAddType').value = 'vcarve'; document.getElementById('treeAdd').click(); true");
  await waitFor(() => js(`${SEL}?.type === 'vcarve' && !!document.getElementById('opForm')`), "the V-carve form", 60000);
  await js("const inp = document.querySelector('#opForm input[data-svg]'); const dt = new DataTransfer(); dt.items.add(new File(['<svg xmlns=\"http://www.w3.org/2000/svg\"><polygon points=\"0,0 20,0 10,8\"/></svg>'], 'tri.svg', { type: 'image/svg+xml' })); inp.files = dt.files; document.getElementById('opForm').elements.__svg_height.value = '8'; inp.dispatchEvent(new Event('change', { bubbles: true })); true");
  await waitFor(() => js("document.getElementById('opForm').elements.paths.value.includes('30 16')"), "the SVG outline in the form", 60000);
  const svgField = await js("document.getElementById('opForm').elements.paths.value");
  await js("document.getElementById('opForm').requestSubmit(); true");
  await waitFor(() => js(`document.querySelectorAll('#tree .tree-op').length === ${vOps + 1}`), "the V-carve to be added", 60000);
  check("SVG import replaces the V-carve outline (at the old outline's corner, 8 mm tall)", /^20 24; 40 24; 30 16; 20 24$/.test(svgField), svgField);
  await clickGenerate();
  await waitDone("plate generation", 240000);
  const ptabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  const plimits = await js("document.getElementById('summary').innerText");
  const pkinds = await js("[...new Set(window.__marcam.state.result.programs.flatMap(p => { const a = p.toolpath.points; const k = []; for (let i = 3; i < a.length; i += 4) k.push(a[i]); return k; }))].sort().join(',')");
  check("plate: two programs verified OK, machine limits OK, XYZ toolpaths", ptabs.length === 2 && ptabs.every((t) => t.endsWith("OK")) && /Machine limits: OK/.test(plimits) && pkinds === "2,3", `${ptabs.join(", ")}; kinds ${pkinds}`);
  await sleep(1500);
  await shot("07_plate");
  // batch simulation of the block: dome finished with no gouge, no rapids through stock
  await js("document.getElementById('simulate').click()");
  await waitDone("plate simulation", 240000);
  await js("document.querySelectorAll('#tabs button')[1].click()");
  await sleep(1000);
  const psim = await js("document.getElementById('summary').innerText.split('\\n').pop()");
  const pleft = parseFloat((psim.match(/left max ([0-9.]+)/) || [])[1]);
  check("plate simulated: dome finished, no gouge, no rapids through stock", /deepest gouge 0\.00\d mm; 0 rapid/.test(psim) && pleft < 0.5, psim);
  await shot("08_plate_simulated");
  // machining playback: to the end, every operation's colour on the block
  await js("document.getElementById('simOpen').click()");
  await waitFor(() => js("window.__marcam.state.machining.open && !!window.__marcam.state.machining.stockMesh"), "the plate machining view", 60000);
  const psegs = await js("window.__marcam.state.machining.session.segments");
  await js(`document.getElementById('simScrub').value = '${psegs}'; document.getElementById('simScrub').dispatchEvent(new Event('input')); true`);
  await waitFor(() => js(`window.__marcam.state.machining.pos === ${psegs} && !window.__marcam.state.machining.busy`), "the plate playback to finish", 120000);
  const ptags = await js("[...new Set(window.__marcam.state.machining.op)].sort((a, b) => a - b).join(',')");
  const pz = await js("(() => { const m = window.__marcam.state.machining; const nx = m.session.grid.nx; const at = (x, y) => m.r[Math.floor(y / 0.25) * nx + Math.floor(x / 0.25)]; return [at(15, 20), at(40, 6), at(46, 20)].map(v => v.toFixed(2)).join(','); })()");
  const vtx = await js("(() => { const m = window.__marcam.state.machining; const nx = m.session.grid.nx; const k = Math.floor(20 / 0.25) * nx + Math.floor(15 / 0.25); return m.stockMesh.geometry.attributes.position.array[3 * k + 2].toFixed(2); })()");
  check("plate playback: block cut by all eight operations (six + the added drill and V-carve), mesh follows the engine", ptags === "1,2,3,4,5,6,7,8" && pz.startsWith("-3.50,-4.50,") && Math.abs(parseFloat(pz.split(",")[2]) + 0.5) < 0.03 && vtx === "-3.50", `tags ${ptags}; pocket, hole, dome top ${pz}; vertex ${vtx}`);
  await sleep(800);
  await shot("09_plate_machining");
  await js("document.getElementById('simClose').click(); true");
  await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  await settle();

  // 8f. machining time (Mar: "I don't see the machine time estimate anymore"): after
  // Generate, the status line, the summary, every file and every enabled operation show a
  // non-zero time, on screen, for a rotary job, the vise plate (arcs), the indexed knob and an
  // imported STL; out of date they stay; hidden toolpaths do not hide them
  for (const ex of ["knob", "plate", "knob_indexed"]) {
    await js(`document.getElementById('example').value = '${ex}'; document.getElementById('loadExample').click(); true`);
    await settle();
    await clickGenerate();
    await waitDone(`${ex} generation for times`, 240000);
    await settle();
    const t = await js(SHOWN_TIMES);
    const bad = timesOk(t);
    check(`time estimates shown after Generate (${ex}): status, summary, each file and each operation, non-zero`, bad.length === 0, bad.join("; ") || `${t.files.length} files, ${t.ops.length} operations, total ${Math.round(t.total)} s`);
    if (ex === "plate") {
      // hidden toolpaths: view only, the times stay
      await js("document.getElementById('eyeNone').click(); true");
      await sleep(400);
      const h = await js(SHOWN_TIMES);
      check("time estimates stay with every toolpath hidden", timesOk(h).length === 0, timesOk(h).join("; "));
      await js("document.getElementById('eyeAll').click(); true");
      // an edit: out of date, the times stay but dimmed; Generate again: fresh
      const op = t.ops[0].id;
      await js(`(() => { const j = JSON.parse(document.getElementById('job').value); const o = j.setups[0].operations.find((x) => x.id === '${op}'); o.feed = (o.feed || 500) * 0.5; document.getElementById('job').value = JSON.stringify(j, null, 2); document.getElementById('job').dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
      await waitFor(() => js("document.body.classList.contains('stale')"), "the plate out of date", 60000);
      await settle();
      const s1 = await js(SHOWN_TIMES);
      check("out of date: the old times stay", s1.stale && s1.ops.filter((o) => o.on).every((o) => o.s > 0), JSON.stringify(s1.ops));
      await clickGenerate();
      await waitDone("the plate again", 240000);
      await settle();
      const s2 = await js(SHOWN_TIMES);
      const o1 = s2.ops.find((o) => o.id === op), o0 = t.ops.find((o) => o.id === op);
      check("generated again: fresh times, the slower operation longer", !s2.stale && timesOk(s2).length === 0 && o1.s > o0.s, `${o0.s} -> ${o1.s} s; ${timesOk(s2).join("; ")}`);
    }
  }
  {
    // (Mar) a new project with an imported part starts with no operations: none picked for
    // the person, nothing planned, no error; they add them
    const knobStl = readFileSync(join(root, "examples", "knob", "knob.stl")).toString("base64");
    await js(`await window.__marcam.project.openText(${JSON.stringify(JSON.stringify({ format: "marcam-project", version: 1, project: { schema_version: "0.2", name: "bare", models: [], tools: [], setups: [] } }))}, "bare.marcam.json").then(() => true)`);
    await settle();
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${knobStl}"), (c) => c.charCodeAt(0))], "knob.stl")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await waitFor(() => js("(() => { try { const d = JSON.parse(document.getElementById('job').value); return d.setups?.length === 1 && d.models?.length === 1 && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "the part imported with no operations", 120000);
    await setKind("vise");
    const bare = await js("(() => { const d = JSON.parse(document.getElementById('job').value); return { ops: d.setups[0].operations.length, error: document.getElementById('error').textContent, result: !!window.__marcam.state.result, status: document.getElementById('status').textContent }; })()");
    check("a new project with an imported part has no operations until the person adds them (nothing planned, no error)", bare.ops === 0 && !bare.error && !bare.result, JSON.stringify(bare));
    // (Mar) a setup added: none either (a new part's, or the cut result of the one before)
    for (const prev of [false, true]) {
      // (added at once, carrying the cut result; its own stock chosen in its Stock)
      await js("document.getElementById('treeAddSetup').click(); true");
      await waitFor(() => js(`(JSON.parse(document.getElementById('job').value).setups || []).length === ${prev ? 3 : 2}`), "the setup added", 60000);
      await settle();
      if (!prev) await ownStock("block");
    }
    const added = await js("JSON.parse(document.getElementById('job').value).setups.map((s) => s.operations.length)");
    check("a setup added (its own stock, or the cut result of the one before) has no operations until the person adds them", added.join() === "0,0,0", JSON.stringify(added));
    // the sections below were written with starter operations: asked for, as a caller may
    await js("localStorage.setItem('marcam.starters', '1'); window.__marcam.project.settings.starters = true; true");
  }
  {
    // an imported STL on a vise (its starter operations), generated by the import
    const knobStl = readFileSync(join(root, "examples", "knob", "knob.stl")).toString("base64");
    await js(`await window.__marcam.project.openText(${JSON.stringify(JSON.stringify({ format: "marcam-project", version: 1, project: { schema_version: "0.2", name: "times", models: [], tools: [], setups: [] } }))}, "times.marcam.json").then(() => true)`);
    await settle();
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${knobStl}"), (c) => c.charCodeAt(0))], "knob.stl")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await waitFor(() => js("(() => { try { return JSON.parse(document.getElementById('job').value).setups?.length === 1 && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "the knob STL imported", 120000);
    await settle();
    await setKind("vise");
    // (planned: the status says what it took)
    await clickGenerate();
    await waitDone("the imported STL", 240000);
    await settle();
    const t = await js(SHOWN_TIMES);
    check("time estimates shown for an imported STL (vise): status, summary, each file and operation", timesOk(t).length === 0, timesOk(t).join("; ") || `total ${Math.round(t.total)} s`);
  }
  await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  await settle();

  // 8g. simulation resolution: the setting changes the grid (shown before running), the
  // machining view at that grid stays watertight, and Ultra shows a decimated display
  // top-level await: the evaluation does not wait for a promise an async IIFE returns
  const closedNow = async () => (await js("const { checkClosed: cc } = await import('./meshes.js'); const gg = window.__marcam.state.machining.stockMesh.geometry; String(cc({ positions: gg.attributes.position.array, indices: gg.index.array }).closed)")) === "true";
  await js("document.getElementById('simRes').value = 'fine'; document.getElementById('simRes').dispatchEvent(new Event('change')); true");
  await waitFor(() => js("(window.__marcam.state.simGrid?.na === 720) && document.getElementById('simResInfo').textContent.includes('cells')"), "the Fine grid info", 60000);
  const fineInfo = await js("document.getElementById('simResInfo').textContent");
  const fineCells = await js("window.__marcam.state.simGrid.cells");
  await js("document.getElementById('simOpen').click(); true");
  await waitFor(() => js("window.__marcam.state.machining.open && window.__marcam.state.machining.session?.grid.na === 720 && !!window.__marcam.state.machining.stockMesh"), "the Fine machining view", 60000);
  await js("document.getElementById('simScrub').value = '3000'; document.getElementById('simScrub').dispatchEvent(new Event('input')); true");
  await waitFor(() => js("window.__marcam.state.machining.pos === 3000 && !window.__marcam.state.machining.busy"), "the Fine playback", 120000);
  const fineClosed = await closedNow();
  const fineStride = await js("window.__marcam.state.machining.stride");
  await js("document.getElementById('simClose').click(); true");
  check("Fine grid: cell count shown, machining view at 720 cells round, watertight", fineInfo.includes(Number(fineCells).toLocaleString("en")) && fineClosed === true && fineStride === 1, `${fineInfo}; closed ${fineClosed}`);
  await js("document.getElementById('simRes').value = 'ultra'; document.getElementById('simRes').dispatchEvent(new Event('change')); true");
  await waitFor(() => js("window.__marcam.state.simGrid?.na === 1440"), "the Ultra grid info", 60000);
  await js("document.getElementById('simOpen').click(); true");
  await waitFor(() => js("window.__marcam.state.machining.open && window.__marcam.state.machining.session?.grid.na === 1440 && !!window.__marcam.state.machining.stockMesh"), "the Ultra machining view", 60000);
  await js("document.getElementById('simScrub').value = '2000'; document.getElementById('simScrub').dispatchEvent(new Event('input')); true");
  await waitFor(() => js("window.__marcam.state.machining.pos === 2000 && !window.__marcam.state.machining.busy"), "the Ultra playback", 120000);
  const ultra = await js("JSON.stringify({ s: window.__marcam.state.machining.stride, cells: window.__marcam.state.simGrid.cells, disp: window.__marcam.state.machining.dr.length })");
  const ultraClosed = await closedNow();
  await js("document.getElementById('simClose').click(); document.getElementById('simRes').value = 'normal'; document.getElementById('simRes').dispatchEvent(new Event('change')); true");
  const u = JSON.parse(ultra);
  check("Ultra grid: display decimated to at most 1M cells, still watertight", u.cells > 1e6 && u.s >= 2 && u.disp <= 1e6 && ultraClosed === true, `${ultra}; closed ${ultraClosed}`);
  const remembered = await js("JSON.parse(localStorage.getItem('marcam.simRes')).v");
  check("the resolution setting is remembered", remembered === "normal", remembered);

  // 8g2. face-bounded raster: pick the dome's cap for its raster; the finish stays on it
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl') && !!document.querySelector('#tree .tree-op[data-op=\"op5\"]')"), "the plate again", 60000);
    await js("document.querySelector('#tree .tree-op[data-op=\"op5\"] [data-act=\"select\"]').click(); true");
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "the raster's face picker", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "pick mode on the plate", 60000);
    await js("window.__marcam.setView('iso', {})");
    const at = await js(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(46, 20, -0.6).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()`);
    const capPick = String(await js(`const pr = await window.__marcam.tree.pickAt(${at[0]}, ${at[1]}); pr ? pr.id + ' ' + pr.shape : 'none'`));
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await sleep(400);
    const faces = await js("JSON.stringify(JSON.parse(document.getElementById('job').value).setups[0].operations.find(o => o.id === 'op5').geometry || [])");
    await clickGenerate();
    await waitDone("generation with the picked cap", 240000);
    const far = await js("(() => { const p = window.__marcam.state.result.programs.find(q => q.name === 'plate_2_dome').toolpath.points; let m = 0; for (let i = 0; i < p.length; i += 4) if (p[i + 3] === 3) m = Math.max(m, Math.hypot(p[i] - 46, p[i + 1] - 20)); return m; })()");
    const okTabs = await js("[...document.querySelectorAll('#tabs button')].every(b => b.textContent.endsWith('OK'))");
    check("picking the dome's cap keeps its raster on the cap (cuts within the cap + the tool radius)", capPick.startsWith("f049d19c2") && /f049d19c2/.test(faces) && far <= 6 + 1.5875 + 0.05 && okTabs, `picked ${capPick}; geometry ${faces}; farthest cut ${far.toFixed(3)} mm from the dome's axis`);
    await shot("13b_pick_dome_cap");
    // back to the whole model (the editor may have closed when generating)
    if (!(await js("!!document.getElementById('clearFaces')"))) {
      await js("document.querySelector('#tree .tree-op[data-op=\"op5\"] [data-act=\"select\"]').click(); true");
    }
    await waitFor(() => js("!!document.getElementById('clearFaces')"), "the clear button", 60000);
    await js("document.getElementById('clearFaces').click(); true");
    await waitFor(() => js("!JSON.parse(document.getElementById('job').value).setups[0].operations.find(o => o.id === 'op5').geometry"), "the selection cleared", 60000);
  }
  // 8h. WebGPU stock simulation against the CPU (WASM): Simulate and playback, rotary and
  // 3-axis; indexed jobs and "GPU off" fall back and say so
  const webgpu = String(await js("const adp = navigator.gpu ? await navigator.gpu.requestAdapter() : null; String(!!adp)")) === "true";
  const simulateWith = async (gpuOn) => {
    await js(`document.getElementById('simGpu').checked = ${gpuOn}; window.__marcam.state.sim = null; document.getElementById('simulate').click(); true`);
    await waitFor(() => js("!!window.__marcam.state.sim && document.body.dataset.state !== 'busy'"), "the simulation", 240000);
    await settle();
    return js("window.__marcam.state.simBackend");
  };
  const keepStocks = (name) => js(`window.__${name} = window.__marcam.state.sim.stocks.map(s => ({ h: Array.from(s.r || s.z), g: s.max_gouge, l: s.max_left, f: s.feature_cells, rh: s.rapid_hits })); true`);
  const compareStocks = () => js("let md = 0; window.__gpuS.forEach((s, p) => s.h.forEach((v, k) => { md = Math.max(md, Math.abs(v - window.__cpuS[p].h[k])); })); JSON.stringify({ md, gpu: window.__gpuS.map(s => [s.l.toFixed(3), s.rh, s.f]), cpu: window.__cpuS.map(s => [s.l.toFixed(3), s.rh, s.f]) })");
  for (const [ex, label] of [["knob", "rotary knob"], ["plate", "3-axis plate"]]) {
    await js(`document.getElementById('example').value = '${ex}'; document.getElementById('loadExample').click(); true`);
    await settle();
    await sleep(1500);
    await clickGenerate();
    await waitDone(`${ex} generation for the GPU check`, 240000);
    const bg = await simulateWith(true);
    await keepStocks("gpuS");
    const bc = await simulateWith(false);
    await keepStocks("cpuS");
    const cmp = JSON.parse(await compareStocks());
    if (webgpu) {
      check(`WebGPU Simulate matches the CPU within 2 um (${label})`, String(bg).startsWith("GPU") && bc === "CPU (GPU off)" && cmp.md <= 0.002 && JSON.stringify(cmp.gpu) === JSON.stringify(cmp.cpu), `${bg} vs ${bc}; max difference ${cmp.md.toFixed(4)} mm; ${JSON.stringify(cmp.gpu)}`);
    } else {
      check(`no WebGPU here: Simulate falls back to the CPU and says why (${label})`, String(bg).startsWith("CPU (") && cmp.md === 0, `${bg}`);
    }
  }
  // playback on the GPU ends where the batch simulation does (the plate, Normal grid)
  await js("document.getElementById('simGpu').checked = true; document.getElementById('simOpen').click(); true");
  await waitFor(() => js("window.__marcam.state.machining.open && !!window.__marcam.state.machining.stockMesh"), "the plate machining view", 60000);
  const pn = await js("window.__marcam.state.machining.session.segments");
  await js(`document.getElementById('simScrub').value = '${pn}'; document.getElementById('simScrub').dispatchEvent(new Event('input')); true`);
  await waitFor(() => js(`window.__marcam.state.machining.pos === ${pn} && !window.__marcam.state.machining.busy`), "the playback to the end", 120000);
  const pb = JSON.parse(await js("const mm = window.__marcam.state.machining; let md2 = 0; const last = window.__cpuS[window.__cpuS.length - 1].h; mm.r.forEach((v, k) => { md2 = Math.max(md2, Math.abs(v - last[k])); }); JSON.stringify({ md: md2, backend: mm.backend, label: document.getElementById('simLabel').textContent })"));
  await js("document.getElementById('simClose').click(); true");
  check(webgpu ? "WebGPU playback ends at the CPU's stock within 2 um (label says GPU)" : "no WebGPU: playback runs on the CPU and says so", webgpu ? pb.backend.startsWith("GPU") && pb.label.endsWith("GPU") && pb.md <= 0.002 : pb.backend.startsWith("CPU") && pb.md <= 1e-6, `${pb.backend}; max difference ${pb.md.toFixed(4)} mm; ${pb.label}`);
  // an indexed job: the engine keeps it on the CPU, with the reason shown
  await js("document.getElementById('example').value = 'knob_indexed'; document.getElementById('loadExample').click(); true");
  await settle();
  await sleep(1500);
  await clickGenerate();
  await waitDone("indexed knob generation for the GPU check", 240000);
  const bi = await simulateWith(true);
  check("indexed 3+1 jobs fall back to the CPU and say why", webgpu ? /^CPU \(indexed 3\+1/.test(bi) : String(bi).startsWith("CPU ("), bi);
  await js("document.getElementById('simGpu').checked = true; document.getElementById('simGpu').dispatchEvent(new Event('change')); true");

  // 8d. relief plaque from a grayscale image: roughing + 45 deg parallel finish, simulated
  await js("document.getElementById('example').value = 'plaque'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('plaque.png')"), "the plaque image", 60000);
  await clickGenerate();
  await waitDone("plaque generation", 240000);
  const qtabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  check("plaque: roughing, finishing and engraving programs verified OK", qtabs.length === 3 && qtabs.every((t) => t.endsWith("OK")), qtabs.join(", "));
  await js("document.getElementById('simulate').click()");
  await waitDone("plaque simulation", 240000);
  await js("document.querySelectorAll('#tabs button')[1].click()");
  await sleep(1500);
  const qsim = await js("document.getElementById('summary').innerText.split('\\n').pop()");
  const qmean = parseFloat((qsim.match(/mean ([0-9.]+)/) || [])[1]);
  check("plaque simulated: finished to the relief, no gouge, no rapids through stock", /deepest gouge 0\.00\d mm; 0 rapid/.test(qsim) && qmean < 0.05, qsim);
  await shot("11_plaque_simulated");

  // 8e. wrapped engraving on the Z1: grooves, text round the bar and a V-carved band
  await js("document.getElementById('example').value = 'ring'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.querySelectorAll('#tree .tree-op').length === 3"), "the ring in the tree", 60000);
  await clickGenerate();
  await waitDone("ring generation", 240000);
  const rtabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  await js("document.getElementById('simulate').click()");
  await waitDone("ring simulation", 240000);
  const rcut = await js("(() => { const s = window.__marcam.state.sim; const g = s.grid; const r = s.stocks[0].r; const at = (x, deg) => r[Math.floor((x - g.x0) / g.dx) * g.na + Math.floor(deg / 360 * g.na) % g.na]; return [at(-31, 90), at(-40, 200), at(-25, 90)].map(v => v.toFixed(2)).join(','); })()");
  check("ring: wrapped engraving verified and simulated (diamond floor, groove all round, bar elsewhere)", rtabs.length === 1 && rtabs[0].endsWith("OK") && /^10\.6\d,11\.7\d,12\.00$/.test(rcut), `${rtabs.join(", ")}; ${rcut}`);
  const rsum = await js("document.getElementById('summary').innerText.split('\\n').pop()");
  check("ring: intended engraving is not reported as a gouge", /deepest gouge 0\.0\d\d mm/.test(rsum) && /cut below the part on purpose/.test(rsum), rsum);
  await sleep(1000);
  await shot("12_ring_engraved");

  // 8c. indexed 3+1 on the knob: D-flat at A0, cross hole at A90, simulated on the bar
  await js("document.getElementById('machine').value = 'makera-z1-4axis'; document.getElementById('example').value = 'knob_indexed'; document.getElementById('loadExample').click(); true");
  await waitFor(() => js("document.querySelectorAll('#tree .tree-op').length === 5"), "the indexed knob in the tree", 60000);
  await clickGenerate();
  await waitDone("indexed knob generation", 240000);
  const itabs = await js("[...document.querySelectorAll('#tabs button')].map(b => b.textContent)");
  const igold = await js("window.__marcam.state.result.programs.slice(0, 2).map(p => p.gcode)");
  check("indexed knob: three programs OK, turning and knurl still the goldens", itabs.length === 3 && itabs.every((t) => t.endsWith("OK")) && igold[0] === golden("knob_1_profile") && igold[1] === golden("knob_2_knurl"), itabs.join(", "));
  await js("document.getElementById('simulate').click()");
  await waitDone("indexed knob simulation", 240000);
  const iflat = await js("(() => { const s = window.__marcam.state.sim; const g = s.grid; const r = s.stocks[2].r; const at = (x, deg) => r[Math.floor((x - g.x0) / g.dx) * g.na + Math.floor(deg / 360 * g.na) % g.na]; return [at(-20.05, 90), at(-20.05, 270), at(-8.05, 0.5)].map(v => v.toFixed(2)).join(','); })()");
  check("indexed knob simulated: flat at R2.5 on top, round below, hole to R6", iflat === "2.50,3.00,6.00", iflat);
  await js("document.querySelectorAll('#tabs button')[2].click()");
  await sleep(1500);
  await shot("10_knob_indexed");
  await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
  await settle();

  async function drag(id, dx, dy) {
    const [x, y, w, h] = await rect(id);
    const cx = x + w / 2, cy = y + h / 2;
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: cx, y: cy, button: "left", buttons: 1, clickCount: 1 });
    for (let k = 1; k <= 5; k++) await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: cx + (dx * k) / 5, y: cy + (dy * k) / 5, button: "left", buttons: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: cx + dx, y: cy + dy, button: "left", buttons: 0, clickCount: 1 });
    await sleep(400); // ResizeObservers
  }
  const L0 = (await rect("left"))[2], R0 = (await rect("right"))[2];
  await drag("splitLeft", 120, 0);
  const L1 = (await rect("left"))[2], R1 = (await rect("right"))[2];
  // (the right pane takes what is left: give or take the page's scrollbar, which comes or goes
  // as the left pane's text wraps differently with the fonts a machine has)
  check("dragging the left bar resizes both panes", Math.abs(L1 - L0 - 120) <= 3 && Math.abs(R0 - R1 - 120) <= 20, `left ${L0}->${L1}, right ${R0}->${R1}`);
  // (Mar) no G-code pane: the viewer takes the results' whole width; Download .nc stays
  const c1 = await js("window.__marcam.state.three.renderer.domElement.clientWidth");
  const v3 = (await rect("view3d"))[2], views = (await rect("views"))[2];
  const dlShown = await js("(() => { const b = document.getElementById('download'); return !!b && b.checkVisibility(); })()");
  check("no G-code pane: the 3D view has the results' width, its canvas follows, Download .nc is on show",
    Math.abs(c1 - v3) <= 2 && v3 >= views - 4 && !(await js("document.getElementById('codeWrap').checkVisibility()")) && dlShown, `canvas ${c1}, view ${v3}, views ${views}, download ${dlShown}`);
  const s0 = (await rect("side"))[3];
  await drag("split3d", 0, -100);
  const s1 = (await rect("side"))[3];
  const sideBuf = await js("document.getElementById('side').height / (window.devicePixelRatio || 1)");
  check("dragging the 3D/side bar grows the side view, and its canvas follows", Math.abs(s1 - s0 - 100) <= 3 && Math.abs(sideBuf - s1) <= 2, `side ${s0}->${s1}, canvas ${sideBuf}`);
  await js("document.querySelector('#tree [data-act=library]')?.click(); document.getElementById('libraryPanel').open = true; true");
  await js("document.getElementById('splitTools').scrollIntoView({ block: 'center' }); true");
  const tw0 = (await rect("toolTableWrap"))[3];
  await drag("splitTools", 0, 80);
  const tw1 = (await rect("toolTableWrap"))[3];
  check("dragging the tool list bar resizes the tool list", Math.abs(tw1 - tw0 - 80) <= 3, `${tw0}->${tw1}`);
  await js("document.getElementById('splitLeft').focus(); true");
  await S("Input.dispatchKeyEvent", { type: "keyDown", key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 });
  await S("Input.dispatchKeyEvent", { type: "keyUp", key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 });
  await sleep(100);
  const L2 = (await rect("left"))[2];
  check("arrow keys move a focused bar", L2 === L1 + 10, `${L1}->${L2}`);
  await S("Page.reload");
  await waitFor(async () => (await state()) === "ready", "reload", 30000);
  await sleep(500);
  const sizes = [(await rect("left"))[2], (await rect("side"))[3]];
  check("pane sizes survive a reload", sizes[0] === L2 && sizes[1] === s1, `left ${sizes[0]}, side ${sizes[1]}`);
  await drag("splitLeft", -2000, 0);
  await drag("split3d", 0, 2000);
  const mins = [(await rect("left"))[2], (await rect("side"))[3], (await rect("view3d"))[3]];
  check("minimum sizes hold", mins[0] === 300 && mins[1] === 80 && mins[2] >= 250, `left ${mins[0]}, side ${mins[1]}, 3D ${mins[2]}`);
  await shot("12_resized_panes");
  for (const id of ["splitLeft", "split3d", "splitTools"]) {
    const [x, y, w, h] = await rect(id);
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: x + w / 2, y: y + h / 2, button: "left", buttons: 1, clickCount: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: x + w / 2, y: y + h / 2, button: "left", buttons: 0, clickCount: 1 });
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: x + w / 2, y: y + h / 2, button: "left", buttons: 1, clickCount: 2 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: x + w / 2, y: y + h / 2, button: "left", buttons: 0, clickCount: 2 });
  }
  await sleep(300);
  const reset = [(await rect("left"))[2], (await rect("side"))[3]];
  check("double-click resets bars to the default layout", reset[0] === L0 && reset[1] === s0, `left ${reset[0]} (default ${L0}), side ${reset[1]} (default ${s0})`);

  // 8j. project workflow and setups: New (the setup dialog) -> Import STL (binary, ASCII by
  // drag-and-drop, inch) -> every view refreshed, stock sized, defaults filled -> Generate ->
  // Save -> New -> Open the saved file -> the same G-code; each setup type from New; editing a
  // setup's type converts it (and Undo); adding a setup; unsaved-changes warnings; bad files
  {
    await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
    const knobStl = readFileSync(join(root, "examples", "knob", "knob.stl"));
    // the same triangles as ASCII, and scaled to inches (binary)
    const n = knobStl.readUInt32LE(80);
    let ascii = "solid knob\n";
    const inch = Buffer.alloc(84 + 50 * n);
    inch.writeUInt32LE(n, 80);
    for (let t = 0; t < n; t++) {
      const o = 84 + 50 * t;
      ascii += " facet normal 0 0 0\n  outer loop\n";
      for (let k = 0; k < 3; k++) {
        const v = [0, 1, 2].map((c) => knobStl.readFloatLE(o + 12 + 12 * k + 4 * c));
        ascii += `   vertex ${v.join(" ")}\n`;
        v.forEach((x, c) => inch.writeFloatLE(x / 25.4, o + 12 + 12 * k + 4 * c));
      }
      ascii += "  endloop\n endfacet\n";
    }
    ascii += "endsolid knob\n";
    const fileJs = (name, buf) => `new File([Uint8Array.from(atob("${Buffer.from(buf).toString("base64")}"), (c) => c.charCodeAt(0))], ${JSON.stringify(name)})`;
    const setFile = (input, name, buf) => js(`(() => { const dt = new DataTransfer(); dt.items.add(${fileJs(name, buf)}); const i = document.getElementById('${input}'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    const drop = (name, buf) => js(`(() => { const dt = new DataTransfer(); dt.items.add(${fileJs(name, buf)}); document.dispatchEvent(new DragEvent('dragover', { dataTransfer: dt, bubbles: true, cancelable: true })); document.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true })); return true; })()`);
    const cdpDialogs = () => cdp.dialogs.map((d) => `${d.type}: ${d.message}`);
    const allGcode = () => js("window.__marcam.state.result.programs.map((p) => p.name + '\\n' + p.gcode).join('\\n')");
    const viewsOk = () => js(`(() => { const v = window.__marcam.state.three; const s = document.getElementById('side'); const px = s.getContext('2d').getImageData(0, 0, s.width, s.height).data; let ink = 0; for (let i = 3; i < px.length; i += 4) if (px[i]) ink++;
      return { part: v.groups.part.children.length, stock: v.groups.stock.children.length, path: v.groups.path.children.length, side: ink, tabs: [...document.querySelectorAll('#tabs button')].map((b) => b.textContent), ops: document.querySelectorAll('#tree .tree-op').length, stale: document.body.classList.contains('stale') }; })()`);
    // (Mar: no setup dialogs) New: an empty project at once; an STL: placed at once, planned
    const newProject = async () => {
      await js("document.getElementById('projectNew').click(); true");
      await emptyReady();
    };
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    const emptyReady = () => waitFor(() => js("!!document.getElementById('emptyState') && document.body.dataset.state === 'ready'"), "the empty project", 60000).catch(async (e) => {
      throw new Error(`${e.message}: ${await js("JSON.stringify({ empty: !!document.getElementById('emptyState'), state: document.body.dataset.state, status: document.getElementById('status').textContent })")}`);
    });
    // New, from a project with unsaved edits: asked first; declining keeps it
    await js("document.getElementById('job').dispatchEvent(new Event('input')); true");
    const dirty0 = await js("!document.getElementById('projectDirty').classList.contains('hidden')");
    cdp.dialogAnswer = false;
    await js("document.getElementById('projectNew').click(); true");
    await sleep(300);
    const kept = await js("document.querySelectorAll('#tree .tree-op').length");
    const noDialog = !(await js("(document.getElementById('setupDialog').open && !document.getElementById('setupDialog').classList.contains('embedded'))"));
    cdp.dialogAnswer = true;
    check("unsaved changes are marked, and New asks before discarding them (declined: nothing lost)", dirty0 && /unsaved changes/.test(cdpDialogs().pop() || "") && kept > 0 && noDialog, `${cdpDialogs().slice(-1)}; ${kept} operations kept`);

    // New: no dialog, an empty project at once; its setups added in the tree
    await newProject();
    const choices = await js("[...(document.getElementById('treeAddKind')?.options || [])].map((o) => o.textContent)");
    const empty = await js("({ name: document.getElementById('projectName').textContent, ops: document.querySelectorAll('#tree .tree-op').length, tabs: document.querySelectorAll('#tabs button').length, gcode: (window.__marcam.state.result?.programs || []).length, cta: document.getElementById('emptyState').textContent })");
    // nothing to plan: Generate is disabled and says why
    const nothing = await js("document.getElementById('generate').disabled ? document.getElementById('generate').title : 'enabled'");
    check("New: no dialog; an empty project at once saying \"No setups yet\" (its + Add setup: 3-axis or 4th axis)",
      !(await js("(document.getElementById('setupDialog').open && !document.getElementById('setupDialog').classList.contains('embedded'))")) && choices.join() === "3-axis,4th axis" && empty.name === "untitled" && empty.ops === 0 && empty.tabs === 0 && /No setups yet/i.test(empty.cta) && /Import an STL first|setup/i.test(nothing),
      `${choices.join(" | ")}; ${JSON.stringify(empty)}`);
    await shot("14a_new_project");
    // an STL into it: placed at once (4th axis), made a 3-axis vise in its Setup
    await setFile("importStlFile", "dome.stl", readFileSync(join(root, "examples", "plate", "dome.stl")));
    await waitDone("generation after importing into the new project", 240000);
    await setKind("vise");
    await clickGenerate();
    await waitDone("generation of the vise setup", 240000);
    const dv = await doc();
    check("Import STL into it, then its Setup made a 3-axis vise: block stock, 3-axis operations generated",
      dv.setups[0].kind === "vise" && dv.setups[0].stock.shape === "box" && dv.setups[0].operations.map((o) => o.type).join() === "rough3d,raster3d" && (await viewsOk()).tabs.every((t) => t.endsWith("OK")),
      JSON.stringify(dv.setups[0].stock));

    // each setup type: New, the STL, its Setup's type
    const byKind = {};
    for (const [kind, ops] of [["rotary", "rotary_rough,rotary_finish"], ["indexed", "rough3d,rough3d,raster3d,raster3d"], ["vise", "rough3d,raster3d"]]) {
      await newProject();
      await setFile("importStlFile", "knob.stl", knobStl);
      await waitDone("generation of the imported knob", 240000);
      if (kind !== "rotary") {
        await setKind(kind);
        await clickGenerate();
        await waitDone(`generation of a new ${kind} project`, 240000);
      }
      await shot(`15b_new_${kind}`);
      const info = await js("document.getElementById('meshInfo').textContent");
      const d = await doc();
      const v = await viewsOk();
      const offered = await addTypes();
      byKind[kind] = { d, offered };
      const fitsOffered = kind === "rotary" ? !offered.includes("pocket") && offered.includes("rotary_finish")
        : kind === "indexed" ? offered.includes("pocket") && !offered.includes("rotary_finish") && !offered.includes("helical_knurl")
        : offered.includes("pocket") && !offered.includes("rotary_finish");
      check(`New ${kind} project with the STL: placed, ${kind === "vise" ? "block" : "bar"} stock, ${ops.split(",").length} starter operations, generated, only fitting types offered`,
        d.setups[0].kind === kind && d.setups[0].operations.map((o) => o.type).join() === ops && (kind !== "indexed" || d.setups[0].operations.map((o) => o.index_a).join() === "0,180,0,180") && v.tabs.length >= 2 && v.tabs.every((t) => t.endsWith("OK")) && v.part > 0 && v.path > 0 && fitsOffered,
        `${info}; ${d.setups[0].operations.map((o) => o.type + (o.index_a !== undefined ? "@" + o.index_a : "")).join(",")}; tabs ${v.tabs}; offered ${offered.length}`);
      await shot(`15c_${kind}_generated`);
    }

    // a rotary project: import, save, reopen (the earlier round trip), then change its type
    await newProject();
    await setFile("importStlFile", "knob.stl", knobStl);
    await waitDone("generation after the STL import", 240000);
    const info1 = await js("document.getElementById('meshInfo').textContent");
    const doc1 = await doc();
    const v1 = await viewsOk();
    check("Import STL (binary): part size shown, bar stock sized round it, the part on the axis with its front at X0",
      /binary STL, 10080 triangles/.test(info1) && /21\.70 x 20\.00 x 20\.00 mm/.test(info1) && JSON.stringify(doc1.setups[0].stock) === '{"shape":"cylinder","radius":10.5,"length":37}' && doc1.models[0].axis === "x" && Math.abs(doc1.models[0].translate[0]) < 1e-6, `${info1}; ${JSON.stringify(doc1.setups[0].stock)}`);
    check("Import STL: starter roughing and finishing filled by the engine's defaults, generated, every view refreshed",
      v1.ops === 2 && v1.tabs.length === 2 && v1.tabs.every((t) => t.endsWith("OK")) && v1.part > 0 && v1.stock > 0 && v1.path > 0 && v1.side > 1000 && !v1.stale && doc1.setups[0].operations.every((o) => o.type && o.feed > 0),
      JSON.stringify(v1));
    const pickRegions = await js("(await window.__marcam.engine('faceRegions', JSON.parse(document.getElementById('job').value), 'part', window.__marcam.state.mesh)).result.regions.length");
    check("Import STL: the new part's faces are there to pick", pickRegions === 8, `${pickRegions} face regions`);
    await shot("14b_imported_stl");
    const gcode1 = await allGcode();
    const name1 = await js("document.getElementById('projectName').textContent");

    // an edit marks the results stale until the next Generate
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const op = j.schema_version === '0.2' ? j.setups[0].operations[0] : j.programs[0].operations[0]; op.feed = (op.feed || 500) + 1; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await sleep(300);
    const stale = await js("({ body: document.body.classList.contains('stale'), banner: !!document.getElementById('staleBanner') })");
    check("an edit marks the results out of date (banner, dimmed views) until Generate", stale.body && stale.banner, JSON.stringify(stale));
    await shot("14c_stale_results");
    await js("document.getElementById('staleRegen').click(); true");
    await waitDone("regeneration", 240000);
    check("Generate again clears the stale mark", !(await js("document.body.classList.contains('stale')")));
    const gcode2 = await allGcode();

    // Save (asks for a name, downloads one file with the mesh in it), then New, then Open it
    const before = new Set(readdirSync(downloads));
    await js("document.getElementById('projectSave').click(); true");
    const saved = await waitFor(() => readdirSync(downloads).find((f) => !before.has(f) && f.endsWith(".marcam") && !f.endsWith(".crdownload")), "the saved project", 60000);
    await sleep(300);
    const savedBytes = readFileSync(join(downloads, saved));
    // (Mar) one .marcam file: a zip of project.toml (TOML) and the part
    const inside = await js(`await (async () => { const b = Uint8Array.from(atob("${savedBytes.toString("base64")}"), (c) => c.charCodeAt(0)); const { result } = await window.__marcam.engine("bundleRead", b);
      return { kind: result.project.setups[0].kind, files: result.files.map((f) => [f.name, atob(f.bytes).length]) }; })()`);
    const clean1 = await js("document.getElementById('projectDirty').classList.contains('hidden')");
    check("Save: one .marcam file (a zip of project.toml and the part STL); the unsaved mark goes",
      saved === `${name1}.marcam` && savedBytes[0] === 0x50 && savedBytes[1] === 0x4b && savedBytes.includes(Buffer.from("project.toml")) && inside.kind === "rotary" &&
      inside.files.some(([n, l]) => n === "knob.stl" && l === 84 + 50 * 10080) && clean1,
      `${saved}; ${JSON.stringify(inside)}`);
    const savedText = savedBytes;
    await newProject();
    const dialogsBefore = cdp.dialogs.length;
    await setFile("projectOpenFile", saved, Buffer.from(savedText));
    await waitFor(() => js("document.body.dataset.state === 'ready' && !!document.getElementById('emptyGenerate')"), "the opened project", 30000);
    const openedPlans = await js("window.__marcam.calls.planAndPost || 0");
    await js("document.getElementById('emptyGenerate').click(); true");
    await waitDone("generation after opening the project", 240000);
    check("Open plans nothing until asked: an empty state with Generate, then one plan", (await js("window.__marcam.calls.planAndPost")) === openedPlans + 1);
    const reopened = await allGcode();
    const v2 = await viewsOk();
    check("Open: the saved project comes back whole (no question asked, it was saved) and its G-code is byte-identical",
      reopened === gcode2 && v2.ops === 2 && v2.part > 0 && cdp.dialogs.length === dialogsBefore && (await js("document.getElementById('projectName').textContent")) === name1,
      `${reopened.length} chars vs ${gcode2.length}; dialogs ${cdp.dialogs.length - dialogsBefore}`);
    await shot("14d_reopened");

    // Edit setup: rotary -> 3-axis vise converts it (warned), regenerates, and can be undone
    const editPre = { kind: (await doc()).setups[0].kind };
    const nDialogs = cdp.dialogs.length;
    await setKind("vise");
    await shot("15d_edit_setup_dialog");
    const staleAfterEdit = await js("({ stale: document.body.classList.contains('stale'), attention: document.getElementById('generate').classList.contains('attention') })");
    const convertMsg = await js("document.getElementById('error').textContent");
    check("a setup change marks the results out of date and highlights Generate (nothing planned yet)", staleAfterEdit.stale && staleAfterEdit.attention, JSON.stringify(staleAfterEdit));
    await clickGenerate();
    await waitDone("regeneration after the type change", 240000);
    const warned = cdp.dialogs.slice(nDialogs).map((d) => d.message).join(" ");
    const dc = await doc();
    const vc = await viewsOk();
    const undoShown = await js("!document.getElementById('undoBar').classList.contains('hidden')");
    const offeredC = await addTypes();
    check("Edit setup: rotary to 3-axis vise warns, removes the rotary operations, adds 3-axis ones, re-places the part in a block and regenerates",
      editPre.kind === "rotary" && /removes op1 \(rotary_rough\), op2 \(rotary_finish\)/.test(warned) && dc.setups[0].kind === "vise" && dc.setups[0].stock.shape === "box" && dc.setups[0].operations.map((o) => o.type).join() === "rough3d,raster3d" && dc.models.find((m) => m.kind === "mesh").axis === "x" && vc.tabs.every((t) => t.endsWith("OK")) && vc.part > 0 && undoShown && !offeredC.includes("rotary_finish") && /removed/.test(convertMsg),
      `${JSON.stringify(editPre)}; ${warned.slice(0, 120)}; ${dc.setups[0].operations.map((o) => o.type)}; ${JSON.stringify(dc.setups[0].stock)}; axis ${dc.models.find((m) => m.kind === "mesh").axis}; tabs ${vc.tabs}; part ${vc.part}; undo ${undoShown}; error ${(await js("document.getElementById('error').textContent")).slice(0, 80)}`);
    await shot("15e_converted_to_vise");
    await js("document.getElementById('undoSetup').click(); true");
    await waitFor(() => js("document.body.dataset.state === 'ready'"), "the undone setup", 60000);
    await clickGenerate();
    await waitDone("regeneration after Undo", 240000);
    const du = await doc();
    check("Undo brings the rotary setup back, regenerated", du.setups[0].kind === "rotary" && du.setups[0].operations.map((o) => o.type).join() === "rotary_rough,rotary_finish" && (await allGcode()) === gcode2,
      du.setups[0].operations.map((o) => o.type).join());

    // + Setup: a second setup for the same part, its own placement (independent), made
    // indexed 3+1 and named in its Setup
    await js("(() => { for (const [id, v] of [['treeAddKind', 'rotary'], ['treeAddFrom', 'own']]) { const e = document.getElementById(id); e.value = v; e.dispatchEvent(new Event('change', { bubbles: true })); } document.getElementById('treeAddSetup').click(); return true; })()");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 2 && document.body.dataset.state !== 'busy'"), "the added setup", 60000);
    await settle();
    await setKind("indexed");
    await inDetails("const n = document.getElementById('sdName'); n.value = 'Flats'; n.dispatchEvent(new Event('change', { bubbles: true }))",
      "JSON.parse(document.getElementById('job').value).setups[1].name === 'Flats' && document.body.dataset.state !== 'busy'", "the setup named");
    await shot("15f_add_setup_dialog");
    await clickGenerate();
    await waitDone("generation of the added setup", 240000);
    const da = await doc();
    const va = await viewsOk();
    check("+ Setup: an indexed 3+1 setup joins the project with its own part placement and starters, generated",
      da.setups.length === 2 && da.setups[1].kind === "indexed" && da.setups[1].name === "Flats" && da.setups[1].part[0] !== da.setups[0].part[0] && da.setups[1].operations.length === 4 && (await js("document.getElementById('treeSetup').options.length")) === 2 && va.tabs.every((t) => t.endsWith("OK")),
      `${da.setups.map((s) => s.id + ":" + s.kind + ":" + s.operations.length).join(", ")}; tabs ${va.tabs}`);
    await shot("15g_two_setups");

    // ASCII STL dropped on the page, and an inch STL: the same part
    await newProject();
    await drop("knob_ascii.stl", Buffer.from(ascii));
    await waitDone("generation after dropping an ASCII STL", 240000);
    const infoA = await js("document.getElementById('meshInfo').textContent");
    const gA = await allGcode();
    check("drag-and-drop an ASCII STL: imported and generated like the binary one", /ASCII STL, 10080 triangles/.test(infoA) && /21\.70 x 20\.00 x 20\.00 mm/.test(infoA) && gA.replace(/knob_ascii/g, "knob") === gcode1, infoA);
    await shot("14e_ascii_dropped");
    await newProject();
    await setFile("importStlFile", "knob_inch.stl", inch);
    await waitDone("generation after an inch STL", 240000);
    // (its Part: "File in inches")
    await js("document.querySelector('#tree .tree-setup.active [data-act=part]').click(); true");
    await waitFor(() => js("!!document.getElementById('ptInch')"), "the Part pane", 30000);
    await js("document.getElementById('ptInch').click(); true");
    await waitFor(() => js("/21\\.70 x 20\\.00 x 20\\.00 mm/.test(document.getElementById('meshInfo').textContent) && document.body.dataset.state !== 'busy'"), "the part in mm", 120000);
    const infoI = await js("document.getElementById('meshInfo').textContent");
    check("an inch STL, its Part's \"File in inches\": the same size in mm", /21\.70 x 20\.00 x 20\.00 mm/.test(infoI), infoI);

    // a second STL into a project with a part: replace (OK) or add (Cancel)
    await setFile("importStlFile", "knob.stl", knobStl);
    await waitDone("generation after replacing the part", 240000);
    const asked = cdpDialogs().pop() || "";
    check("importing into a project with a part: replace (OK) or add (Cancel)", /Replace the part with knob\.stl/.test(asked), asked);

    // bad files: a clear error, nothing replaced
    const opsBefore = await js("document.querySelectorAll('#tree .tree-op').length");
    await setFile("importStlFile", "notes.stl", Buffer.from("these are not triangles"));
    await sleep(500);
    const badStl = await js("document.getElementById('error').textContent");
    await setFile("projectOpenFile", "broken.marcam.json", Buffer.from("{ not json"));
    await sleep(500);
    const badJson = await js("document.getElementById('error').textContent");
    check("bad files: a clear error and the project left as it was",
      /notes\.stl: not an STL file/.test(badStl) && /broken\.marcam\.json: not a marcam project/.test(badJson) && (await js("document.querySelectorAll('#tree .tree-op').length")) === opsBefore && !(await js("(document.getElementById('setupDialog').open && !document.getElementById('setupDialog').classList.contains('embedded'))")),
      `${badStl} | ${badJson}`);
    await shot("14g_bad_file");

    // full reset (Mar: "after a new project, the old model is still hanging around"): New ->
    // Import A -> Generate (+ playback, face picking) -> New: nothing of A left; Import B:
    // only B; New three times: the scene and the GPU memory do not grow
    {
      const domeStl = readFileSync(join(root, "examples", "plate", "dome.stl"));
      const domeTris = domeStl.readUInt32LE(80);
      const scene = () => js(`(() => { const v = window.__marcam.state.three; let objs = 0, tris = 0; v.scene.traverse((o) => { if (o.geometry) { objs++; if (o.isMesh) tris += (o.geometry.index ? o.geometry.index.count : o.geometry.attributes.position.count) / 3; } });
        const g = v.groups; const kids = (k) => g[k] ? g[k].children.length : 0;
        return { objs, tris, part: kids('part'), stock: kids('stock'), path: kids('path'), sim: kids('sim'), tool: kids('tool'), machining: !!g.machining, pickMesh: !!window.__marcam.state.tree.pickMesh,
          geometries: v.renderer.info.memory.geometries, textures: v.renderer.info.memory.textures,
          partTris: g.part.children.reduce((n, m) => n + (m.geometry.index ? m.geometry.index.count : m.geometry.attributes.position.count) / 3, 0),
          gcode: (window.__marcam.state.result?.programs || []).length, tabs: document.querySelectorAll('#tabs button').length, ops: document.querySelectorAll('#tree .tree-op').length,
          mesh: !!window.__marcam.state.mesh, meshInfo: document.getElementById('meshInfo').textContent, result: !!window.__marcam.state.result, sim: !!window.__marcam.state.sim,
          playback: !!window.__marcam.state.machining.open, session: !!window.__marcam.state.machining.session, undo: !document.getElementById('undoBar').classList.contains('hidden'),
          error: document.getElementById('error').textContent, side: (() => { const c = document.getElementById('side'); const px = c.getContext('2d').getImageData(0, 0, c.width, c.height).data; let ink = 0; for (let i = 3; i < px.length; i += 4) if (px[i]) ink++; return ink; })() }; })()`);
      const workerSession = () => js("await window.__marcam.engine('simSeek', 1).then((r) => !(r.result && r.result.error), () => false)");
      const newEmpty = async () => {
        await newProject();
        await sleep(300);
        return scene();
      };
      const base = await newEmpty();
      // A: the knob, generated, with the playback open and faces being picked
      await setFile("importStlFile", "knob.stl", knobStl);
      await waitDone("generation of part A", 240000);
      await js("document.getElementById('simOpen').click(); true");
      await waitFor(() => js("!!window.__marcam.state.machining?.open && document.body.dataset.state !== 'busy'"), "the playback of A", 120000);
      await js("document.getElementById('simClose').click(); true");
      await js("document.querySelectorAll('#tree .tree-op [data-act=\"select\"]')[1].click(); true");
      const canPick = await js("!!window.__marcam.state.tree.pickMesh");
      if (canPick) {
        // (live picking: on while the operation is selected; no button)
        await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "picking on A", 60000);
      }
      const withA = await scene();
      const sessionA = await workerSession();
      // New: nothing of A
      const afterNew = await newEmpty();
      const sessionAfter = await workerSession();
      check("full reset: after New, no part, stock, toolpaths, playback, pick highlight, G-code, tree, side view, mesh or worker session of the old project",
        withA.part > 0 && withA.path > 0 && withA.machining && withA.gcode > 0 && sessionA &&
        afterNew.objs === base.objs && afterNew.part + afterNew.stock + afterNew.path + afterNew.sim + afterNew.tool === 0 && !afterNew.machining && !afterNew.pickMesh &&
        afterNew.gcode === 0 && afterNew.tabs === 0 && afterNew.ops === 0 && !afterNew.mesh && /No mesh/.test(afterNew.meshInfo) && !afterNew.result && !afterNew.sim &&
        !afterNew.playback && !afterNew.session && !afterNew.undo && !afterNew.error && afterNew.side === 0 && !sessionAfter && afterNew.geometries <= base.geometries,
        `picking ${canPick}; with A ${JSON.stringify(withA)}; after New ${JSON.stringify(afterNew)}; base ${base.objs} objects ${base.geometries} geometries; worker session ${sessionA} -> ${sessionAfter}`);
      await shot("14h_reset_after_new");
      // B: only B's geometry and G-code
      await setFile("importStlFile", "dome.stl", domeStl);
      await waitDone("generation of part B", 240000);
      const withB = await scene();
      const gB = await allGcode();
      // (B's playback may already be open: its plan's toolpaths handed to the simulation, it
      // opens at once; what it plays is B's, nothing of A's)
      const playsB = await js("(() => { const s = window.__marcam.state.machining.session; return s ? s.ops.map((o) => o.program).join(',') : ''; })()");
      check("after the reset, importing B shows only B: B's triangles, B's G-code (nothing named after A)",
        withB.partTris === domeTris && /dome\.stl/.test(withB.meshInfo) && !/knob/i.test(gB) && /dome/i.test(gB) && withB.tabs >= 2 && !/knob/i.test(playsB) && !withB.pickMesh,
        `${withB.partTris} part triangles (B has ${domeTris}); ${withB.meshInfo}; ${withB.tabs} programs; G-code knob ${/knob/i.test(gB)} dome ${/dome/i.test(gB)}; playing ${playsB || "nothing"}; picking ${withB.pickMesh}`);
      await shot("14i_only_b");
      // New three times: no growth
      const runs = [];
      for (let k = 0; k < 3; k++) runs.push(await newEmpty());
      check("New three times: the scene's objects and the renderer's geometries and textures do not grow",
        runs.every((r) => r.objs === base.objs && r.geometries <= base.geometries && r.textures <= base.textures && !r.mesh && r.gcode === 0),
        runs.map((r) => `${r.objs} objects, ${r.geometries} geometries, ${r.textures} textures`).join("; ") + ` (base ${base.objs}/${base.geometries}/${base.textures})`);
      // an example goes through the same reset
      await js("document.getElementById('example').value = 'knob_finish'; document.getElementById('loadExample').click(); true");
      await settle();
      await clickGenerate();
      await waitDone("the example after the resets", 240000);
      await js("document.getElementById('example').value = 'ring'; document.getElementById('loadExample').click(); true");
      await settle();
      const ex = await scene();
      // (the ring's own bar and part show at once: drawn from the project)
      check("loading an example resets too: the previous example's part, toolpaths and mesh are gone; only the ring's bar and part",
        ex.path === 0 && ex.part === 1 && ex.stock === 1 && !ex.mesh && ex.gcode === 0 && !ex.result && ex.partTris !== domeTris, JSON.stringify(ex));
    }

    // leaving with unsaved changes: the browser asks
    await js("document.getElementById('job').dispatchEvent(new Event('input')); true");
    await S("Page.reload");
    await waitFor(() => js("document.body.dataset.state === 'ready'"), "the page after reload", 30000);
    check("leaving with unsaved changes: the browser asks first", cdp.dialogs.some((d) => d.type === "beforeunload"), cdpDialogs().slice(-1).join());
  }

  // 8k. planning only when asked: view actions plan nothing and mark nothing stale; an edit
  // marks the results out of date and waits for Generate; auto-regenerate (off by default)
  // plans once after a drag is released
  {
    const plans = () => js("window.__marcam.calls.planAndPost || 0");
    const isStale = () => js("document.body.classList.contains('stale')");
    await js("document.getElementById('example').value = 'knob_finish'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("knob mesh generation for the plan count", 240000);
    const p0 = await plans();
    const autoDefault = await js("document.getElementById('autoGen').checked");
    // orbit, pan and zoom on the 3D view (mouse drags and the wheel)
    const [vx, vy, vw, vh] = await rect("view3d");
    const drag = async (button, buttons) => {
      await S("Input.dispatchMouseEvent", { type: "mousePressed", x: vx + vw / 2, y: vy + vh / 2, button, buttons, clickCount: 1 });
      for (let k = 1; k <= 8; k++) await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: vx + vw / 2 + 12 * k, y: vy + vh / 2 + 5 * k, button, buttons });
      await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: vx + vw / 2 + 96, y: vy + vh / 2 + 40, button, buttons: 0, clickCount: 1 });
    };
    await drag("left", 1);
    await drag("right", 2);
    for (let k = 0; k < 5; k++) await S("Input.dispatchMouseEvent", { type: "mouseWheel", x: vx + vw / 2, y: vy + vh / 2, deltaX: 0, deltaY: k % 2 ? 120 : -120 });
    await js("window.__marcam.setView('front', {}); window.__marcam.setView('back', {}); window.__marcam.setView('iso', {}); true");
    // layers, scrubbing the moves, switching program tabs, resizing panes, selecting in the tree
    for (const id of ["showRapids", "showStock", "showPart", "showSim"]) await js(`document.getElementById('${id}').click(); document.getElementById('${id}').click(); true`);
    for (const v of [10, 200, 2000, 0]) await js(`const s = document.getElementById('scrub'); s.value = ${v}; s.dispatchEvent(new Event('input')); true`);
    await js("document.querySelectorAll('#tabs button')[1].click(); document.querySelectorAll('#tabs button')[0].click(); true");
    const [lx, ly, lw, lh] = await rect("splitLeft");
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: lx + lw / 2, y: ly + lh / 2, button: "left", buttons: 1, clickCount: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: lx + lw / 2 + 60, y: ly + lh / 2, button: "left", buttons: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: lx + lw / 2 + 60, y: ly + lh / 2, button: "left", buttons: 0, clickCount: 1 });
    await js("document.querySelector('#tree .tree-op [data-act=\"select\"]').click(); true");
    await sleep(300);
    await js("document.querySelector('#tree .tree-op [data-act=\"select\"]').click(); true");
    // machining playback: open, play a little, scrub, close
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("!!window.__marcam.state.machining?.open && document.body.dataset.state !== 'busy'"), "the machining playback", 120000);
    await js("const s = document.getElementById('simScrub'); s.value = Math.floor(s.max / 2); s.dispatchEvent(new Event('input')); true");
    await sleep(300);
    await js("document.getElementById('simClose').click(); true");
    await sleep(800);
    const viewPlans = (await plans()) - p0;
    const viewStale = await isStale();
    const dirtyAfterView = await js("!document.getElementById('projectDirty').classList.contains('hidden')");
    check("view actions (orbit, pan, zoom, presets, layers, scrub, tabs, pane resize, tree selection, playback) plan nothing and mark nothing stale",
      viewPlans === 0 && !viewStale && !dirtyAfterView, `${viewPlans} plans; stale ${viewStale}; unsaved ${dirtyAfterView}`);
    await shot("16a_after_view_actions");
    // a colour (display only): saved, nothing stale, nothing planned
    await js("const c = document.querySelector('#tree .tree-op input[data-act=\"color\"]'); c.value = '#ff00aa'; c.dispatchEvent(new Event('change', { bubbles: true })); true");
    await sleep(600);
    check("changing an operation's colour plans nothing and leaves the results current (only unsaved)",
      (await plans()) === p0 && !(await isStale()) && (await js("!document.getElementById('projectDirty').classList.contains('hidden')")), `${(await plans()) - p0} plans; stale ${await isStale()}`);
    // a real edit: out of date, Generate highlighted, nothing planned until Generate (once
    // the colour's save has redrawn the panels: a slow machine is still at it)
    await settle();
    // (an operation's feed, in the project text: the quick panel's fields depend on whether
    // the job is still 0.1 or already saved as 0.2)
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value);
      const op = j.schema_version === '0.2' ? j.setups[0].operations[0] : j.programs[0].operations[0];
      op.feed = Math.round((op.feed || 500) * 1.1); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await sleep(AUTO_WAIT);
    const edited = await js("({ stale: document.body.classList.contains('stale'), banner: !!document.getElementById('staleBanner'), attention: document.getElementById('generate').classList.contains('attention') })");
    check("live updating is on by default; switched off, editing a parameter marks the results out of date and highlights Replan, and plans nothing",
      liveDefault === true && autoDefault === false && edited.stale && edited.banner && edited.attention && (await plans()) === p0, `${JSON.stringify(edited)}; live by default ${liveDefault}, here ${autoDefault}; ${(await plans()) - p0} plans`);
    await shot("16b_out_of_date");
    await clickGenerate();
    await waitDone("generation after the edit", 240000);
    check("Generate: exactly one plan, and the results are current again", (await plans()) === p0 + 1 && !(await isStale()) && !(await js("document.getElementById('generate').classList.contains('attention')")));
    // auto-regenerate on: twenty values while the mouse is held, one plan after the release
    await js("document.getElementById('autoGen').click(); true");
    const p1 = await plans();
    // (the mouse held down on the status line, as it would be dragging a value)
    const [qx, qy, qw, qh] = await js("(() => { const r = document.getElementById('status').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()");
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: qx + qw / 2, y: qy + qh / 2, button: "left", buttons: 1, clickCount: 1 });
    for (let k = 1; k <= 20; k++) {
      await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const op = j.schema_version === '0.2' ? j.setups[0].operations[0] : j.programs[0].operations[0]; op.feed = (op.feed || 500) + 1; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
      await sleep(60);
    }
    await sleep(AUTO_WAIT);
    const whileHeld = (await plans()) - p1;
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: qx + qw / 2, y: qy + qh / 2, button: "left", buttons: 0, clickCount: 1 });
    await waitFor(async () => (await plans()) > p1, "the auto-regenerate", 60000);
    await waitDone("the auto-regenerated plan", 240000);
    await sleep(AUTO_WAIT);
    const afterRelease = (await plans()) - p1;
    check("auto-regenerate: 20 edits while dragging plan nothing; one plan after the release",
      whileHeld === 0 && afterRelease === 1 && !(await isStale()), `${whileHeld} while held, ${afterRelease} after`);
    await js("document.getElementById('autoGen').click(); true");
    await shot("16c_auto_regenerated");
  }

  }
  // 8l. picking: the bracket's detected holes and pocket start operations; the picking mode
  // lights up what an operation takes, chips list the picks, "select all similar", hover
  // labels, a click on the canvas toggles a feature, wrong features are refused with a note
  {
    await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
    const fx = join(root, "tests", "fixtures", "picking");
    const proj = JSON.parse(readFileSync(join(fx, "bracket.project.json"), "utf8"));
    const stl64 = readFileSync(join(fx, "bracket.stl")).toString("base64");
    const file = JSON.stringify({ format: "marcam-project", version: 1, project: proj, meshes: { "bracket.stl": stl64 } });
    cdp.dialogAnswer = true;
    await js(`await window.__marcam.project.openText(${JSON.stringify(file)}, "bracket.marcam.json").then(() => true)`);
    await waitFor(() => js("document.querySelectorAll('#detected .detected-btn').length === 3"), "the detected features", 30000);
    const detectedText = await js("[...document.querySelectorAll('#detected .detected-btn')].map((b) => b.textContent)");
    check("an imported part's holes and pockets are detected, holes grouped by size (drilled with a tool of their size, else bored)",
      detectedText.includes("Drill 4 × 5.00 mm through") && detectedText.includes("Bore 1 × 6.00 mm blind") && detectedText.some((x) => /^Pocket 20\.00 x 24\.00/.test(x)), detectedText.join(" | "));
    await shot("17a_detected_features");
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    // Drill 4 x 5.00: a drilling operation with the 5 mm tool, the four centres, through depth
    await js("[...document.querySelectorAll('#detected .detected-btn')][0].click(); true");
    await waitFor(async () => (await doc()).setups[0].operations.length === 1, "the drilling operation", 60000);
    const drill = (await doc()).setups[0].operations[0];
    check("Drill 4 × 5.00: drilling with the 5 mm tool, four points, from the top, through (12.5), peck one diameter",
      drill.type === "drill" && drill.tool === 1 && drill.points.length === 4 && drill.top === 0 && drill.depth === 12.5 && drill.peck === 5, JSON.stringify(drill));
    // picking mode on it: the four holes picked as chips
    // the new operation is selected: its editor offers picking
    await waitFor(() => js("window.__marcam.state.tree.selected === 'op1' && !!window.__marcam.state.tree.pickMesh"), "the drilling editor", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && document.querySelectorAll('#pickChips .pick-chip').length === 4"), "picking on the drill", 30000);
    await sleep(400);
    // a canvas point that hits the feature `pred` picks (by its pickable), found by casting a
    // ray at its triangles' centres: [client x, y, canvas x, y]
    const screenOf = (pred) => js(`(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const pk = t.pick; const target = pk.list.find(${pred}); if (!target) return null;
      const want = new Set(target.faces); const el = v.renderer.domElement; const r = el.getBoundingClientRect(); const tri = t.regions.tri; const ray = new v.THREE.Raycaster();
      // the centre and points near each corner (a deep wall shows only near its rim)
      const W = [[1/3, 1/3, 1/3], [0.9, 0.05, 0.05], [0.05, 0.9, 0.05], [0.05, 0.05, 0.9]];
      for (let k = 0; k < t.regions.of.length; k++) { if (!want.has(t.regions.list[t.regions.of[k]].id)) continue;
        for (const w of W) {
          const c = new v.THREE.Vector3(...[0, 1, 2].map((d) => w[0] * tri[9*k+d] + w[1] * tri[9*k+3+d] + w[2] * tri[9*k+6+d])).project(v.camera);
          if (Math.abs(c.x) > 0.95 || Math.abs(c.y) > 0.95) continue;
          ray.setFromCamera(new v.THREE.Vector2(c.x, c.y), v.camera); const hit = ray.intersectObject(t.pickMesh)[0];
          if (hit && want.has(t.regions.list[t.regions.of[hit.faceIndex]].id)) { const px = (c.x + 1) / 2 * el.clientWidth, py = (1 - c.y) / 2 * el.clientHeight; return [r.left + px, r.top + py, px, py]; } } }
      return null; })()`);
    const chips = () => js("document.querySelectorAll('#pickChips .pick-chip').length");
    // remove one chip: three points
    await js("document.querySelector('#pickChips [data-unpick]').click(); true");
    await waitFor(async () => (await chips()) === 3, "a chip removed", 10000);
    const three = await js("window.__marcam.state.tree.draft?.op || JSON.parse(JSON.stringify(window.__marcam.state.tree.doc.setups[0].operations[0]))");
    // select all similar: back to four
    await js("document.getElementById('pickSimilar').click(); true");
    await waitFor(async () => (await chips()) === 4, "select all similar", 10000);
    check("chips list the picks; removing one drops its point; select all similar adds the rest",
      three.points.length === 3 && (await js("window.__marcam.state.tree.doc.setups[0].operations[0].points.length")) === 4);
    // the hole at (40, 8) on the canvas: hover shows its label, a click un-picks it
    const at = await screenOf("(x) => x.kind === 'hole' && x.data.through && Math.abs(x.data.center[0] - 40) < 0.01 && Math.abs(x.data.center[1] - 8) < 0.01");
    await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: at[0], y: at[1], buttons: 0 });
    await waitFor(() => js("/Hole 5\\.00 mm through/.test(document.getElementById('pickInfo').textContent)"), "the hover label", 10000);
    const hovered = await js("document.getElementById('pickInfo').textContent");
    await shot("17b_picking_holes");
    const picked = await js(`const pr = await window.__marcam.tree.pickAt(${at[2]}, ${at[3]}); pr && pr.pickable ? pr.pickable.kind : "none"`);
    await waitFor(async () => (await chips()) === 3, "the hole clicked off", 10000);
    // a click on the pocket floor: drilling does not take it
    // (the pocket floor is no drilling target, so it is not in the picking list: find it by
    // its faces among all regions instead)
    const floor = await js(`(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const el = v.renderer.domElement; const tri = t.regions.tri; const ray = new v.THREE.Raycaster();
      for (let k = 0; k < t.regions.of.length; k++) { const z = (tri[9*k+2]+tri[9*k+5]+tri[9*k+8])/3; if (Math.abs(z + 5) > 1e-6) continue;
        const c = new v.THREE.Vector3((tri[9*k]+tri[9*k+3]+tri[9*k+6])/3, (tri[9*k+1]+tri[9*k+4]+tri[9*k+7])/3, z).project(v.camera);
        ray.setFromCamera(new v.THREE.Vector2(c.x, c.y), v.camera); const hit = ray.intersectObject(t.pickMesh)[0];
        if (hit && hit.faceIndex === k) return [(c.x + 1) / 2 * el.clientWidth, (1 - c.y) / 2 * el.clientHeight]; }
      return null; })()`);
    await js(`await window.__marcam.tree.pickAt(${floor[0]}, ${floor[1]}).then(() => true)`);
    const refused = await js("document.getElementById('pickNotes')?.textContent || ''");
    check("hovering a hole shows its label; clicking it toggles it; a face drilling cannot take is refused with a note",
      /Hole 5\.00 mm through/.test(hovered) && picked === "hole" && (await chips()) === 3 && /not a hole/.test(refused), `${hovered} | ${picked} | ${refused}`);
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await sleep(400);
    check("done picking writes the picks once: three points", (await doc()).setups[0].operations[0].points.length === 3 && !(await js("!!window.__marcam.state.tree.pickMesh")));
    // the blind 6 mm hole: no 6 mm tool, so it is bored (with the largest that fits: 5 mm)
    await js("[...document.querySelectorAll('#detected .detected-btn')].find((b) => /^Bore 1 × 6\.00 mm blind/.test(b.textContent)).click(); true");
    await waitFor(async () => (await doc()).setups[0].operations.length === 2, "the boring", 60000);
    const bored = (await doc()).setups[0].operations[1];
    // a drilling picked on it says use Bore instead, and becomes a bore
    await js("document.getElementById('treeAddType').value = 'drill'; document.getElementById('treeAdd').click(); true");
    await waitFor(() => js(`${SEL}?.type === 'drill' && !!window.__marcam.state.tree.pickMesh`), "the drilling", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking for the draft", 30000);
    await sleep(300);
    const blindAt = await screenOf("(x) => x.kind === 'hole' && !x.data.through");
    await js(`const pr = await window.__marcam.tree.pickAt(${blindAt[2]}, ${blindAt[3]}); true`);
    await waitFor(() => js("!!document.getElementById('boreInstead')"), "the Bore instead button", 10000);
    const boreNote = await js("document.getElementById('pickNotes').textContent");
    await js("document.getElementById('boreInstead').click(); true");
    await waitFor(() => js(`${SEL}?.type === 'bore'`), "the drilling bored", 10000);
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(async () => (await doc()).setups[0].operations.length === 3, "the bore applied", 60000);
    const converted = (await doc()).setups[0].operations[2];
    check("holes without a tool of their size are bored; a drilling on one says use Bore instead, and Bore instead makes it a bore",
      bored.type === "bore" && bored.tool === 1 && bored.diameter === 6 && bored.depth === 6 && JSON.stringify(bored.points) === "[[46,20]]" &&
      /Bore/.test(boreNote) && converted.type === "bore" && converted.diameter === 6 && JSON.stringify(converted.points) === "[[46,20]]",
      `${JSON.stringify({ type: bored.type, tool: bored.tool, d: bored.diameter, depth: bored.depth })} | ${boreNote} | ${converted.type}`);
    // the pocket: its outline and island, its depth
    await js("[...document.querySelectorAll('#detected .detected-btn')].find((b) => b.dataset.start === 'pocket').click(); true");
    await waitFor(async () => (await doc()).setups[0].operations.length === 4, "the pocket operation", 60000).catch(async (e) => {
      // (CI-only: why not)
      throw new Error(`${e.message}: ${await js("JSON.stringify({ ops: JSON.parse(document.getElementById('job').value).setups[0].operations.map((o) => o.type), error: document.getElementById('error').textContent, draft: window.__marcam.state.tree.draft?.op.type, buttons: [...document.querySelectorAll('#detected .detected-btn')].map((b) => b.dataset.start + ':' + b.textContent).slice(0, 8) })")}`);
    });
    const pocket = (await doc()).setups[0].operations.find((o) => o.type === "pocket");
    check("the pocket takes its outline, island and depth",
      pocket.paths.length === 2 && pocket.depth === 5 && pocket.top === 0, JSON.stringify({ paths: pocket.paths?.length, depth: pocket.depth }));
    // it all plans and verifies
    await js("document.getElementById('error').textContent = ''; document.getElementById('generate').click(); true");
    await waitDone("generation of the picked bracket", 240000);
    const tabs = await js("[...document.querySelectorAll('#tabs button')].map((b) => b.textContent)");
    const arcs = await js("window.__marcam.state.result.programs.reduce((n, p) => n + p.gcode.split('\\n').filter((l) => l.startsWith('G3 ')).length, 0)");
    check("the picked operations plan and verify clean; the bores are written as G3 arcs", tabs.length >= 1 && tabs.every((x) => x.endsWith("OK")) && arcs > 0, `${tabs.join(", ")}; ${arcs} arcs`);
    await shot("17c_picked_generated");
    // a 3D parallel finish says what it will do: the plate's dome raster has no roughing
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    const rid = await js("window.__marcam.state.tree.doc.setups[0].operations.find((o) => o.type === 'raster3d').id");
    await js(`document.querySelector('#tree .tree-op[data-op="${rid}"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('rasterInfo')"), "the raster's info", 10000);
    const rinfo = await js("document.getElementById('rasterInfo').textContent");
    check("a 3D parallel finish shows its stepover, rows and levels, and suggests a roughing when none comes first",
      /stepover [\d.]+ mm/.test(rinfo) && /\d+ rows/.test(rinfo) && /no 3D roughing before it/.test(rinfo), rinfo);
  }

  // (the fast suite, MARCAM_E2E_FAST: these sections are left to the full one)
  if (!FAST) {
  // 8m. picking on every operation type: rotary turning (a band -> its X span), knurl (a
  // cone -> the body), chamfer (a round edge), engraving wrapped round the bar (its radius,
  // the text centred), facing on an indexed setup (a side face -> its A index and depth)
  {
    const screenOf = (pred) => js(`(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const pk = t.pick; const target = pk.list.find(${pred}); if (!target) return null;
      const want = new Set(target.faces); const el = v.renderer.domElement; const r = el.getBoundingClientRect(); const tri = t.regions.tri; const ray = new v.THREE.Raycaster();
      const W = [[1/3, 1/3, 1/3], [0.9, 0.05, 0.05], [0.05, 0.9, 0.05], [0.05, 0.05, 0.9]];
      for (let k = 0; k < t.regions.of.length; k++) { if (!want.has(t.regions.list[t.regions.of[k]].id)) continue;
        for (const w of W) {
          const c = new v.THREE.Vector3(...[0, 1, 2].map((d) => w[0] * tri[9*k+d] + w[1] * tri[9*k+3+d] + w[2] * tri[9*k+6+d])).project(v.camera);
          if (Math.abs(c.x) > 0.95 || Math.abs(c.y) > 0.95) continue;
          ray.setFromCamera(new v.THREE.Vector2(c.x, c.y), v.camera); const hit = ray.intersectObject(t.pickMesh)[0];
          if (hit && want.has(t.regions.list[t.regions.of[hit.faceIndex]].id)) { const px = (c.x + 1) / 2 * el.clientWidth, py = (1 - c.y) / 2 * el.clientHeight; return [r.left + px, r.top + py, px, py]; } } }
      return null; })()`);
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    // pick `pred` for operation `k` (by its row) and write it; returns the operation
    async function pickFor(k, pred, what) {
      const id = await js(`document.querySelectorAll('#tree .tree-op')[${k}].dataset.op`);
      if ((await js("window.__marcam.state.tree.selected")) !== id) await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="select"]').click(); true`);
      await waitFor(() => js(`window.__marcam.state.tree.selected === '${id}' && !!window.__marcam.state.tree.pickMesh`), `${what}: its editor`, 60000);
      // (live picking: on while the operation is selected; no button)
      await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), `${what}: picking`, 30000);
      await sleep(300);
      const at = await screenOf(pred);
      if (!at) throw new Error(`${what}: the feature is not visible`);
      const got = await js(`const pr = await window.__marcam.tree.pickAt(${at[2]}, ${at[3]}); pr && pr.pickable ? pr.pickable : null`);
      await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
      await waitFor(() => js("!window.__marcam.state.tree.pickMesh"), `${what}: done`, 60000);
      await sleep(300);
      const s = (await doc()).setups.find((x) => x.operations.some((o) => o.id === id));
      return { op: s.operations.find((o) => o.id === id), got };
    }
    // the knob: turning, knurl, chamfer
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    const turn = await pickFor(0, "(x) => x.kind === 'cylindrical_band' && Math.abs(x.data.r[1] - 5) < 0.01", "turning");
    const knurl = await pickFor(1, "(x) => x.kind === 'revolved_band' && x.data.shape === 'cone' && x.data.r[1] > 9.9", "knurl");
    const chamfer = await pickFor(2, "(x) => x.kind === 'circular_edge'", "chamfer");
    check("rotary picks: turning takes the boss's X span, the knurl the taper as its body, the chamfer a round edge",
      turn.got?.kind === "cylindrical_band" && JSON.stringify(turn.op.x_range) === JSON.stringify(turn.got.data.x) &&
      knurl.op.body_back[0] === -15 && Math.abs(knurl.op.body_back[1] - 10) < 0.01 && knurl.op.body_front[0] < 0 &&
      chamfer.got?.kind === "circular_edge" && chamfer.op.edge[0] === chamfer.got.data.x && chamfer.op.edge[1] === chamfer.got.data.r,
      JSON.stringify({ x_range: turn.op.x_range, back: knurl.op.body_back, front: knurl.op.body_front, edge: chamfer.op.edge }));
    await shot("17d_rotary_picks");
    // the ring: its text engraving onto the bar (R12), centred on the band picked
    await js("document.getElementById('example').value = 'ring'; document.getElementById('loadExample').click(); true");
    await settle();
    const rows = await js("[...document.querySelectorAll('#tree .tree-op')].map((r) => r.querySelector('.tree-label').textContent)");
    const textRow = Math.max(0, (await doc()).setups[0].operations.findIndex((o) => o.type === "engrave" && o.text));
    const eng = await pickFor(textRow, "(x) => x.kind === 'cylindrical_band'", "engraving");
    check("engraving picks a cylinder: its radius is the surface, the text centred on it (round the top at A0)",
      eng.got?.kind === "cylindrical_band" && Math.abs(eng.op.top - eng.got.data.r[1]) < 1e-6 && Math.abs(eng.op.text_at[1] + eng.op.text_height / 2) < 1e-6,
      `${rows.length} rows; top ${eng.op.top}; text_at ${eng.op.text_at}`);
    // the bracket on a 4th axis (indexed): a new facing operation, its underside picked
    const fx = join(root, "tests", "fixtures", "picking");
    const proj = JSON.parse(readFileSync(join(fx, "bracket.project.json"), "utf8"));
    Object.assign(proj.setups[0], { id: "idx", name: "Indexed", kind: "indexed", machine: "makera-z1-4axis", stock: { shape: "cylinder", radius: 40, length: 70 }, holding: { kind: "rotary_chuck", tailstock: false } });
    const file = JSON.stringify({ format: "marcam-project", version: 1, project: proj, meshes: { "bracket.stl": readFileSync(join(fx, "bracket.stl")).toString("base64") } });
    await js(`await window.__marcam.project.openText(${JSON.stringify(file)}, "bracket_indexed.marcam.json").then(() => true)`);
    await settle();
    await js("document.getElementById('treeAddType').value = 'face'; document.getElementById('treeAdd').click(); true");
    await waitFor(() => js(`${SEL}?.type === 'face' && !!window.__marcam.state.tree.pickMesh`), "the facing", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking a side face", 30000);
    // orbit underneath to see the underside
    await js("(() => { const v = window.__marcam.state.three; v.camera.position.set(30, -50, -70); v.controls.target.set(30, 20, -6); v.controls.update(); v.renderer.render(v.scene, v.camera); return true; })()");
    await sleep(300);
    const sideAt = await screenOf("(x) => x.kind === 'side_face' && Math.abs(Math.abs(x.data.index_a) - 180) < 1e-6");
    const side = sideAt ? await js(`const pr = await window.__marcam.tree.pickAt(${sideAt[2]}, ${sideAt[3]}); pr && pr.pickable ? pr.pickable.kind : "none"`) : "not visible";
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await sleep(400);
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(async () => (await doc()).setups[0].operations.some((o) => o.type === "face"), "the facing applied", 60000);
    const face = (await doc()).setups[0].operations.find((o) => o.type === "face");
    check("indexed: picking the underside sets the facing's A index (180) and its depth from the bar (40 - 12 = 28)",
      side === "side_face" && Math.abs(Math.abs(face.index_a) - 180) < 1e-6 && Math.abs(face.depth - 28) < 1e-6, `${side}; index_a ${face.index_a}; depth ${face.depth}`);
    await shot("17e_indexed_side_face");
    // every operation type offers picking
    const types = await js("window.__marcam.state.tree.types.filter((t) => !(t.picks || []).length).map((t) => t.kind)");
    check("every operation type takes picks", types.length === 0, types.join(", "));
  }

  // 8n. (Mar) removing every operation keeps the model: part and stock come from the project,
  // Generate says why it cannot; disabling all and reopening such a project too. And toolpath
  // visibility per operation, per file, per setup (view only)
  {
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    const view = () => js("(() => { const v = window.__marcam.state.three; return { part: v.groups.part.children.length, stock: v.groups.stock.children.length, path: v.groups.path.children.reduce((n, l) => n + (l.geometry?.attributes.position?.count || 0), 0) }; })()");
    // import the knob into a vise setup (two starter operations), generate
    const knobStl = readFileSync(join(root, "examples", "knob", "knob.stl")).toString("base64");
    const proj0 = { schema_version: "0.2", name: "noops", models: [], tools: [], setups: [] };
    await js(`await window.__marcam.project.openText(${JSON.stringify(JSON.stringify({ format: "marcam-project", version: 1, project: proj0 }))}, "noops.marcam.json").then(() => true)`);
    await settle();
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${knobStl}"), (c) => c.charCodeAt(0))], "knob.stl")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await waitFor(() => js("(() => { try { return JSON.parse(document.getElementById('job').value).setups?.length === 1 && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "the knob imported", 120000);
    await settle();
    await setKind("vise");
    // delete every operation, one by one
    for (const id of await js("[...document.querySelectorAll('#tree .tree-op')].map((r) => r.dataset.op).reverse()")) {
      await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="delete"]').click(); true`);
      await sleep(600);
    }
    await sleep(800);
    const none = await view();
    const g0 = await js("({ disabled: document.getElementById('generate').disabled, title: document.getElementById('generate').title, empty: document.getElementById('emptyState')?.textContent || '', detected: !!document.getElementById('detected'), ops: document.querySelectorAll('#tree .tree-op').length })");
    check("every operation removed: the part and stock stay, no old toolpath, Generate disabled with the reason, an empty state that says so",
      none.part > 0 && none.stock > 0 && none.path === 0 && g0.ops === 0 && g0.disabled && /No operation/.test(g0.title) && /No operations/.test(g0.empty) && g0.detected,
      `${JSON.stringify(none)} ${JSON.stringify(g0)}`);
    await shot("18a_no_operations");
    // reopen it saved with no operations: still the part and stock
    const saved = await js("JSON.stringify((() => { const st = window.__marcam.state; let bin = ''; for (let i = 0; i < st.mesh.length; i += 8192) bin += String.fromCharCode(...st.mesh.subarray(i, i + 8192)); return { format: 'marcam-project', version: 1, project: JSON.parse(document.getElementById('job').value), meshes: { 'knob.stl': btoa(bin) } }; })())");
    await js(`await window.__marcam.project.openText(${JSON.stringify(saved)}, "noops2.marcam.json").then(() => true)`);
    await settle();
    await sleep(800);
    const reopened = await view();
    check("a project saved with no operations reopens with its part and stock", reopened.part > 0 && reopened.stock > 0, JSON.stringify(reopened));
    // add an operation back: Generate works
    await js("document.getElementById('treeAddType').value = 'raster3d'; document.getElementById('treeAdd').click(); true");
    await waitFor(() => js(`${SEL}?.type === 'raster3d' && !!document.getElementById('opForm')`), "the new raster", 60000);
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(async () => (await doc()).setups[0].operations.length === 1, "the raster applied", 60000);
    await sleep(500);
    const enabled = await js("!document.getElementById('generate').disabled");
    await clickGenerate();
    await waitDone("generation after adding an operation back", 240000);
    const back = await view();
    check("an operation added back: Generate is enabled again and plans", enabled && back.path > 0 && back.part > 0, JSON.stringify(back));
    // disabled, all of them: the same as none
    await js("document.querySelector('#tree .tree-op [data-act=\"enable\"]').click(); true");
    await sleep(800);
    const off = await view();
    const g1 = await js("({ disabled: document.getElementById('generate').disabled, title: document.getElementById('generate').title })");
    check("every operation disabled: the part and stock stay, Generate disabled with the reason", off.part > 0 && off.stock > 0 && g1.disabled && /enable one/.test(g1.title), `${JSON.stringify(off)} ${JSON.stringify(g1)}`);
    await js("document.querySelector('#tree .tree-op [data-act=\"enable\"]').click(); true");
    await sleep(500);

    // visibility: the plate (several operations in one file), generated
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the plate for visibility", 240000);
    const plans0 = await js("window.__marcam.calls.planAndPost || 0");
    const gcode0 = await js("window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')");
    const all = (await view()).path;
    const opIds = await js("window.__marcam.state.tree.doc.setups[0].outputs[0].operations");
    // hide the first operation (facing)
    await js(`document.querySelector('#tree .tree-op[data-op="${opIds[0]}"] [data-act="eye"]').click(); true`);
    await sleep(500);
    const lessOne = (await view()).path;
    const eye = await js(`document.querySelector('#tree .tree-op[data-op="${opIds[0]}"] [data-act="eye"]').classList.contains('off')`);
    // hide all, then show only the pocket (Alt-click)
    await js("document.getElementById('eyeNone').click(); true");
    await sleep(400);
    const noneShown = (await view()).path;
    await js(`(() => { const b = document.querySelector('#tree .tree-op[data-op="${opIds[1]}"] [data-act="eye"]'); b.dispatchEvent(new MouseEvent('click', { bubbles: true, altKey: true })); return true; })()`);
    await sleep(400);
    const only = (await view()).path;
    const hiddenNow = await js("[...window.__marcam.state.hidden]");
    const stale = await js("document.body.classList.contains('stale')");
    const plans1 = await js("window.__marcam.calls.planAndPost || 0");
    const gcode1 = await js("window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')");
    check("hiding toolpaths is view only: one hidden (fewer moves drawn, its eye off), all hidden (none), show only one (Alt-click); nothing planned, nothing out of date, the same G-code",
      lessOne < all && lessOne > 0 && eye && noneShown === 0 && only > 0 && only < all && hiddenNow.length === (await js("window.__marcam.state.tree.doc.setups[0].operations.length")) - 1 && plans1 === plans0 && !stale && gcode1 === gcode0,
      `${all} -> ${lessOne} -> ${noneShown} -> ${only}; hidden ${hiddenNow.length}; plans ${plans1 - plans0}; stale ${stale}`);
    await shot("18b_toolpaths_hidden");
    // saved and reopened: the same ones hidden
    const savedPlate = await js("JSON.stringify((() => { const st = window.__marcam.state; let bin = ''; for (let i = 0; i < st.mesh.length; i += 8192) bin += String.fromCharCode(...st.mesh.subarray(i, i + 8192)); return { format: 'marcam-project', version: 1, project: JSON.parse(document.getElementById('job').value), meshes: { 'dome.stl': btoa(bin) }, view: { hidden: [...st.hidden] } }; })())");
    await js(`await window.__marcam.project.openText(${JSON.stringify(savedPlate)}, "plate_view.marcam.json").then(() => true)`);
    await settle();
    const restored = await js("[...window.__marcam.state.hidden].sort().join(',')");
    check("which toolpaths are hidden is saved with the project and comes back on open", restored === hiddenNow.slice().sort().join(","), restored);
    // (Mar) hidden operations are not simulated either (their material stays), in the
    // simulated stock and in the playback; showing them again simulates again; nothing is
    // planned for any of it
    await clickGenerate();
    await waitDone("the plate for the simulations", 240000);
    const plansS = await js("window.__marcam.calls.planAndPost || 0");
    const cutCells = () => js("(() => { const s = window.__marcam.state.sim; if (!s) return -1; const top = s.stock_box.max[2]; return s.stocks[s.stocks.length - 1].z.filter((z) => z < top - 0.01).length; })()");
    await js("document.getElementById('simulate').click(); true");
    await waitFor(() => js("!!window.__marcam.state.sim && document.body.dataset.state === 'done'"), "the simulation with one operation shown", 240000);
    const cutOne = await cutCells();
    const note = await js("document.getElementById('simHidden')?.textContent || ''");
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("!!window.__marcam.state.machining?.open && document.body.dataset.state !== 'busy'"), "the machining playback", 120000);
    const playOps = await js("window.__marcam.state.machining.session.ops.length");
    await js("document.getElementById('simClose').click(); true");
    await sleep(500);
    // all shown again: simulated again (in the background), more cut, more in the playback
    await js("document.getElementById('eyeAll').click(); true");
    await sleep(600);
    await waitFor(async () => (await js("document.body.dataset.state")) !== "busy" && (await cutCells()) > cutOne, "the simulation again with everything shown", 240000).catch(() => {});
    const cutAll = await cutCells();
    const allOps = await js("window.__marcam.state.result.programs.reduce((n, p) => n + (p.time?.ops?.length || 0), 0)");
    check("hidden operations are not simulated (their material stays) nor played back; shown again, simulated again; nothing planned",
      cutOne > 0 && cutAll > cutOne && /hidden: not simulated/.test(note) && playOps >= 1 && playOps < allOps && (await js("window.__marcam.calls.planAndPost || 0")) === plansS,
      `cut ${cutOne} -> ${cutAll}; playback ${playOps} of ${allOps} operations; ${note}`);
    await shot("18c_simulation_hidden");
    // (Mar) hiding operations never hides the tool nor changes a colour in the playback: the
    // first hidden, the last, the one playing, all but one; the tool drawn where the path is
    // at each scrub position, each played operation in its tree colour; nothing planned
    {
      const plansH = await js("window.__marcam.calls.planAndPost || 0");
      const ids = await js("window.__marcam.state.tree.doc.setups[0].operations.map((o) => o.id)");
      const treeColor = await js("Object.fromEntries([...document.querySelectorAll('#tree .tree-op')].map((r) => [r.dataset.op, r.querySelector('input[data-act=\"color\"]').value]))");
      const setHide = (list) => js(`window.__marcam.setHidden(${JSON.stringify(list.map((id) => `${"vise1"}/${id}`))}); true`);
      const results = [];
      for (const [label, hide] of [["the first", [ids[0]]], ["the last", [ids[ids.length - 1]]], ["all but one", ids.slice(1)], ["the playing one", null]]) {
        await setHide(hide || []);
        await sleep(400);
        await js("document.getElementById('simOpen').click(); true");
        await waitFor(() => js(PLAYBACK_SETTLED), `the playback (${label} hidden)`, 120000);
        if (!hide) {
          // the op playing at the middle: hidden now, with the playback open (it reopens)
          const mid = await js("(() => { const m = window.__marcam.state.machining; const k = Math.floor(m.session.segments / 2); const tag = m.session.path[7 * k + 6]; return m.session.ops.find((o) => o.tag === tag)?.project_id; })()");
          await js(`await window.__marcam.machining.seek(Math.floor(window.__marcam.state.machining.session.segments / 2))`);
          await setHide([mid]);
          await sleep(1500);
          await waitFor(() => js(PLAYBACK_SETTLED), "the playback reopened", 120000);
        }
        const r = await js(`await (async () => { const m = window.__marcam.state.machining, mc = window.__marcam.machining, v = window.__marcam.state.three; const n = m.session.segments; const out = [];
          for (const f of [0.1, 0.5, 0.9]) { const k = Math.max(1, Math.floor(n * f)); await mc.seek(k); await new Promise((r) => setTimeout(r, 120));
            const p = m.session.path; const want = [p[7 * (k - 1)], p[7 * (k - 1) + 1], p[7 * (k - 1) + 2]]; const at = v.groups.toolModel.position;
            out.push({ visible: v.groups.toolModel.visible && v.groups.toolModel.children.length > 0, near: Math.hypot(at.x - want[0], at.y - want[1], at.z - want[2]) < 1e-3 }); }
          const colours = m.session.ops.map((o) => [o.project_id, [...document.querySelectorAll('#opLegend input[type=color]')].find((e) => Number(e.dataset.tag) === o.tag)?.value]);
          return { out, colours, ops: m.session.ops.length }; })()`);
        const colourOk = r.colours.every(([id, c]) => c && treeColor[id] && c.toLowerCase() === treeColor[id].toLowerCase());
        results.push({ label, tool: r.out.every((x) => x.visible && x.near), colourOk, ops: r.ops });
        await js("document.getElementById('simClose').click(); true");
        await sleep(400);
      }
      await setHide([]);
      check("hiding the first, the last, the playing or all but one operation: the tool is always drawn where the path is, every played operation keeps its colour; nothing planned",
        results.every((x) => x.tool && x.colourOk && x.ops >= 1) && (await js("window.__marcam.calls.planAndPost || 0")) === plansH, JSON.stringify(results));
    }
  }

  // 8o. (Mar's oval part) a block with two recessed oval flats and a textured grip between:
  // facing picked on one oval stays on it, a pocket picked on the other stays on it, a 2D
  // contour picked on an outer wall cuts the part out; simulated, the grip is untouched
  {
    const fx = join(root, "tests", "fixtures", "picking");
    const proj = JSON.parse(readFileSync(join(fx, "oval_grip.project.json"), "utf8"));
    const s0 = proj.setups[0];
    s0.operations = [
      { id: "op1", type: "face", tool: 2, depth: 0.1, step_down: 0.5, stepover: 0.6, feed: 800, plunge_feed: 200 },
      { id: "op2", type: "pocket", tool: 2, rect: [1, 1, 5, 5], depth: 0.1, step_down: 0.5, stepover: 0.5, feed: 800, plunge_feed: 200 },
      { id: "op3", type: "contour", tool: 2, path: [[1, 1], [5, 1], [5, 5]], side: "outside", depth: 0.1, step_down: 1.5, feed: 800, plunge_feed: 200 },
    ];
    s0.outputs = ["face", "pocket", "cutout"].map((n, k) => ({ name: `grip_${n}`, title: n, operations: [`op${k + 1}`] }));
    const file = JSON.stringify({ format: "marcam-project", version: 1, project: proj, meshes: { "oval_grip.stl": readFileSync(join(fx, "oval_grip.stl")).toString("base64") } });
    await js(`await window.__marcam.project.openText(${JSON.stringify(file)}, "oval_grip.marcam.json").then(() => true)`);
    await settle();
    const screenOf = (pred) => js(`(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const pk = t.pick; const target = pk.list.find(${pred}); if (!target) return null;
      const want = new Set(target.faces); const el = v.renderer.domElement; const tri = t.regions.tri; const ray = new v.THREE.Raycaster();
      for (let k = 0; k < t.regions.of.length; k++) { if (!want.has(t.regions.list[t.regions.of[k]].id)) continue;
        const c = new v.THREE.Vector3(...[0, 1, 2].map((d) => (tri[9*k+d] + tri[9*k+3+d] + tri[9*k+6+d]) / 3)).project(v.camera);
        if (Math.abs(c.x) > 0.95 || Math.abs(c.y) > 0.95) continue;
        ray.setFromCamera(new v.THREE.Vector2(c.x, c.y), v.camera); const hit = ray.intersectObject(t.pickMesh)[0];
        if (hit && want.has(t.regions.list[t.regions.of[hit.faceIndex]].id)) return [(c.x + 1) / 2 * el.clientWidth, (1 - c.y) / 2 * el.clientHeight]; }
      return null; })()`);
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    async function pickFor(id, pred, what) {
      await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="select"]').click(); true`);
      await waitFor(() => js(`window.__marcam.state.tree.selected === '${id}' && !!window.__marcam.state.tree.pickMesh`), `${what}: its editor`, 60000);
      // (live picking: on while the operation is selected; no button)
      await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), `${what}: picking`, 30000);
      await sleep(300);
      const at = await screenOf(pred);
      if (!at) throw new Error(`${what}: the feature is not visible`);
      const got = await js(`const pr = await window.__marcam.tree.pickAt(${at[0]}, ${at[1]}); pr && pr.pickable ? pr.pickable.kind : null`);
      await shot(`19_grip_pick_${what}`);
      await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
      await waitFor(() => js("!window.__marcam.state.tree.pickMesh"), `${what}: done`, 60000);
      await sleep(300);
      return { op: (await doc()).setups[0].operations.find((o) => o.id === id), got };
    }
    const face = await pickFor("op1", "(x) => x.kind === 'flat_face' && x.data.z < -1.4 && x.data.cx < 30", "facing");
    const pocket = await pickFor("op2", "(x) => x.kind === 'pocket' && x.data.outline[0][0] > 30", "pocket");
    const cut = await pickFor("op3", "(x) => x.kind === 'outline'", "cutout");
    check("the grip block: facing takes an oval flat (bounded by it, to its depth), the pocket the other oval, the contour the part's outline (outside, to just below the part)",
      face.got === "flat_face" && face.op.paths?.length >= 1 && Math.abs(face.op.depth - 2) < 1e-6 &&
      pocket.got === "pocket" && pocket.op.paths?.length >= 1 && !pocket.op.rect &&
      cut.got === "outline" && cut.op.side === "outside" && Math.abs(cut.op.depth - 8.7) < 1e-6 && cut.op.path.length >= 4,
      JSON.stringify({ face: [face.got, face.op.depth], pocket: [pocket.got, pocket.op.depth], cut: [cut.got, cut.op.depth] }));
    await clickGenerate();
    await waitDone("the grip block's three programs", 240000);
    await js("document.getElementById('simulate').click(); true");
    await waitFor(() => js("!!window.__marcam.state.sim && document.body.dataset.state === 'done'"), "the grip block simulated", 240000);
    await shot("19_grip_simulated");
    // the simulated stock after each program: where it was cut
    const sim = await js(`(() => { const s = window.__marcam.state.sim, g = s.grid, top = s.stock_box.max[2];
      const R = 3.175 / 2, ovals = [[16, 15], [44, 15]], ax = 10 + R, ay = 7 + R;
      const inOval = (x, y, k) => Math.abs(x - ovals[k][0]) <= ax && Math.abs(y - ovals[k][1]) <= ay;
      return s.stocks.map((st, p) => { let stray = 0, cut = 0, grip = 0, ring = 0, ringCells = 0;
        for (let j = 0; j < g.ny; j++) for (let i = 0; i < g.nx; i++) {
          const x = g.x0 + (i + 0.5) * g.cell, y = g.y0 + (j + 0.5) * g.cell, z = st.z[j * g.nx + i];
          const inside = x > 0 && x < 60 && y > 0 && y < 30;
          if (z < top - 0.01) { cut++; if (inside && !inOval(x, y, 0) && !(p >= 1 && inOval(x, y, 1))) stray++; }
          // the grip between the ovals
          if (x > 28 && x < 32 && y > 5 && y < 25 && z < top - 0.01) grip++;
          // a ring just outside the part
          const ox = Math.max(-x, x - 60, 0), oy = Math.max(-y, y - 30, 0), d = Math.hypot(ox, oy);
          if (!inside && d > 0.4 && d < 1.2) { ringCells++; if (z <= -8.19) ring++; }
        }
        return { cut, stray, grip, ring, ringCells, gouge: st.max_gouge }; }); })()`);
    check("simulated: facing cuts only inside its oval, the pocket only inside the other, the grip between them untouched, no gouges",
      sim[0].cut > 100 && sim[0].stray === 0 && sim[1].cut > sim[0].cut && sim[1].stray === 0 && sim.every((s) => s.grip === 0 && s.gouge < 0.05),
      JSON.stringify(sim));
    check("simulated: the cut-out goes all round the part, to just below its bottom (the part separated)",
      sim[2].ringCells > 100 && sim[2].ring >= sim[2].ringCells * 0.98, `${sim[2].ring}/${sim[2].ringCells}`);

    // 8p. (Mar) Replace model: the same project, the part replaced by one without the right
    // oval. Facing (left oval) and the cut-out find their picks again; the pocket's oval is
    // gone, so the pocket is in error: marked, named, not planned. Undo brings the old part
    // back; saved and reopened the error stays; picked again, it plans.
    const picksKept = (await doc()).setups[0].operations.every((o) => o.picks?.length === 1);
    const replaceWith = async (name) => {
      const b64 = readFileSync(join(fx, name)).toString("base64");
      await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${b64}"), (c) => c.charCodeAt(0))], "${name}")); const i = document.getElementById('replaceModelFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
      await sleep(500);
      await waitFor(() => js("document.body.dataset.state !== 'busy'"), `replaced by ${name}`, 240000);
      await settle();
    };
    await replaceWith("oval_grip_one_oval.stl");
    const r1 = await js(`(() => { const d = JSON.parse(document.getElementById('job').value), ops = d.setups[0].operations;
      return { file: d.models[0].file, errs: ops.map((o) => o.error || ''), warn: [...document.querySelectorAll('#tree .tree-op')].map((r) => !!r.querySelector('.op-error')),
        programs: window.__marcam.state.result?.programs.map((p) => p.name) || [], msg: document.getElementById('error').textContent, undo: !document.getElementById('undoBar').classList.contains('hidden') }; })()`);
    check("Replace model: picks were kept as signatures; on a part without the right oval the pocket is in error (marked, named), facing and the cut-out are found again",
      picksKept && r1.file === "oval_grip_one_oval.stl" && !r1.errs[0] && /Pocket .*not on the new model/.test(r1.errs[1]) && !r1.errs[2] &&
      JSON.stringify(r1.warn) === "[false,true,false]" && /op2/.test(r1.msg) && r1.undo, JSON.stringify(r1));
    check("the operation in error is left out of the G-code; the others are planned on the new part",
      JSON.stringify(r1.programs) === JSON.stringify(["grip_face", "grip_cutout"]), r1.programs.join(","));
    await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
    await sleep(300);
    const edMsg = await js("document.getElementById('opError')?.textContent || ''");
    await shot("19_replace_model_error");
    // undo: the old part and the pocket as it was
    await js("document.getElementById('undoSetup').click(); true");
    await settle(800);
    const r2 = await js("(() => { const d = JSON.parse(document.getElementById('job').value); return { file: d.models[0].file, errs: d.setups[0].operations.filter((o) => o.error).length, warn: document.querySelectorAll('#tree .op-error').length }; })()");
    check("the editor says why it is not planned; Undo brings the old part back, no operation in error",
      /Not planned: Pocket/.test(edMsg) && r2.file === "oval_grip.stl" && r2.errs === 0 && r2.warn === 0, `${edMsg} | ${JSON.stringify(r2)}`);
    // again, then saved and reopened: still in error
    await replaceWith("oval_grip_one_oval.stl");
    const savedReplaced = await js(`JSON.stringify((() => { const st = window.__marcam.state; let bin = ''; for (let i = 0; i < st.mesh.length; i += 8192) bin += String.fromCharCode(...st.mesh.subarray(i, i + 8192)); return { format: 'marcam-project', version: 1, project: JSON.parse(document.getElementById('job').value), meshes: { 'oval_grip_one_oval.stl': btoa(bin) } }; })())`);
    await js(`await window.__marcam.project.openText(${JSON.stringify(savedReplaced)}, "replaced.marcam.json").then(() => true)`);
    await settle(500);
    const r3 = await js("(() => { const d = JSON.parse(document.getElementById('job').value); return { err: d.setups[0].operations.find((o) => o.id === 'op2').error || '', warn: document.querySelectorAll('#tree .op-error').length }; })()");
    check("saved and reopened, the pocket is still in error", /not on the new model/.test(r3.err) && r3.warn === 1, JSON.stringify(r3));
    // picked again (the left oval, which this part has): the error goes, it plans
    await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
    await waitFor(() => js("window.__marcam.state.tree.selected === 'op2' && !!window.__marcam.state.tree.pickMesh"), "the pocket's editor", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking for the pocket", 30000);
    await sleep(300);
    const at = await screenOf("(x) => x.kind === 'pocket'");
    const repicked = await js(`await window.__marcam.tree.pickAt(${at[0]}, ${at[1]}).then((r) => (r && r.pickable ? r.pickable.id : "nothing")) + " | sel " + [...window.__marcam.state.tree.pick.sel].join(",") + " | " + (document.getElementById('pickNotes')?.textContent || "")`);
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await waitFor(() => js("!window.__marcam.state.tree.pickMesh"), "done picking", 60000);
    await settle(300);
    const cleared = await waitFor(() => js("!JSON.parse(document.getElementById('job').value).setups[0].operations.find((o) => o.id === 'op2').error"), "the pocket's error cleared by the new pick", 10000).then(() => true, () => false);
    if (!cleared) console.log(`  re-pick: ${repicked}; op2 ${await js("JSON.stringify(JSON.parse(document.getElementById('job').value).setups[0].operations.find((o) => o.id === 'op2'))")}`.slice(0, 1500));
    await clickGenerate();
    await waitDone("the repaired project", 240000);
    const r4 = await js("(() => { const d = JSON.parse(document.getElementById('job').value); return { err: d.setups[0].operations.filter((o) => o.error).length, programs: window.__marcam.state.result.programs.map((p) => p.name) }; })()");
    check("picked again, the pocket is planned again", r4.err === 0 && r4.programs.length === 3, JSON.stringify(r4));

    // 8t. (Mar) several picks in one operation, and the Geometry list: the grip block again,
    // one facing picked on both ovals (a click adds, another removes); the list shows them,
    // × removes one, Clear all clears them, Undo brings them back
    await js(`await window.__marcam.project.openText(${JSON.stringify(file)}, "oval_grip2.marcam.json").then(() => true)`);
    await settle();
    if ((await js("window.__marcam.state.tree.selected")) !== "op1") await js(`document.querySelector('#tree .tree-op[data-op="op1"] [data-act="select"]').click(); true`);
    await waitFor(() => js("window.__marcam.state.tree.selected === 'op1' && !!window.__marcam.state.tree.pickMesh"), "the facing's editor (multi)", 60000);
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking for the facing", 30000);
    await sleep(300);
    const flatAt = (left) => screenOf(`(x) => x.kind === 'flat_face' && x.data.z < -1.4 && (x.data.cx < 30) === ${left}`);
    const a1 = await flatAt(true), a2 = await flatAt(false);
    await js(`await window.__marcam.tree.pickAt(${a1[0]}, ${a1[1]}).then(() => true)`);
    await js(`await window.__marcam.tree.pickAt(${a2[0]}, ${a2[1]}).then(() => true)`);
    const chips2 = await js("document.querySelectorAll('#pickChips .pick-chip').length");
    // a click on a picked one removes it, again adds it back
    await js(`await window.__marcam.tree.pickAt(${a2[0]}, ${a2[1]}).then(() => true)`);
    const chips1 = await js("document.querySelectorAll('#pickChips .pick-chip').length");
    await js(`await window.__marcam.tree.pickAt(${a2[0]}, ${a2[1]}).then(() => true)`);
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await waitFor(() => js("!window.__marcam.state.tree.pickMesh"), "done picking (multi)", 60000);
    await settle();
    const f2 = (await doc()).setups[0].operations.find((o) => o.id === "op1");
    const rows = await js("[...document.querySelectorAll('#geometryList .geom-row')].map((r) => r.textContent.replace('×', '').trim())");
    const shown = await js("(() => { const v = window.__marcam.state.three; let n = 0; v.scene.traverse((o) => { if (o.userData?.geom != null) n++; }); return n; })()");
    check("facing takes several flat faces: a click adds one, another removes it; two picked: two regions, each to its depth; the Geometry list shows them and they are drawn on the model",
      chips2 === 2 && chips1 === 1 && f2.picks?.length === 2 && f2.regions?.length === 2 && f2.regions.every((r) => Math.abs(r.depth - 2) < 1e-6) && rows.length === 2 && rows.every((r) => /Flat face at Z-1\.50/.test(r)) && shown === 2,
      `chips ${chips2}/${chips1}; picks ${f2.picks?.length}; regions ${JSON.stringify(f2.regions?.map((r) => r.depth))}; rows ${JSON.stringify(rows)}; drawn ${shown}`);
    await clickGenerate();
    await waitDone("the facing of both ovals", 240000);
    await shot("19_grip_two_ovals");
    // × on the first row: one left (no regions, its own fields); Clear all: none (the whole
    // top again); Undo: back
    await js("document.querySelector('#geometryList [data-ungeom=\"0\"]').click(); true");
    await settle();
    const f1 = (await doc()).setups[0].operations.find((o) => o.id === "op1");
    await js("document.getElementById('geomClear').click(); true");
    await settle();
    const f0 = (await doc()).setups[0].operations.find((o) => o.id === "op1");
    await js("document.getElementById('undoSetup').click(); true");
    await settle();
    const fu = (await doc()).setups[0].operations.find((o) => o.id === "op1");
    check("the Geometry list: × removes one (the other stays, picked alone), Clear all removes them (the whole top again), Undo puts them back",
      f1.picks?.length === 1 && !f1.regions?.length && f1.paths?.length >= 1 && !f0.picks && !f0.paths?.length && !f0.regions?.length && fu.picks?.length === 1,
      `${f1.picks?.length} ${f1.regions?.length} ${f1.paths?.length} | ${JSON.stringify(f0.picks)} ${f0.paths?.length} | ${fu.picks?.length}`);
  }

  // 8s. (Mar) Generate plans only what changed: unchanged operations are reused from the last
  // plan (the same G-code as planning everything), an edit to one plans that one
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the plate", 240000);
    const n = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.length");
    const editFeed = async (k) => {
      await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const o = j.setups[0].operations[${k}]; o.feed = Math.round(o.feed * 0.9); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
      await settle();
      await clickGenerate();
      await waitDone("the plate after an edit", 240000);
      return js("window.__marcam.state.result.plan_cache");
    };
    const c1 = await editFeed(n - 1);
    const c2 = await editFeed(1);
    // the same as planning from scratch
    const same = await js(`await (async () => { const m = window.__marcam, st = m.state, a = m.planArgs(); const job = JSON.parse(document.getElementById('job').value);
      const { result } = await m.engine('planAndPost', job, a.machine, st.mesh, { max_points: 150000, libraries: a.libraries, setup: st.tree.setup, cache: false });
      return result.programs.map((p) => p.gcode).join('|') === st.result.programs.map((p) => p.gcode).join('|'); })()`);
    const status = await js("document.getElementById('status').textContent");
    check("Generate plans only the edited operation (the others reused), with the same G-code as planning them all",
      c1.planned === 1 && c1.reused === n - 1 && c2.planned === 1 && c2.reused === n - 1 && same && /planned 1 operation, \d+ unchanged/.test(status),
      `${JSON.stringify(c1)} ${JSON.stringify(c2)} same ${same} | ${status}`);
  }

  // 8q. (Mar) the view cube: each face turns the camera to look from that side (machine
  // directions), home goes back to the iso view; view only, nothing planned
  for (const ex of ["plate", "knob"]) {
    await js(`document.getElementById('example').value = '${ex}'; document.getElementById('loadExample').click(); true`);
    await settle();
    const p0 = await js("window.__marcam.calls.planAndPost || 0");
    const dirs = {};
    const camDir = () => js("(() => { const v = window.__marcam.state.three; const d = v.camera.position.clone().sub(v.controls.target).normalize(); return [d.x, d.y, d.z].map((c) => Math.round(c * 100) / 100); })()");
    // the camera at rest (its move animated: a slow machine takes longer than a fixed pause)
    const still = async () => {
      let last = "";
      await waitFor(async () => { const d = (await camDir()).join(","); const same = d === last; last = d; await sleep(120); return same; }, "the camera at rest", 60000);
    };
    const clickAt = async (x, y) => {
      await S("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", buttons: 1, clickCount: 1 });
      await S("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", buttons: 0, clickCount: 1 });
      await sleep(300);
      await still();
    };
    for (const name of ["top", "front", "right", "back", "left", "bottom"]) {
      // look from near that side first (its face toward the camera), then click the face
      await js(`(() => { const d = { top: [0.4, -0.5, 1], bottom: [0.4, -0.5, -1], front: [0.4, -1, 0.5], back: [-0.4, 1, 0.5], right: [1, -0.4, 0.5], left: [-1, -0.4, 0.5] }['${name}']; window.__marcam.viewCube.goTo(d, 1); return true; })()`);
      await still();
      const at = await js(`window.__marcam.viewCube.screenOf('${name}')`);
      await clickAt(at[0], at[1]);
      dirs[name] = (await camDir()).join(",");
    }
    const hb = await js("(() => { const b = document.getElementById('viewHome').getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2]; })()");
    await clickAt(hb[0], hb[1]);
    const home = await camDir();
    const want = { top: "0,0,1", front: "0,-1,0", right: "1,0,0", back: "0,1,0", left: "-1,0,0", bottom: "0,0,-1" };
    const ok = Object.entries(want).every(([k, d]) => dirs[k].split(",").map(Number).every((c, i) => Math.abs(c - Number(d.split(",")[i])) < 0.02));
    check(`view cube (${ex}): each face looks from its side (Top +Z, Front -Y, Right +X ...), home back to iso; nothing planned`,
      ok && home[1] < -0.5 && home[2] > 0.3 && (await js("window.__marcam.calls.planAndPost || 0")) === p0, `${JSON.stringify(dirs)} home ${home}`);
    await shot(`20_view_cube_${ex}`);
  }

  // 8r. (Mar) the Tool field lists the project's tools by name, type and size, those the type
  // cannot use greyed with the reason; a new tool replans; a tool removed from the project
  // puts its operations in error (named) and out of the G-code
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    const d0 = await doc();
    const s0 = d0.setups[0];
    const face = s0.operations.find((o) => o.type === "face");
    await js(`document.querySelector('#tree .tree-op[data-op="${face.id}"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('opForm')?.elements.tool"), "the facing's editor", 60000);
    const opts = await js("[...document.getElementById('opForm').elements.tool.options].map((o) => ({ v: o.value, t: o.textContent, off: o.disabled }))");
    const ball = opts.find((o) => /\(ball/.test(o.t));
    const flats = opts.filter((o) => /\(flat/.test(o.t) && !o.off);
    check("the Tool field names each tool (T1 flat 3.175); facing cannot use a ball: greyed with the reason",
      opts.filter((o) => o.v !== "__add").every((o) => /^(T\d+|—) · /.test(o.t)) && opts[opts.length - 1].v === "__add" && (!ball || (ball.off && /takes flat/.test(ball.t))) && flats.length >= 1, JSON.stringify(opts));
    // another flat tool (a copy of T1 with another number, if the project has one flat only)
    let other = flats.find((o) => Number(o.v) !== face.tool);
    if (!other) {
      const t1 = d0.tools.find((t) => t.number === face.tool);
      const n = Math.max(...d0.tools.map((t) => t.number)) + 1;
      await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); j.tools.push({ ...${JSON.stringify(t1)}, number: ${n}, description: "copy" }); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); t.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
      await settle();
      if (!(await js("!!document.getElementById('opForm')"))) await js(`document.querySelector('#tree .tree-op[data-op="${face.id}"] [data-act="select"]').click(); true`);
      await waitFor(() => js("!!document.getElementById('opForm')?.elements.tool"), "the facing's editor again", 60000);
      other = { v: String(n) };
    }
    await js(`(() => { const e = document.getElementById('opForm').elements.tool; e.value = '${other.v}'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await sleep(800);
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(async () => (await doc()).setups[0].operations.find((o) => o.id === face.id).tool === Number(other.v), "the new tool applied", 60000);
    await clickGenerate();
    await waitDone("the plate with the new tool", 240000);
    const g = await js(`window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')`);
    check("a new tool for an operation: applied and planned with it", new RegExp(`T${other.v}\\b`).test(g), `T${other.v}`);
    // the tool removed from the project: the operation in error, named, not planned
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); j.tools = j.tools.filter((x) => x.number !== ${other.v}); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); t.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await settle();
    const row = await js(`(() => { const r = document.querySelector('#tree .tree-op[data-op="${face.id}"] .op-error'); return r ? r.title : ''; })()`);
    await clickGenerate();
    await waitDone("the plate without that tool", 240000);
    const g2 = await js(`window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')`);
    check("its tool removed from the project: the operation is marked in error (naming the tool) and left out of the G-code",
      new RegExp(`tool T${other.v} is not in the project`).test(row) && !new RegExp(`T${other.v}\\b`).test(g2), row);

    // (Mar) "Add tool from library...": the last entry of the Tool field opens the library;
    // the tool picked joins the project without a T number and becomes the operation's; the
    // G-code then uses the next free number and warns; given a number (Tools > Tool numbers),
    // the warning goes
    if ((await js("window.__marcam.state.tree.selected")) !== face.id) await js(`document.querySelector('#tree .tree-op[data-op="${face.id}"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('opForm')?.elements.tool"), "the facing's editor (3)", 60000);
    await js("(() => { const e = document.getElementById('opForm').elements.tool; e.value = '__add'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await waitFor(() => js("document.getElementById('toolPick').open"), "the tool library", 10000);
    const offered = await js("document.querySelectorAll('#tpList .tp-row').length");
    const kindFilter = await js("document.getElementById('tpKind').value");
    // a flat tool (the filter starts on the operation's tool type), not the one the project
    // has already as T1 (that would be one tool under two numbers, warned about)
    await js("[...document.querySelectorAll('#tpList .tp-row')].find((r) => !/Spiral O Metal 3\.175 x 12/.test(r.textContent)).click(); true");
    await waitFor(() => js("!document.getElementById('toolPick').open"), "the library closed", 10000);
    await settle();
    await waitFor(() => js("!!document.getElementById('opForm')"), "the editor after the pick", 10000);
    const picked = await js("(() => { const d = JSON.parse(document.getElementById('job').value); const sel = document.getElementById('opForm').elements.tool; return { n: Number(sel.value), label: sel.options[sel.selectedIndex].textContent, unindexed: d.unindexed || [], tools: d.tools.length }; })()");
    await js("document.getElementById('opForm').requestSubmit(); true");
    await settle();
    await clickGenerate();
    await waitDone("the plate with a library tool", 240000);
    const warn = await js("document.getElementById('toolWarnings')?.textContent || ''");
    const g3 = await js(`window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')`);
    check("Add tool from library: the library opens on the operation's type, the tool joins the project as the next free number (T<n> in the list) and is the operation's; no warning about its number",
      offered > 0 && kindFilter === "flat" && !picked.unindexed.includes(picked.n) && new RegExp(`^T${picked.n} · `).test(picked.label) && !/has no tool number in the project/.test(warn) && !/\(Warning: .* has no tool number/.test(g3),
      `${offered} offered (${kindFilter}); ${JSON.stringify(picked)}; ${warn}`);
    // numbered: no warning
    await js("document.getElementById('menuToolNumbers').click(); true");
    await waitFor(() => js("document.getElementById('toolNumbers').open"), "the tool numbers", 10000);
    await js("(() => { for (const c of document.querySelectorAll('#toolNumbers .tn-has')) c.checked = true; document.getElementById('tnApply').click(); return true; })()");
    await settle();
    await clickGenerate();
    await waitDone("the plate with every tool numbered", 240000);
    const warn2 = await js("document.getElementById('toolWarnings')?.textContent || ''");
    const un2 = await js("JSON.parse(document.getElementById('job').value).unindexed || []");
    check("Tool numbers: every tool given its number, the warning goes", !warn2 && un2.length === 0, `${warn2} | ${JSON.stringify(un2)}`);
  }

  // 8s. (Mar) cutting data by tool × operation type × stock material: a row added in the
  // tool library's form is kept in the library; giving the setup that material (Edit setup)
  // updates the values not typed over and keeps the typed ones; a new
  // operation takes the row's values
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    const lib = () => js("JSON.parse(localStorage.getItem('marcam.libraries')).find((l) => l.id === 'my-tools').tools.find((t) => t.id === 'spiral-o-metal-3.175x12')");
    await js("document.querySelector('#tree [data-act=library]')?.click(); document.getElementById('libraryPanel').open = true; const l = document.getElementById('library'); l.value = 'my-tools'; l.dispatchEvent(new Event('change')); true");
    const ti = await js("JSON.parse(localStorage.getItem('marcam.libraries')).find((l) => l.id === 'my-tools').tools.findIndex((t) => t.id === 'spiral-o-metal-3.175x12')");
    await js(`document.querySelector('#toolTable button[data-edit="${ti}"]').click(); true`);
    await waitFor(() => js("document.querySelectorAll('#tfCutTable .cut-op option').length > 5"), "the operation types in the cutting data", 60000);
    const rows0 = await js("[...document.querySelectorAll('#tfCutTable tr.cut-row')].map((r) => r.querySelector('.cut-material').value + ' ' + (r.querySelector('.cut-op').value || 'any') + ' ' + r.querySelector('.cut-feed').value)");
    await js(`document.getElementById('tfCutAdd').click(); const r = [...document.querySelectorAll('#tfCutTable tr.cut-row')].pop();
      r.querySelector('.cut-material').value = 'aluminium'; r.querySelector('.cut-op').value = 'pocket'; r.querySelector('.cut-feed').value = '450';
      r.querySelector('.cut-plunge').value = '150'; r.querySelector('.cut-stepdown').value = '0.4'; true`);
    await js("document.getElementById('tfCutTable').scrollIntoView(); true");
    await shot("21a_cutting_data");
    await js("document.getElementById('tfSave').click(); true");
    const saved = (await lib()).presets;
    check("the tool library's form edits a tool's cutting data by material and operation type, kept in the library",
      JSON.stringify(rows0) === JSON.stringify(["aluminium any 500"]) && saved.length === 2 && saved[1].material === "aluminium" && saved[1].op === "pocket" && saved[1].feed === 450 && saved[1].plunge === 150 && saved[1].stepdown === 0.4,
      `${rows0.join(" | ")} -> ${JSON.stringify(saved)}`);
    // the project's T1 from "My tools"; two pockets with the computed values (no material),
    // the second one's feed typed over
    await js(`await (async () => { const d = JSON.parse(document.getElementById('job').value); d.tools.find((t) => t.number === 1).library = 'my-tools';
      const s = d.setups[0];
      const { result } = await window.__marcam.engine('opDefaults', d, { type: 'pocket', setup: s.id, tool: 1 });
      s.operations.push({ ...result, id: 'auto' }, { ...result, id: 'typed', feed: 777 });
      const j = document.getElementById('job'); j.value = JSON.stringify(d, null, 2); j.dispatchEvent(new Event('input')); return true; })()`);
    await settle();
    const before = await js("(() => { const o = JSON.parse(document.getElementById('job').value).setups[0].operations.find((o) => o.id === 'auto'); return { feed: o.feed, step_down: o.step_down }; })()");
    const plans0 = await js("window.__marcam.calls.planAndPost || 0");
    const listed = await js("[...document.querySelectorAll('#sdMaterials option')].map((o) => o.value)");
    await inDetails("document.getElementById('sdMaterial').value = 'aluminium'", "JSON.parse(document.getElementById('job').value).setups[0].material === 'aluminium'", "the material in the setup's details");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups[0].material === 'aluminium'"), "the material set", 30000);
    await settle();
    const after = await js("(() => { const d = JSON.parse(document.getElementById('job').value); const g = (id) => d.setups[0].operations.find((o) => o.id === id); return { auto: { feed: g('auto').feed, plunge: g('auto').plunge_feed, step_down: g('auto').step_down }, typed: { feed: g('typed').feed, step_down: g('typed').step_down }, n: d.setups[0].operations.length, status: document.getElementById('status').textContent }; })()");
    const plans1 = await js("window.__marcam.calls.planAndPost || 0");
    check("the setup's material (offered from a list) updates the cutting values not typed over and keeps the typed ones",
      listed.includes("aluminium") && listed.includes("hardwood") && before.feed === 300 && after.auto.feed === 450 && after.auto.plunge === 150 && after.auto.step_down === 0.4 &&
      after.typed.feed === 777 && after.typed.step_down === 0.4 && after.n === 8 && /updated/.test(after.status),
      `${JSON.stringify(before)} -> ${JSON.stringify(after)}; plans ${plans1 - plans0}`);
    // a new pocket with T1 takes the row; a face (no row for it) the tool's aluminium data
    const fresh = await js(`await (async () => { const d = JSON.parse(document.getElementById('job').value); const s = d.setups[0];
      const p = (await window.__marcam.engine('opDefaults', d, { type: 'pocket', setup: s.id, tool: 1 })).result;
      const f = (await window.__marcam.engine('opDefaults', d, { type: 'face', setup: s.id, tool: 1 })).result;
      return { pocket: p.feed, depth: p.step_down, face: f.feed }; })()`);
    // the setup's details show the material
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await waitFor(() => js("!!document.querySelector('#setupHost #setupDialog[open]')"), "the setup's form in its details", 60000);
    const shown = await js("document.getElementById('sdMaterial').value");
    check("a new operation takes the tool's data for its type in the setup's material, else the tool's for the material; the setup's details show the material",
      fresh.pocket === 450 && fresh.depth === 0.4 && fresh.face === 500 && shown === "aluminium", `${JSON.stringify(fresh)} ${shown}`);
  }

  // 8s1. the feeds and speeds calculator: a cutting-data row calculated in the tool library's
  // form (within the Carvera's spindle range), an operation's feeds from its tool, stepover
  // and depth in the setup's material (aluminium, set above), faster for a thin adaptive cut
  {
    const ti = await js("JSON.parse(localStorage.getItem('marcam.libraries')).find((l) => l.id === 'my-tools').tools.findIndex((t) => t.id === 'spiral-o-metal-3.175x12')");
    await js(`document.querySelector('#tree [data-act=library]')?.click(); document.getElementById('libraryPanel').open = true; document.querySelector('#toolTable button[data-edit="${ti}"]').click(); true`);
    await waitFor(() => js("document.querySelectorAll('#tfCutTable .cut-op option').length > 5"), "the tool form", 60000);
    await js(`document.getElementById('tfCutAdd').click(); const r = [...document.querySelectorAll('#tfCutTable tr.cut-row')].pop();
      r.querySelector('.cut-material').value = 'hardwood'; r.querySelector('.cut-op').value = 'contour'; r.querySelector('.cut-calc').click(); true`);
    await waitFor(() => js("[...document.querySelectorAll('#tfCutTable tr.cut-row')].pop().querySelector('.cut-feed').value !== ''"), "the calculated row", 30000);
    const row = await js("(() => { const r = [...document.querySelectorAll('#tfCutTable tr.cut-row')].pop(); const v = (k) => +r.querySelector('.cut-' + k).value; return { rpm: v('rpm'), feed: v('feed'), plunge: v('plunge'), depth: v('stepdown'), notes: document.getElementById('tfCalcNotes').textContent }; })()");
    await js("document.getElementById('tfCancel').click(); true");
    check("Calc in the tool library's form fills a cutting-data row: speed within the spindle's range, feed from the chip load, a slot's depth",
      row.rpm > 0 && row.rpm <= 15000 && row.feed > 500 && row.feed <= 3000 && row.plunge > 0 && row.plunge < row.feed && Math.abs(row.depth - 3.18) < 0.02 && /rpm/.test(row.notes),
      JSON.stringify(row));
    // the plate's pocket op2, in aluminium: its feeds, then adaptive at a 10% stepover
    await js(`document.querySelector('#tree .tree-op[data-op="op2"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('opCalcFeeds')"), "the pocket's form", 60000);
    const feedOf = async () => {
      await js("document.getElementById('opFeedsNotes').textContent = ''; document.getElementById('opCalcFeeds').click(); true");
      await waitFor(() => js("document.getElementById('opFeedsNotes').textContent !== ''"), "the calculated feeds", 30000);
      return js("({ feed: +document.getElementById('opForm').elements.feed.value, plunge: +document.getElementById('opForm').elements.plunge_feed.value, notes: document.getElementById('opFeedsNotes').textContent })");
    };
    const side = await feedOf();
    await js("const f = document.getElementById('opForm'); f.elements.adaptive.checked = true; f.elements.stepover.value = '0.1'; true");
    const thin = await feedOf();
    check("Calculate feeds in an operation's form: from its tool, stepover and depth in the setup's material at the setup's speed; a thin adaptive cut feeds faster (chip thinning)",
      side.feed > 0 && side.plunge > 0 && side.plunge < side.feed && thin.feed > 1.3 * side.feed && /chip thinning/.test(thin.notes),
      `${JSON.stringify(side)} -> ${JSON.stringify(thin)}`);
    await js("document.getElementById('opForm').reset?.(); true");
  }

  // 8s1b. stock carried between setups: the plate's second setup takes what the first left,
  // turned over (about Y); the engine turns the part with it, plans it from that stock
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate's dome mesh", 60000);
    await settle();
    // (Mar: no setup dialogs) + Add setup: 3-axis, continuing; turned over in its Stock
    const offered = await js("(() => { const f = document.getElementById('treeAddFrom'); return !!f && [...f.options].some((o) => o.value === 'carry'); })()");
    await js("(() => { for (const [id, v] of [['treeAddKind', 'vise'], ['treeAddFrom', 'carry']]) { const e = document.getElementById(id); e.value = v; e.dispatchEvent(new Event('change', { bubbles: true })); } document.getElementById('treeAddSetup').click(); return true; })()");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 2"), "the setup added", 60000);
    await settle();
    await js("document.querySelector('#tree .tree-setup.active [data-act=stock]').click(); true");
    await waitFor(() => js("!!document.getElementById('stType')"), "the Stock pane", 30000);
    await js("(() => { const m = document.querySelector('#stockPane details.op-more'); if (m) m.open = true; return true; })()");
    const hidden = await js("({ carry: /Cut result/.test(document.getElementById('stType').selectedOptions[0]?.textContent || ''), dims: !document.getElementById('stX') })");
    await js("(() => { const t = document.getElementById('stTurn'); t.value = 'y180'; t.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await shot("32a_carry_dialog");
    await waitFor(() => js("document.body.dataset.state !== 'busy' && JSON.parse(document.getElementById('job').value).setups[1].stock.turn === 'y180'"), "the carried setup turned over", 60000);
    // (back to the setup's details, as the sections after expect)
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    const d = JSON.parse(await js("document.getElementById('job').value"));
    const s2 = d.setups[1];
    const frame = await js(`await window.__marcam.engine('setupFrame', JSON.parse(document.getElementById('job').value), { setup: '${s2.id}' }).then((r) => r.result)`);
    await waitFor(() => js("window.__marcam.state.three.groups.stock.children.length > 0 && !window.__marcam.state.three.drawing"), "the carried stock drawn", 60000);
    await clickGenerate();
    await waitDone("generation of the carried setup", 240000);
    const v = await js("({ stock: window.__marcam.state.three.groups.stock.children.length, tabs: [...document.querySelectorAll('#tabs button')].map((b) => b.textContent) })");
    await shot("32b_carried_setup");
    check("a setup takes the stock the one before left, turned over: offered in + Setup, the part turned with it, the block where it was, planned",
      offered && hidden.carry && hidden.dims && s2.stock.shape === "from_previous" && s2.stock.turn === "y180" && !s2.stock.rotate && frame.mesh.turn === "y180" &&
      JSON.stringify([frame.stock_box.min, frame.stock_box.max]) === JSON.stringify([d.setups[0].stock.min, d.setups[0].stock.max]) && frame.carry.from === d.setups[0].id && v.stock > 0 && v.tabs.length >= 1 && v.tabs.every((t) => t.endsWith("OK")),
      `${JSON.stringify(s2.stock)}; ${JSON.stringify(frame.stock_box)}; turn ${frame.mesh?.turn}; tabs ${v.tabs}`);
  }

  // 8s1c. the setup's G-code options: work offset, start and end lines, line numbers (Edit
  // setup: nothing replanned, the G-code written again)
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate's dome mesh", 60000);
    await settle();
    await clickGenerate();
    await waitDone("the plate generated", 240000);
    await inDetails(`document.getElementById('sdPostBox').open = true; document.getElementById('sdWork').value = 'G55';
      document.getElementById('sdHeader').value = 'M8'; document.getElementById('sdFooter').value = 'M9'; document.getElementById('sdLineNums').checked = true`, "!!JSON.parse(document.getElementById('job').value).setups[0].post", "the G-code options in the setup's details");
    await shot("33a_gcode_options");
    await waitFor(() => js("!!JSON.parse(document.getElementById('job').value).setups[0].post"), "the options saved", 60000);
    await settle();
    await clickGenerate();
    await waitDone("generation with the options", 240000);
    const g = await js("window.__marcam.state.result.programs[0].gcode");
    const lines = g.trim().split(String.fromCharCode(10));
    const post = JSON.parse(await js("JSON.stringify(JSON.parse(document.getElementById('job').value).setups[0].post)"));
    check("the setup's G-code options: work offset, start and end lines, numbered lines",
      JSON.stringify(post) === JSON.stringify({ work_offset: "G55", header: ["M8"], footer: ["M9"], line_numbers: true }) &&
      lines.every((l, i) => l.startsWith(`N${10 * (i + 1)} `)) && lines.some((l) => / G55$/.test(l)) && lines.some((l) => / M8$/.test(l)) && lines.some((l) => / M9$/.test(l)),
      `${JSON.stringify(post)}; ${lines.slice(0, 6).join(" | ")}`);
  }

  // 8s2. (Mar) parting: a new operation type, placed by a click on the part (snapped to a
  // band's end when near it), typed or defaulted to the part's back; it plans with a time
  {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    await js("document.getElementById('treeAddType').value = 'parting'; document.getElementById('treeAdd').click(); true");
    await waitFor(() => js("!!document.getElementById('opForm')"), "the parting form", 60000);
    const x0 = await js("document.getElementById('opForm').elements.x.value");
    // pick: a click on the taper, mid-way (not near its ends)
    // (live picking: on while the operation is selected; no button)
    await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "pick mode for the parting", 60000);
    await js("window.__marcam.setView('iso', {})");
    const at = await js(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(-7.5, 0, -9.0).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()`);
    const hit = await js(`const pr = await window.__marcam.tree.pickAt(${at[0]}, ${at[1]}); pr ? (pr.pickable?.kind || 'none') + ' at ' + pr.point.x.toFixed(2) : 'nothing'`);
    await sleep(300);
    const hitX = parseFloat(String(hit).split(" at ")[1]);
    const picked = await js("({ x: +document.getElementById('opForm').elements.x.value, notes: [...document.querySelectorAll('#opNotes, .pick-notes')].map((e) => e.textContent).join(' ') })");
    await shot("21c_parting_pick");
    await js("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true"); // done picking: Escape deselects
    await sleep(300);
    // a groove down to 7 mm (the knob's T1 cannot reach the axis), then applied
    await js("const f = document.getElementById('opForm'); f.elements.stop_radius.value = '7'; f.requestSubmit(); true");
    await waitFor(() => js("[...document.querySelectorAll('#tree .tree-op')].some((r) => /Parting/.test(r.textContent))"), "the parting operation", 60000);
    await settle();
    await sleep(500);
    await clickGenerate();
    await waitDone("planning with the parting", 120000);
    const res = await js("(() => { const r = window.__marcam.state.result; const p = r.programs.find((q) => /--- parting at X/.test(q.gcode)); return p ? { name: p.name, head: p.gcode.split('\\n').find((l) => /--- parting at X/.test(l)), ops: p.time?.ops || p.ops || null, issues: p.issues.length, total: r.time_total_s } : null; })()");
    check("parting: a new operation at the part's back by default, placed by a click on the part (in the project at once, no Apply), cut as a groove to a radius, planned with a time",
      Math.abs(+x0 - -21.7) < 1e-6 && /band at -/.test(hit) && Math.abs(picked.x - hitX) < 0.01 && hitX > -14 && hitX < -1.8 && res && new RegExp(`X${picked.x.toFixed(3)}: down to a radius of 7\.000`).test(res.head) && res.issues === 0 && res.total > 0,
      `default ${x0}; ${hit}; picked ${JSON.stringify(picked)}; ${JSON.stringify(res)}`);
  }

  // 8t. (Mar) a long simulation: asked about first with how long it takes (and a quicker
  // setting), a ring in a corner of the 3D view shows it going, a click on it cancels; one
  // simulation at a time; the engine works again after
  {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the knob planned", 120000);
    const sel = (v) => js(`const s = document.getElementById('simRes'); s.value = '${v}'; s.dispatchEvent(new Event('change')); true`);
    // on the CPU (a GPU would be quick, and is not asked about)
    const gpuWas = await js("document.getElementById('simGpu').checked");
    await js("const g = document.getElementById('simGpu'); g.checked = false; g.dispatchEvent(new Event('change')); true");
    await sel("ultra");
    // (the background simulation after the plan finishes first: this one is the person's)
    // (stopped rather than waited for: on a slow machine it takes minutes)
    await stopBackground();
    const nDialogs = cdp.dialogs.length;
    cdp.dialogAnswer = true;
    await js("document.getElementById('simulate').click(); true");
    await waitFor(() => js("!!document.getElementById('simRing') && !document.getElementById('simRing').classList.contains('hidden')"), "the progress ring", 60000);
    const asked = cdp.dialogs.slice(nDialogs).map((d) => d.message).join(" | ");
    await sleep(1500);
    const ring = await js("(() => { const r = document.getElementById('simRing'); const b = r.getBoundingClientRect(), v = document.getElementById('view3d').getBoundingClientRect(); return { label: r.getAttribute('aria-label'), p: +getComputedStyle(r).getPropertyValue('--p'), w: b.width, inView: b.left >= v.left && b.bottom <= v.bottom }; })()");
    await shot("21b_sim_ring");
    // a second simulation meanwhile waits (and the cancel drops it too)
    const plays0 = await js("window.__marcam.calls.simOpen || 0");
    await js("window.__marcam.machining.open(); true");
    await sleep(300);
    const second = await js("({ status: document.getElementById('status').textContent, opens: window.__marcam.calls.simOpen || 0 })");
    // cancel: the ring goes, the status says so, nothing is left busy
    await js("document.getElementById('simRing').click(); true");
    await waitFor(() => js("document.getElementById('simRing').classList.contains('hidden') && !window.__marcam.simLock.busy"), "the cancelled simulation", 60000);
    await sleep(500);
    const after = await js("({ status: document.getElementById('status').textContent, error: document.getElementById('error').textContent, opens: window.__marcam.calls.simOpen || 0, waiting: window.__marcam.simLock.waiting })");
    check("a long simulation is asked about first, with its time and a quicker setting; a ring in the 3D view shows its progress; a second one meanwhile waits; a click on the ring cancels both",
      /Ultra|ultra/.test(asked) && /about \d+ (s|min)/.test(asked) && /(Fine|Normal|Coarse): about/.test(asked) && /click to cancel/.test(ring.label) && ring.p > 0 && ring.w >= 16 && ring.w <= 28 && ring.inView &&
      second.opens === plays0 && /Waiting for the simulation that is running/.test(second.status) && after.opens === plays0 && /cancelled/i.test(after.status) && !after.error,
      `${asked} | ${JSON.stringify(ring)} | ${JSON.stringify(second)} | ${JSON.stringify(after)}`);
    // the engine works again: a coarse simulation runs to the end
    await sel("coarse");
    await js("document.getElementById('simulate').click(); true");
    await waitFor(() => js("/Simulated in/.test(document.getElementById('status').textContent)"), "a simulation after the cancel", 120000);
    // (the tab shown again after it can open its playback, a moment of ring: gone when done)
    await waitFor(() => js("document.getElementById('simRing').classList.contains('hidden')"), "the ring gone", 60000).catch(() => {});
    const ok2 = await js("({ stocks: window.__marcam.state.sim?.stocks?.length || 0, ring: document.getElementById('simRing').classList.contains('hidden') })");
    check("after a cancel the engine works again (a new simulation runs, the ring gone when done)", ok2.stocks > 0 && ok2.ring, JSON.stringify(ok2));
    await sel("normal");
    await js(`const g = document.getElementById('simGpu'); g.checked = ${gpuWas}; g.dispatchEvent(new Event('change')); true`);
  }

  }
  // 8v. (Mar) live updating, no Generate: a project in is planned by itself; an edit shows its
  // operation waiting while the others stay planned, then plans only it; the settled results
  // are what a fresh plan gives; view actions plan nothing; the G-code download waits for the
  // update; the simulated stock is simulated again in the background; the time from an edit
  // to current results on the plate, the knob and the oval part
  {
    const plans = () => js("window.__marcam.calls.planAndPost || 0");
    await js("window.__genClicks = 0; document.getElementById('generate').addEventListener('click', () => window.__genClicks++); true");
    await js("const a = document.getElementById('autoGen'); a.checked = true; a.dispatchEvent(new Event('change')); true");
    const settled = (what) => waitFor(() => js("!!window.__marcam.state.result && window.__marcam.planIdle() && document.body.dataset.state !== 'busy'"), what, 240000);
    const badges = () => js("[...document.querySelectorAll('#tree .op-state')].map((e) => e.dataset.stateFor + ':' + e.dataset.state)");
    // (a 0.1 job, the knob example, has programs instead of setups)
    const editFeed = (k, f) => js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const o = (j.setups ? j.setups[0].operations : j.programs.flatMap((p) => p.operations))[${k}]; o.feed = ${f}; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return o.id; })()`);
    const fresh = () => js(`await (async () => { const m = window.__marcam, st = m.state, a = m.planArgs(); const job = JSON.parse(document.getElementById('job').value);
      const { result } = await m.engine('planAndPost', job, a.machine, st.mesh, { max_points: 150000, libraries: a.libraries, setup: st.tree.setup, cache: false });
      return result.programs.map((p) => p.gcode).join('|') === st.result.programs.map((p) => p.gcode).join('|'); })()`);
    const latency = async (k, f) => {
      const t0 = Date.now();
      await editFeed(k, f);
      await sleep(50);
      await js("await window.__marcam.whenSettled()");
      return Date.now() - t0;
    };

    // in: the plate, planned by itself
    let t0 = Date.now();
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settled("the plate planned by itself");
    const tOpen = Date.now() - t0;
    const b0 = await badges();
    const p0 = await plans();
    const id = await editFeed(1, 333);
    await sleep(120);
    const b1 = await badges();
    const banner = await js("!!document.getElementById('staleBanner') || document.body.classList.contains('stale')");
    await shot("22a_live_pending");
    await js("await window.__marcam.whenSettled()");
    await settled("the plate updated");
    const b2 = await badges();
    const pc = await js("window.__marcam.state.result.plan_cache");
    const planned = (await plans()) - p0;
    const same = await fresh();
    const waitingOk = b1.includes(`${id}:pending`) || b1.includes(`${id}:planning`);
    const othersOk = b1.filter((b) => !b.startsWith(`${id}:`)).every((b) => b.endsWith(":planned"));
    check("live: a project in is planned by itself; an edit shows its operation waiting (no out-of-date marks) while the others stay planned, then plans only it, as a fresh plan would; no Generate",
      (await js("window.__genClicks")) === 0 && b0.length > 0 && b0.every((b) => b.endsWith(":planned")) && waitingOk && othersOk && !banner &&
      b2.every((b) => b.endsWith(":planned")) && pc.planned === 1 && same && planned === 1,
      `open ${tOpen} ms; ${b1.join(" ")} -> ${b2.join(" ")}; cache ${JSON.stringify(pc)}; same ${same}; banner ${banner}; ${planned} plan(s)`);

    // view actions: nothing planned
    const p1 = await plans();
    await js("window.__marcam.setView('front', {})");
    await js("window.__marcam.setView('iso', {})");
    await js("document.querySelector('#tree .tree-op [data-act=\"eye\"]').click(); true");
    await sleep(300);
    await js("document.querySelector('#tree .tree-op [data-act=\"eye\"]').click(); true");
    await js("const c = document.querySelector('#tree .tree-op [data-act=\"color\"]'); c.value = '#123456'; c.dispatchEvent(new Event('change', { bubbles: true })); true");
    await js("document.querySelectorAll('#tabs button')[1]?.click(); document.querySelectorAll('#tabs button')[0].click(); true");
    await sleep(1500);
    check("live: view actions (views, eyes, a colour, tabs) plan nothing", (await plans()) === p1 && (await js("window.__marcam.planIdle()")), `${(await plans()) - p1} plans`);

    // the download waits for the update
    await js("window.__lastBlob = null; window.__createURL = window.__createURL || URL.createObjectURL; URL.createObjectURL = (b) => { window.__lastBlob = b; return window.__createURL.call(URL, b); }; true");
    await js("document.querySelectorAll('#tabs button')[0].click(); true");
    await editFeed(0, 444);
    await js("document.getElementById('download').click(); true");
    await waitFor(() => js("!!window.__lastBlob"), "the G-code download", 120000);
    const dl = await js("await window.__lastBlob.text()");
    const cur = await js("window.__marcam.state.result.programs[0].gcode");
    await js("URL.createObjectURL = window.__createURL; true");
    check("live: Download right after an edit waits for the update: the G-code saved is the edited one", dl === cur && /F444\b/.test(dl), `${dl.length} chars, F444 ${/F444\b/.test(dl)}`);

    // the simulated stock: simulated again in the background, nothing asked
    await js("const s = document.getElementById('simRes'); s.value = 'coarse'; s.dispatchEvent(new Event('change')); true");
    await js("document.getElementById('simulate').click(); true");
    await waitFor(() => js("!!window.__marcam.state.sim && !window.__marcam.simLock.busy"), "the simulated plate", 120000);
    const sims0 = await js("(window.__marcam.calls.simulate || 0) + (window.__marcam.calls.gpuOpen || 0)");
    const dialogs0 = cdp.dialogs.length;
    await editFeed(2, 555);
    await js("await window.__marcam.whenSettled()");
    await waitFor(async () => (await js("(window.__marcam.calls.simulate || 0) + (window.__marcam.calls.gpuOpen || 0)")) > sims0 && !(await js("window.__marcam.simLock.busy")) && (await js("!!window.__marcam.state.sim")), "the background simulation", 120000);
    check("live: with the stock simulated, an update simulates it again in the background (nothing asked)", cdp.dialogs.length === dialogs0, `${(await js("(window.__marcam.calls.simulate || 0) + (window.__marcam.calls.gpuOpen || 0)")) - sims0} simulations (CPU or GPU)`);
    await js("const s = document.getElementById('simRes'); s.value = 'normal'; s.dispatchEvent(new Event('change')); true");

    // latency: an edit to current results
    const lat = { plate: await latency(3, 610) };
    t0 = Date.now();
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settled("the knob planned by itself");
    lat.knobOpen = Date.now() - t0;
    lat.knobKnurl = await latency(1, 190);
    lat.knobTurning = await latency(0, 290);
    const fx = join(root, "tests", "fixtures", "picking");
    const gp = JSON.parse(readFileSync(join(fx, "oval_grip.project.json"), "utf8"));
    // its operations as section 8o gives them (the fixture has none)
    gp.setups[0].operations = [
      { id: "op1", type: "face", tool: 2, depth: 0.1, step_down: 0.5, stepover: 0.6, feed: 800, plunge_feed: 200 },
      { id: "op2", type: "pocket", tool: 2, rect: [1, 1, 5, 5], depth: 0.1, step_down: 0.5, stepover: 0.5, feed: 800, plunge_feed: 200 },
      { id: "op3", type: "contour", tool: 2, path: [[1, 1], [5, 1], [5, 5]], side: "outside", depth: 0.1, step_down: 1.5, feed: 800, plunge_feed: 200 },
    ];
    gp.setups[0].outputs = ["face", "pocket", "cutout"].map((n, k) => ({ name: `grip_${n}`, title: n, operations: [`op${k + 1}`] }));
    const grip = JSON.stringify({ format: "marcam-project", version: 1, project: gp, meshes: { "oval_grip.stl": readFileSync(join(fx, "oval_grip.stl")).toString("base64") } });
    t0 = Date.now();
    await js(`await window.__marcam.project.openText(${JSON.stringify(grip)}, "oval_grip.marcam.json").then(() => true)`);
    const gripOps = await js("JSON.parse(document.getElementById('job').value).setups?.[0]?.operations?.length || 0");
    if (gripOps) {
      await settled("the oval part planned by itself");
      lat.gripOpen = Date.now() - t0;
      lat.gripEdit = await latency(gripOps - 1, 700);
    }
    check("live: time from an edit to current results (ms; the plate, the knob, the oval part)", lat.plate > 0 && lat.knobKnurl > 0 && lat.gripEdit > 0 && (await js("window.__genClicks")) === 0, JSON.stringify(lat));
    await js("const a = document.getElementById('autoGen'); a.checked = false; a.dispatchEvent(new Event('change')); true");
  }

  // 8w. (Mar) one viewer: the playback plays the toolpath with the tool at its place, the
  // stock simulated along or not (Stock), at a speed in machine time (×1 to ×1000, a log
  // slider, remembered) with a jump to the end; Stock switched on mid-way keeps the place
  {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the knob for the playback", 120000);
    await js("const s = document.getElementById('simStock'); s.checked = false; s.dispatchEvent(new Event('change')); true");
    // (the background simulation after the plan stopped first: on a slow machine it takes minutes)
    await stopBackground();
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("window.__marcam.state.machining.open && window.__marcam.state.machining.session?.path_only === true"), "the playback without stock", 60000);
    const noStock = await js(`(() => { const m = window.__marcam.state.machining, v = window.__marcam.state.three; return { stockMesh: !!m.stockMesh, path: !!m.pathLine, part: v.groups.work.children.filter((c) => c.userData.shared).length, tool: v.groups.toolModel.visible, times: m.session.times?.length || 0, segs: m.session.segments, label: document.getElementById('simLabel').textContent }; })()`);
    // ×1000: a second of play is about a thousand seconds of machining
    await js("const s = document.getElementById('simSpeed'); s.value = '3'; s.dispatchEvent(new Event('input')); true");
    const speedLabel = await js("document.getElementById('simSpeedLabel').textContent");
    // (from the start)
    await js("const s = document.getElementById('simScrub'); s.value = '0'; s.dispatchEvent(new Event('input')); true");
    await waitFor(() => js("window.__marcam.state.machining.pos === 0"), "the playback at its start", 10000);
    await js("document.getElementById('simPlay').click(); true");
    await sleep(1500);
    await js("document.getElementById('simPlay').click(); true");
    const played = await js("(() => { const m = window.__marcam.state.machining; return { pos: m.pos, t: m.pos ? m.session.times[m.pos - 1] : 0 }; })()");
    await shot("23a_playback_no_stock");
    await js("document.getElementById('simEnd').click(); true");
    await sleep(200);
    const atEnd = await js("(() => { const m = window.__marcam.state.machining; return { pos: m.pos, segs: m.session.segments, drawn: m.pathLine.geometry.drawRange.count / 2, label: document.getElementById('simLabel').textContent }; })()");
    check("one viewer, no stock: the playback turns the part with its toolpath under the tool, at ×1000 machine time, and jumps to the end",
      !noStock.stockMesh && noStock.path && noStock.part > 0 && noStock.tool && noStock.times === noStock.segs && /no stock/.test(noStock.label) &&
      speedLabel === "×1000" && played.t > 200 && played.t < 3000 && atEnd.pos === atEnd.segs && atEnd.drawn === atEnd.segs && /\d+:\d\d \/ [\d:]+/.test(atEnd.label),
      `${JSON.stringify(noStock)}; ${speedLabel}: ${JSON.stringify(played)} after 1.5 s; end ${JSON.stringify(atEnd)}`);
    // Stock on mid-way: the stock simulated to the same place
    await js("const s = document.getElementById('simScrub'); s.value = String(Math.floor(+s.max / 2)); s.dispatchEvent(new Event('input')); true");
    const mid = await js("window.__marcam.state.machining.pos");
    await js("const s = document.getElementById('simStock'); s.checked = true; s.dispatchEvent(new Event('change')); true");
    await waitFor(() => js("window.__marcam.state.machining.open && !window.__marcam.state.machining.session?.path_only && !!window.__marcam.state.machining.stockMesh"), "the playback with stock", 120000);
    await waitFor(() => js(`window.__marcam.state.machining.pos === ${mid}`), "the same place with the stock", 60000).catch(() => {});
    const withStock = await js("(() => { const m = window.__marcam.state.machining; return { pos: m.pos, cut: [...m.op].some((t) => t > 0), path: !!m.pathLine, label: document.getElementById('simLabel').textContent }; })()");
    const remembered = await js("({ speed: localStorage.getItem('marcam.playSpeed'), stock: JSON.parse(localStorage.getItem('marcam.view') || '{}').stock })");
    await shot("23b_playback_stock");
    check("Stock on mid-way: the stock simulated to the same place, the path still drawn; speed and stock remembered",
      withStock.pos === mid && withStock.cut && withStock.path && !/no stock/.test(withStock.label) && remembered.speed === "3" && remembered.stock === true,
      `mid ${mid}: ${JSON.stringify(withStock)}; ${JSON.stringify(remembered)}`);
    await js("document.getElementById('simClose').click(); true");
    await js("const s = document.getElementById('simSpeed'); s.value = '1'; s.dispatchEvent(new Event('input')); true");
  }

  // (the fast suite, MARCAM_E2E_FAST: these sections are left to the full one)
  if (!FAST) {
  // 8x. (Mar) contour tabs placed by hand: a click on the outline in the 3D view adds a tab
  // there (kept as a fraction of the way round the outline), the toolpath leaves it; it
  // stays put when the direction changes; a click on it removes it
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    const doc = async () => JSON.parse(await js("document.getElementById('job').value"));
    const cid = (await doc()).setups[0].operations.find((o) => o.type === "contour").id;
    await js(`document.querySelector('#tree .tree-op[data-op="${cid}"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('placeTabs')"), "the tab button", 60000);
    await js("document.getElementById('placeTabs').click(); true");
    await js("window.__marcam.setView('iso', {})");
    // the outline's first side, 46 mm along its 184 (a quarter of the way round): (48, 2)
    const at = await js(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(48, 1.5, -0.5).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()`);
    const added = await js(`await window.__marcam.tree.tabAt(${at[0]}, ${at[1]})`);
    await settle();
    await clickGenerate();
    await waitDone("the plate with a tab", 120000);
    const gc = async () => js("window.__marcam.state.result.programs.map((p) => p.gcode).join('\\n')");
    const g1 = await gc();
    const placed = (await doc()).setups[0].operations.find((o) => o.id === cid).tab_at || [];
    await shot("24a_contour_tab");
    // conventional now: the tab where it was
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); j.setups[0].operations.find((o) => o.id === '${cid}').direction = 'conventional'; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await settle();
    await clickGenerate();
    await waitDone("the plate, conventional", 120000);
    const kept = (await doc()).setups[0].operations.find((o) => o.id === cid).tab_at || [];
    const g2 = await gc();
    // the same click again: removed (the contour selected, if it is not still)
    if ((await js("window.__marcam.state.tree.selected")) !== cid) await js(`document.querySelector('#tree .tree-op[data-op="${cid}"] [data-act="select"]').click(); true`);
    await waitFor(() => js("!!document.getElementById('placeTabs')"), "the tab button again", 60000);
    if (!(await js("window.__marcam.state.tree.tabMode"))) await js("document.getElementById('placeTabs').click(); true");
    // (planning again refits the view: the point projected again)
    await js("window.__marcam.setView('iso', {})");
    const at2 = await js(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(48, 1.5, -0.5).project(v.camera); const el = v.renderer.domElement; return [(p.x + 1) / 2 * el.clientWidth, (1 - p.y) / 2 * el.clientHeight]; })()`);
    const removed = await js(`await window.__marcam.tree.tabAt(${at2[0]}, ${at2[1]})`);
    await settle();
    const after = (await doc()).setups[0].operations.find((o) => o.id === cid).tab_at || [];
    check("contour tabs placed by hand: a click on the outline adds one (a quarter of the way round), the toolpath leaves it, it stays put when the direction changes, a click on it removes it",
      placed.length === 1 && Math.abs(placed[0] - 0.25) < 0.02 && /contour \(with tabs\)/.test(g1) && JSON.stringify(kept) === JSON.stringify(placed) && /contour \(with tabs\)/.test(g2) && g1 !== g2 && removed?.removed === true && after.length === 0,
      `${JSON.stringify(added)} -> ${JSON.stringify(placed)}; kept ${JSON.stringify(kept)}; removed ${JSON.stringify(removed)} -> ${JSON.stringify(after)}`);
    await js("window.__marcam.state.tree.tabMode = false; true");
  }

  // 8y. (Mar) the colours menu: every viewer colour, changed in View > Colours, used at once
  // (toolpaths, part, background), kept in the browser, and reset
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the plate for the colours", 120000);
    await js("document.getElementById('menuColors').click(); true");
    await waitFor(() => js("document.getElementById('colorsDialog')?.open"), "the colours dialog", 10000);
    const keys = await js("[...document.querySelectorAll('#colorsDialog input[type=color]')].map((i) => i.dataset.color)");
    const set = (k, v) => js(`const i = document.querySelector('#colorsDialog input[data-color="${k}"]'); i.value = '${v}'; i.dispatchEvent(new Event('input', { bubbles: true })); true`);
    await set("cut", "#00ff00");
    await set("part", "#ff8800");
    await set("background", "#112233");
    await sleep(500);
    const got = await js(`(() => { const v = window.__marcam.state.three; let part = null; v.groups.part.traverse((c) => { if (!part && c.material?.color) part = c.material.color.getHexString(); });
      return { cut: v.cutLines.material.color.getHexString(), part, bg: document.getElementById('view3d').style.background, stored: JSON.parse(localStorage.getItem('marcam.colors') || '{}') }; })()`);
    await shot("25a_colours");
    await js("document.getElementById('colorsReset').click(); true");
    await sleep(500);
    const reset = await js(`(() => { const v = window.__marcam.state.three; return { cut: v.cutLines.material.color.getHexString(), bg: document.getElementById('view3d').style.background, stored: localStorage.getItem('marcam.colors') }; })()`);
    await js("document.getElementById('colorsClose').click(); true");
    check("the colours menu: every viewer colour listed; a change is used at once (cuts, part, background) and kept in the browser; Reset all goes back",
      keys.length >= 9 && got.cut === "00ff00" && got.part === "ff8800" && /17, 34, 51|#112233/.test(got.bg) && got.stored.cut === "#00ff00" &&
      reset.cut === "2563eb" && !reset.bg && (reset.stored === "{}" || reset.stored === null),
      `${keys.join(",")}; ${JSON.stringify(got)}; reset ${JSON.stringify(reset)}`);
  }

  // 8z. (Mar) a project's own cutting data for a library tool (Tools > Tool numbers): kept in
  // the project, picked before the library's for a new operation
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); j.setups[0].material = 'aluminium'; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await settle();
    await js("document.getElementById('menuToolNumbers').click(); true");
    await waitFor(() => js("document.getElementById('toolNumbers')?.open"), "the tool numbers dialog", 10000);
    const i1 = await js("JSON.parse(document.getElementById('job').value).tools.findIndex((t) => t.number === 1)");
    await js(`document.querySelector('.tn-cut[data-i="${i1}"]').click(); document.querySelector('.pc-add[data-i="${i1}"]').click(); true`);
    const lib = await js(`document.querySelector('.tn-cutrow[data-i="${i1}"] .muted').textContent`);
    await js(`(() => { const r = [...document.querySelectorAll('.pc-table[data-i="${i1}"] tr.pc-row')].pop(); r.querySelector('.pc-material').value = 'aluminium'; r.querySelector('.pc-op').value = 'pocket'; r.querySelector('.pc-feed').value = '222'; return true; })()`);
    await shot("26a_project_cutting_data");
    await js("document.getElementById('tnApply').click(); true");
    await settle();
    const t1 = await js("JSON.parse(document.getElementById('job').value).tools.find((t) => t.number === 1)");
    const d = await js(`await (async () => { const j = JSON.parse(document.getElementById('job').value); return (await window.__marcam.engine('opDefaults', j, { type: 'pocket', setup: j.setups[0].id, tool: 1 })).result.feed; })()`);
    const face = await js(`await (async () => { const j = JSON.parse(document.getElementById('job').value); return (await window.__marcam.engine('opDefaults', j, { type: 'face', setup: j.setups[0].id, tool: 1 })).result.feed; })()`);
    check("a project's own cutting data for a library tool: edited in Tool numbers (the library's shown), kept in the project, used before the library's",
      t1.cutting?.length === 1 && t1.cutting[0].op === "pocket" && t1.cutting[0].feed === 222 && /aluminium/.test(lib) && d === 222 && face === 500,
      `${JSON.stringify(t1.cutting)}; library: ${lib}; pocket ${d}, face ${face}`);
  }

  // 8x. rest roughing: a 3D roughing with "Rest roughing" on cuts only what the operations
  // before it left (a smaller tool after a larger one): far less than the same pass over the
  // whole block; the form shows the switch, and turning it off plans the full pass
  {
    const fx = join(root, "tests", "fixtures", "picking");
    const gp = JSON.parse(readFileSync(join(fx, "oval_grip.project.json"), "utf8"));
    const rough = (id, tool, rest) => ({ id, type: "rough3d", tool, x_range: [-5, 65], y_range: [-5, 35], stepover: 1.2, sample: 0.4, step_down: 1.5, floor: -8.5, leave: 0.25, feed: 600, plunge_feed: 100, ...(rest ? { rest: true } : {}) });
    gp.setups[0].operations = [rough("op1", 1, false), rough("op2", 2, true)];
    gp.setups[0].outputs = [{ name: "grip_rough", title: "rough", operations: ["op1", "op2"] }];
    const text = JSON.stringify({ format: "marcam-project", version: 1, project: gp, meshes: { "oval_grip.stl": readFileSync(join(fx, "oval_grip.stl")).toString("base64") } });
    await js(`await window.__marcam.project.openText(${JSON.stringify(text)}, "rest.marcam.json").then(() => true)`);
    await settle();
    await clickGenerate();
    await waitDone("the rest roughing", 240000);
    const opTime = (id) => js(`(() => { for (const p of window.__marcam.state.result.programs) { const o = (p.time?.ops || []).find((x) => x.id === '${id}'); if (o) return o.seconds; } return null; })()`);
    const restS = await opTime("op2");
    // the switch in the form
    await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
    await waitFor(() => js("!!document.getElementById('opForm')?.elements.rest"), "the rest switch", 60000);
    const shown = await js("document.getElementById('opForm').elements.rest.checked");
    await shot("28a_rest_roughing");
    // off: the full pass
    await js("const f = document.getElementById('opForm'); f.elements.rest.checked = false; f.requestSubmit(); true");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups[0].operations[1].rest !== true"), "rest switched off", 60000);
    await settle();
    await clickGenerate();
    await waitDone("the full second roughing", 240000);
    const fullS = await opTime("op2");
    check("rest roughing: a smaller tool after a larger one cuts only what was left; the form's switch turns it off",
      // (the full pass no longer rides its lower levels over the part where it already cut,
      // so it is much quicker than it was; rest roughing still rides them: not slower by half)
      shown === true && restS > 0 && fullS > 0 && restS < 1.5 * fullS,
      `rest ${Math.round(restS)} s, full ${Math.round(fullS)} s; switch shown ${shown}`);
  }

  }
  // 8w2. (Mar: "way too busy") the viewer's toggles: a row of icons in the 3D view's corner
  // (Part, Stock, Toolpaths all / up to the playhead / off, Rapids, Tool, Only selected), each
  // with a key, remembered; view only (nothing planned); calm by default (part and toolpaths,
  // no rapids, no stock, no tool at rest, the playback folded to a play button); the selected
  // operation's toolpath stands out
  {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the knob for the toggles", 120000);
    await stopBackground();
    // the defaults (as a person first sees them)
    await js("localStorage.removeItem('marcam.view'); true");
    await js("document.getElementById('simClose').click(); true");
    await js("(() => { const m = window.__marcam.state.machining; Object.assign(m.view, { part: true, stock: false, paths: 'all', rapids: false, tool: true, only: false }); window.__marcam.machining.renderToggles(); window.__marcam.machining.toggle('part'); window.__marcam.machining.toggle('part'); return true; })()");
    // (stopping the background simulation ended the playback too: open again, at rest)
    await js("window.__marcam.machining.open({ auto: true }).then(() => true)");
    await waitFor(() => js("window.__marcam.state.machining.open && !!window.__marcam.state.machining.paths"), "the still view", 60000);
    await sleep(300);
    const plans0 = await js("window.__marcam.calls.planAndPost || 0");
    const look = `(() => { const m = window.__marcam.state.machining, v = window.__marcam.state.three;
      return { cut: m.paths.cut[0].visible, drawn: m.paths.cut[0].geometry.drawRange.count, rapids: m.paths.rapid[0].visible,
        part: v.groups.work.children.filter((c) => c.userData.shared).some((c) => c.visible), tool: v.groups.toolModel.visible,
        turn: v.groups.work.rotation.x, controls: !document.getElementById('simControls').classList.contains('hidden'),
        play: !document.getElementById('vtPlay').classList.contains('hidden'), segs: m.session.segments }; })()`;
    const calm = await js(look);
    await shot("29a_viewer_calm_knob");
    // (the view's letters: pressed over the view)
    const key = (k) => js(`document.querySelector('#view3d canvas').dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', bubbles: true })); true`);
    const seen = {};
    await key("p"); seen.partOff = (await js(look)).part; await key("p"); seen.partOn = (await js(look)).part;
    await key("r"); seen.rapidsOn = (await js(look)).rapids; await key("r");
    await key("t"); seen.playhead = await js("window.__marcam.state.machining.view.paths");
    await key("t"); seen.pathsOff = (await js(look)).cut; await key("t"); seen.pathsBack = (await js(look)).cut;
    // the selected operation stands out; Only: the others not drawn
    await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
    await sleep(300);
    const dimmed = await js("(() => { const m = window.__marcam.state.machining; const c = m.paths.cut[0].geometry.attributes.color.array, c0 = m.pathBase.col; let d = 0; for (let k = 0; k < c.length; k += 6) if (Math.abs(c[k] - c0[k]) > 1e-3) d++; return d; })()");
    await key("o");
    await sleep(100);
    const folded = await js("(() => { const m = window.__marcam.state.machining; const p = m.paths.cut[0].geometry.attributes.position.array; let f = 0; for (let k = 0; k < p.length; k += 6) if (p[k] === p[k + 3] && p[k + 1] === p[k + 4] && p[k + 2] === p[k + 5]) f++; return f; })()");
    await shot("29b_only_selected");
    await key("o");
    await js("document.querySelector('#tree .tree-op[data-op=\"op2\"] [data-act=\"select\"]').click(); true");
    // the play button: the controls open, the tool shows and the part turns
    await js("document.getElementById('vtPlay').click(); true");
    await sleep(1200);
    const playing = await js(look);
    await js("if (window.__marcam.state.machining.playing) document.getElementById('simPlay').click(); true");
    await shot("29c_playing");
    await js("document.getElementById('simClose').click(); true");
    const folded2 = await js(look);
    // the stock: the background simulation's result at rest
    await key("s");
    await waitFor(() => js("(window.__marcam.state.three.groups.simFinal?.children.length || 0) > 0"), "the simulated stock at rest", 240000);
    const stockAtRest = await js("({ shown: window.__marcam.state.three.groups.simFinal.visible, part: window.__marcam.state.three.groups.work.children.filter((c) => c.userData.shared).some((c) => c.visible) })");
    await shot("29d_stock_at_rest");
    await key("s");
    const saved = await js("localStorage.getItem('marcam.view')");
    const plans = (await js("window.__marcam.calls.planAndPost || 0")) - plans0;
    check("the viewer's toggles: calm by default (part and toolpaths only, no rapids, no tool, still, the playback folded); each toggle shows and hides; Only and the selection; the play button; the stock at rest from the background simulation; remembered; nothing planned",
      calm.cut && calm.drawn === 2 * calm.segs && !calm.rapids && calm.part && !calm.tool && calm.turn === 0 && !calm.controls && calm.play &&
      seen.partOff === false && seen.partOn === true && seen.rapidsOn === true && seen.playhead === "playhead" && seen.pathsOff === false && seen.pathsBack === true &&
      dimmed > 0 && folded > 0 && playing.controls && playing.tool && !playing.play && !folded2.controls && !folded2.tool &&
      stockAtRest.shown && !stockAtRest.part && /"paths":"all"/.test(saved) && plans === 0,
      `${JSON.stringify(calm)} ${JSON.stringify(seen)} dimmed ${dimmed} folded ${folded} playing ${JSON.stringify(playing)} after ${JSON.stringify(folded2)} stock ${JSON.stringify(stockAtRest)} saved ${saved} plans ${plans}`);
  }

  // 8st. (Mar) the stock: every shape (plate/block, round, square, rectangular and hex bars),
  // each with all its sizes (labelled, with units), given or fitted to the part, in mm or
  // inches, from a list of common sizes; written to the project (and its file), drawn
  {
    // (Mar: no setup dialogs) its Stock in the tree: the type, its sizes, Fit to part
    await js("window.__confirm0 = window.confirm; window.confirm = () => true; true");
    const openStock = async () => {
      await js("document.querySelector('#tree .tree-setup.active [data-act=stock]').click(); true");
      await waitFor(() => js("!!document.getElementById('stType')"), "the Stock pane", 30000);
    };
    // (each change applied as a person's is; the pane drawn again after it)
    const set = async (id, v) => {
      await js(`(() => { const e = document.getElementById('${id}'); e.value = '${v}'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
      await sleep(250);
      await settle();
      await openStock();
    };
    const shown = () => js("[...document.querySelectorAll('#stockPane label input')].map((i) => i.id)");
    const stockNow = () => js("JSON.parse(document.getElementById('job').value).setups[0].stock");
    // the bars, on the knob (rotary)
    await js("document.getElementById('example').value = 'knob_indexed'; document.getElementById('loadExample').click(); true");
    await settle();
    const bars = {};
    for (const [shape, fields, want] of [
      ["round", { stDia: 25, stLen: 40 }, { shape: "cylinder", radius: 12.5, length: 40 }],
      ["square", { stSide: 22, stAngle: 0, stLen: 40 }, { shape: "square", side: 22, length: 40, angle: 0 }],
      ["rect", { stW: 30, stH: 22, stAngle: 15, stLen: 40 }, { shape: "rect", width: 30, height: 22, length: 40, angle: 15 }],
      ["hex", { stAf: 24, stAngle: 30, stLen: 40 }, { shape: "hex", across_flats: 24, length: 40, angle: 30 }],
    ]) {
      await openStock();
      await set("stType", shape);
      const fieldsShown = await shown();
      for (const [id, v] of Object.entries(fields)) await set(id, v);
      const units = await js("[...document.querySelectorAll('#stockPane label .u')].map((u) => u.textContent).join(',')");
      await shot(`30_stock_${shape}`);
      const st = await stockNow();
      const drawnR = await js("window.__marcam.state.three.bounds.R");
      bars[shape] = { ok: Object.entries(want).every(([k, v]) => Math.abs((st[k] ?? NaN) - v) < 1e-6 || st[k] === v), st, fieldsShown, units, drawnR };
    }
    const hexR = 24 / Math.sqrt(3);
    // fitted to the part: a hex round the knob (its mesh)
    await js("document.getElementById('example').value = 'knob_finish'; document.getElementById('loadExample').click(); true");
    await settle();
    await openStock();
    await set("stType", "hex");
    await js("(() => { const m = document.getElementById('stMore'); if (m) m.open = true; return true; })()");
    await set("stMargin", 0.5);
    await js("document.getElementById('stFit').click(); true");
    await sleep(250);
    await settle();
    const fitted = await stockNow();
    // round trip through the project file (TOML)
    const toml = await js("await (async () => (await window.__marcam.engine('toToml', JSON.parse(document.getElementById('job').value))).result.text)()").catch((e) => `error: ${e.message}`);
    // the block, on the plate (vise): its sizes
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await openStock();
    const blockOnly = await js("[...document.getElementById('stType').options].filter((o) => !o.value.startsWith('cut:')).map((o) => o.value).join()");
    const blockFields = await shown();
    for (const [id, v] of Object.entries({ stX: 70, stY: 50, stZ: 12 })) await set(id, v);
    await shot("30_stock_block");
    const block = await stockNow();
    // a hex bar lying in the vise along X, 20 across flats, 70 long: the block round it for
    // the view and the engine (Z0 at its top), planned
    await set("stType", "hex");
    const viseBarFields = await shown();
    for (const [id, v] of Object.entries({ stAf: 20, stAngle: 0, stLen: 70 })) await set(id, v);
    await settle();
    const viseBar = { st: await stockNow(), bounds: await js("window.__marcam.state.three.bounds"), problems: await js("window.__marcam.state.tree.problems.join('; ')"), kind: await js("JSON.parse(document.getElementById('job').value).setups[0].kind") };
    await shot("30_stock_vise_hex");
    await js("window.confirm = window.__confirm0; true");
    check("stock (its Stock in the tree): every shape with all its sizes (labelled, with units), given or fitted to the part; written to the project and its file; drawn at the bar's corners",
      Object.values(bars).every((b) => b.ok) && bars.square.fieldsShown.includes("stSide") && bars.rect.fieldsShown.includes("stW") && bars.rect.fieldsShown.includes("stH") &&
      bars.hex.fieldsShown.includes("stAf") && bars.round.fieldsShown.includes("stDia") && /mm/.test(bars.round.units) &&
      Math.abs(bars.hex.drawnR - hexR) < 1e-6 && Math.abs(bars.square.drawnR - 11 * Math.SQRT2) < 1e-6 &&
      fitted.shape === "hex" && fitted.across_flats >= 20.5 && fitted.across_flats <= 22 &&
      /shape = "hex"/.test(toml) && /across_flats/.test(toml) &&
      blockOnly === "block,round,square,hex,rect" && ["stX", "stY", "stZ"].every((id) => blockFields.includes(id)) &&
      block.shape === "box" && block.max[0] - block.min[0] === 70 && block.max[1] - block.min[1] === 50 && block.max[2] - block.min[2] === 12 &&
      viseBar.st.shape === "hex" && viseBar.st.across_flats === 20 && viseBar.kind === "vise" && viseBarFields.includes("stAf") &&Math.abs(viseBar.bounds.xmax - 70) < 1e-6 && !/needs/.test(viseBar.problems),
      `${JSON.stringify(bars)} fitted ${JSON.stringify(fitted)} toml ${String(toml).split(String.fromCharCode(10)).filter((l) => /stock|shape|across/.test(l)).join(" | ").slice(0, 200)} block ${blockOnly} ${blockFields} ${JSON.stringify(block)} vise bar ${JSON.stringify(viseBar)} ${viseBarFields}`);
  }

  // 8st3. (Mar) the vise's jaws: the depth they grip (3 mm or a third of a block by default,
  // or given), drawn beside the stock (with the part or the stock shown), and a warning when a
  // cut reaches them
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    await clickGenerate();
    await waitDone("the plate", 120000);
    const jawsLook = () => js("(() => { const g = window.__marcam.state.three.groups.jaws; return { n: g?.children.length || 0, visible: !!g?.visible, top: g?.children[0] ? +(g.children[0].position.z + g.children[0].children[0].geometry.parameters.depth / 2).toFixed(3) : null }; })()");
    const before = { look: await jawsLook(), warnings: await js("window.__marcam.state.result.programs.flatMap((p) => p.warnings || [])") };
    // (the grip depth: the Fixture's)
    await js("document.querySelector('#tree .tree-setup.active [data-act=fixture]').click(); true");
    await waitFor(() => js("!!document.getElementById('fxGrip')"), "the Fixture pane", 30000);
    const shownJaws = await js("!!document.getElementById('fxGrip')");
    await js("(() => { const e = document.getElementById('fxGrip'); e.value = '8.5'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups[0].holding?.jaws === 8.5"), "the grip depth", 60000);
    await settle();
    await clickGenerate();
    await waitDone("the plate with the jaws higher", 120000);
    await sleep(500);
    const after = { look: await jawsLook(), warnings: await js("window.__marcam.state.result.programs.flatMap((p) => p.warnings || [])"), grip: await js("JSON.parse(document.getElementById('job').value).setups[0].holding.jaws"), shown: await js("document.getElementById('toolWarnings')?.textContent || ''") };
    await shot("31_vise_jaws");
    // hidden with the part and the stock
    // (the view's letters: pressed over the view)
    const key = (k) => js(`document.querySelector('#view3d canvas').dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', bubbles: true })); true`);
    const view0 = await js("JSON.stringify(window.__marcam.state.machining.view)");
    await js("(() => { const v = window.__marcam.state.machining.view; if (v.stock) window.__marcam.machining.toggle('stock'); if (v.part) window.__marcam.machining.toggle('part'); return true; })()");
    await sleep(200);
    const hidden = await jawsLook();
    await js("window.__marcam.machining.toggle('part'); true");
    check("vise jaws: 3 mm of the plate gripped by default, drawn beside it; gripped 8.5 mm the outline's pass past the Y faces is warned about (as it cuts into them); hidden with the part and the stock",
      shownJaws && before.look.n === 2 && Math.abs(before.look.top - -7) < 1e-3 && before.warnings.every((w) => !/jaws/.test(w)) &&
      after.grip === 8.5 && Math.abs(after.look.top - -1.5) < 1e-3 && after.warnings.some((w) => /cuts into the vise jaws/.test(w)) && /cuts into the vise jaws/.test(after.shown) && !hidden.visible,
      `${JSON.stringify(before)} ${JSON.stringify(after)} hidden ${JSON.stringify(hidden)} view ${view0}`);
  }

  // 8st2. (Mar) no form field silently lost: every operation type's form shows each of its
  // registry parameters (but the documented exceptions), and the setup dialog each stock size
  {
    await js("document.getElementById('example').value = 'knob_indexed'; document.getElementById('loadExample').click(); true");
    await settle();
    const gaps = await js(`await (async () => {
      const t = window.__marcam.state.tree, out = [], slow = [];
      const kinds = [...document.getElementById('treeAddType').options].map((o) => o.value);
      for (const kind of kinds) {
        // (the tree draws its Add box again after a cancel: found each time)
        document.getElementById('treeAddType').value = kind;
        document.getElementById('treeAdd').click();
        // (the form complete: its defaults in, the feed last)
        const sel = () => (t.draft?.op.id === t.selected ? t.draft.op : t.doc?.setups.find((x) => x.id === t.setup)?.operations.find((o) => o.id === t.selected));
        // (its defaults come from the engine worker that plans: on a slow runner after the
        // last add's plan; Windows CI took over 10 s, the form before it judged instead)
        const w0 = performance.now();
        // (rotary turning's feeds are its roughing and finishing ones: no "feed"; waited for,
        // it ran out the time and the next type's form was judged too early)
        const ready = () => { const e = document.getElementById('opForm')?.elements; return sel()?.type === kind && e?.__type?.value === kind && !!(e.feed || e.rough_feed); };
        for (let i = 0; i < 3600 && !ready(); i++) await new Promise((r) => setTimeout(r, 50));
        if (performance.now() - w0 > 5000) slow.push(kind + " " + Math.round(performance.now() - w0) + " ms");
        if (sel()?.type !== kind) out.push(kind + ": no form");
        const f = document.getElementById('opForm');
        const ty = t.types.find((x) => x.kind === kind);
        for (const p of ty.params) {
          // (documented: regions are the Geometry list; one of rotary finishing's two stepovers;
          // no index on a vise)
          if (p.kind === "regions" || /^stepover_(deg|mm)$/.test(p.name) || p.name === "index_a") continue;
          if (!f?.elements[p.name]) out.push(kind + "." + p.name);
        }
        // (in the project at once: deleted again)
        const id = t.selected;
        document.querySelector('#tree .tree-op[data-op="' + id + '"] [data-act="delete"]')?.click();
        for (let i = 0; i < 100 && t.doc?.setups.some((x) => x.operations.some((o) => o.id === id)); i++) await new Promise((r) => setTimeout(r, 50));
      }
      return { out, slow };
    })()`);
    if (gaps.slow.length) console.log(`notice operation forms slow to open: ${gaps.slow.join(", ")}`);
    check("every operation form shows every parameter of its type (the registry's)", gaps.out.length === 0, gaps.out.join(", "));
  }

  // 8y. (Mar) undo and redo for every edit: Edit > Undo / Redo and Ctrl+Z / Ctrl+Shift+Z
  // (Ctrl+Y); typing in the JSON is one step; the keys in a field are the field's own
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await settle();
    const text = () => js("document.getElementById('job').value");
    const base = await text();
    const h0 = await js("({ undo: window.__marcam.state.history.undo.length, disabled: document.getElementById('menuUndo').disabled })");
    // 1. typing: two changes in a row, one step
    for (const f of [0.9, 0.8]) {
      await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const o = j.setups[0].operations[0]; o.feed = Math.round(o.feed * ${f}); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
      await sleep(100);
    }
    await settle();
    // 2. an operation switched off, 3. one deleted (in the tree)
    const off = await js("(() => { const r = [...document.querySelectorAll('#tree .tree-op')].find((x) => x.querySelector('[data-act=\"enable\"]')); r.querySelector('[data-act=\"enable\"]').click(); return r.dataset.op; })()");
    await settle();
    cdp.dialogAnswer = true;
    const del = await js("(() => { const r = [...document.querySelectorAll('#tree .tree-op')].reverse().find((x) => x.querySelector('[data-act=\"delete\"]')); r.querySelector('[data-act=\"delete\"]').click(); return r.dataset.op; })()");
    await settle();
    const final = await text();
    const h1 = await js("({ undo: window.__marcam.state.history.undo.length, title: document.getElementById('menuUndo').title, ops: JSON.parse(document.getElementById('job').value).setups[0].operations.length })");
    const key = (k, shift = false, target = "document.body") => js(`${target}.dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', ctrlKey: true, shiftKey: ${shift}, bubbles: true, cancelable: true })); true`);
    // the key in the JSON field: the field's own undo, not the project's
    await key("z", false, "document.getElementById('job')");
    await settle();
    const inField = (await text()) === final;
    const states = [];
    for (let k = 0; k < 3; k++) {
      await key("z");
      await settle();
      states.push(await text());
    }
    const back = { atBase: states[2] === base, distinct: new Set(states).size === 3, disabled: await js("document.getElementById('menuUndo').disabled"), status: await js("document.getElementById('status').textContent") };
    await shot("27a_undone");
    // redo: Ctrl+Shift+Z twice, Edit > Redo once
    await key("z", true);
    await settle();
    await key("y");
    await settle();
    await js("document.getElementById('menuRedo').click(); true");
    await settle();
    const fwd = { atFinal: (await text()) === final, redoDisabled: await js("document.getElementById('menuRedo').disabled") };
    // an edit after an undo: the redo is gone
    await js("document.getElementById('menuUndo').click(); true");
    await settle();
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); j.setups[0].operations[0].feed += 1; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await settle();
    const redoGone = await js("document.getElementById('menuRedo').disabled");
    check("undo and redo: every edit (typing as one step, an operation switched off, one deleted) undone by Ctrl+Z back to the project as opened, redone by Ctrl+Shift+Z, Ctrl+Y and Edit > Redo; not in a field; a new edit drops the redo",
      h0.undo === 0 && h0.disabled && h1.undo === 3 && /Undo/.test(h1.title) && inField && back.atBase && back.distinct && back.disabled && /Undone/.test(back.status) && fwd.atFinal && fwd.redoDisabled && redoGone,
      `${JSON.stringify(h0)} ${JSON.stringify(h1)} off ${off}, deleted ${del}; in field ${inField}; ${JSON.stringify(back)}; ${JSON.stringify(fwd)}; redo gone ${redoGone}`);
  }

  // 8z. (Mar) the setup tree: every setup with its operations under it, the selected item's
  // details below the tree
  {
    const text = readFileSync(join(root, "examples", "knob", "knob_two_setups.marcam.toml"), "utf8");
    await js(`await window.__marcam.project.openText(${JSON.stringify(text)}, "knob_two_setups.marcam.toml").then(() => true)`);
    await settle();
    await waitFor(() => js("document.querySelectorAll('#tree .tree-setup').length === 2"), "both setups in the tree", 30000);
    // (the setup worked on: its details, whatever was selected before the project opened)
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await sleep(400);
    const t0 = await js("(() => { const h = [...document.querySelectorAll('#tree .tree-setup')]; return { names: h.map((x) => x.querySelector('[data-act=setup]').textContent).join('|'), active: document.querySelector('#tree .tree-setup.active').dataset.setup, details: !!document.getElementById('setupSummary'), ops: document.querySelectorAll('#tree .tree-setup.active .tree-op').length }; })()");
    // the second setup's name: it is worked on, its details shown
    await js("document.querySelectorAll('#tree .tree-setup')[1].querySelector('[data-act=setup]').click(); true");
    await settle();
    const t1 = await js("({ active: document.querySelector('#tree .tree-setup.active').dataset.setup, chooser: document.getElementById('treeSetup').value, details: document.getElementById('setupSummary')?.textContent || '', selected: window.__marcam.state.tree.selected })");
    // the first one unfolded: one of its operations selected goes back to it, its form below
    await js("(() => { const b = document.querySelectorAll('#tree .tree-setup')[0].querySelector('[data-act=fold]'); if (b.getAttribute('aria-expanded') !== 'true') b.click(); return true; })()");
    await settle();
    const other = await js("document.querySelectorAll('#tree .tree-setup')[0].querySelectorAll('.tree-op').length");
    await js("document.querySelectorAll('#tree .tree-setup')[0].querySelector('.tree-op [data-act=select]').click(); true");
    await settle();
    const t2 = await js("({ active: document.querySelector('#tree .tree-setup.active').dataset.setup, selected: window.__marcam.state.tree.selected, form: !!document.querySelector('#treeDetails #opForm'), setupDetails: !!document.getElementById('setupSummary') })");
    // folding the setup worked on hides its operations
    await js("document.querySelector('#tree .tree-setup.active [data-act=fold]').click(); true");
    await settle();
    const folded = await js("document.querySelector('#tree .tree-setup.active .tree-setupbody').hidden");
    await js("document.querySelector('#tree .tree-setup.active [data-act=fold]').click(); true");
    await settle();
    check("setup tree: every setup listed with its operations under it; a setup's name selects it and shows its details below; an operation of another setup goes to it with its form below; folding hides a setup's operations",
      t0.names.split("|").length === 2 && t0.details && t0.ops > 0 && t1.active !== t0.active && t1.chooser === t1.active && t1.details.length > 0 && !t1.selected && other > 0 && t2.active === t0.active && !!t2.selected && t2.form && !t2.setupDetails && folded,
      `${JSON.stringify(t0)} ${JSON.stringify(t1)} other ${other} ${JSON.stringify(t2)} folded ${folded}`);
    await shot("32_setup_tree");
  }
  // (Mar) a setup added carries on from the last one by default; a setup can take the stock
  // of any setup before it; a setup's settings are in its details, not a pop-up
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    const embedded = await js("(() => { const d = document.getElementById('setupDialog'); return d.open && d.classList.contains('embedded') && !!d.closest('#treeDetails'); })()");
    const add = async (from) => {
      await addSetup("vise", "carry");
      await js("document.querySelector('#tree .tree-setup.active [data-act=stock]').click(); true");
      await waitFor(() => js("!!document.getElementById('stType')"), "the Stock pane", 30000);
      const dflt = await js("({ carry: JSON.parse(document.getElementById('job').value).setups.at(-1).stock.shape === 'from_previous', from: document.getElementById('stType').value.replace(/^cut:/, '') })");
      if (from) {
        await js(`(() => { const t = document.getElementById('stType'); t.value = 'cut:${from}'; t.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
        await waitFor(() => js(`JSON.parse(document.getElementById('job').value).setups.at(-1).stock.from === '${from}' && document.body.dataset.state !== 'busy'`), "the stock from that setup", 60000);
        await settle();
      }
      return dflt;
    };
    const d2 = await add(null);
    const first = await js("JSON.parse(document.getElementById('job').value).setups[0].id");
    const d3 = await add(first);
    const ss = await js("JSON.parse(document.getElementById('job').value).setups.map((s) => s.stock)");
    check("setups: a setup's settings in its details; one added carries on from the last by default; one may take the stock of any setup before it",
      embedded && d2.carry && d2.from === first && d3.carry && ss.length === 3 && ss[1].shape === "from_previous" && !ss[1].from && ss[2].shape === "from_previous" && ss[2].from === first,
      `embedded ${embedded}; ${JSON.stringify(d2)} ${JSON.stringify(d3)}; ${JSON.stringify(ss)}`);
  }

  // (Mar) the part's orientation in a setup: quarter turns, any angle, laid on a face
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    const bounds = () => js(`await (async () => { const M = window.__marcam, d = JSON.parse(document.getElementById('job').value); const { result } = await M.engine('meshTriangles', d, { setup: d.setups[0].id, libraries: M.planArgs().libraries }, M.state.mesh); const t = result.triangles; let lo = [1e9, 1e9, 1e9], hi = [-1e9, -1e9, -1e9]; for (let i = 0; i < t.length; i += 3) for (let k = 0; k < 3; k++) { lo[k] = Math.min(lo[k], t[i + k]); hi[k] = Math.max(hi[k], t[i + k]); } return [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]].map((v) => +v.toFixed(2)); })()`);
    const b0 = await bounds();
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await waitFor(() => js("document.getElementById('setupDialog').classList.contains('embedded') && document.getElementById('setupDialog').open"), "the setup's form in its details", 30000);
    await js("document.querySelector('#sdOrient [data-rot=\"2\"]').click(); document.getElementById('sdOk').click(); true");
    await settle();
    await waitFor(async () => (await js("JSON.stringify(JSON.parse(document.getElementById('job').value).models.find((m) => m.kind === 'mesh').rotate || null)")) === "[0,0,90]", "the part turned", 60000).catch(() => {});
    const rot = await js("JSON.stringify(JSON.parse(document.getElementById('job').value).models.find((m) => m.kind === 'mesh').rotate || null)");
    const b1 = await bounds();
    // laid flat: a face down (a quarter or half turn about X or Y here)
    await waitFor(() => js("document.getElementById('setupDialog').classList.contains('embedded') && document.getElementById('setupDialog').open"), "the setup's form again", 30000);
    await js("document.getElementById('sdLayFlat').click(); true");
    const flat = await js("['sdRotX', 'sdRotY', 'sdRotZ'].map((id) => +document.getElementById(id).value)");
    check("orientation: a quarter turn about Z turns the part (its X and Y sizes swap); Lay flat puts a face down",
      rot === "[0,0,90]" && Math.abs(b1[0] - b0[1]) < 0.02 && Math.abs(b1[1] - b0[0]) < 0.02 && flat.every((v) => Number.isFinite(v)) && flat.every((v) => v % 90 === 0),
      `${rot}; ${JSON.stringify(b0)} -> ${JSON.stringify(b1)}; lay flat ${JSON.stringify(flat)}`);
    await js("document.getElementById('sdCancel').click(); true");
  }

  // (Mar) the tool library in the tree, and each setup's tools (one added from the library)
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    await js("document.querySelector('#tree [data-act=library]').click(); true");
    await settle();
    const lib = await js("(() => { const p = document.getElementById('libraryPanel'); return { inDetails: !!p.closest('#treeDetails'), open: p.open, rows: document.querySelectorAll('#toolTable tr[data-tool]').length }; })()");
    await js("document.querySelector('#tree .tree-tool [data-act=tool]').click(); true");
    await settle();
    const tool = await js("({ details: document.getElementById('treeDetails').textContent, parked: !!document.getElementById('libraryPanel').closest('#libraryParked') })");
    const before = await js("[...document.querySelectorAll('#tree .tree-tool')].map((r) => r.dataset.tool).join(',')");
    await js("document.getElementById('treeAddTool').click(); true");
    await waitFor(() => js("document.getElementById('toolPick').open"), "the tool library picker", 10000);
    await js("[...document.querySelectorAll('#tpList .tp-row')].find((r) => !/Spiral O Metal 3\.175 x 12/.test(r.textContent)).click(); true");
    await waitFor(() => js("!document.getElementById('toolPick').open"), "the picker closed", 10000);
    await settle();
    const after = await js("({ rows: [...document.querySelectorAll('#tree .tree-tool')].map((r) => r.dataset.tool).join(','), listed: JSON.stringify(JSON.parse(document.getElementById('job').value).setups[0].tools || []) })");
    check("tool library in the tree: its panel in the details; a setup's tools listed with their details; one added from the library joins the project and the setup",
      lib.inDetails && lib.open && lib.rows > 0 && /T1/.test(tool.details) && /Diameter/.test(tool.details) && tool.parked && after.rows.split(",").length === before.split(",").length + 1 && after.listed !== "[]",
      `${JSON.stringify(lib)} parked ${tool.parked}; ${before} -> ${after.rows}; listed ${after.listed}`);
  }

  // (Mar) each setup its own part: another file in one setup only, or the same as another's
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    // a second setup, its own placement (not carried)
    await addSetup("vise", "own");
    const modelOf = (k) => `(() => { const d = JSON.parse(document.getElementById('job').value); const s = d.setups[${k}]; return d.models.find((m) => s.part.includes(m.id) && m.kind === 'mesh')?.file; })()`;
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await waitFor(() => js("document.getElementById('setupDialog').classList.contains('embedded') && document.getElementById('setupDialog').open"), "setup 2's form", 30000);
    const knob = readFileSync(join(root, "examples", "knob", "knob.stl")).toString("base64");
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${knob}"), (c) => c.charCodeAt(0))], "knob.stl")); const i = document.getElementById('sdModelFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await waitFor(() => js("document.getElementById('sdModel').value === 'picked'"), "the new part read", 30000);
    await js("document.getElementById('sdOk').click(); true");
    await settle();
    await waitFor(async () => (await js(modelOf(1))) === "knob.stl", "setup 2's new part", 60000).catch(() => {});
    const m1 = [await js(modelOf(0)), await js(modelOf(1))];
    await clickGenerate();
    await waitDone("setup 2 planned with its own part", 240000);
    const planned2 = await js("window.__marcam.state.result?.programs?.length >= 0 && document.body.dataset.state === 'done'");
    // the same part as setup 1 again
    await waitFor(() => js("document.getElementById('setupDialog').classList.contains('embedded') && document.getElementById('setupDialog').open"), "setup 2's form again", 30000);
    const first = await js("JSON.parse(document.getElementById('job').value).setups[0].id");
    await js(`(() => { const s = document.getElementById('sdModel'); s.value = 'setup:${first}'; s.dispatchEvent(new Event('change', { bubbles: true })); document.getElementById('sdOk').click(); return true; })()`);
    await settle();
    await waitFor(async () => (await js(modelOf(1))) === "dome.stl", "setup 2 back on setup 1's part", 60000).catch(() => {});
    const m2 = [await js(modelOf(0)), await js(modelOf(1))];
    check("each setup its own part: another file in one setup only (the other keeps its own), planned; then the same part as another setup",
      m1[0] === "dome.stl" && m1[1] === "knob.stl" && planned2 && m2[0] === "dome.stl" && m2[1] === "dome.stl",
      `${JSON.stringify(m1)} planned ${planned2} -> ${JSON.stringify(m2)}`);
  }

  // (Mar) the work zero (WCS): a preset on the stock, the G-code written from it
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    await clickGenerate();
    await waitDone("the plate planned", 240000);
    const g0 = await js("window.__marcam.state.result.programs[0].gcode");
    await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
    await waitFor(() => js("document.getElementById('setupDialog').classList.contains('embedded') && document.getElementById('setupDialog').open"), "the setup's form", 30000);
    await js("const z = document.getElementById('sdZeroAt'); z.value = 'top-c'; z.dispatchEvent(new Event('change', { bubbles: true })); true");
    const typed = await js("['sdZeroX', 'sdZeroY', 'sdZeroZ'].map((id) => +document.getElementById(id).value)");
    await js("document.getElementById('sdOk').click(); true");
    await settle();
    await waitFor(async () => JSON.stringify((await js("JSON.parse(document.getElementById('job').value).setups[0].post?.origin || null"))) === "[30,20,0]", "the work zero saved", 60000).catch(() => {});
    const saved = await js("JSON.parse(document.getElementById('job').value).setups[0].post?.origin || null");
    await clickGenerate();
    await waitDone("the plate planned from the new zero", 240000);
    const g1 = await js("window.__marcam.state.result.programs[0].gcode");
    const firstX = (g) => +(/\bX(-?[\d.]+)/.exec(g.split("\n").find((l) => /^G[01] .*X/.test(l)) || "")?.[1]);
    check("work zero: a preset on the stock (its top centre), the G-code written from it (X and Y 30 and 20 less), said in it",
      JSON.stringify(typed) === "[30,20,0]" && JSON.stringify(saved) === "[30,20,0]" && /Work zero/.test(g1) && Math.abs(firstX(g0) - 30 - firstX(g1)) < 0.002,
      `typed ${JSON.stringify(typed)}, saved ${JSON.stringify(saved)}, first X ${firstX(g0)} -> ${firstX(g1)}`);
    await shot("34_work_zero");
  }

  // (Mar) deleting setups: those carrying from it carry from the one before, said; undoable;
  // asked first when it has operations; never the last
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    for (let k = 0; k < 2; k++) await addSetup("vise", "carry");
    const ids = await js("JSON.parse(document.getElementById('job').value).setups.map((s) => s.id)");
    // (asked first only when it has operations: starter ones, if asked for above)
    const ops2 = await js("JSON.parse(document.getElementById('job').value).setups[1].operations.length");
    const dialogs0 = cdp.dialogs.length;
    // setup 2 (no operations): no question
    await js(`document.querySelector('#tree .tree-setup[data-setup="${ids[1]}"] [data-act=delsetup]').click(); true`);
    await settle();
    const after = await js("JSON.parse(document.getElementById('job').value).setups.map((s) => ({ id: s.id, stock: s.stock }))");
    await js(`document.querySelector('#tree .tree-setup[data-setup="${ids[2]}"] [data-act=setup]').click(); true`);
    await settle();
    const note = await js("document.getElementById('setupNote')?.textContent || ''");
    const asked0 = cdp.dialogs.length - dialogs0;
    // undo: back
    await js("document.getElementById('menuUndo').click(); true");
    await settle();
    const undone = await js("JSON.parse(document.getElementById('job').value).setups.length");
    // setup 1 has operations: asked first (declined: kept)
    cdp.dialogAnswer = false;
    const d1 = cdp.dialogs.length;
    await js(`document.querySelector('#tree .tree-setup[data-setup="${ids[0]}"] [data-act=delsetup]').click(); true`);
    await settle();
    cdp.dialogAnswer = true;
    const asked1 = cdp.dialogs.slice(d1).map((d) => d.message).join(" ");
    const kept = await js("JSON.parse(document.getElementById('job').value).setups.length");
    check("deleting a setup: the next one carries from the one before, said in its details; undone by Undo; asked first when it has operations",
      after.length === 2 && after[1].id === ids[2] && after[1].stock.shape === "from_previous" && !after[1].stock.from && /deleted/.test(note) && asked0 === (ops2 ? 1 : 0) && undone === 3 && /operation/.test(asked1) && kept === 3,
      `${JSON.stringify(after)}; note "${note}"; asked ${asked0} (${ops2} ops); undone ${undone}; asked "${asked1.slice(0, 80)}"; kept ${kept}`);
  }

  // (Mar) deleting the last setup: a new project's state (no setups, models, tools or part;
  // the tool library kept), asked first, the app working with none; Undo brings it back
  {
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    await clickGenerate();
    await waitDone("the plate generated", 240000);
    const lib0 = await js("window.__marcam.tools?.summary?.() || ''");
    const exc = () => cdp.events.filter((e) => e.method === "Runtime.exceptionThrown").map((e) => e.params.exceptionDetails.exception?.description || e.params.exceptionDetails.text);
    const errs0 = exc().length;
    const d0 = cdp.dialogs.length;
    const only = await js("JSON.parse(document.getElementById('job').value).setups[0].id");
    await js(`document.querySelector('#tree .tree-setup[data-setup="${only}"] [data-act=delsetup]').click(); true`);
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 0"), "the last setup deleted", 30000);
    await settle(800);
    const asked = cdp.dialogs.length - d0;
    const st = await js(`(() => { const d = JSON.parse(document.getElementById('job').value); const g = window.__marcam.state.three.groups; return { setups: d.setups.length, models: d.models.length, tools: d.tools.length, mesh: !!window.__marcam.state.mesh, empty: !!document.getElementById('treeEmpty'), add: !!document.getElementById('treeAddSetup'), lib: !!document.querySelector('#tree [data-act=library]'), part: g.part.children.length, stock: g.stock.children.length, path: (g.path?.children.length || 0), result: !!window.__marcam.state.result, emptyState: !!document.getElementById('emptyState'), download: document.getElementById('download').disabled }; })()`);
    const lib1 = await js("window.__marcam.tools?.summary?.() || ''");
    // with none: Generate, the simulation, save (TOML), the library panel, Add setup's dialog
    await js("document.getElementById('generate').click(); true");
    await settle(800);
    await js("document.getElementById('simulate').click(); true");
    await settle(800);
    const toml = await js("await window.__marcam.engine('toToml', JSON.parse(document.getElementById('job').value)).then((r) => r.result.text)");
    await js("document.querySelector('#tree [data-act=library]').click(); true");
    await settle();
    const libShown = await js("!!document.getElementById('libHost')");
    await addSetup("vise", "carry");
    const choices = await js("(() => { const s = JSON.parse(document.getElementById('job').value).setups[0]; return s.part.length === 0 && s.stock.shape === 'box' ? 'none,file' : JSON.stringify(s); })()");
    // (taken back: the project with none again)
    await js("document.getElementById('menuUndo').click(); true");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 0"), "the added setup undone", 30000);
    await settle();
    const errs = exc().slice(errs0);
    await shot("41_no_setups");
    // Undo: the plate back, its part too; Redo: none again
    await js("document.getElementById('menuUndo').click(); true");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 1 && !!window.__marcam.state.mesh"), "the plate back", 30000);
    await settle();
    const back = await js("({ ops: JSON.parse(document.getElementById('job').value).setups[0].operations.length, mesh: document.getElementById('meshInfo').textContent })");
    await js("document.getElementById('menuRedo').click(); true");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 0"), "none again", 30000);
    await settle();
    check("deleting the last setup: asked first, a new project's state (no setups, models, tools or part; library kept, + Add setup), Generate, simulate, save and Add setup with none; Undo / Redo",
      asked === 1 && st.setups === 0 && st.models === 0 && st.tools === 0 && !st.mesh && st.empty && st.add && st.lib && st.part === 0 && st.stock === 0 && st.path === 0 && !st.result && st.download && lib0 === lib1 &&
      /schema_version/.test(toml) && libShown && choices === "none,file" && errs.length === 0 && back.ops > 0 && /dome\.stl/.test(back.mesh),
      `asked ${asked}; ${JSON.stringify(st)}; lib "${lib0}" -> "${lib1}"; toml ${toml.length} chars; library ${libShown}; choices ${choices}; errors ${errs.join(" | ").slice(0, 300)}; back ${JSON.stringify(back)}`);
  }

  // (Mar) the cut result as the next setup's stock, any way round and across kinds: the knob,
  // turned, lying in a vise (no model) and faced; the plate, cut in the vise, chucked in the
  // rotary; the plate stood on its end (Turned Y 90). Drawn as the engine has it; its STL.
  {
    const addCarried = async (kind, rot) => {
      // (the tree's: a 0.1 job shown as a project has its setups there only)
      const n0 = await js("window.__marcam.state.tree.doc.setups.length");
      await addSetup(kind, "carry");
      if (rot) {
        await js("document.querySelector('#tree .tree-setup.active [data-act=stock]').click(); true");
        await waitFor(() => js("!!document.getElementById('stRotY')"), "the carried Stock", 30000);
        await js(`(() => { const y = document.getElementById('stRotY'); y.value = '${rot}'; y.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
        await waitFor(() => js(`(window.__marcam.state.tree.doc.setups.at(-1).stock.rotate || [])[1] === ${rot} && document.body.dataset.state !== 'busy'`), "the carried piece turned", 60000);
      }
      const gate = await js(`(() => { const s = window.__marcam.state.tree.doc.setups.at(-1); return { vise: s.kind === '${kind}', model: s.part.length ? 'own' : 'none', kind: s.kind }; })()`);
      await waitFor(() => js(`window.__marcam.state.tree.doc.setups.length === ${n0 + 1}`), "the carried setup in the project", 60000).catch(async (e) => {
        throw new Error(`${e.message}: ${await js("JSON.stringify({ error: document.getElementById('error').textContent, status: document.getElementById('status').textContent, setups: (JSON.parse(document.getElementById('job').value).setups || []).map((s) => s.id) })")} ${JSON.stringify(gate)}`);
      });
      await settle();
      const d = JSON.parse(await js("document.getElementById('job').value"));
      const s = d.setups[d.setups.length - 1];
      const frame = await js(`await window.__marcam.engine('setupFrame', JSON.parse(document.getElementById('job').value), { setup: '${s.id}' }).then((r) => r.result)`);
      // (why not, if not: the engine's error, the drawing's state)
      await waitFor(() => js("window.__marcam.state.three.groups.stock.children.some((o) => o.userData.carried) && !window.__marcam.state.three.drawing"), "the carried stock drawn as a mesh", 240000).catch(async (e) => {
        throw new Error(`${e.message}: ${await js("JSON.stringify({ kind: '" + kind + "', error: window.__marcam.state.three.carriedError, drawing: window.__marcam.state.three.drawing, stock: window.__marcam.state.three.groups.stock.children.length, modelKey: !!window.__marcam.state.three.modelKey, stock0: JSON.parse(document.getElementById('job').value).setups.map((s) => s.stock?.shape) })")}`);
      });
      const m = await js(`await window.__marcam.engine('carriedMesh', JSON.parse(document.getElementById('job').value), { setup: '${s.id}', stl: true }, window.__marcam.state.mesh).then((r) => ({ total: r.result.total, bounds: r.result.bounds, stl: r.result.stl.length }))`);
      // (its STL: in the setup's details)
      await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
      await sleep(400);
      const button = await js("!!document.getElementById('treeStockStl')");
      return { gate, s, frame, m, button };
    };
    // the knob, lying in a vise
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle();
    const k = await addCarried("vise");
    await js("document.getElementById('treeAddType').value = 'face'; document.getElementById('treeAdd').click(); true");
    await waitFor(() => js(`${SEL}?.type === 'face' && !!document.getElementById('opForm')`), "the facing form", 60000);
    await js("document.getElementById('opForm').requestSubmit(); true");
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups[1].operations.some((o) => o.type === 'face')"), "the facing added", 60000);
    await clickGenerate();
    await waitDone("the knob faced in the vise", 240000);
    const ktabs = await js("[...document.querySelectorAll('#tabs button')].map((b) => b.textContent)");
    // (Mar's knob on end: the simulated stock is this setup's, not the first setup's)
    await js("document.getElementById('simulate').click(); true");
    await waitDone("the knob in the vise simulated", 240000);
    const ksim = await js("({ kind: window.__marcam.state.sim?.kind })");
    await shot("40a_knob_in_vise");
    const kb = k.frame.stock_box;
    check("the turned knob lies in a vise (no model): a block X 0..100 round it, drawn as cut, its STL, faced and verified clean",
      k.gate.vise && k.gate.model === "none" && k.s.kind === "vise" && k.s.stock.shape === "from_previous" && k.s.part.length === 0 && kb && Math.abs(kb.max[0] - kb.min[0] - 100) < 1e-6 && Math.abs(kb.max[1] - kb.min[1] - 25) < 1e-6 &&
      k.m.total > 100 && k.m.stl === 84 + 50 * k.m.total && k.button && ktabs.length >= 1 && ktabs.every((t) => t.endsWith("OK")) && ksim.kind === "xyz",
      `tabs ${ktabs}; simulated ${JSON.stringify(ksim)}; button ${k.button}; ${JSON.stringify(k.gate)}; ${JSON.stringify(k.s.stock)} part ${JSON.stringify(k.s.part)}; box ${JSON.stringify(kb)}; mesh ${JSON.stringify({ total: k.m.total, stl: k.m.stl })}`);
    // the plate, chucked in the rotary; then stood on its end in a vise
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle();
    const r = await addCarried("rotary");
    await shot("40b_plate_in_rotary");
    const st = r.frame.stock;
    check("the plate cut in the vise goes into the rotary: a bar 60 long round it, front at X0, drawn as cut",
      r.s.kind === "rotary" && !r.frame.stock_box && Math.abs(st.length - 60) < 1e-6 && Math.abs(st.radius - Math.hypot(20, 5)) < 1e-6 && Math.abs(r.m.bounds[0][0] + 60) < 0.5 && Math.abs(r.m.bounds[1][0]) < 0.5,
      `${JSON.stringify(r.s.stock)}; ${JSON.stringify(st)}; bounds ${JSON.stringify(r.m.bounds)}`);
    await js(`document.querySelector('#tree .tree-setup[data-setup="${r.s.id}"] [data-act=delsetup]').click(); true`);
    await waitFor(() => js("JSON.parse(document.getElementById('job').value).setups.length === 1"), "the rotary setup deleted", 30000);
    await settle();
    const e = await addCarried("vise", 90);
    await shot("40c_plate_on_end");
    const eb = e.frame.stock_box;
    check("the plate stood on its end (Turned Y 90): its stock turned, a block 10 x 40 x 60, drawn as cut",
      JSON.stringify(e.s.stock.rotate) === "[0,90,0]" && JSON.stringify(eb.min.map(Math.round)) === "[0,0,-60]" && JSON.stringify(eb.max.map(Math.round)) === "[10,40,0]" && e.m.total > 12,
      `${JSON.stringify(e.s.stock)}; ${JSON.stringify(eb)}`);
  }

  // (Mar) the stock in the tree: a Stock item with its type and sizes; the part moved in it,
  // and a carried piece in the vise, with handles in the 3D view (snapped; one edit a drag)
  {
    const job = () => js("document.getElementById('job').value").then(JSON.parse);
    // (a ring: turned `turn` degrees round it, on its band: 5° steps)
    const drag = async (mode, axis, dx, dy, turn = 120, apart = false) => {
      // (a ring: from a point round it that no arrow covers)
      // (`apart`: of those, the point furthest on the screen from the other rings and the
      // arrows: the knob's carried piece, its Y ring's 45° point near its Z ring, a press there
      // read before its handles settled taken by the Z ring or an arrow; else at 45°)
      let p = null, a0 = 45, best = -1;
      const at = async (ax, ang) => JSON.parse(await js(`JSON.stringify(window.__marcam.handles.screenOf('${mode}', '${ax}', ${ang}))`));
      const others = [];
      if (mode === "rotate" && apart) {
        for (const ax of "XYZ".replace(axis, "")) for (let g = 0; g < 360; g += 10) { const q = await at(ax, g); if (q) others.push(q); }
        // (and the arrows, which take a press before the rings: points along each, from the
        // handles' middle out past its head)
        const c = JSON.parse(await js("(() => { const v = window.__marcam.state.three; const p = window.__marcam.handles.state.proxy.position.clone().project(v.camera); const r = v.renderer.domElement.getBoundingClientRect(); return JSON.stringify([r.left + ((p.x + 1) / 2) * r.width, r.top + ((1 - p.y) / 2) * r.height]); })()"));
        for (const ax of "XYZ") {
          const m = JSON.parse(await js(`JSON.stringify(window.__marcam.handles.screenOf('translate', '${ax}'))`));
          if (m) for (let t = 0; t <= 2.2; t += 0.1) others.push([c[0] + (m[0] - c[0]) * t, c[1] + (m[1] - c[1]) * t]);
        }
      }
      for (const ang of mode === "rotate" && apart ? [45, 60, 30, 75, 15, 120, 150, 210, 240, 300, 330] : [45]) {
        const q = await at(axis, ang);
        if (!q) continue;
        const d = others.length ? Math.min(...others.map((o) => Math.hypot(o[0] - q[0], o[1] - q[1]))) : 1;
        if (d > best) (best = d), (p = q), (a0 = ang);
      }
      if (!p) return false;
      // (the rings take the press before the arrows do)
      await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: p[0], y: p[1] });
      await sleep(60);
      await S("Input.dispatchMouseEvent", { type: "mousePressed", x: p[0], y: p[1], button: "left", clickCount: 1 });
      for (let i = 1; i <= 12; i++) {
        // (a ring: round it, a quarter turn and a bit; an arrow: straight)
        const q = mode === "rotate" ? JSON.parse(await js(`JSON.stringify(window.__marcam.handles.screenOf('rotate', '${axis}', ${a0 + ((dx > 0 ? 1 : -1) * turn * i) / 12}))`)) : [p[0] + (dx * i) / 12, p[1] + (dy * i) / 12];
        await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: q[0], y: q[1], button: "left", buttons: 1 });
      }
      const readout = await js("document.getElementById('handleReadout')?.textContent || ''");
      await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: p[0] + dx, y: p[1] + dy, button: "left", clickCount: 1 });
      await settle(600);
      return readout;
    };
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle(1500);
    // the Stock pane: a size typed
    await js("document.querySelector('#tree [data-act=stock]').click(); true");
    await settle();
    const pane = await js("({ types: [...document.getElementById('stType').options].map((o) => o.value).join(','), x: document.getElementById('stX')?.value, setupShown: !!document.getElementById('setupHost') })");
    await js("const x = document.getElementById('stX'); x.value = '70'; x.dispatchEvent(new Event('change', { bubbles: true })); true");
    await settle();
    const sized = (await job()).setups[0].stock;
    // typed in (no change event): applied a moment after, the field keeping the focus
    await js("const y = document.getElementById('stY'); y.focus(); y.value = '45'; y.dispatchEvent(new Event('input', { bubbles: true })); true");
    await sleep(1500);
    // (a block resized about its middle: its width)
    const typed = { y: ((st) => st.max[1] - st.min[1])((await job()).setups[0].stock), focus: await js("document.activeElement?.id") };
    // the part: moved along X by its arrow, turned about Z by its ring
    await js("document.querySelector('#tree [data-act=part]').click(); true");
    await waitFor(() => js("!!window.__marcam.handles.state.tc?.visible"), "the handles", 30000);
    // (shown with the playback open, in the one 3D view there is)
    const inPlayback = await js("({ open: window.__marcam.machining.state.open, views: document.querySelectorAll('#view3d canvas').length, inScene: (() => { let o = window.__marcam.handles.state.tc; while (o && o.parent) o = o.parent; return o === window.__marcam.state.three.scene; })() })");
    const t0 = (await job()).models.find((m) => m.id === "dome");
    const moved = await drag("translate", "X", 60, 0);
    const t1 = (await job()).models.find((m) => m.id === "dome");
    const turned = await drag("rotate", "Z", -220, 160);
    const t2 = (await job()).models.find((m) => m.id === "dome");
    // (planned again: by itself when live, else by Generate)
    if (await js("!!window.__marcam.state.stale || !window.__marcam.state.result")) {
      await clickGenerate();
      await waitDone("the moved part planned", 240000);
    }
    const planned = await js("!window.__marcam.state.stale && [...document.querySelectorAll('#tabs button')].every((b) => b.textContent.endsWith('OK'))");
    await shot("42a_part_handles");
    await js("document.getElementById('menuUndo').click(); true");
    await settle();
    const undone = (await job()).models.find((m) => m.id === "dome");
    const dx = t1.translate[0] - (t0.translate?.[0] ?? 0);
    check("the Stock item: its type and sizes (the plate 70 wide); the part moved by its X arrow (snapped to 0.1 mm) and turned by its Z ring (5° steps on the ring), each drag one edit, planned again",
      pane.types === "block,round,square,hex,rect" && pane.x === "60" && !pane.setupShown && inPlayback.views === 1 && inPlayback.inScene && sized.max[0] - sized.min[0] === 70 && typed.y === 45 && typed.focus === "stY" && dx > 0.5 && Math.abs(dx * 10 - Math.round(dx * 10)) < 1e-6 && /X \+/.test(moved) &&
      t2.rotate?.[0] === 0 && t2.rotate?.[1] === 0 && t2.rotate[2] % 5 === 0 && t2.rotate[2] !== 0 && /Z: -?\d+\.\d\d/.test(turned) && planned && JSON.stringify(undone.translate) === JSON.stringify(t1.translate) && !undone.rotate,
      `pane ${JSON.stringify(pane)}; playback ${JSON.stringify(inPlayback)}; stock ${JSON.stringify(sized)}; typed ${JSON.stringify(typed)}; moved ${JSON.stringify(t0.translate)} -> ${JSON.stringify(t1.translate)} "${moved}"; turned ${JSON.stringify(t2.rotate)} "${turned}"; planned ${planned}; undone ${JSON.stringify(undone)}`);
    // a round bar standing up in the vise: the block round it is its length tall
    await js("document.querySelector('#tree [data-act=stock]').click(); true");
    await settle();
    await js("const t = document.getElementById('stType'); t.value = 'round'; t.dispatchEvent(new Event('change', { bubbles: true })); true");
    await settle();
    await js("const a = document.getElementById('stAlong'); a.value = 'z'; a.dispatchEvent(new Event('change', { bubbles: true })); true");
    await settle();
    const bar = (await job()).setups[0];
    const bframe = await js(`await window.__marcam.engine('setupFrame', JSON.parse(document.getElementById('job').value), { setup: '${bar.id}' }).then((r) => r.result.stock_box)`);
    await shot("42b_bar_standing");
    check("a round bar standing up in the vise (Stock: Round bar, Axis Z): the block round it as tall as the bar",
      bar.stock.shape === "cylinder" && bar.holding.along === "z" && Math.abs(bframe.max[2] - bframe.min[2] - bar.stock.length) < 1e-6 && Math.abs(bframe.max[0] - 2 * bar.stock.radius) < 1e-6,
      `${JSON.stringify(bar.stock)} along ${bar.holding.along}; box ${JSON.stringify(bframe)}`);
    // the knob's cut result in a vise: stood on end by its Y ring (stock and all)
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await settle(1500);
    await addSetup("vise", "carry");
    // (its handles under its Part: Mar, only in the part modification screen)
    await js("document.querySelector('#tree [data-act=part]').click(); true");
    await waitFor(() => js("!!window.__marcam.handles.state.tc?.visible && window.__marcam.handles.target()?.kind === 'carried'"), "the carried piece's handles", 60000);
    // (the carried piece drawn and its handles on it before they are read off the screen)
    await settle(1500);
    const stood = await drag("rotate", "Y", -220, 160, 90, true);
    const ks = (await job()).setups[1];
    const kframe = await js(`await window.__marcam.engine('setupFrame', JSON.parse(document.getElementById('job').value), { setup: '${ks.id}' }).then((r) => r.result.stock_box)`);
    await shot("42c_knob_stood");
    check("the knob's cut result in a vise stood on end by its Y ring: turned 90° (stock and part together), the block round it 100 tall",
      /Y: -?90\.00/.test(stood) && Math.abs(Math.abs(ks.stock.rotate?.[1] ?? 0) - 90) < 1e-9 && Math.abs(kframe.max[2] - kframe.min[2] - 100) < 1e-6,
      `"${stood}" ${JSON.stringify(ks.stock)}; box ${JSON.stringify(kframe)}`);
  }

  // (Mar) duplicating an operation: the icon (right after it, "<name> copy", selected, one
  // Undo), Ctrl+D, and Ctrl+drag onto another setup's operation (a copy there)
  {
    const job = () => js("document.getElementById('job').value").then(JSON.parse);
    await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.getElementById('meshInfo').textContent.includes('dome.stl')"), "the plate", 60000);
    await settle(1000);
    const d0 = await job();
    const s0 = d0.setups[0];
    const src = s0.operations[1];
    const file0 = s0.outputs.find((o) => o.operations.includes(src.id));
    await js(`document.querySelector('#tree .tree-op[data-op="${src.id}"] [data-act="dup"]').click(); true`);
    await waitFor(() => js(`JSON.parse(document.getElementById('job').value).setups[0].operations.length === ${s0.operations.length + 1}`), "the duplicate", 30000);
    await settle();
    const d1 = await job();
    const s1 = d1.setups[0];
    const copy = s1.operations[2];
    const file1 = s1.outputs.find((o) => o.name === file0.name);
    const same = JSON.stringify({ ...src, id: 0, name: 0 }) === JSON.stringify({ ...copy, id: 0, name: 0 });
    const sel = await js("({ selected: window.__marcam.state.tree.selected, form: !!document.getElementById('opForm') })");
    await shot("43_duplicated");
    await js("document.getElementById('menuUndo').click(); true");
    await settle();
    const undone = (await job()).setups[0].operations.length;
    // Ctrl+D on the selected one
    await js(`document.querySelector('#tree .tree-op[data-op="${src.id}"] [data-act="select"]').click(); true`);
    await settle();
    await js("document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'd', ctrlKey: true, bubbles: true })); true");
    await waitFor(() => js(`JSON.parse(document.getElementById('job').value).setups[0].operations.length === ${s0.operations.length + 1}`), "the Ctrl+D duplicate", 30000);
    await settle();
    const keyed = (await job()).setups[0].operations[2];
    check("duplicate an operation: right after it in its file, \"<name> copy\", a new id, everything else the same, selected with its form; Undo takes it back; Ctrl+D too",
      copy.id !== src.id && !d0.setups.some((x) => x.operations.some((o) => o.id === copy.id)) && copy.name === `${src.name || "Pocket"} copy` && same &&
      file1.operations.indexOf(copy.id) === file1.operations.indexOf(src.id) + 1 && sel.selected === copy.id && sel.form && undone === s0.operations.length && keyed?.name === copy.name,
      `${src.id} -> ${copy.id} "${copy.name}" same ${same}; file ${JSON.stringify(file1.operations)}; ${JSON.stringify(sel)}; undone ${undone}; Ctrl+D ${keyed?.id} "${keyed?.name}"`);
    // Ctrl+drag onto another setup's operation: a copy there (its picks looked for there)
    await addSetup("vise", "carry");
    const d2 = await job();
    const other = d2.setups[1];
    // (from the first setup: worked on again, its operation dragged onto the second's head)
    await js(`document.querySelector('#tree .tree-setup[data-setup="${d2.setups[0].id}"] [data-act=setup]').click(); true`);
    await settle();
    await js(`document.querySelector('#tree .tree-setup[data-setup="${other.id}"] [data-act=fold]')?.getAttribute('aria-expanded') === 'false' && document.querySelector('#tree .tree-setup[data-setup="${other.id}"] [data-act=fold]').click(); true`);
    await settle();
    const dropped = await js(`(() => {
      const from = document.querySelector('#tree .tree-op[data-op="${src.id}"]');
      const dt = new DataTransfer();
      from.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
      // onto the other setup's head row, or an operation of it (it has none: its head)
      const to = document.querySelector('#tree .tree-setup[data-setup="${other.id}"] .tree-op') || document.querySelector('#tree .tree-setup[data-setup="${other.id}"] .tree-setuphead .tree-label');
      if (!to) return 'no target';
      to.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt, ctrlKey: true }));
      return 'ok';
    })()`);
    await settle(800);
    const d3 = await job();
    const there = d3.setups[1].operations.find((o) => o.name === `${src.name || "Pocket"} copy`);
    check("Ctrl+drag an operation onto another setup (its line or one of its operations): a copy there, its picks looked for on that setup's part",
      dropped === "no target" ? d3.setups[1].operations.length === other.operations.length : !!there && d3.setups[0].operations.length === d2.setups[0].operations.length,
      `${dropped}; there ${JSON.stringify(there && { id: there.id, error: there.error })}`);
  }

  // (Mar's scrub glitch, rebuilt: the knob's cut result standing in a vise on the Z1, facing,
  // contour and pocket, two hidden) a fast scrub: every frame one view, the work not turned,
  // the stock no taller than its block
  {
    const fx = readFileSync(join(root, "web", "tests", "fixtures", "knob_on_end.marcam.json"), "utf8");
    await js(`await window.__marcam.project.openText(${JSON.stringify(fx)}, "knob_on_end.marcam.json").then(() => true)`);
    await settle(1500);
    await js("document.querySelector('#tree .tree-setup[data-setup=\"setup2\"] [data-act=setup]').click(); true");
    await settle(1000);
    for (const id of ["op5", "op6"]) {
      await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="eye"]').click(); true`);
      await sleep(500);
    }
    await clickGenerate();
    await waitDone("the knob on end planned", 240000);
    await js("if (!window.__marcam.machining.state.view.stock) document.querySelector('#view3d canvas').dispatchEvent(new KeyboardEvent('keydown', { key: 's', bubbles: true })); window.__marcam.machining.expand?.(); true");
    await waitFor(() => js("!!window.__marcam.machining.state.session?.segments && !window.__marcam.machining.state.busy && !window.__marcam.machining.state.swapping"), "the knob's playback", 120000);
    await js(`(() => { const M = window.__marcam, m = M.machining.state, v = M.state.three; window.__scrubFrames = []; const r0 = v.renderer.render.bind(v.renderer); window.__scrubR0 = r0; v.renderer.render = (s, c) => { if (s === v.scene) { const g = m.stockMesh?.geometry; let top = -1e9; if (g) { const p = g.attributes.position.array; for (let i = 2; i < p.length; i += 9) top = Math.max(top, p[i]); } window.__scrubFrames.push([m.pos, top, v.groups.work.rotation.x, document.querySelectorAll('#view3d canvas').length]); } return r0(s, c); }; return true; })()`);
    const segs = await js("window.__marcam.machining.state.session.segments");
    for (const f of [0.15, 0.5, 0.8, 0.6, 0.75, 0.3, 0.9, 0.4]) {
      for (let i = 0; i < 4; i++) {
        await js(`(() => { const s = document.getElementById('simScrub'); s.value = String(${Math.round(f * segs)} + ${i}); s.dispatchEvent(new Event('input')); return true; })()`);
        await sleep(30);
      }
    }
    await sleep(2000);
    const fr = await js("window.__scrubFrames.splice(0)");
    await js("window.__marcam.state.three.renderer.render = window.__scrubR0; true");
    const bad = fr.filter(([, top, rot, views]) => top > 0.05 || Math.abs(rot) > 1e-9 || views !== 1);
    check("a fast scrub on the knob standing in a vise (Mar's): every frame one 3D view, the work never turned, the stock no taller than its block",
      fr.length > 10 && !bad.length, `${fr.length} frames; bad ${JSON.stringify(bad.slice(0, 3))}`);
  }

  // 9. phone width: layout stacks, no horizontal scroll
  await S("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await sleep(800);
  const overflow = await js("document.documentElement.scrollWidth - document.documentElement.clientWidth");
  check("phone width: no horizontal scroll", overflow <= 0, `${overflow}px`);
  await shot("06_phone");

  const errors = cdp.events.filter((e) => e.method === "Runtime.exceptionThrown").map((e) => e.params.exceptionDetails.exception?.description || e.params.exceptionDetails.text)
    .concat(consoleErrors());
  if (errors.length) console.log(`uncaught page errors:\n${errors.join("\n---\n")}`);
  check("no uncaught page errors", errors.length === 0, errors.join(" | ").slice(0, 300));
} catch (e) {
  failed = true;
  console.log(`FAIL  ${e.message}`);
  try { await shot("99_failure"); } catch {}
} finally {
  // (closed in order: the browser asked, our socket closed, then whatever is left killed)
  closing = true;
  try { await Promise.race([cdp.send("Browser.close"), sleep(2000)]); } catch {}
  try { cdp.ws.onmessage = null; cdp.ws.close(); } catch {}
  await sleep(300);
  cleanup();
  await sleep(500);
  try { rmSync(profile, { recursive: true, force: true }); } catch {}
}
const bad = results.filter((r) => !r.ok).length + (failed ? 1 : 0);
console.log(`\n${results.length - results.filter((r) => !r.ok).length}/${results.length} checks passed; screenshots in web/tests/screenshots/`);
process.exit(bad ? 1 : 0);
