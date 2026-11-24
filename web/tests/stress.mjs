// Stateful browser tests of the web app (Mar: "hand-editing operations is buggy"): scripted
// scenarios and a seeded random "monkey", both driving the real UI and checking the same
// invariants after every action.
//
//   node web/tests/stress.mjs                       (the scenarios and the CI seeds)
//   node web/tests/stress.mjs --seeds 1,2,3 --steps 60
//   node web/tests/stress.mjs --random 20 --steps 120   (20 fresh seeds: a longer local run)
//   node web/tests/stress.mjs --only monkey|scenarios
//   node web/tests/stress.mjs --only scenarios --match "replace the model"
//
// A failure prints the seed and the actions so far: `--seeds <seed> --steps <n>` replays it.
// Needs web/pkg (scripts/build-wasm.*) and Chrome, Edge or Chromium, like e2e.mjs.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

const arg = (name, dflt) => (process.argv.includes(name) ? process.argv[process.argv.indexOf(name) + 1] : dflt);
// CI: these seeds, this many actions each
const CI_SEEDS = [1, 2, 3, 4, 5, 6];
let seeds = arg("--seeds", null)?.split(",").map(Number) || CI_SEEDS;
if (arg("--random", null)) seeds = Array.from({ length: Number(arg("--random")) }, () => Math.floor(Math.random() * 1e9));
const STEPS = Number(arg("--steps", 40));
const ONLY = arg("--only", "");
// --match <text>: only the scenarios whose name has it
const MATCH = arg("--match", "");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function freePort() {
  return new Promise((resolve) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => resolve(p)); });
  });
}
async function waitFor(fn, what, timeoutMs = 30000) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < timeoutMs) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(150);
  }
  throw new Error(`timed out waiting for ${what} (${last})`);
}
function findBrowser() {
  const cands = process.platform === "win32"
    ? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"]
    : ["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/opt/pw-browsers/chromium", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"];
  const found = cands.find((p) => existsSync(p));
  if (!found) throw new Error("no Chrome/Edge/Chromium found");
  return found;
}

// ------------------------------------------------------------------ browser
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
const port = await freePort();
const py = spawn(process.platform === "win32" ? "python" : "python3", ["-m", "http.server", String(port), "--bind", "127.0.0.1", "--directory", root], { stdio: "ignore" });
children.push(py);
const url = `http://127.0.0.1:${port}/web/`;
await waitFor(() => httpOk(url), "the web server", 15000);
const profile = mkdtempSync(join(tmpdir(), "marcam-stress-"));
const dport = await freePort();
const browser = spawn(findBrowser(), [
  "--headless=new", `--remote-debugging-port=${dport}`, `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
  "--disable-extensions", "--window-size=1400,900",
  ...(process.env.MARCAM_E2E_SOFTWARE_GL ? ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] : []), "about:blank",
], { stdio: "ignore" });
children.push(browser);
const version = await waitFor(() => httpGet(`http://127.0.0.1:${dport}/json/version`).then((r) => r.json()), "the browser", 20000);
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("CDP connect failed")); });
let msgId = 0;
const waiters = new Map();
const events = [];
const dialogs = [];
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.method === "Page.javascriptDialogOpening") {
    dialogs.push(msg.params.message);
    send("Page.handleJavaScriptDialog", { accept: true, promptText: msg.params.defaultPrompt ?? "" }, msg.sessionId);
  }
  if (msg.id && waiters.has(msg.id)) {
    const { resolve, reject } = waiters.get(msg.id);
    waiters.delete(msg.id);
    msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
  } else if (msg.method) events.push(msg);
};
function send(method, params = {}, sessionId) {
  const id = ++msgId;
  ws.send(JSON.stringify({ id, method, params, sessionId }));
  return new Promise((resolve, reject) => waiters.set(id, { resolve, reject }));
}
const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
const S = (m, p) => send(m, p, sessionId);
await S("Page.enable");
await S("Runtime.enable");
await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });

async function js(expr) {
  const r = await S("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, replMode: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result.value;
}
async function shot(name) {
  const { data } = await S("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(shots, `${name}.png`), Buffer.from(data, "base64"));
}
// page errors since `from` (uncaught exceptions and console.error)
const EXPECTED_CONSOLE = [/favicon/i, /net::ERR_/, /Failed to load resource/];
function pageErrors(from = 0) {
  return events.slice(from).flatMap((e) => {
    if (e.method === "Runtime.exceptionThrown") return [e.params.exceptionDetails.exception?.description || e.params.exceptionDetails.text];
    if (e.method === "Runtime.consoleAPICalled" && e.params.type === "error") {
      const t = e.params.args.map((a) => a.value ?? a.description ?? "").join(" ");
      return EXPECTED_CONSOLE.some((r) => r.test(t)) ? [] : [t];
    }
    return [];
  });
}

const busy = () => js("document.body.dataset.state === 'busy' || !!window.__marcam.state.three?.drawing");
const app_settle = () => settle(400);
async function settle(ms = 250) {
  await waitFor(async () => !(await busy()) && (await js("!!window.__marcam.state.registry")), "the app to settle", 240000);
  await sleep(ms);
}

// ------------------------------------------------------------------ invariants
// Everything that must hold whatever was done: the project text parses and the engine finds no
// problem in it; the tree shows the project's operations in its files' order, each once; the
// editor shows the selected operation's own type and only that type's fields; a new operation
// (draft) is what is selected; picking offers only what the operation takes; Generate is
// enabled exactly when there is something to plan; the part is drawn; no page errors.
const INVARIANTS = `(async () => {
  const m = window.__marcam, st = m.state, t = st.tree, bad = [];
  let doc;
  try { doc = JSON.parse(document.getElementById('job').value); } catch (e) { return ['the project text does not parse: ' + e.message]; }
  if (doc.schema_version !== '0.2') return bad;
  try {
    const { result } = await m.engine('checkProject', doc);
    if (result.problems.length) bad.push('project problems: ' + result.problems.join('; '));
  } catch (e) { bad.push('checkProject failed: ' + e.message); }
  if (!t.doc) return bad.concat(['the tree has no project']);
  if (JSON.stringify(t.doc.setups.map((s) => s.id)) !== JSON.stringify(doc.setups.map((s) => s.id))) bad.push('the tree setups differ from the project');
  const s = doc.setups.find((x) => x.id === t.setup);
  if (doc.setups.length && !s) bad.push('the tree shows a setup the project does not have: ' + t.setup);
  if (s) {
    const rows = [...document.querySelectorAll('#tree .tree-op')].map((r) => r.dataset.op);
    const want = s.outputs.flatMap((o) => o.operations);
    if (JSON.stringify(rows) !== JSON.stringify(want)) bad.push('tree rows ' + rows + ' vs outputs ' + want);
    const ids = s.operations.map((o) => o.id);
    if (new Set(want).size !== want.length) bad.push('an operation is in two files');
    for (const id of ids) if (!want.includes(id)) bad.push(id + ' is in no file');
    for (const id of want) if (!ids.includes(id)) bad.push(id + ' in a file but not an operation');
    for (const r of document.querySelectorAll('#tree .tree-op')) {
      const op = s.operations.find((o) => o.id === r.dataset.op);
      const en = r.querySelector('[data-act="enable"]');
      if (op && en && en.checked !== (op.enabled !== false)) bad.push(op.id + ': enable box out of step');
    }
  }
  // the editor
  const form = document.getElementById('opForm');
  if (t.draft && t.selected !== t.draft.op.id) bad.push('a new operation (' + t.draft.op.id + ') is open but ' + t.selected + ' is selected');
  if (t.selected) {
    const op = t.draft && t.draft.op.id === t.selected ? t.draft.op : t.retype && t.retype.op.id === t.selected ? t.retype.op : s?.operations.find((o) => o.id === t.selected);
    if (!op) bad.push('the selected operation ' + t.selected + ' does not exist (setup ' + t.setup + ': ' + (s?.operations.map((o) => o.id).join(',') ?? 'none') + '; draft ' + (t.draft?.op.id ?? '-') + '; status ' + document.getElementById('status').textContent + '; editor ' + (() => { try { return JSON.parse(document.getElementById('job').value).setups.find((x) => x.id === t.setup).operations.map((o) => o.id).join(','); } catch (e) { return e.message; } })() + '; errors ' + (document.getElementById('opErrors')?.textContent || '') + ')');
    else if (!form) bad.push('no editor for the selected ' + t.selected);
    else {
      const ty = t.types.find((x) => x.kind === op.type);
      if (!ty) bad.push('unknown type ' + op.type);
      const sel = form.elements.__type;
      if (!sel || sel.value !== op.type) bad.push('the editor type ' + (sel && sel.value) + ' is not the operation type ' + op.type);
      if (ty) {
        const names = new Set(ty.params.map((p) => p.name));
        for (const el of form.elements) if (el.name && !el.name.startsWith('__') && !names.has(el.name)) bad.push('the editor shows ' + el.name + ', not a field of ' + op.type);
        if (names.has('tool') && !form.elements.tool) bad.push('no tool field');
      }
    }
  } else if (form) bad.push('an editor with nothing selected');
  // picking
  if (t.picking) {
    const op = t.selected && (t.draft?.op.id === t.selected ? t.draft.op : t.retype?.op.id === t.selected ? t.retype.op : s?.operations.find((o) => o.id === t.selected));
    const ty = op && t.types.find((x) => x.kind === op.type);
    const kinds = ty?.picks?.length ? ty.picks : ['cylindrical_band', 'revolved_band', 'surface'];
    if (!t.pick || !t.pickMesh) bad.push('picking without a picker');
    else {
      if (JSON.stringify(t.pick.mode.kinds) !== JSON.stringify(kinds)) bad.push('the picker takes ' + t.pick.mode.kinds + ' but ' + (op && op.type) + ' takes ' + kinds);
      for (const x of t.pick.list) if (!kinds.includes(x.kind)) bad.push('offered a ' + x.kind + ' for ' + (op && op.type));
      for (const id of t.pick.sel) if (!t.pick.list.some((x) => x.id === id)) bad.push('picked ' + id + ', which is not offered');
    }
  } else if (t.pickMesh) bad.push('the pick highlight stayed after picking ended');
  // Generate: enabled exactly when there is an enabled operation (not while busy)
  if (document.body.dataset.state !== 'busy' && st.registry) {
    const ops = doc.setups.some((x) => x.operations.some((o) => o.enabled !== false && !o.error && (doc.tools || []).some((t) => t.number === o.tool)));
    if (document.getElementById('generate').disabled === ops) bad.push('Generate ' + (ops ? 'disabled with operations' : 'enabled with none'));
  }
  if (st.stale && !st.result) bad.push('out of date without results');
  // the part is drawn (a mesh part needs its mesh)
  const v = st.three;
  if (v && !v.failed && doc.models?.length && (doc.models.some((x) => x.kind !== 'mesh') || st.mesh)) {
    if (!v.groups.part.children.length && !t.pickMesh && !v.drawing) bad.push('the part is not drawn');
  }
  return bad;
})()`;

// results of the last Generate: as a fresh plan of the saved project gives, with times shown
const RESULT_CHECK = `(async () => {
  const m = window.__marcam, st = m.state, bad = [];
  if (!st.result || st.stale) return bad;
  const job = JSON.parse(document.getElementById('job').value);
  const a = m.planArgs();
  let fresh;
  try {
    ({ result: fresh } = await m.engine('planAndPost', job, a.machine, st.mesh, { max_points: 150000, libraries: a.libraries, setup: job.schema_version === '0.2' ? st.tree?.setup : undefined, cache: false }));
  } catch (e) { return ['a fresh plan of the project fails: ' + e.message]; }
  const g = (r) => r.programs.map((p) => p.name + '\\n' + p.gcode);
  const x = g(st.result), y = g(fresh);
  if (x.length !== y.length) bad.push('programs ' + x.length + ' vs a fresh plan ' + y.length);
  else x.forEach((s, i) => { if (s !== y[i]) bad.push('program ' + st.result.programs[i].name + ' differs from a fresh plan'); });
  for (const p of st.result.programs) if (!(p.time?.total_s > 0)) bad.push(p.name + ': no time estimate');
  const est = document.getElementById('timeEst');
  if (!est || !est.checkVisibility()) bad.push('no estimated time shown');
  return bad;
})()`;

// ------------------------------------------------------------------ actions
// Each returns a short description for the log, or null when it does not apply now.
const EXAMPLES = ["knob", "knob_finish", "plate", "plaque", "ring", "knob_indexed"];
const STLS = [["knob.stl", "examples/knob/knob.stl"], ["bracket.stl", "tests/fixtures/picking/bracket.stl"], ["oval_grip.stl", "tests/fixtures/picking/oval_grip.stl"]];
// what a model can be replaced with (the one-oval grip: a part whose pocket is gone)
const REPLACEMENTS = [...STLS, ["oval_grip_one_oval.stl", "tests/fixtures/picking/oval_grip_one_oval.stl"]];
const b64 = (p) => readFileSync(join(root, p)).toString("base64");

const A = {
  async loadExample(r) {
    const ex = r.pick(EXAMPLES);
    await js(`document.getElementById('example').value = '${ex}'; document.getElementById('loadExample').click(); true`);
    await settle(400);
    return `load example ${ex}`;
  },
  async importStl(r, which, kind) {
    const [name, path] = which ? STLS.find((x) => x[0] === which) : r.pick(STLS);
    kind = kind || r.pick(name === "knob.stl" ? ["rotary", "indexed", "vise"] : ["vise", "indexed"]);
    await js(`await window.__marcam.project.openText(${JSON.stringify(JSON.stringify({ format: "marcam-project", version: 1, project: { schema_version: "0.2", name: "monkey", models: [], tools: [], setups: [] } }))}, "monkey.marcam.json").then(() => true)`);
    await settle();
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${b64(path)}"), (c) => c.charCodeAt(0))], "${name}")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    // (Mar: no setup dialogs) placed at once on the 4th axis; its type then in its Setup
    await waitFor(() => js("(() => { try { return JSON.parse(document.getElementById('job').value).setups?.length === 1 && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "the STL imported", 60000);
    await settle(600);
    if (kind !== "rotary") {
      await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
      await waitFor(() => js("!!document.querySelector('#setupHost #setupDialog[open]')"), "the setup's form in its details", 30000);
      await js(`(() => { const b = document.querySelector('#setupHost .sd-kind[data-kind="${kind}"]'); if (b) b.click(); return true; })()`);
      await waitFor(() => js(`JSON.parse(document.getElementById('job').value).setups[0].kind === '${kind}' && document.body.dataset.state !== 'busy'`), `the setup a ${kind}`, 60000);
      await settle(600);
    }
    return `import ${name} as ${kind}`;
  },
  async addOp(r, type) {
    const types = await js("[...document.querySelectorAll('#treeAddType option')].map((o) => o.value)");
    if (!types.length) return null;
    type = type || r.pick(types);
    if (!types.includes(type)) return null;
    await js(`document.getElementById('treeAddType').value = '${type}'; document.getElementById('treeAdd').click(); true`);
    // (Mar: no Apply) a whole one is in the project at once; one not complete yet waits
    await waitFor(() => js(`(() => { const t = window.__marcam.state.tree; const op = t.draft?.op.id === t.selected ? t.draft.op : t.doc?.setups.find((x) => x.id === t.setup)?.operations.find((o) => o.id === t.selected); return op?.type === '${type}' && !!document.getElementById('opForm'); })()`), `a new ${type}`, 15000).catch(() => {});
    await sleep(200);
    return `add ${type}`;
  },
  async apply() {
    // (Enter in the form: it applies as it is changed, this applies it at once)
    if (!(await js("!!document.getElementById('opForm')"))) return null;
    const before = await js("document.getElementById('job').value");
    const what = await js("(() => { const t = window.__marcam.state.tree; return (t.draft?.op.id === t.selected ? 'new ' : t.retype ? 'retyped ' : '') + t.selected; })()");
    await js("document.getElementById('opForm').requestSubmit(); true");
    await sleep(300);
    // a new operation: until it is in the project or refused (its check can wait in the
    // engine behind a live update)
    if (what.startsWith("new ")) {
      const id = what.slice(4);
      await waitFor(() => js(`(() => { const e = document.getElementById('opErrors'); if (e && !e.classList.contains('hidden') && e.textContent) return true; try { return JSON.parse(document.getElementById('job').value).setups.some((s) => s.operations.some((o) => o.id === '${id}')); } catch { return false; } })()`), `${id} applied`, 60000).catch(() => {});
    }
    await waitFor(async () => (await js("document.getElementById('job').value")) !== before || (await js("!document.getElementById('opErrors')?.classList.contains('hidden') || !window.__marcam.state.tree.draft")), "Apply", 15000).catch(() => {});
    await settle(200);
    const err = await js("(() => { const e = document.getElementById('opErrors'); return e && !e.classList.contains('hidden') ? e.textContent : ''; })()");
    return `apply ${what}${err ? ` (refused: ${err.slice(0, 120)})` : ""}`;
  },
  async changeType(r, type) {
    const opts = await js("[...(document.getElementById('opForm')?.elements.__type?.options || [])].map((o) => o.value)");
    if (!opts.length) return null;
    const cur = await js("document.getElementById('opForm').elements.__type.value");
    type = type || r.pick(opts.filter((x) => x !== cur));
    if (!type) return null;
    await js(`(() => { const s = document.getElementById('opForm').elements.__type; s.value = '${type}'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    // (a preset, "rough3d:indexed": the editor shows its type)
    await waitFor(() => js(`document.getElementById('opForm')?.elements.__type?.value === '${type.split(":")[0]}'`), `the editor as ${type}`, 15000).catch(() => {});
    await sleep(200);
    return `change type ${cur} -> ${type}`;
  },
  async editParam(r) {
    const fields = await js(`[...(document.getElementById('opForm')?.elements || [])].filter((e) => e.name && !e.name.startsWith('__') && e.tagName === 'INPUT' && e.type !== 'checkbox' && e.value !== '' && !isNaN(Number(e.value))).map((e) => e.name)`);
    if (!fields.length) return null;
    const f = r.pick(fields);
    const v = await js(`Number(document.getElementById('opForm').elements['${f}'].value)`);
    const nv = Math.round(v * r.pick([0.5, 0.8, 1.25, 1.5, 2]) * 1000) / 1000 || 0.5;
    await js(`(() => { const e = document.getElementById('opForm').elements['${f}']; e.value = '${nv}'; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await sleep(150);
    return `set ${f} ${v} -> ${nv}`;
  },
  async addLibraryTool(r) {
    if (!(await js("!!document.getElementById('opForm')?.elements.tool"))) return null;
    await js("(() => { const e = document.getElementById('opForm').elements.tool; e.value = '__add'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    const open = await waitFor(() => js("document.getElementById('toolPick').open"), "the tool library", 10000).then(() => true, () => false);
    if (!open) return "add a library tool (no dialog)";
    const n = await js("document.querySelectorAll('#tpList .tp-row').length");
    if (!n) {
      await js("document.getElementById('toolPick').close(); true");
      await sleep(200);
      return "add a library tool (none of its type)";
    }
    const k = r.int(n);
    await js(`document.querySelectorAll('#tpList .tp-row')[${k}].click(); true`);
    await sleep(300);
    await settle(200);
    return `add library tool #${k}`;
  },
  async changeTool(r) {
    const opts = await js("[...(document.getElementById('opForm')?.elements.tool?.options || [])].map((o) => o.value)");
    if (opts.length < 2) return null;
    const v = r.pick(opts);
    await js(`(() => { const e = document.getElementById('opForm').elements.tool; e.value = '${v}'; e.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await sleep(300);
    await settle(100);
    return `tool ${v}`;
  },
  async pick(r) {
    if (!(await js("!!document.getElementById('pickFaces')"))) return null;
    await js("document.getElementById('pickFaces').click(); true");
    const ok = await waitFor(() => js("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick"), "picking", 30000).then(() => true, () => false);
    if (!ok) return "pick (no picker)";
    await sleep(200);
    const n = await js("window.__marcam.state.tree.pick.list.length");
    let what = "nothing to pick";
    if (n) {
      const k = r.int(n);
      // a canvas point on one of its faces
      const at = await js(`(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const target = t.pick.list[${k}];
        const want = new Set(target.faces); const el = v.renderer.domElement; const tri = t.regions.tri; const ray = new v.THREE.Raycaster();
        for (let q = 0; q < t.regions.of.length; q++) { if (!want.has(t.regions.list[t.regions.of[q]].id)) continue;
          const c = new v.THREE.Vector3(...[0, 1, 2].map((d) => (tri[9*q+d] + tri[9*q+3+d] + tri[9*q+6+d]) / 3)).project(v.camera);
          if (Math.abs(c.x) > 0.95 || Math.abs(c.y) > 0.95) continue;
          ray.setFromCamera(new v.THREE.Vector2(c.x, c.y), v.camera); const hit = ray.intersectObject(t.pickMesh)[0];
          if (hit && want.has(t.regions.list[t.regions.of[hit.faceIndex]].id)) return [(c.x + 1) / 2 * el.clientWidth, (1 - c.y) / 2 * el.clientHeight, target.label]; }
        return null; })()`);
      if (at) {
        await js(`await window.__marcam.tree.pickAt(${at[0]}, ${at[1]}).then(() => true)`);
        what = at[2];
      } else what = "a feature out of view";
    }
    await sleep(200);
    const invPicking = await js(`await ${INVARIANTS}`);
    await js("document.getElementById('pickFaces')?.click(); true");
    await sleep(300);
    return { log: `pick ${what}`, extra: invPicking };
  },
  async ungeom(r) {
    const n = await js("document.querySelectorAll('#geometryList [data-ungeom]').length");
    if (!n) return null;
    const k = r.int(n + 1);
    if (k === n) await js("document.getElementById('geomClear').click(); true");
    else await js(`document.querySelectorAll('#geometryList [data-ungeom]')[${k}].click(); true`);
    await sleep(300);
    await settle(200);
    return k === n ? "clear the picks" : `remove pick ${k}`;
  },
  async select(r) {
    const ids = await js("[...document.querySelectorAll('#tree .tree-op')].map((x) => x.dataset.op)");
    if (!ids.length) return null;
    const id = r.pick(ids);
    await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="select"]').click(); true`);
    await sleep(200);
    return `select ${id}`;
  },
  async move(r) {
    const ids = await js("[...document.querySelectorAll('#tree .tree-op')].map((x) => x.dataset.op)");
    if (ids.length < 2) return null;
    const [a, b] = [r.pick(ids), r.pick(ids)];
    if (a === b) return null;
    const after = r.int(2) === 1;
    await js(`await window.__marcam.tree.moveOp('${a}', '${b}', ${after}).then(() => true)`);
    await settle(100);
    return `move ${a} ${after ? "after" : "before"} ${b}`;
  },
  async nudge(r) {
    const sel = await js("window.__marcam.state.tree.selected");
    if (!sel || !(await js(`!!document.querySelector('#tree .tree-op[data-op="${sel}"]')`))) return null;
    const up = r.int(2) === 1;
    await js(`(() => { document.getElementById('tree').dispatchEvent(new KeyboardEvent('keydown', { key: '${up ? "ArrowUp" : "ArrowDown"}', altKey: true, bubbles: true })); return true; })()`);
    await sleep(300);
    await settle(100);
    return `Alt+${up ? "Up" : "Down"} ${sel}`;
  },
  async rowAct(r, act) {
    const ids = await js(`[...document.querySelectorAll('#tree .tree-op')].filter((x) => { const b = x.querySelector('[data-act="${act}"]'); return b && !b.disabled; }).map((x) => x.dataset.op)`);
    if (!ids.length) return null;
    const id = r.pick(ids);
    await js(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="${act}"]').click(); true`);
    await sleep(300);
    await settle(100);
    return `${act} ${id}`;
  },
  async replaceModel(r, which) {
    // a project with a mesh model only
    if (!(await js("(() => { try { return JSON.parse(document.getElementById('job').value).models?.some((m) => m.kind === 'mesh') && !!window.__marcam.state.mesh; } catch { return false; } })()"))) return null;
    const [name, path] = which ? REPLACEMENTS.find((x) => x[0] === which) : r.pick(REPLACEMENTS);
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${b64(path)}"), (c) => c.charCodeAt(0))], "${name}")); const i = document.getElementById('replaceModelFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await sleep(400);
    await settle(400);
    // whatever is in error is marked in the tree, and nothing in error is planned
    const st = await js(`(() => { const d = JSON.parse(document.getElementById('job').value), s = d.setups.find((x) => x.id === window.__marcam.state.tree.setup) || d.setups[0];
      const errs = s.operations.filter((o) => o.error).map((o) => o.id);
      const marked = [...document.querySelectorAll('#tree .tree-op')].filter((x) => x.querySelector('.op-error')).map((x) => x.dataset.op);
      return { errs, marked, file: d.models.find((m) => m.kind === 'mesh')?.file }; })()`);
    const extra = [];
    if (JSON.stringify(st.errs) !== JSON.stringify(st.marked)) extra.push(`operations in error ${st.errs} but marked ${st.marked}`);
    return { log: `replace the model with ${name}${st.errs.length ? ` (in error: ${st.errs.join(", ")})` : ""}`, extra };
  },
  async cancel() {
    if (!(await js("!!document.getElementById('cancelDraft')"))) return null;
    await js("document.getElementById('cancelDraft').click(); true");
    await sleep(200);
    return "cancel";
  },
  async undo() {
    if (!(await js("!!document.getElementById('undoSetup') && !document.getElementById('undoBar').classList.contains('hidden')"))) return null;
    await js("document.getElementById('undoSetup').click(); true");
    await settle(400);
    return "undo";
  },
  // Edit > Undo / Redo by their keys (on the page, not in a field)
  async histUndo() {
    if (!(await js("window.__marcam.state.history.undo.length > 0 && !document.querySelector('dialog[open]:not(.embedded)')"))) return null;
    await js("document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, bubbles: true })); true");
    await settle(400);
    return "Ctrl+Z";
  },
  async histRedo() {
    if (!(await js("window.__marcam.state.history.redo.length > 0 && !document.querySelector('dialog[open]:not(.embedded)')"))) return null;
    await js("document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, shiftKey: true, bubbles: true })); true");
    await settle(400);
    return "Ctrl+Shift+Z";
  },
  /** Undo everything: the project as it was opened (its text and its mesh); redo
   *  everything: the newest project the history holds. */
  async historyRoundTrip() {
    const r = await js(`await (async () => {
      const pr = window.__marcam.project, h = window.__marcam.state.history, st = window.__marcam.state;
      // (redone first, what is still to redo: the newest state the history holds)
      while (await pr.redo());
      const final = { text: document.getElementById('job').value, mesh: st.mesh };
      let n = 0, m = 0;
      while (await pr.undo()) n++;
      const back = { text: document.getElementById('job').value === h.base.text, mesh: st.mesh === h.base.mesh };
      while (await pr.redo()) m++;
      const fwd = { text: document.getElementById('job').value === final.text, mesh: st.mesh === final.mesh };
      return { n, m, back, fwd };
    })()`);
    await settle(400);
    const extra = [];
    if (!r.back.text || !r.back.mesh) extra.push(`undo all (${r.n}): not the project as opened (text ${r.back.text}, mesh ${r.back.mesh})`);
    if (!r.fwd.text || !r.fwd.mesh) extra.push(`redo all (${r.m}): not the final project (text ${r.fwd.text}, mesh ${r.fwd.mesh})`);
    if (r.n !== r.m) extra.push(`${r.n} undone but ${r.m} redone`);
    return { log: `undo all (${r.n}), redo all (${r.m})`, extra };
  },
  async generate() {
    // (disabled while an update after the edits plans: waited for, a while)
    await waitFor(() => js("!document.getElementById('generate').disabled || document.body.dataset.state !== 'busy'"), "Replan all", 120000).catch(() => {});
    if (await js("document.getElementById('generate').disabled")) return null;
    await js("document.getElementById('generate').click(); true");
    await sleep(300);
    await settle(200);
    const err = await js("document.body.dataset.state === 'error' ? document.getElementById('error').textContent : ''");
    return { log: `generate${err ? ` (failed: ${err.slice(0, 150)})` : ""}`, extra: err ? [] : await js(`await ${RESULT_CHECK}`) };
  },
  async saveReopen() {
    const before = await js("document.getElementById('job').value");
    const text = await js(`JSON.stringify((() => { const st = window.__marcam.state; const project = JSON.parse(document.getElementById('job').value);
      const meshes = {}; const mf = project.models?.find((m) => m.kind === 'mesh')?.file;
      if (mf && st.mesh) { let bin = ''; for (let i = 0; i < st.mesh.length; i += 8192) bin += String.fromCharCode(...st.mesh.subarray(i, i + 8192)); meshes[mf] = btoa(bin); }
      return { format: 'marcam-project', version: 1, project, meshes, view: { hidden: [...st.hidden] } }; })())`);
    await js(`await window.__marcam.project.openText(${JSON.stringify(text)}, "reopened.marcam.json").then(() => true)`);
    await settle(400);
    const after = await js("document.getElementById('job').value");
    const same = JSON.stringify(JSON.parse(before)) === JSON.stringify(JSON.parse(after));
    return { log: "save and reopen", extra: same ? [] : ["the reopened project differs from the saved one"] };
  },
};

// the monkey's choice of action, weighted
const WEIGHTS = [
  ["loadExample", 3], ["importStl", 2], ["addOp", 8], ["apply", 8], ["changeType", 6], ["editParam", 6], ["changeTool", 3], ["addLibraryTool", 1],
  ["pick", 5], ["ungeom", 2], ["select", 6], ["move", 2], ["nudge", 2], ["enable", 3], ["eye", 2], ["delete", 3], ["cancel", 2], ["undo", 1], ["histUndo", 3], ["histRedo", 2],
  ["generate", 4], ["saveReopen", 2], ["replaceModel", 2],
];
function rng(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return { next, int: (n) => Math.floor(next() * n), pick: (xs) => xs[Math.floor(next() * xs.length)] };
}
async function act(name, r, ...args) {
  if (["enable", "eye", "delete"].includes(name)) return A.rowAct(r, name);
  return A[name](r, ...args);
}

/** Run actions, checking the invariants after each; throws with the log on a failure. */
async function run(label, steps, strict = false) {
  const log = [];
  for (const step of steps) {
    const mark = events.length;
    let out;
    try {
      out = await step();
    } catch (e) {
      throw Object.assign(new Error(`${label}: ${e.message}`), { log });
    }
    if (out == null) {
      // a scenario's steps must all happen; the monkey's may not apply
      if (strict) throw Object.assign(new Error(`${label}: step ${log.length + 1} could not be done`), { log });
      continue;
    }
    const { log: line, extra = [] } = typeof out === "string" ? { log: out } : out;
    log.push(line);
    await sleep(100);
    const bad = [...extra, ...(await js(`await ${INVARIANTS}`)), ...pageErrors(mark).map((e) => `page error: ${e.slice(0, 300)}`)];
    if (bad.length) throw Object.assign(new Error(`${label}, after "${line}":\n  ${bad.join("\n  ")}`), { log });
  }
  return log;
}

// ------------------------------------------------------------------ scenarios
const results = [];
function report(name, ok, detail = "") {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}
const r0 = rng(0);
const SCENARIOS = {
  // Mar's: a new Bore turned into a 3D finish before Apply: the picker takes faces
  "a new bore changed to 3D finishing before Apply picks faces, not holes": () => [
    () => A.importStl(r0, "bracket.stl", "vise"),
    () => A.addOp(r0, "bore"),
    () => A.changeType(r0, "raster3d"),
    async () => {
      const r = await A.pick(rng(7));
      const s = await js("({ type: document.getElementById('opForm').elements.__type.value, notes: document.getElementById('pickNotes')?.textContent || '' })");
      return { log: r.log, extra: [...r.extra, ...(/not a hole/.test(s.notes) ? ["the picker still wants holes"] : []), ...(s.type !== "raster3d" ? [`type ${s.type}`] : [])] };
    },
    () => A.apply(),
    () => A.generate(),
  ],
  // the same through the Add box: a bore open and picking, then a 3D finish added instead
  "a new bore being picked for, replaced by a new 3D finish: the picker follows": () => [
    () => A.importStl(r0, "bracket.stl", "vise"),
    () => A.addOp(r0, "bore"),
    async () => {
      await js("document.getElementById('pickFaces').click(); true");
      await waitFor(() => js("!!window.__marcam.state.tree.pickMesh"), "picking for the bore", 30000);
      return "start picking for the bore";
    },
    () => A.addOp(r0, "raster3d"),
    () => A.pick(rng(5)),
    () => A.apply(),
  ],
  // monkey seed 214319562: enabling an operation whose type change is pending
  "enable/disable an operation while its type change is pending": () => [
    () => A.importStl(r0, "knob.stl", "vise"),
    () => A.addOp(r0, "contour"),
    () => A.apply(),
    async () => {
      const id = await js("document.querySelector('#tree .tree-op:last-child')?.dataset.op || [...document.querySelectorAll('#tree .tree-op')].pop().dataset.op");
      if ((await js("window.__marcam.state.tree.selected")) !== id) await A.select({ pick: () => id });
      return `select ${id}`;
    },
    () => A.changeType(r0, "bore"),
    () => A.rowAct({ pick: (xs) => xs[xs.length - 1] }, "enable"),
    () => A.rowAct({ pick: (xs) => xs[xs.length - 1] }, "enable"),
    () => A.apply(),
  ],
  // monkey seed 775709100: a new operation still being made when the project is reopened
  "a new operation still being made when another project opens is dropped": () => [
    () => A.loadExample({ pick: () => "knob" }),
    async () => {
      // Add, and at once open the project again (no waiting for the engine's defaults)
      await js("document.getElementById('treeAddType').value = 'rotary_finish'; document.getElementById('treeAdd').click(); true");
      const r = await A.saveReopen();
      const draft = await js("!!window.__marcam.state.tree.draft");
      return { log: "add, reopen at once", extra: [...r.extra, ...(draft ? ["the new operation came into the reopened project"] : [])] };
    },
    () => A.addOp(r0, "bore"),
    () => A.apply(),
  ],
  // Mar: replace the model; operations whose picked feature is gone are in error, left out
  "replace the model: a pocket whose oval is gone is in error and left out; undo; picked again": () => [
    () => A.importStl(r0, "oval_grip.stl", "vise"),
    async () => {
      // a pocket on the right oval (detected), facing on the left one
      // the detected pocket whose outline is on the right (X > 30)
      await js(`await (async () => { const st = window.__marcam.state; const d = JSON.parse(document.getElementById('job').value);
        const { result } = await window.__marcam.engine('pickables', d, { setup: d.setups[0].id }, st.mesh);
        const right = result.pickables.find((x) => x.kind === 'pocket' && x.data.outline[0][0] > 30);
        const b = [...document.querySelectorAll('#detected .detected-btn')].find((x) => (x.dataset.ids || '').split(',').includes(right.id));
        b.click(); return true; })()`);
      await app_settle();
      return "pocket the right oval";
    },
    () => A.generate(),
    async () => {
      const r = await A.replaceModel(r0, "oval_grip_one_oval.stl");
      const s = await js("(() => { const d = JSON.parse(document.getElementById('job').value); return { errs: d.setups[0].operations.filter((o) => o.error).length, n: d.setups[0].operations.length }; })()");
      return { log: r.log, extra: [...r.extra, ...(s.errs === 1 ? [] : [`${s.errs} operations in error, wanted 1 (the pocket)`])] };
    },
    () => A.generate(),
    () => A.undo(),
    async () => {
      const errs = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.filter((o) => o.error).length");
      return { log: "after undo", extra: errs ? [`${errs} still in error after undo`] : [] };
    },
    () => A.replaceModel(r0, "oval_grip_one_oval.stl"),
    () => A.saveReopen(),
    async () => {
      const errs = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.filter((o) => o.error).length");
      return { log: "reopened", extra: errs === 1 ? [] : [`${errs} in error after reopening, wanted 1`] };
    },
    () => A.replaceModel(r0, "oval_grip.stl"),
    async () => {
      const errs = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.filter((o) => o.error).length");
      return { log: "the original part again", extra: errs ? [`${errs} still in error on the original part`] : [] };
    },
    () => A.generate(),
  ],
  // monkey seed 1: the plaque (a relief from an image) replaced by an STL was not drawn
  "the plaque's relief replaced by an STL is drawn and plans": () => [
    () => A.loadExample({ pick: () => "plaque" }),
    () => A.replaceModel(r0, "oval_grip_one_oval.stl"),
    () => A.generate(),
  ],
  // (forms apply as they change, no Cancel: Undo is the way back)
  "an applied operation's type changed, then undone: the old one is back": () => [
    () => A.importStl(r0, "bracket.stl", "vise"),
    () => A.select({ pick: (xs) => xs[0] }),
    async () => {
      const before = await js("document.getElementById('job').value");
      await A.changeType(r0, "contour");
      await settle(400);
      const changed = await js("document.getElementById('job').value");
      await A.histUndo();
      const after = await js("document.getElementById('job').value");
      const t = await js("document.getElementById('opForm')?.elements.__type?.value || ''");
      return { log: "retype then undo", extra: changed !== before && before === after && t !== "contour" ? [] : [`undo did not restore the operation (changed ${changed !== before}, restored ${before === after}, form ${t})`] };
    },
  ],
  "an applied operation's type changed and applied: picks it cannot take are cleared": () => [
    () => A.importStl(r0, "bracket.stl", "vise"),
    async () => {
      // the detected holes start a drilling
      await js("document.querySelector('#detected .detected-btn')?.click(); true");
      await settle(500);
      return "drill the detected holes";
    },
    async () => {
      await A.changeType(r0, "pocket");
      const note = await js("document.getElementById('typeNote')?.textContent || ''");
      return { log: "drill -> pocket", extra: /points/.test(note) ? [] : [`no note about the cleared points: ${note}`] };
    },
    () => A.apply(),
    async () => {
      const op = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.find((o) => o.type === 'pocket')");
      return { log: "check the pocket", extra: op && op.points === undefined ? [] : [`pocket ${JSON.stringify(op)}`] };
    },
    () => A.generate(),
  ],
  "switching to another operation with a new one open drops the new one, Apply edits the selected": () => [
    () => A.loadExample(rng(3)),
    () => A.addOp(r0, "raster3d"),
    () => A.select({ pick: (xs) => xs[0] }),
    async () => {
      const n0 = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.length");
      await A.apply();
      const n1 = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.length");
      return { log: "apply the selected", extra: n1 === n0 ? [] : [`operations ${n0} -> ${n1}: the dropped new one came in`] };
    },
  ],
  "deleting the operation being edited, changing the setup mid-edit": () => [
    () => A.loadExample({ pick: () => "knob_indexed" }),
    () => A.select({ pick: (xs) => xs[0] }),
    () => A.changeType(r0),
    () => A.rowAct({ pick: (xs) => xs[0] }, "delete"),
    () => A.addOp(r0),
    async () => {
      const n = await js("document.querySelectorAll('#treeSetup option').length");
      if (n < 2) return "one setup";
      await js("(() => { const s = document.getElementById('treeSetup'); s.value = s.options[1].value; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
      await sleep(300);
      return "other setup mid-edit";
    },
    () => A.generate(),
  ],
  "every example: add every type, generate, save and reopen": () =>
    EXAMPLES.flatMap((ex) => [
      () => A.loadExample({ pick: () => ex }),
      async () => {
        const types = await js("[...document.querySelectorAll('#treeAddType option')].map((o) => o.value)");
        const bad = [];
        for (const ty of types) {
          await A.addOp(r0, ty);
          const msg = await A.apply();
          if (/refused/.test(msg)) bad.push(`${ex}: a new ${ty} with its defaults is refused: ${msg}`);
          const inv = await js(`await ${INVARIANTS}`);
          if (inv.length) bad.push(`${ex} + ${ty}: ${inv.join("; ")}`);
        }
        return { log: `${ex}: added ${types.length} types`, extra: bad };
      },
      () => A.generate(),
      () => A.saveReopen(),
    ]),
  "imported STLs: every type before and after Apply, picks, generate": () =>
    STLS.flatMap(([name]) => [
      () => A.importStl(r0, name, name === "knob.stl" ? "rotary" : "vise"),
      async () => {
        const types = await js("[...document.querySelectorAll('#treeAddType option')].map((o) => o.value)");
        const bad = [];
        for (const ty of types) {
          await A.addOp(r0, ty);
          // before Apply: to the next type and back
          // (an Add list entry may be a type's preset, "rough3d:indexed": its type in the form)
          const other = types[(types.indexOf(ty) + 1) % types.length];
          await A.changeType(r0, other);
          await A.changeType(r0, ty);
          const inv = await js(`await ${INVARIANTS}`);
          if (inv.length) bad.push(`${name} ${ty}<->${other}: ${inv.join("; ")}`);
          const msg = await A.apply();
          if (/refused/.test(msg)) bad.push(`${name}: ${ty} refused after a round trip of types: ${msg}`);
        }
        return { log: `${name}: ${types.length} types round-tripped`, extra: bad };
      },
      async () => {
        // the first operation selected (a click on a selected one would unselect it)
        const id = await js("document.querySelector('#tree .tree-op')?.dataset.op || ''");
        if (!id) return null;
        if ((await js("window.__marcam.state.tree.selected")) !== id) await A.select({ pick: () => id });
        return `select ${id}`;
      },
      () => A.pick(rng(11)),
      () => A.generate(),
    ]),
};

// ------------------------------------------------------------------ run
let failed = false;
try {
  await S("Page.navigate", { url });
  await waitFor(() => js("document.body.dataset.state === 'ready'"), "the engine", 60000);
  // (written with an import's starter operations: asked for, as a caller may; Mar: the app
  // itself starts a new project with none)
  await js("localStorage.setItem('marcam.starters', '1'); window.__marcam.project.settings.starters = true; true");
  if (ONLY !== "monkey") {
    for (const [name, steps] of Object.entries(SCENARIOS).filter(([n]) => n.includes(MATCH))) {
      try {
        const log = await run(name, steps(), true);
        report(`scenario: ${name}`, true, `${log.length} steps`);
      } catch (e) {
        failed = true;
        report(`scenario: ${name}`, false, e.message);
        console.log(`  steps: ${(e.log || []).join(" | ")}`);
        await shot(`stress_fail_${name.replace(/\W+/g, "_").slice(0, 40)}`);
      }
    }
  }
  if (ONLY !== "scenarios") {
    const table = WEIGHTS.flatMap(([n, w]) => Array(w).fill(n));
    for (const seed of seeds) {
      const r = rng(seed);
      const steps = [() => A.loadExample(r)];
      for (let k = 0; k < STEPS; k++) steps.push(() => act(r.pick(table), r));
      // the history: undo all gives the project as opened, redo all the final one
      steps.push(() => A.historyRoundTrip());
      try {
        const log = await run(`monkey seed ${seed}`, steps);
        report(`monkey seed ${seed}`, true, `${log.length} actions`);
      } catch (e) {
        failed = true;
        report(`monkey seed ${seed}`, false, e.message);
        console.log(`  replay: node web/tests/stress.mjs --only monkey --seeds ${seed} --steps ${STEPS}`);
        console.log(`  actions: ${(e.log || []).map((x, i) => `${i + 1}. ${x}`).join("\n           ")}`);
        await shot(`stress_fail_seed_${seed}`);
        // a fresh page for the next seed
        await S("Page.navigate", { url });
        await waitFor(() => js("document.body.dataset.state === 'ready'"), "the engine", 60000);
      }
    }
  }
} catch (e) {
  failed = true;
  console.log(`FAIL  ${e.message}`);
} finally {
  // (closed in order: the browser asked, our socket closed, then whatever is left killed)
  closing = true;
  try { await Promise.race([send("Browser.close"), sleep(2000)]); } catch {}
  try { ws.onmessage = null; ws.close(); } catch {}
  await sleep(300);
  cleanup();
  await sleep(300);
  try { rmSync(profile, { recursive: true, force: true }); } catch {}
}
const bad = results.filter((x) => !x.ok).length;
console.log(`\n${results.length - bad}/${results.length} passed`);
process.exit(failed || bad ? 1 : 0);
