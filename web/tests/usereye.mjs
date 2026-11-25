// The "user-eye" suite (Mar: features reported live that were not on the screen). It drives
// the app only as a person does, and believes only what is on the screen:
// - real mouse clicks and drags at the screen positions of visible things, typing into the
//   focused field, a file through the file chooser; no app internals, no window.* hooks, no
//   state set directly (the page's DOM is read only for where visible things are, and what
//   they say);
// - asserts on screenshots (pixels of the visible view), what is under the pointer, how many
//   3D views are drawn; never on the app's own variables.
// Mar's path: a fresh page, New project, the knob STL imported, the setup switched to a
// 3-axis vise; the handles there (their red, green and blue in the view), the stock's size
// typed digit by digit (the stock drawn wider as typed), a handle dragged (the part moved on
// screen, its value shown), planned again with nothing pressed; no dialog, no Generate card,
// no red error on the way. Then a setup added (the cut result carried): its handles too.
//
//   node web/tests/usereye.mjs            served from this checkout, as :8000 serves it
//   node web/tests/usereye.mjs --url http://127.0.0.1:8000/web/   (after a deploy)
//
// Screenshots go to web/tests/screenshots/usereye/ (kept on a failure for CI).

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";
import { findBrowser, freePort, httpGet, sleep, waitFor } from "./harness.mjs";
import { finishedPartStl } from "./finished_part.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const shots = join(root, "web", "tests", "screenshots", "usereye");
mkdirSync(shots, { recursive: true });
const arg = (k) => {
  const i = process.argv.indexOf(k);
  return i > 0 ? process.argv[i + 1] : null;
};

// ---------------------------------------------------------------- PNG pixels
function png(buf) {
  let p = 8, w = 0, h = 0, ct = 0;
  const idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p);
    const type = buf.toString("ascii", p + 4, p + 8);
    const data = buf.subarray(p + 8, p + 8 + len);
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      ct = data[9];
    } else if (type === "IDAT") idat.push(data);
    p += 12 + len;
  }
  const bpp = ct === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const out = Buffer.alloc(w * h * bpp);
  const stride = w * bpp;
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)];
    const row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? out[y * stride + x - bpp] : 0;
      const b = y > 0 ? out[(y - 1) * stride + x] : 0;
      const c = x >= bpp && y > 0 ? out[(y - 1) * stride + x - bpp] : 0;
      let v = row[x];
      if (f === 1) v += a;
      else if (f === 2) v += b;
      else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) {
        const pp = a + b - c, pa = Math.abs(pp - a), pb = Math.abs(pp - b), pc = Math.abs(pp - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      out[y * stride + x] = v & 255;
    }
  }
  return { w, h, bpp, px: out, at: (x, y) => [out[(y * w + x) * bpp], out[(y * w + x) * bpp + 1], out[(y * w + x) * bpp + 2]] };
}

// ---------------------------------------------------------------- the page
async function start() {
  const children = [];
  const cleanup = () => children.forEach((c) => { try { c.kill(); } catch {} });
  process.on("exit", cleanup);
  let url = arg("--url");
  if (!url) {
    // served as :8000 is served (the same server: its caching headers too)
    const port = await freePort();
    children.push(spawn(process.platform === "win32" ? "python" : "python3", [join(root, "scripts", "serve.py"), String(port), root], { stdio: "ignore" }));
    url = `http://127.0.0.1:${port}/web/`;
    // (node:http, its body read: Node's fetch left a body unread and undici asserted on
    // Windows CI, `assert(!this.paused)`, after e2e had passed)
    await waitFor(async () => { try { return (await httpGet(url)).ok; } catch { return false; } }, "the server", 15000);
  }
  const profile = mkdtempSync(join(tmpdir(), "marcam-eye-"));
  const dport = await freePort();
  children.push(spawn(findBrowser(), ["--headless=new", `--remote-debugging-port=${dport}`, `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", ...(process.env.MARCAM_E2E_SOFTWARE_GL ? ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] : []), "about:blank"], { stdio: "ignore" }));
  const version = await waitFor(async () => { try { return (await httpGet(`http://127.0.0.1:${dport}/json/version`)).json(); } catch { return null; } }, "the browser", 20000);
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener("open", r, { once: true }));
  let n = 0;
  const waiters = new Map();
  const events = [];
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && waiters.has(msg.id)) {
      const w = waiters.get(msg.id);
      waiters.delete(msg.id);
      msg.error ? w.reject(new Error(msg.error.message)) : w.resolve(msg.result);
    } else if (msg.method) {
      events.push(msg);
      // (a page's own question: leaving with unsaved changes, a confirm) answered OK, as a
      // person clicks it; left open, the page waits on it and every later step hangs
      if (msg.method === "Page.javascriptDialogOpening") ws.send(JSON.stringify({ id: ++n, method: "Page.handleJavaScriptDialog", params: { accept: true }, ...(msg.sessionId ? { sessionId: msg.sessionId } : {}) }));
    }
  });
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++n;
    waiters.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  const S = (m, p) => send(m, p, sessionId);
  await S("Page.enable");
  await S("Runtime.enable");
  await S("DOM.enable");
  await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await S("Page.setInterceptFileChooserDialog", { enabled: true });
  // reading the page: where visible things are, and what they say (never the app's variables)
  const read = async (expr) => {
    const r = await S("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  await S("Page.navigate", { url });
  return { S, read, events, url };
}

/** A part like an impeller (a hub along X, three blades out to radius 18), as binary STL
 *  (about 24,000 triangles). */
function bladesStl() {
  const tris = [];
  const quad = (a, b, c, d) => tris.push([a, b, c], [a, c, d]);
  // (a hub of 4000 sides: over 20,000 triangles, as big a file as the page shares the angle
  // search out for)
  const n = 4000;
  const rp = (k, r, x) => {
    const t = (k / n) * 2 * Math.PI;
    return [x, r * Math.cos(t), r * Math.sin(t)];
  };
  for (let k = 0; k < n; k++) {
    quad(rp(k, 6, -30), rp(k + 1, 6, -30), rp(k + 1, 6, 0), rp(k, 6, 0));
    quad([-30, 0, 0], rp(k + 1, 6, -30), rp(k, 6, -30), [-30, 0, 0]);
    quad([0, 0, 0], rp(k, 6, 0), rp(k + 1, 6, 0), [0, 0, 0]);
  }
  for (let b = 0; b < 3; b++) {
    const t = (b / 3) * 2 * Math.PI;
    const [c, s] = [Math.cos(t), Math.sin(t)];
    const at = (x, r, w) => [x, r * c - w * s, r * s + w * c];
    const [x0, x1, r0, r1, w] = [-28, -2, 5, 18, 1.5];
    const P = [[x0, r0, -w], [x1, r0, -w], [x1, r1, -w], [x0, r1, -w], [x0, r0, w], [x1, r0, w], [x1, r1, w], [x0, r1, w]].map(([x, r, ww]) => at(x, r, ww));
    for (const f of [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [2, 3, 7, 6], [1, 2, 6, 5], [0, 4, 7, 3]]) quad(P[f[0]], P[f[1]], P[f[2]], P[f[3]]);
  }
  const buf = Buffer.alloc(84 + 50 * tris.length);
  buf.writeUInt32LE(tris.length, 80);
  tris.forEach((t, i) => t.forEach((p, j) => p.forEach((v, k) => buf.writeFloatLE(v, 84 + 50 * i + 12 + 12 * j + 4 * k))));
  return buf;
}

const { S, read, events, url } = await start();
const fails = [];
let step = 0;
const shot = async (name) => {
  const { data } = await S("Page.captureScreenshot", { format: "png" });
  const buf = Buffer.from(data, "base64");
  writeFileSync(join(shots, `${String(++step).padStart(2, "0")}_${name}.png`), buf);
  return png(buf);
};
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
  if (!ok) fails.push(name);
};

// ---- visible things, by what a person sees
const VISIBLE = `(e) => { if (!e) return false; const r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) return false; const s = getComputedStyle(e); if (s.visibility === 'hidden' || s.display === 'none' || +s.opacity === 0) return false; const c = document.elementFromPoint(Math.min(innerWidth - 1, Math.max(0, r.left + r.width / 2)), Math.min(innerHeight - 1, Math.max(0, r.top + r.height / 2))); return !!c && (c === e || e.contains(c) || c.contains(e)); }`;
/** The centre of the visible element (`css` selector, its text matching `text`). */
const where = (css, text) => read(`(() => { const vis = ${VISIBLE}; const re = ${text ? `new RegExp(${JSON.stringify(text)}, 'i')` : "null"}; for (const e of document.querySelectorAll(${JSON.stringify(css)})) { if (re && !re.test(e.textContent.trim() || e.value || e.title || '')) continue; if (e.getBoundingClientRect().width < 2) continue; e.scrollIntoView({ block: 'nearest' }); if (!vis(e)) continue; const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; } return null; })()`);
const click = async (css, text) => {
  const p = await where(css, text);
  if (!p) throw new Error(`nothing visible to click: ${css} ${text ?? ""}`);
  await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: p[0], y: p[1] });
  await S("Input.dispatchMouseEvent", { type: "mousePressed", x: p[0], y: p[1], button: "left", clickCount: 1 });
  await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: p[0], y: p[1], button: "left", clickCount: 1 });
  return p;
};
const drag = async (from, to, steps = 12) => {
  await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: from[0], y: from[1] });
  await sleep(50);
  await S("Input.dispatchMouseEvent", { type: "mousePressed", x: from[0], y: from[1], button: "left", clickCount: 1 });
  for (let i = 1; i <= steps; i++) await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: from[0] + ((to[0] - from[0]) * i) / steps, y: from[1] + ((to[1] - from[1]) * i) / steps, button: "left", buttons: 1 });
  const mid = await read("document.getElementById('handleReadout') && getComputedStyle(document.getElementById('handleReadout')).display !== 'none' && !document.getElementById('handleReadout').classList.contains('hidden') ? document.getElementById('handleReadout').textContent : ''");
  await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: to[0], y: to[1], button: "left", clickCount: 1 });
  return mid;
};
const key = async (k, mods = 0) => {
  await S("Input.dispatchKeyEvent", { type: "keyDown", key: k, modifiers: mods, windowsVirtualKeyCode: k.length === 1 ? k.toUpperCase().charCodeAt(0) : 0 });
  await S("Input.dispatchKeyEvent", { type: "keyUp", key: k, modifiers: mods });
};
const type = async (ch) => {
  // (a full stop's key code is 190: its character code, 46, is the Delete key's)
  await S("Input.dispatchKeyEvent", { type: "keyDown", key: ch, text: ch, windowsVirtualKeyCode: ch === "." ? 190 : ch === "-" ? 189 : ch.charCodeAt(0) });
  await S("Input.dispatchKeyEvent", { type: "keyUp", key: ch });
};
const viewRect = () => read("(() => { const r = document.getElementById('view3d').getBoundingClientRect(); return [r.left, r.top, r.width, r.height].map(Math.round); })()");
/** The negative checks, at every step: no dialog up, no Generate card, no red error. */
const clean = async (what) => {
  const s = await read(`(() => { const vis = ${VISIBLE}; return { dialog: [...document.querySelectorAll('dialog')].some((d) => d.open && !d.classList.contains('embedded') && vis(d)), generate: [...document.querySelectorAll('button')].some((b) => /^Generate$/.test(b.textContent.trim()) && vis(b)) || (vis(document.getElementById('emptyState')) && /\\bGenerate\\b/.test(document.getElementById('emptyState').textContent)), error: vis(document.getElementById('error')) ? document.getElementById('error').textContent.trim() : '' }; })()`);
  check(`${what}: no dialog, no Generate card, no red error`, !s.dialog && !s.generate && !s.error, JSON.stringify(s));
};
/** The visible 3D views (canvases of the view's size), and whether the one at its centre is
 *  what is under the pointer there. */
const views = () => read(`(() => { const vis = ${VISIBLE}; const host = document.getElementById('view3d'); const cs = [...host.querySelectorAll('canvas')].filter((c) => c.getBoundingClientRect().width > 200 && vis(c)); const r = host.getBoundingClientRect(); const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return { n: cs.length, top: top?.tagName }; })()`);
/** Pixels of the handles' colours in the view: red, green, blue (pure, bright). */
function handleColours(img, [x0, y0, w, h]) {
  const n = { red: 0, green: 0, blue: 0 };
  const pts = { red: [], green: [], blue: [] };
  for (let y = y0; y < y0 + h; y += 1)
    for (let x = x0; x < x0 + w; x += 1) {
      const [r, g, b] = img.at(x, y);
      const k = r > 200 && g < 70 && b < 70 ? "red" : g > 200 && r < 90 && b < 90 ? "green" : b > 200 && r < 70 && g < 90 ? "blue" : null;
      if (k) {
        n[k]++;
        if (pts[k].length < 4000) pts[k].push([x, y]);
      }
    }
  return { n, pts };
}
/** The centroid of the part's colour (#c8ccd2, lit: greys) in the view. */
/** Where the part is drawn on the screen: its drawn meshes' box (the part's group, or the
 *  playback's copy of it) projected by the camera, `{cx, w, h}` in page pixels; null when
 *  none is drawn. (CI's software GL: grey pixels matched the stock and the panes as well,
 *  a drag of 60 px read as 2) */
const partDrawn = () => read(`(() => { const v = window.__marcam.state.three; const T = v.THREE; const shown = (o) => { for (let x = o; x; x = x.parent) { if (!x.visible) return false; if (x === v.scene) return true; } return false; }; const box = new T.Box3(); const add = (root) => root?.traverse((o) => { if ((o.isMesh) && shown(o) && o.geometry?.attributes?.position?.count) { o.updateMatrixWorld(true); if (!o.geometry.boundingBox) o.geometry.computeBoundingBox(); box.union(o.geometry.boundingBox.clone().applyMatrix4(o.matrixWorld)); } }); add(v.groups.part); v.groups.work?.children.forEach((c) => c.userData.shared && add(c)); if (box.isEmpty()) return null; const el = v.renderer.domElement, rc = el.getBoundingClientRect(); let lo = [Infinity, Infinity], hi = [-Infinity, -Infinity]; for (const x of [box.min.x, box.max.x]) for (const y of [box.min.y, box.max.y]) for (const z of [box.min.z, box.max.z]) { const p = new T.Vector3(x, y, z).project(v.camera); const sx = rc.left + ((p.x + 1) / 2) * rc.width, sy = rc.top + ((1 - p.y) / 2) * rc.height; lo = [Math.min(lo[0], sx), Math.min(lo[1], sy)]; hi = [Math.max(hi[0], sx), Math.max(hi[1], sy)]; } return { cx: (lo[0] + hi[0]) / 2, cy: (lo[1] + hi[1]) / 2, w: Math.round(hi[0] - lo[0]), h: Math.round(hi[1] - lo[1]) }; })()`);
try {
  // ---------------------------------------------------------------- a fresh page
  await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
  await sleep(1500);
  // New project (File > New)
  await click("#menuFile > summary");
  await sleep(200);
  await click("#projectNew");
  await sleep(1200);
  await clean("New project");
  // (Mar's left pane) no Machine and no Attach mesh at its top; "No setups yet" and its
  // button; Add setup makes one at once (a plate), its part then loaded on its Part line
  const top = await read(`(() => { const vis = ${VISIBLE}; return { machine: vis(document.getElementById('machine')), mesh: vis(document.getElementById('mesh')) }; })()`);
  check("a new project: no Machine and no Attach mesh at the top of the left pane", !top.machine && !top.mesh, JSON.stringify(top));
  const prompt = await read("document.getElementById('emptyState')?.textContent || ''");
  check("a new project says \"No setups yet\" with its + Add setup button (no Import STL there)", /No setups yet/.test(prompt) && !!(await where("#emptyState button", "Add setup")) && !(await where("#emptyState button", "Import STL")), prompt.trim());
  await click("#emptyState button", "Add setup");
  await waitFor(() => where("#tree .tree-setup [data-act=setup]"), "the setup added", 60000).catch(() => {});
  await sleep(1500);
  await clean("Add setup with nothing loaded");
  check("Add setup with nothing loaded: a setup at once (a plate)", /block|plate/i.test(await read("document.querySelector('#tree .tree-setup.active')?.textContent || ''")), await read("document.querySelector('#tree .tree-setup.active')?.textContent.trim().slice(0, 80) || ''"));
  await click("#tree .tree-setup.active [data-act=part]");
  await sleep(800);
  const stockShown = () => read("document.querySelector('#tree .tree-setup.active [data-act=stock]')?.textContent.trim() || ''");
  const plate = await stockShown();
  // the knob's STL through the Part line's file chooser
  const chooser = new Promise((resolve) => {
    const t0 = events.length;
    const tick = setInterval(() => {
      const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
      if (e) {
        clearInterval(tick);
        resolve(e.params);
      }
    }, 50);
  });
  await click("#ptReplace", "Import model");
  const fc = await Promise.race([chooser, sleep(10000).then(() => null)]);
  if (!fc) throw new Error("no file chooser opened by the Part line's Import model");
  await S("DOM.setFileInputFiles", { files: [join(root, "examples", "knob", "knob.stl")], backendNodeId: fc.backendNodeId });
  await sleep(1500);
  await clean("Import model (the Part line)");
  await waitFor(() => read("/knob/.test(document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || '')"), "the knob as the setup's part", 60000);
  // (Mar: "importing a model shouldn't change the stock") the plate as it was, the knob in it
  check("importing a model leaves the stock as it was", (await stockShown()) === plate && /\d/.test(plate), `${plate} -> ${await stockShown()}`);
  // the setup selected: switched to a 3-axis vise in its details
  await click("#tree .tree-setup [data-act=setup]");
  await sleep(800);
  await clean("the setup selected");
  const vise = await where(".sd-kind", "3-axis vise");
  check("the setup's type can be changed in its details (no Edit setup button, no dialog)", !!vise && !(await where("button", "^Edit setup")));
  if (vise) {
    await click(".sd-kind", "3-axis vise");
    await waitFor(() => read("/vise/i.test(document.querySelector('#tree .tree-setup.active')?.textContent || '')"), "the setup a vise", 60000).catch(() => {});
    await sleep(2500);
  }
  await clean("switched to a vise");
  const r = await viewRect();
  let img = await shot("vise_setup_selected");
  let v = await views();
  check("exactly one 3D view drawn, and it is what is under the pointer", v.n === 1 && v.top === "CANVAS", JSON.stringify(v));
  let hc = handleColours(img, r);
  check("the setup selected: handles in the view (red, green and blue)", hc.n.red > 40 && hc.n.green > 40 && hc.n.blue > 40, JSON.stringify(hc.n));
  // the Stock: its handles; its X size typed digit by digit, drawn wider as typed
  await click("#tree [data-act=stock]");
  await sleep(1000);
  const paneShown = (id) => read(`(() => { const vis = ${VISIBLE}; const e = document.getElementById('${id}'); if (!e) return false; e.scrollIntoView({ block: 'nearest' }); return vis(e.querySelector('.tree-outname') || e); })()`);
  check("clicking the Stock line (on its sizes) shows the Stock's pane", await paneShown("stockPane") && !(await paneShown("setupHost")));
  img = await shot("stock_selected");
  hc = handleColours(img, r);
  check("the Stock selected: handles in the view", hc.n.red > 40 && hc.n.green > 40 && hc.n.blue > 40, JSON.stringify(hc.n));
  // (how wide the stock is drawn, in the view's pixels: its box through the view's camera,
  // within the view. By its outline's colour the span came out short on CI: software GL draws
  // thin lines another way, and a long stock's edges were only partly found)
  const drawnSpan = () => read("(() => { const v = window.__marcam.state.three; const g = v.groups.stock; g.updateMatrixWorld(true); const b = new v.THREE.Box3().setFromObject(g); if (b.isEmpty()) return 0; const el = v.renderer.domElement; const w = el.clientWidth; let lo = Infinity, hi = -Infinity; for (const x of [b.min.x, b.max.x]) for (const y of [b.min.y, b.max.y]) for (const z of [b.min.z, b.max.z]) { const p = new v.THREE.Vector3(x, y, z).project(v.camera); const sx = Math.max(0, Math.min(w, ((p.x + 1) / 2) * w)); lo = Math.min(lo, sx); hi = Math.max(hi, sx); } return Math.round(hi - lo); })()");
  const span0 = await drawnSpan();
  const field = await where("#stX") || await where("#stLen");
  check("the stock's size is a field in the pane", !!field);
  const spans = [];
  if (field) {
    await click(await read("document.getElementById('stX') ? '#stX' : '#stLen'"));
    await key("a", 2);
    for (const ch of ["1", "5", "0"]) {
      await type(ch);
      await sleep(900);
      await shot(`typed_${ch}`);
      spans.push(await drawnSpan());
    }
    const shown = await read("(document.getElementById('stX') || document.getElementById('stLen')).value");
    check("typing 150 shows 150 and the stock is drawn bigger as it is typed", shown === "150" && spans[2] > spans[0] && spans[2] > span0, `shown ${shown}; spans ${span0} -> ${spans.join(", ")}`);
  }
  await clean("the stock typed");
  // the Part: its handles; the X arrow dragged, the part moved on screen, its value shown
  await click("#tree [data-act=part]");
  await sleep(1000);
  check("clicking the Part line (on its file name) shows the Part's pane", await paneShown("partPane"));
  img = await shot("part_selected");
  hc = handleColours(img, r);
  check("the Part selected: handles in the view", hc.n.red > 40 && hc.n.green > 40 && hc.n.blue > 40, JSON.stringify(hc.n));
  const before = (await partDrawn())?.cx;
  if (hc.pts.red.length) {
    // the red arrow: from its pixels' middle, outward from the handles' middle
    const mid = (ps) => ps.reduce((a, p) => [a[0] + p[0] / ps.length, a[1] + p[1] / ps.length], [0, 0]);
    const c = mid([...hc.pts.green, ...hc.pts.blue]);
    const red = mid(hc.pts.red);
    const d = [red[0] - c[0], red[1] - c[1]];
    let l = Math.hypot(...d) || 1;
    // (the arrow's shaft starts at the middle, the ring goes round it: the red nearest the
    // middle, a little out, is the arrow)
    const near = hc.pts.red.filter((p) => Math.hypot(p[0] - c[0], p[1] - c[1]) > 12).sort((a, b) => Math.hypot(a[0] - c[0], a[1] - c[1]) - Math.hypot(b[0] - c[0], b[1] - c[1]));
    // (the arrow where the handles say it is: red pixels may be its ring's too)
    const ax = await read("JSON.stringify(window.__marcam.handles.screenOf('translate', 'X'))").then((t) => (t ? JSON.parse(t) : null));
    const from = ax || near[Math.min(near.length - 1, 6)] || red;
    d[0] = from[0] - c[0];
    d[1] = from[1] - c[1];
    l = Math.hypot(...d) || 1;
    const shownMid = await drag(from, [from[0] + (60 * d[0]) / l, from[1] + (60 * d[1]) / l]);
    await sleep(2500);
    img = await shot("part_dragged");
    const after = (await partDrawn())?.cx;
    check("a handle dragged: its value shown while dragging, the part moved on screen", /[XYZ] [+-][\d.]+ /.test(shownMid) && before != null && after != null && Math.abs(after - before) > 3, `shown "${shownMid}"; part x ${before?.toFixed(0)} -> ${after?.toFixed(0)}`);
  }
  await clean("the part dragged");
  // planned again with nothing pressed: the status moves on by itself
  const status = await read("document.getElementById('status').textContent");
  check("planned again by itself (no Generate pressed)", !/Generate/.test(status), status);
  // an inch file: the Part's button makes it 25.4 times bigger (the stock fitted round it),
  // Ctrl+Z takes it back
  const stockLine = () => read("document.querySelector('#tree .tree-setup.active ~ * [data-act=stock], #tree [data-act=stock]')?.textContent || ''");
  const firstNum = (t) => +(/([\d.]+)\s*x/.exec(t)?.[1] ?? NaN);
  const s0 = await stockLine();
  await click("#ptInch");
  await sleep(2500);
  const s1 = await stockLine();
  await key("z", 2);
  await sleep(2000);
  const s2 = await stockLine();
  check("an inch file: the Part's button makes it 25.4 times bigger, the stock fitted round it; Ctrl+Z takes it back", firstNum(s1) > 500 && firstNum(s2) === firstNum(s0), `${s0} | ${s1} | ${s2}`);
  await clean("the part in inches");
  // ---------------------------------------------------------------- placement that sticks
  // (Mar: "they just snap back"; "typing ... doesn't work either") every placement judged
  // after release and after the plan has settled, on the screen: in setup 1, in setup 2
  // continuing from setup 1 (the carried piece moves), in setup 2 independent
  const settled = async () => {
    await waitFor(() => read("document.body.dataset.state !== 'busy'"), "the app at rest", 120000).catch(() => {});
    await sleep(2500);
  };
  const partBox = (img, [x0, y0, w, h]) => {
    let lo = [Infinity, Infinity], hi = [-Infinity, -Infinity], sx = 0, n = 0;
    for (let y = y0; y < y0 + h; y += 2)
      for (let x = x0; x < x0 + w; x += 2) {
        const [rr, g, b] = img.at(x, y);
        if (rr > 120 && Math.abs(rr - g) < 12 && Math.abs(g - b) < 14 && rr < 245) {
          lo = [Math.min(lo[0], x), Math.min(lo[1], y)];
          hi = [Math.max(hi[0], x), Math.max(hi[1], y)];
          sx += x;
          n++;
        }
      }
    return n ? { cx: sx / n, w: hi[0] - lo[0], h: hi[1] - lo[1], n } : null;
  };
  /** Type `text` into field `css` (its old text replaced) and Tab out. */
  const typeInto = async (css, text) => {
    await click(css);
    await key("a", 2);
    for (const ch of text) await type(ch);
    await S("Input.dispatchKeyEvent", { type: "keyDown", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    await S("Input.dispatchKeyEvent", { type: "keyUp", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
  };
  const openMore = async (pane) => {
    const open = await read(`!!document.querySelector('#${pane} details.op-more[open]')`);
    if (!open) await click(`#${pane} details.op-more > summary`);
    await sleep(300);
  };
  /** The red arrow dragged 60 px outward; then where the part is, settled. */
  const dragArrow = async () => {
    const img0 = await shot("before_drag");
    const hc0 = handleColours(img0, r);
    if (!hc0.pts.red.length) return { moved: false, why: "no red handle" };
    const mid = (ps) => ps.reduce((a, p) => [a[0] + p[0] / ps.length, a[1] + p[1] / ps.length], [0, 0]);
    const c = mid([...hc0.pts.green, ...hc0.pts.blue]);
    const near = hc0.pts.red.filter((p) => Math.hypot(p[0] - c[0], p[1] - c[1]) > 12).sort((a, b) => Math.hypot(a[0] - c[0], a[1] - c[1]) - Math.hypot(b[0] - c[0], b[1] - c[1]));
    const ax = await read("JSON.stringify(window.__marcam.handles.screenOf('translate', 'X'))").then((t) => (t ? JSON.parse(t) : null));
    const from = ax || near[Math.min(near.length - 1, 6)];
    const d = [from[0] - c[0], from[1] - c[1]];
    const l = Math.hypot(...d) || 1;
    const b0 = await partDrawn();
    await drag(from, [from[0] + (60 * d[0]) / l, from[1] + (60 * d[1]) / l]);
    await settled();
    await shot("after_drag_settled");
    const b1 = await partDrawn();
    return { moved: !!(b0 && b1 && Math.abs(b1.cx - b0.cx) > 3), why: `part x ${b0?.cx.toFixed(0)} -> ${b1?.cx.toFixed(0)}` };
  };
  /** A typed value: it stays in its field after the plan settles, and the part moves. */
  const typed = async (pane, css, text) => {
    await openMore(pane);
    await shot("before_typed");
    const b0 = await partDrawn();
    await typeInto(css, text);
    await settled();
    await openMore(pane);
    const shown = await read(`document.querySelector('${css}')?.value`);
    await shot("after_typed");
    const b1 = await partDrawn();
    return { ok: shown === text && !!(b0 && b1 && (Math.abs(b1.cx - b0.cx) > 3 || Math.abs(b1.w - b0.w) > 3 || Math.abs(b1.h - b0.h) > 3)), why: `shown ${shown}; part ${JSON.stringify(b0 && [b0.cx.toFixed(0), b0.w, b0.h])} -> ${JSON.stringify(b1 && [b1.cx.toFixed(0), b1.w, b1.h])}` };
  };
  // setup 1: the part's position and angle typed (the handle drag above, settled, below)
  await click("#tree [data-act=part]");
  await settled();
  let res = await dragArrow();
  check("setup 1: the part dragged stays moved after release and the plan settling", res.moved, res.why);
  res = await typed("partPane", "#ptTX", "7");
  check("setup 1: a position typed (X 7) stays and moves the part", res.ok, res.why);
  res = await typed("partPane", "#ptRZ", "90");
  check("setup 1: an angle typed (Z 90) stays and turns the part", res.ok, res.why);
  await clean("setup 1 placed");
  // ---------------------------------------------------------------- the part's presets (vise)
  // (Mar: preset buttons) the knob in the vise: Center XY puts its middle at the block's, Top
  // flush its top at the block's top, Z +90° turns it (its outline on the screen turns); each
  // one edit. In setup 1, then in setup 2 independent (its own block), then continuing (the
  // piece itself is the block: turns only)
  const placed3 = () => read(`(async () => { const M = window.__marcam; const d = JSON.parse(document.getElementById('job').value); const id = M.state.tree.setup; const [lo, hi] = (await M.engine('meshTriangles', d, { setup: id }, M.state.mesh)).result.bounds; const sb = (await M.engine('setupFrame', d, { setup: id })).result.stock_box; return { mid: [0, 1, 2].map((i) => (lo[i] + hi[i]) / 2), top: hi[2], size: [0, 1, 2].map((i) => hi[i] - lo[i]), sb }; })()`);
  const presets3 = async (where_, positions) => {
    await click("#tree .tree-setup.active [data-act=part]");
    await settled();
    if (positions) {
      await click(".part-presets button", "^Center XY$");
      await settled();
      let p = await placed3();
      const cx = (p.sb.min[0] + p.sb.max[0]) / 2, cy = (p.sb.min[1] + p.sb.max[1]) / 2;
      check(`${where_}: Center XY puts the part's middle at the block's`, Math.abs(p.mid[0] - cx) < 0.01 && Math.abs(p.mid[1] - cy) < 0.01, `part ${p.mid.slice(0, 2).map((v) => v.toFixed(3))}, block ${[cx, cy].map((v) => v.toFixed(3))}`);
      await click(".part-presets button", "^Top flush$");
      await settled();
      p = await placed3();
      check(`${where_}: Top flush puts the part's top at the block's top`, Math.abs(p.top - p.sb.max[2]) < 0.01, `${p.top.toFixed(3)} vs ${p.sb.max[2]}`);
    }
    const before = await placed3();
    const vr3 = await viewRect();
    const i0 = await shot("preset_before_turn");
    await click(".part-presets button", "^Z \\+90°$");
    await settled();
    const after = await placed3();
    const i1 = await shot("preset_after_turn");
    let px = 0;
    for (let y = vr3[1]; y < vr3[1] + vr3[3]; y += 2)
      for (let x = vr3[0]; x < vr3[0] + vr3[2]; x += 2) {
        const p = i0.at(x, y), q = i1.at(x, y);
        if (Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 60) px++;
      }
    const swapped = Math.abs(after.size[0] - before.size[1]) < 0.05 && Math.abs(after.size[1] - before.size[0]) < 0.05;
    check(`${where_}: Z +90° turns the part a quarter (its X and Y sizes swap; the screen changes)`, swapped && px > 150, `${before.size.slice(0, 2).map((v) => v.toFixed(2))} -> ${after.size.slice(0, 2).map((v) => v.toFixed(2))}; ${px} px changed`);
    await read("document.activeElement?.blur?.(); true");
    await key("z", 2);
    await settled();
    const undone = await placed3();
    check(`${where_}: Ctrl+Z takes the turn back (one edit)`, Math.abs(undone.size[0] - before.size[0]) < 0.05 && Math.abs(undone.size[1] - before.size[1]) < 0.05, `${undone.size.slice(0, 2).map((v) => v.toFixed(2))}`);
  };
  if (await read("JSON.parse(document.getElementById('job').value).setups[0].kind === 'vise'")) await presets3("setup 1", true);
  else check("setup 1 is a vise setup (for its presets)", false);
  // ---------------------------------------------------------------- the rotate rings
  // (Mar: "Bambu Studio-style") the part's Z ring dragged with the pointer on its ticks' band:
  // 5° at a time, released at exactly 40.00; then dragged inside the ring: free, an angle no
  // multiple of 5; both stay after the plan settles, and the part turns on the screen
  const ringAt = (ang, rf) => read(`JSON.stringify(window.__marcam.handles.screenOf('rotate', 'Z', ${ang}, ${rf}))`).then((s) => (s ? JSON.parse(s) : null));
  const rotZ = () => read("(() => { const d = JSON.parse(document.getElementById('job').value); const s = d.setups.find((x) => x.id === window.__marcam.state.tree.setup); const m = d.models.find((x) => s.part.includes(x.id) && x.kind === 'mesh'); return m?.rotate?.[2] ?? 0; })()");
  const turnRing = async (a0, a1, rf, name) => {
    const p0 = await ringAt(a0, 1);
    if (!p0) return { ok: false, why: "no Z ring" };
    const before = await shot(`${name}_before`);
    await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: p0[0], y: p0[1] });
    await sleep(60);
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: p0[0], y: p0[1], button: "left", clickCount: 1 });
    let seen = "";
    for (let i = 1; i <= 16; i++) {
      const q = await ringAt(a0 + ((a1 - a0) * i) / 16, 1 + (rf - 1) * Math.min(1, (2 * i) / 16));
      await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: q[0], y: q[1], button: "left", buttons: 1 });
      await sleep(20);
    }
    seen = await read("(() => { const e = document.getElementById('handleReadout'); return e && !e.classList.contains('hidden') ? e.textContent : ''; })()");
    const q = await ringAt(a1, rf);
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: q[0], y: q[1], button: "left", clickCount: 1 });
    await settled();
    const after = await shot(`${name}_after`);
    const vr = await viewRect();
    let px = 0;
    for (let y = vr[1]; y < vr[1] + vr[3]; y += 2)
      for (let x = vr[0]; x < vr[0] + vr[2]; x += 2) {
        const a = before.at(x, y), b = after.at(x, y);
        if (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 60) px++;
      }
    return { seen, px };
  };
  {
    await click("#tree .tree-setup.active [data-act=part]");
    await settled();
    const z0 = await rotZ();
    const band = await turnRing(120, 160, 1.12, "ring_band");
    const z1 = await rotZ();
    check("the Z ring on its band: 5° at a time, released at exactly 40.00 (said while turning); the part turns on the screen", Math.abs(z1 - z0 - 40) < 1e-9 && /Z: 40\.00/.test(band.seen || "") && band.px > 150, `${z0} -> ${z1}; "${band.seen}"; ${band.px} px`);
    const free = await turnRing(120, 157.3, 0.55, "ring_inside");
    const z2 = await rotZ();
    const d = z2 - z1;
    check("inside the ring: free (an angle no multiple of 5), kept after the plan settles", Math.abs(d) > 1 && Math.abs(d / 5 - Math.round(d / 5)) > 0.01 && free.px > 150, `${z1} -> ${z2}; "${free.seen}"; ${free.px} px`);
    await clean("the rings turned");
  }

  // ---------------------------------------------------------------- the cut result carried
  await click("#treeAddSetup");
  await sleep(2500);
  await clean("Add setup");
  img = await shot("setup_added");
  const stockOf = await read("document.getElementById('stType') ? document.getElementById('stType').selectedOptions[0]?.textContent : ''");
  check("a setup added at once, its Stock shown: the cut result of the one before", /Cut result/.test(stockOf), stockOf);
  hc = handleColours(img, r);
  check("the added setup's stock has handles in the view", hc.n.red > 40 && hc.n.green > 40 && hc.n.blue > 40, JSON.stringify(hc.n));
  v = await views();
  check("still exactly one 3D view", v.n === 1 && v.top === "CANVAS", JSON.stringify(v));
  // setup 2 continuing from setup 1: the carried piece moved by the handles and typed values
  await click("#tree .tree-setup.active [data-act=part]");
  await settled();
  res = await dragArrow();
  check("setup 2 (continuing): the carried piece dragged stays moved after the plan settles", res.moved, res.why);
  await click("#tree .tree-setup.active [data-act=stock]");
  await settled();
  res = await typed("stockPane", "#stOffX", "5");
  check("setup 2 (continuing): its offset typed (X 5) stays and moves the piece", res.ok, res.why);
  await clean("setup 2 continuing placed");
  // setup 2 independent: its own stock and part, placed as in setup 1
  await click("#tree .tree-setup.active [data-act=setup]");
  await settled();
  const src = await where("#stSource");
  check("setup 2: where it starts is a choice in its details (continuing / independent)", !!src);
  {
    // (an older build without the choice: its own stock picked in the Stock pane, as before)
    if (src) await read("(() => { const s = document.getElementById('stSource'); s.value = 'own'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    else {
      await click("#tree .tree-setup.active [data-act=stock]");
      await settled();
      await read("(() => { const t = document.getElementById('stType'); t.value = 'block'; t.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    }
    await settled();
    const own = await read("document.getElementById('stType')?.selectedOptions[0]?.textContent || ''");
    check("setup 2 independent: its own stock (not the cut result)", !!own && !/Cut result/.test(own), own);
    await click("#tree .tree-setup.active [data-act=part]");
    await settled();
    res = await dragArrow();
    check("setup 2 (independent): the part dragged stays moved after the plan settles", res.moved, res.why);
    res = await typed("partPane", "#ptTX", "4");
    check("setup 2 (independent): a position typed (X 4) stays and moves the part", res.ok, res.why);
    // back to continuing, by Undo (each change one step)
    // (out of the field first: Ctrl+Z in a field undoes its text; the app's edits undone one
    // at a time until the setup continues again, three of them: placed, dragged, independent)
    await read("document.activeElement?.blur?.(); true");
    for (let i = 0; i < 3; i++) {
      await key("z", 2);
      await sleep(900);
    }
    await settled();
    await click("#tree .tree-setup.active [data-act=setup]");
    await settled();
    const back = await read("document.getElementById('stSource')?.value");
    check("Undo takes setup 2 back to continuing from setup 1", back === "carry", back);
  }
  await clean("setup 2 independent placed");
  // the presets in setup 2: continuing (turns only: the piece is the block), then independent
  await presets3("setup 2 (continuing)", false);
  await click("#tree .tree-setup.active [data-act=setup]");
  await settled();
  await read("(() => { const s = document.getElementById('stSource'); s.value = 'own'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await settled();
  await presets3("setup 2 (independent)", true);

  // ---------------------------------------------------------------- an operation edited live
  // (Mar: "don't want the apply button ... as you change the toolpaths should be updating")
  // the plate example's pocket: its step down typed digit by digit, nothing clicked after;
  // the toolpath on the screen changes, the field keeps what was typed, Ctrl+Z brings it back
  await read("window.confirm = () => true; true");
  await read("(() => { const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#loadExample");
  await waitFor(() => read("(() => { try { return JSON.parse(document.getElementById('job').value).setups[0].operations.some((o) => o.id === 'op2') && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "the plate example", 180000);
  await settled();
  await click("#tree .tree-op[data-op=op2] .tree-label");
  await settled();
  const sdCss = "#opForm [name=step_down]";
  check("the pocket's form: no Apply or Cancel button anywhere", !!(await where(sdCss)) && !(await where("button", "^(Apply|Cancel)$")));
  const diff = (a, b, [x0, y0, w, h]) => {
    let n = 0;
    for (let y = y0; y < y0 + h; y += 2)
      for (let x = x0; x < x0 + w; x += 2) {
        const p = a.at(x, y), q = b.at(x, y);
        if (Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 60) n++;
      }
    return n;
  };
  const lr = await viewRect();
  // (the background simulation done first, each time: its stock coming in between two shots
  // was counted as the toolpath's change, on CI's slower software GL)
  const quiet = () => waitFor(() => read("!window.__marcam.simLock?.busy && (document.getElementById('simRing')?.classList.contains('hidden') ?? true)"), "the background simulation", 120000 * Number(process.env.MARCAM_PERF_SLACK || 1)).catch(() => {});
  await quiet();
  const v0 = await shot("pocket_before");
  const sd0 = await read(`document.querySelector('${sdCss}')?.value`);
  await click(sdCss);
  await key("a", 2);
  for (const ch of "2.5") {
    await type(ch);
    await sleep(120);
  }
  await settled();
  await quiet();
  const shownSd = await read(`document.querySelector('${sdCss}')?.value`);
  const focused = await read(`document.activeElement === document.querySelector('${sdCss}')`);
  const v1 = await shot("pocket_step_down_typed");
  const d1 = diff(v0, v1, lr);
  const saved = await read("JSON.parse(document.getElementById('job').value).setups[0].operations.find((o) => o.id === 'op2').step_down");
  check("a step down typed (2.5), nothing clicked: the toolpath on the screen changes", d1 > 150 && saved === 2.5, `pixels changed ${d1}; saved ${saved}`);
  check("the field keeps 2.5 and the focus after the plan settles", shownSd === "2.5" && focused, `shown ${shownSd}; focused ${focused}`);
  await clean("an operation edited live");
  await read("document.activeElement?.blur?.(); true");
  await key("z", 2);
  await settled();
  await quiet();
  const v2 = await shot("pocket_undone");
  const d2 = diff(v0, v2, lr);
  const back = await read(`document.querySelector('${sdCss}')?.value`);
  check("Ctrl+Z: the old toolpath back on the screen (one step for the typing)", back === sd0 && d2 < d1 / 3, `field ${back} (was ${sd0}); pixels off the old ${d2} (edit ${d1})`);
  // ---------------------------------------------------------------- the playback's controls
  // (Mar: "the play stop and rewind controls are gone") after a plan lands: the small play
  // button opens the controls; Play, Step, the end, the speed and the playhead on the screen
  // (nothing over them); Play moves the playhead, Pause stops it, the playhead dragged to its
  // start rewinds. Again after the playback was cancelled from the ring in the view's corner
  // (that left both the controls and the play button gone). In setup 1 and in setup 2.
  const playhead = () => read("+document.getElementById('simScrub').value");
  const controls = async (where_) => {
    await settled();
    // (the controls' state before ▶, just after, a second later: for CI's log)
    let trail = [];
    // (the playback's controls, or the small play button that opens them)
    if (!(await where("#simPlay"))) {
      const p = await where("#vtPlay");
      check(`${where_}: the play button is on the screen when the controls are folded`, !!p);
      if (!p) return;
      // (a person presses it once it is enabled: pressed while disabled it does nothing, and
      // on CI's software GL the setup's playback can still be opening)
      await waitFor(() => read("!document.getElementById('vtPlay').disabled"), "the play button enabled", 60000 * Number(process.env.MARCAM_PERF_SLACK || 1)).catch(() => {});
      const st = "(() => { const m = window.__marcam.state.machining; return (m.expanded ? 'E' : 'e') + (m.open ? 'O' : 'o') + (m.session ? 'S' : 's') + (document.getElementById('vtPlay').disabled ? 'd' : '') + '/' + (document.body.dataset.playback || '-'); })()";
      trail = [await read(st)];
      const at = await click("#vtPlay");
      // (what is under the point pressed, and how many play buttons the page has)
      trail.push(await read(`(() => { const e = document.elementFromPoint(${at[0]}, ${at[1]}); return 'hit:' + (e ? (e.id || e.tagName + '.' + e.className) + (e.closest('#vtPlay') ? '(in vtPlay)' : '') : 'none') + ' n:' + document.querySelectorAll('#vtPlay').length; })()`));
      trail.push(await read(st));
      await sleep(1000);
      trail.push(await read(st));
      // (a setup continuing from another: its stock worked out first, from the setup before,
      // planned and simulated; slow on CI's software GL, hence its slack)
      await waitFor(() => where("#simPlay"), "the playback's controls", 60000 * Number(process.env.MARCAM_PERF_SLACK || 1)).catch(() => {});
      await sleep(500);
      // (the play button starts it: paused, to be judged below)
      if (/Pause/.test(await read("document.getElementById('simPlay').textContent"))) await click("#simPlay");
    }
    const seen = {};
    for (const id of ["simPlay", "simStep", "simEnd", "simSpeed", "simScrub"]) seen[id] = !!(await where(`#${id}`));
    // (not there: what the playback was doing, for CI's log, where its screenshots are not)
    const why = Object.values(seen).every(Boolean) ? "" : ` trail ${trail.join(" ")} ${JSON.stringify(await read("(() => { const m = window.__marcam.state.machining; return { open: m.open, expanded: m.expanded, stock: m.stock, busy: m.busy, session: !!m.session, controls: document.getElementById('simControls').className, playback: document.body.dataset.playback, lock: window.__marcam.simLock?.busy, status: document.getElementById('status').textContent.slice(0, 120), error: document.getElementById('error').textContent.slice(0, 160) }; })()"))}`;
    if (why) await shot(`controls_missing_${where_.replace(/\W+/g, "_")}`);
    check(`${where_}: Play, Step, the end, the speed and the playhead on the screen`, Object.values(seen).every(Boolean), JSON.stringify(seen) + why);
    if (!seen.simPlay || !seen.simScrub) return;
    const p0 = await playhead();
    await click("#simPlay");
    await sleep(1500);
    const p1 = await playhead();
    await click("#simPlay");
    await sleep(300);
    const p2 = await playhead();
    await sleep(1000);
    const p3 = await playhead();
    check(`${where_}: Play moves the playhead, Pause stops it`, p1 > p0 && p3 === p2, `${p0} -> ${p1} (playing), ${p2} -> ${p3} (paused)`);
    // rewound: the playhead dragged to its start
    const sr = await read("(() => { const r = document.getElementById('simScrub').getBoundingClientRect(); return [r.left, r.top + r.height / 2, r.width]; })()");
    const knob = [sr[0] + 6 + ((sr[2] - 12) * p3) / Math.max(1, await read("+document.getElementById('simScrub').max")), sr[1]];
    await drag(knob, [sr[0] - 20, sr[1]]);
    await sleep(2500);
    const p4 = await playhead();
    check(`${where_}: the playhead dragged to its start rewinds to 0`, p4 === 0, `${p3} -> ${p4}`);
  };
  await controls("setup 1 (the plate)");
  // cancelled from the ring while the stock is simulated along: the controls (or the play
  // button) still there
  // (Mar's impeller, a local fixture never committed: its playback open, a finer grid, an
  // edit: the playback lost on the way left both the controls and the play button gone.
  // Skipped where the file is not, as in CI.)
  const imp = join(process.env.MARCAM_FIXTURES_LOCAL || join(root, "..", "fixtures-local"), "53_1.marcam");
  if (existsSync(imp)) {
    const doc = await S("DOM.getDocument");
    const { nodeId } = await S("DOM.querySelector", { nodeId: doc.root.nodeId, selector: "#projectOpenFile" });
    await S("DOM.setFileInputFiles", { files: [imp], nodeId });
    await waitFor(() => read("document.title.includes('53_1') && document.body.dataset.state !== 'busy'"), "the impeller project", 180000).catch(() => {});
    await settled();
    if (await where("#vtPlay")) await click("#vtPlay");
    await sleep(3000);
    await read("(() => { const s = document.getElementById('simRes'); s.value = 'ultra'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#tree .tree-op [data-act=enable]");
    await sleep(8000);
    await settled();
    const left = !!((await where("#simPlay")) || (await where("#vtPlay")));
    check("the impeller (local): a playback lost after a finer grid and an edit leaves its controls or the play button", left);
    await read("(() => { const s = document.getElementById('simRes'); s.value = 'normal'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await read("window.confirm = () => true; (() => { const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
  } else console.log("      (the impeller check skipped: no local fixture)");
  // (the simulation answered late, ?testSimDelayMs, a test's knob for timing only: its
  // ring up long enough to cancel it, or to open the project again over it)
  {
    const slow = url + (url.includes("?") ? "&" : "?") + "testSimDelayMs=2500";
    await S("Page.navigate", { url: slow });
    await waitFor(() => read("!!document.getElementById('loadExample') && !!window.__marcam?.project"), "the page (slow simulation)", 60000);
    await read("window.confirm = () => true; (() => { const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
    // the playback open (late), then the stock simulated along again (off, on): late too, its
    // ring up while the playback is open
    // (the play button does nothing until the playback is made: pressed again until it opens)
    const openPlayback = async () => {
      for (let i = 0; i < 30 && !(await where("#simPlay")); i++) {
        if (await read("!document.getElementById('vtPlay').disabled")) await click("#vtPlay").catch(() => {});
        await sleep(3000);
      }
      return where("#simPlay");
    };
    const resimulate = async () => {
      const on = (await read("document.querySelector('button[data-v=stock]')?.dataset.state")) === "on";
      if (on) await click("button[data-v=stock]");
      await sleep(300);
      await click("button[data-v=stock]");
      return waitFor(() => where("#simRing"), "the simulation's ring", 20000).catch(() => null);
    };
    const left = async () => !!((await where("#simPlay")) || (await where("#vtPlay")));
    const opened = !!(await openPlayback());
    if (!opened) await shot("slow_playback_not_open");
    check("the playback opens (its simulation answered late)", opened, opened ? "" : await read("JSON.stringify({ctl: !document.getElementById('simControls').classList.contains('hidden'), m: document.body.dataset.machining, p: document.body.dataset.playback, st: document.getElementById('status').textContent.slice(0, 90), title: document.title, url: location.search})"));
    let ring = await resimulate();
    check("cancel: the simulation's ring shows while the playback is open", !!ring);
    if (ring) {
      await click("#simRing");
      await sleep(1500);
      check("a playback cancelled from the ring: its controls or the play button on the screen", await left());
    }
    await openPlayback();
    ring = await resimulate();
    check("superseded: the simulation's ring shows while the playback is open", !!ring);
    if (ring) {
      await click("#loadExample");
      await settled();
      await sleep(1000);
      check("a playback superseded (the project opened again meanwhile): its controls or the play button on the screen", await left());
    }
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('loadExample') && !!window.__marcam?.project"), "the page", 60000);
    await read("window.confirm = () => true; (() => { const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
  }
  await controls("setup 1 after a lost playback");
  // setup 2, continuing: a facing on it, planned; its playback
  await click("#treeAddSetup");
  await settled();
  await read("(() => { const s = document.getElementById('treeAddType'); s.value = 'face'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#treeAdd");
  await waitFor(() => read("(() => { try { const d = JSON.parse(document.getElementById('job').value); return d.setups[1]?.operations.length === 1 && document.body.dataset.state !== 'busy'; } catch { return false; } })()"), "setup 2's facing", 120000).catch(() => {});
  await controls("setup 2 (continuing)");
  // (Windows CI: the example list gone here, the page itself gone or another: what it was,
  // for the log)
  if (!(await waitFor(() => read("!!document.getElementById('example')"), "the example list", 30000).then(() => true, () => false)))
    console.log(`  the page after setup 2: ${JSON.stringify(await read("({ url: location.href, ready: document.readyState, title: document.title, body: (document.body?.innerText || '').slice(0, 300) })").catch((e) => String(e)))}`);
  // ---------------------------------------------------------------- Replace model
  // (Mar: "the replace model button doesn't seem to work": the knurled knob example, its
  // Part's Replace model..., another file) the part on the screen changes, the Part line
  // names the new file, no error; Ctrl+Z brings the knob back
  await read("(() => { const s = document.getElementById('example'); s.value = 'knob'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#loadExample");
  await settled();
  await click("#tree [data-act=part]");
  await settled();
  const vr = await viewRect();
  const r0 = partBox(await shot("replace_before"), vr);
  const line0 = await read("document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || ''");
  check("the knob's Part pane has Replace model...", !!(await where("#ptReplace")));
  {
    const doc = await S("DOM.getDocument");
    const { nodeId } = await S("DOM.querySelector", { nodeId: doc.root.nodeId, selector: "#replaceModelFile" });
    await S("DOM.setFileInputFiles", { files: [join(root, "examples", "plate", "dome.stl")], nodeId });
  }
  await settled();
  const r1 = partBox(await shot("replace_after"), vr);
  const line1 = await read("document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || ''");
  const err1 = await read("document.getElementById('error').classList.contains('hidden') ? '' : document.getElementById('error').textContent");
  check("Replace model on the knob (a revolved profile): the Part line names the new file, no error", /dome/.test(line1) && !err1, `${line0.trim()} -> ${line1.trim()}; ${err1}`);
  check("the part on the screen is the new one", !!(r0 && r1 && (Math.abs(r1.w - r0.w) > 6 || Math.abs(r1.h - r0.h) > 6 || Math.abs(r1.cx - r0.cx) > 6)), `${JSON.stringify(r0 && [r0.cx.toFixed(0), r0.w, r0.h])} -> ${JSON.stringify(r1 && [r1.cx.toFixed(0), r1.w, r1.h])}`);
  await read("document.activeElement?.blur?.(); true");
  await key("z", 2);
  await settled();
  const line2 = await read("document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || ''");
  check("Ctrl+Z: the knob's own part back", !/dome/.test(line2), line2.trim());
  // ---------------------------------------------------------------- the 4th axis: across it
  // (Mar: "handles to translate ... in the radial directions too"; "a way in 4th to center up
  // a part") the knob's mesh on the rotary axis: its blue (Z) arrow dragged, the part off the
  // axis on the screen and in its fields; Center on axis brings it back; Flush to front puts
  // its front at X0
  await read("(() => { const s = document.getElementById('example'); s.value = 'knob_finish'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#loadExample");
  await settled();
  await click("#tree .tree-setup.active [data-act=part]");
  await settled();
  await openMore("partPane");
  const tOf = () => read("['ptTX', 'ptTY', 'ptTZ'].map((id) => +document.getElementById(id)?.value)");
  const t0 = await tOf();
  const rv = await viewRect();
  const imgR = await shot("radial_before");
  const hcR = handleColours(imgR, rv);
  check("on the 4th axis: the part's arrows across the axis (green, blue) too", hcR.n.green > 40 && hcR.n.blue > 40, JSON.stringify(hcR.n));
  if (hcR.pts.blue.length) {
    const mid = (ps) => ps.reduce((a, p) => [a[0] + p[0] / ps.length, a[1] + p[1] / ps.length], [0, 0]);
    const c = mid([...hcR.pts.red, ...hcR.pts.green]);
    const far = hcR.pts.blue.filter((p) => Math.hypot(p[0] - c[0], p[1] - c[1]) > 14).sort((a, b) => Math.hypot(a[0] - c[0], a[1] - c[1]) - Math.hypot(b[0] - c[0], b[1] - c[1]));
    const from = far[Math.min(far.length - 1, 6)];
    const b0 = await partDrawn();
    await drag(from, [from[0], from[1] - 50]);
    await settled();
    await openMore("partPane");
    const t1 = await tOf();
    await shot("radial_dragged");
    const b1 = await partDrawn();
    check("the blue arrow dragged: the part off the axis on the screen and in its Z field", Math.abs(t1[2] - t0[2]) > 0.5 && !!(b0 && b1 && Math.hypot(b1.cx - b0.cx, b1.cy - b0.cy) > 20), `Z ${t0[2]} -> ${t1[2]}; part centre ${JSON.stringify(b0 && [Math.round(b0.cx), Math.round(b0.cy)])} -> ${JSON.stringify(b1 && [Math.round(b1.cx), Math.round(b1.cy)])}`);
  }
  check("on the 4th axis: Center on axis, Center along stock, Flush to front and back in the Part", !!(await where(".part-presets button", "^Center on axis$")) && !!(await where(".part-presets button", "^Flush to front$")));
  await click(".part-presets button", "^Center on axis$");
  await settled();
  await openMore("partPane");
  const t2 = await tOf();
  check("Center on axis: back on the axis (its Y and Z as they were)", Math.abs(t2[1] - t0[1]) < 0.01 && Math.abs(t2[2] - t0[2]) < 0.01, `${JSON.stringify(t0)} -> ${JSON.stringify(t2)}`);
  await click(".part-presets button", "^Flush to front$");
  await settled();
  await clean("4th axis placed");
  await openMore("partPane");
  const t4 = await tOf();
  // (halfway along the bar: the knob's front then stands in the bar, which a 12 mm flute
  // cannot cut down to the axis: the engine says so, rightly; not judged here)
  await click(".part-presets button", "^Center along stock$");
  await settled();
  await openMore("partPane");
  const t3 = await tOf();
  check("Flush to front puts its front at X0 (as it came); Center along stock moves it along X", Math.abs(t4[0] - t0[0]) < 0.3 && Math.abs(t3[0] - t4[0]) > 1, `X ${t0[0]} -> ${t4[0]} -> ${t3[0]}`);
  // (back flush to the front: halfway along, its roughing runs down to the axis, long)
  await click(".part-presets button", "^Flush to front$");
  await settled();
  // ---------------------------------------------------------------- vise, then the 4th axis
  // (Mar: "carry over the cut stock from a vise setup to a 4th axis isn't really working")
  // a square bar stood in the vise (its top a Ø25 step and a Ø15 boss), sized 30 x 30 by
  // typing; then on the 4th axis continuing, turned 90° about Y; a knurl added (its V-bit
  // offered from the library): its body on the part as the 4th axis holds it (it was the part
  // where the vise had it, X 2..32 and radii 60..68)
  await click("#menuFile");
  await click("#menuFile button, #menuFile [role=menuitem], #menuFile li, #menuFile a", "^New");
  await settled();
  {
    const doc = await S("DOM.getDocument");
    const { nodeId } = await S("DOM.querySelector", { nodeId: doc.root.nodeId, selector: "#importStlFile" });
    await S("DOM.setFileInputFiles", { files: [join(root, "tests", "fixtures", "stepped_bar.stl")], nodeId });
  }
  await settled();
  await click("#tree .tree-setup.active [data-act=setup]");
  await settled();
  await click("#setupHost .sd-kind[data-kind=vise]");
  await settled();
  check("a setup made a vise is named as one (it was 'Rotary 4th axis')", /vise/i.test(await read("JSON.parse(document.getElementById('job').value).setups[0].name")));
  await click("#tree .tree-setup.active [data-act=stock]");
  await settled();
  for (const [id, v] of [["#stX", "30"], ["#stY", "30"], ["#stZ", "62"]]) {
    await click(id);
    await key("a", 2);
    for (const ch of v) await type(ch);
  }
  await S("Input.dispatchKeyEvent", { type: "keyDown", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
  await S("Input.dispatchKeyEvent", { type: "keyUp", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
  await settled();
  const placed = await read("(() => { const d = JSON.parse(document.getElementById('job').value); return { tr: d.models[0].translate, st: d.setups[0].stock }; })()");
  check("the block typed down to 30 x 30: the part still in its middle", Math.abs(placed.tr[0] - (placed.st.min[0] + placed.st.max[0]) / 2) < 0.01 && Math.abs(placed.tr[1] - (placed.st.min[1] + placed.st.max[1]) / 2) < 0.01, JSON.stringify(placed));
  await read("(() => { for (const [id, v] of [['treeAddKind', 'rotary'], ['treeAddFrom', 'carry']]) { const s = document.getElementById(id); if (s) { s.value = v; s.dispatchEvent(new Event('change', { bubbles: true })); } } return true; })()");
  await click("#treeAddSetup");
  await settled();
  await click("#tree .tree-setup.active [data-act=stock]");
  await settled();
  await openMore("stockPane");
  await typeInto("#stRotY", "90");
  await settled();
  await read("(() => { const s = document.getElementById('treeAddType'); s.value = 'helical_knurl'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#treeAdd");
  const lib = await waitFor(() => where("dialog[open] button", "V-bit"), "the library offering a V-bit", 30000).catch(() => null);
  check("a knurl with no V-bit in the setup: the library opens on V-bits (no error below the fold)", !!lib);
  if (lib) {
    await click("dialog[open] button", "V-bit");
    await waitFor(() => read("!!document.querySelector('#opForm [name=body_back]')"), "the knurl's form", 60000).catch(() => {});
    await settled();
    const body = await read("[document.querySelector('#opForm [name=body_back]')?.value, document.querySelector('#opForm [name=body_front]')?.value]");
    const nums = (body || []).map((t) => (t || "").split(",").map(Number));
    const ok = nums.length === 2 && nums.every(([x, r]) => x >= -60.6 && x <= 0.01 && r > 0 && r < 21.3);
    check("the knurl's body on the part as the 4th axis holds it (X within -60.5..0, radius under the bar's)", ok, JSON.stringify(body));
  }
  await clean("vise then the 4th axis");
  // ---------------------------------------------------------------- the Fixture
  // (Mar) a 4th-axis setup added from the tree, made 3-axis in its details: its Fixture line
  // a Vise, its opening typed wider, the jaws move on the screen; made 4th axis again with a
  // Chuck + tailstock: its sizes given; no dialogs
  await click("#menuFile > summary");
  await sleep(200);
  await click("#projectNew");
  await settled();
  await read("(() => { const s = document.getElementById('treeAddKind'); s.value = 'rotary'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#treeAddSetup");
  await settled();
  check("a 4th-axis setup added from the tree (+ Add setup, 4th axis)", /4th axis|Rotary/i.test(await read("document.querySelector('#tree .tree-setup.active')?.textContent || ''")));
  await click("#tree .tree-setup.active [data-act=setup]");
  await settled();
  await click("#setupHost .sd-kind[data-kind=vise]");
  await settled();
  check("made 3-axis in its details", /vise/i.test(await read("document.querySelector('#tree .tree-setup.active')?.textContent || ''")));
  await click("#tree .tree-setup.active [data-act=fixture]");
  await settled();
  const fxOpts = await read("[...(document.getElementById('fxPreset')?.options || [])].map((o) => o.textContent).join(',')");
  check("its Fixture line: a Vise (the presets that fit a 3-axis setup)", fxOpts === "Vise", fxOpts);
  const jawsAt = () => read("(() => { const g = window.__marcam.state.three.groups.jaws; return g ? g.children.filter((c) => c.userData.jaw).map((c) => +c.position.y.toFixed(2)) : []; })()");
  const vrF = await viewRect();
  const j0 = await jawsAt();
  const iJ0 = await shot("fixture_jaws_before");
  await typeInto("#fxOpen", "90");
  await settled();
  const j1 = await jawsAt();
  const iJ1 = await shot("fixture_jaws_after");
  let pxJ = 0;
  for (let y = vrF[1]; y < vrF[1] + vrF[3]; y += 2)
    for (let x = vrF[0]; x < vrF[0] + vrF[2]; x += 2) {
      const a = iJ0.at(x, y), b = iJ1.at(x, y);
      if (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 60) pxJ++;
    }
  const spread = (j) => (j.length === 2 ? Math.abs(j[1] - j[0]) : 0);
  check("the Vise's opening typed (90): the jaws move apart, on the screen", spread(j1) > spread(j0) + 20 && pxJ > 150, `jaws ${JSON.stringify(j0)} -> ${JSON.stringify(j1)}; ${pxJ} px`);
  await clean("the Vise's opening");
  await click("#tree .tree-setup.active [data-act=setup]");
  await settled();
  await click("#setupHost .sd-kind[data-kind=rotary]");
  await settled();
  await click("#tree .tree-setup.active [data-act=fixture]");
  await settled();
  await read("(() => { const s = document.getElementById('fxPreset'); s.value = 'chuck_tailstock'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await settled();
  // (Mar: neither the chuck nor the tailstock is drawn on the 4th axis; their sizes are there)
  const tailRow = await where("#fxDepth");
  const drawn = await read("!!window.__marcam.state.three.groups.jaws?.children.some((c) => c.userData.tailstock || c.userData.chuck)");
  check("Chuck + tailstock: its sizes in the Fixture (the centre's depth), nothing drawn for it on the 4th axis", !!tailRow && !drawn, JSON.stringify({ row: !!tailRow, drawn }));
  await clean("Chuck + tailstock");
  // ---------------------------------------------------------------- indexed 3+1 roughing
  // (Mar: "not seeing the new indexed roughing for 4 axis") a new project, + Add setup as 4th
  // axis, the knob through its Part line; "Indexed roughing (3+1)" in the operation picker;
  // the library for a flat; its angles (index count) in its form; planned: on the screen
  await click("#menuFile > summary");
  await sleep(200);
  await click("#projectNew");
  await settled();
  await read("(() => { const s = document.getElementById('treeAddKind'); s.value = 'rotary'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#treeAddSetup");
  await settled();
  await click("#tree .tree-setup.active [data-act=part]");
  await sleep(800);
  const chooser2 = new Promise((resolve) => {
    const t0 = events.length;
    const tick = setInterval(() => {
      const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
      if (e) {
        clearInterval(tick);
        resolve(e.params);
      }
    }, 50);
  });
  await click("#ptReplace", "Import model");
  const fc2 = await Promise.race([chooser2, sleep(10000).then(() => null)]);
  if (!fc2) throw new Error("no file chooser opened by the 4th-axis setup's Import model");
  await S("DOM.setFileInputFiles", { files: [join(root, "examples", "knob", "knob.stl")], backendNodeId: fc2.backendNodeId });
  await waitFor(() => read("/knob/.test(document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || '')"), "the knob on the 4th axis", 60000);
  await settled();
  const offered = await read("[...document.getElementById('treeAddType').options].map((o) => o.textContent)");
  check("a 4th-axis setup's operation picker offers \"Indexed roughing (3+1)\"", offered.includes("Indexed roughing (3+1)"), offered.join(" | "));
  const vrI = await viewRect();
  const iI0 = await shot("indexed_before");
  await read("(() => { const s = document.getElementById('treeAddType'); s.value = [...s.options].find((o) => o.textContent === 'Indexed roughing (3+1)')?.value ?? ''; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
  await click("#treeAdd");
  // (no tool in the new project: the library opens on the cutters that suit it)
  const flat = await waitFor(() => where("dialog[open] .tp-row", "flat 3\\.175"), "the library offering a 3.175 flat", 30000).catch(() => null);
  check("Indexed roughing with no cutter in the setup: the library opens on the ones that suit it", !!flat);
  if (flat) await click("dialog[open] .tp-row", "flat 3\\.175");
  await waitFor(() => where("#opForm [name=index_count]"), "the index count in the form", 60000).catch(() => {});
  const fields = await read(`(() => { const vis = ${VISIBLE}; const f = (n) => { const e = document.querySelector('#opForm [name=' + n + ']'); if (!e) return null; e.scrollIntoView({ block: 'nearest' }); return { value: e.value, shown: vis(e), more: !!e.closest('#opMore') }; }; return { count: f('index_count'), indexes: f('indexes'), pattern: f('pattern') }; })()`);
  check("its form shows its angles: index count 6 (between the blades), indexes, adaptive; none under More",
    fields.count?.shown && fields.count.value === "6" && !fields.count.more && fields.indexes?.shown && !fields.indexes.more && fields.pattern?.value === "adaptive",
    JSON.stringify(fields));
  await settled();
  const planned = await waitFor(() => read(`(() => { const vis = ${VISIBLE}; const t = [...document.querySelectorAll('#tabs button')].find((b) => vis(b)); return t && /OK|issue/.test(t.textContent) ? t.textContent : null; })()`), "the indexed roughing planned", 240000).catch(() => null);
  const iI1 = await shot("indexed_planned");
  let pxI = 0;
  for (let y = vrI[1]; y < vrI[1] + vrI[3]; y += 2)
    for (let x = vrI[0]; x < vrI[0] + vrI[2]; x += 2) {
      const q = iI1.at(x, y);
      // (the cuts: blue)
      if (q[2] > 150 && q[2] > q[0] + 60 && q[2] > q[1] + 20) pxI++;
    }
  const line = await read("[...document.querySelectorAll('#tree .tree-op .tree-label')].map((e) => e.textContent.trim()).join(' | ')");
  check("planned: its program's tab, the operation in the tree by its name, and its cuts drawn on the screen", !!planned && /Indexed roughing \(3\+1\)/.test(line) && pxI > 400, `${planned}; ${line}; ${pxI} blue px`);
  await clean("indexed 3+1 roughing");
  // ---------------------------------------------------------------- "No setups yet"
  // (Mar: "the 'no setups yet' banner sticks around even after making the first setup") gone
  // once a setup is made (+ Add setup; File > Import part into an empty project), back when
  // the last one is deleted
  const banner = () => read(`(${VISIBLE})(document.getElementById('emptyState'))`);
  await click("#menuFile > summary");
  await sleep(200);
  await click("#projectNew");
  await settled();
  const b0 = await banner();
  await click("#emptyState button", "Add setup");
  await settled();
  const b1 = await banner();
  await click("#tree .tree-setup.active [data-act=delsetup]");
  await settled();
  const b2 = await banner();
  check("\"No setups yet\": shown in a new project, gone once + Add setup makes one, back when the last is deleted", b0 && !b1 && b2, JSON.stringify({ b0, b1, b2 }));
  const chooser3 = new Promise((resolve) => {
    const t0 = events.length;
    const tick = setInterval(() => {
      const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
      if (e) {
        clearInterval(tick);
        resolve(e.params);
      }
    }, 50);
  });
  await click("#menuFile > summary");
  await sleep(200);
  await click("#menuImport");
  const fc3 = await Promise.race([chooser3, sleep(10000).then(() => null)]);
  if (!fc3) throw new Error("no file chooser opened by File > Import part");
  await S("DOM.setFileInputFiles", { files: [join(root, "examples", "knob", "knob.stl")], backendNodeId: fc3.backendNodeId });
  await waitFor(() => where("#tree .tree-setup [data-act=setup]"), "the setup the import made", 60000).catch(() => {});
  await settled();
  const b3 = await banner();
  check("\"No setups yet\" gone after File > Import part into an empty project (its setup made)", !b3 && !!(await where("#tree .tree-setup [data-act=setup]")), JSON.stringify({ b3 }));
  await clean("File > Import part into an empty project");
  // ---------------------------------------------------------------- 4th axis: quarter turns
  // (Mar: "missing the quick buttons for part rotation") the knob just imported (on the 4th
  // axis): its Part's Rotation row; Z +90° turns it on the screen, kept on the axis with its
  // front at X0; Center on axis then leaves it there
  const turnedBox = () => read("(async () => { const M = window.__marcam; const d = M.state.tree.doc; const { result } = await M.engine('meshTriangles', d, { setup: M.state.tree.setup }, M.state.mesh); const [lo, hi] = result.bounds; return { lo: lo.map((v) => +v.toFixed(2)), hi: hi.map((v) => +v.toFixed(2)) }; })()");
  const onAxis = (b) => Math.abs(b.lo[1] + b.hi[1]) < 0.05 && Math.abs(b.lo[2] + b.hi[2]) < 0.05 && Math.abs(b.hi[0]) < 0.05;
  await click("#tree .tree-setup.active [data-act=part]");
  await settled();
  const rowShown = !!(await where("#partPane [data-preset=turn]", "Z \\+90")) && !!(await where("#partPane [data-preset=unturn]", "0°"));
  check("a 4th-axis setup's Part: the Rotation row (X/Y/Z −90° / +90°, 0°) beside its Position buttons", rowShown && !!(await where("#partPane [data-preset=axis]", "Center on axis")));
  const vrR = await viewRect();
  const pb0 = await turnedBox();
  const iR0 = await shot("rotary_turn_before");
  await click("#partPane [data-preset=turn]", "Z \\+90");
  await settled();
  const pb1 = await turnedBox();
  const iR1 = await shot("rotary_turn_after");
  let pxR = 0;
  for (let y = vrR[1]; y < vrR[1] + vrR[3]; y += 2)
    for (let x = vrR[0]; x < vrR[0] + vrR[2]; x += 2) {
      const a = iR0.at(x, y), b = iR1.at(x, y);
      if (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 60) pxR++;
    }
  check("Z +90°: the part turns on the screen, kept on the axis with its front at X0", pxR > 300 && onAxis(pb1) && JSON.stringify(pb1) !== JSON.stringify(pb0), `${pxR} px changed; ${JSON.stringify(pb0)} -> ${JSON.stringify(pb1)}`);
  await click("#partPane [data-preset=axis]", "Center on axis");
  await settled();
  const pb2 = await turnedBox();
  check("Center on axis afterwards: on the axis", onAxis(pb2) || (Math.abs(pb2.lo[1] + pb2.hi[1]) < 0.05 && Math.abs(pb2.lo[2] + pb2.hi[2]) < 0.05), JSON.stringify(pb2));
  await click("#partPane [data-preset=unturn]", "0°");
  await settled();
  const pb3 = await turnedBox();
  check("0°: not turned, still on the axis with its front at X0", onAxis(pb3) && Math.abs(pb3.hi[0] - pb3.lo[0] - (pb0.hi[0] - pb0.lo[0])) < 0.05, `${JSON.stringify(pb0)} -> ${JSON.stringify(pb3)}`);
  await clean("the 4th axis's quarter turns");
  // ---------------------------------------------------------------- indexed roughing, timed
  // (Mar's option b and a: the angle search across workers, the stock sampled coarser) a part
  // like the impeller (a hub with three blades) through Mar's path in a fresh page: Indexed
  // roughing (3+1) on the screen within the time it should take, the page answering
  // meanwhile; then the same with the browser saying it has one core (no workers): planned
  // the same (the same G-code)
  const blades = join(mkdtempSync(join(tmpdir(), "marcam-blades-")), "blades.stl");
  writeFileSync(blades, bladesStl());
  const SLACK = Number(process.env.MARCAM_PERF_SLACK || 1);
  const indexedRun = async (what) => {
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await click("#menuFile > summary");
    await sleep(200);
    await click("#projectNew");
    await settled();
    await read("(() => { const s = document.getElementById('treeAddKind'); s.value = 'rotary'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#treeAddSetup");
    await settled();
    await click("#tree .tree-setup.active [data-act=part]");
    await sleep(800);
    const ch = new Promise((resolve) => {
      const t0 = events.length;
      const tick = setInterval(() => {
        const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
        if (e) {
          clearInterval(tick);
          resolve(e.params);
        }
      }, 50);
    });
    await click("#ptReplace", "Import model");
    const fc = await Promise.race([ch, sleep(10000).then(() => null)]);
    if (!fc) throw new Error(`${what}: no file chooser from Import model`);
    await S("DOM.setFileInputFiles", { files: [blades], backendNodeId: fc.backendNodeId });
    await waitFor(() => read("/blades/.test(document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || '')"), "the blades on the 4th axis", 60000);
    await settled();
    // (the bar round it: its Stock's Fit to part)
    await click("#tree .tree-setup.active [data-act=stock]");
    await settled();
    await click("#stFit", "Fit to part");
    await settled();
    // the page's frames from here on: the longest wait between two
    await read("(() => { window.__eyeGap = 0; let last = performance.now(); const f = (t) => { window.__eyeGap = Math.max(window.__eyeGap, t - last); last = t; window.__eyeRaf = requestAnimationFrame(f); }; cancelAnimationFrame(window.__eyeRaf); window.__eyeRaf = requestAnimationFrame(f); return true; })()");
    await read("(() => { const s = document.getElementById('treeAddType'); s.value = [...s.options].find((o) => o.textContent === 'Indexed roughing (3+1)')?.value ?? ''; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#treeAdd");
    await waitFor(() => where("dialog[open] .tp-row", "flat 3\\.175"), `${what}: the library`, 30000);
    const t0 = Date.now();
    await click("dialog[open] .tp-row", "flat 3\\.175");
    // (Mar: "a real progress wheel") the operation's ring, as it plans: on the screen, filling
    const fills = [];
    let tab = null;
    while (Date.now() - t0 < 180000) {
      const r = await read(`(() => { const vis = ${VISIBLE}; const t = [...document.querySelectorAll('#tabs button')].find((b) => vis(b)); const e = document.querySelector('#tree .op-state.planning'); return { tab: t && /OK|issue/.test(t.textContent) && !document.querySelector('#tree .op-state.planning') ? t.textContent : null, ring: e && vis(e) ? (e.classList.contains('spin') ? 'spin' : +getComputedStyle(e).getPropertyValue('--p')) : null }; })()`);
      if (r.ring != null && fills[fills.length - 1] !== r.ring) fills.push(r.ring);
      if (r.tab) {
        tab = r.tab;
        break;
      }
      await sleep(200);
    }
    const ms = Date.now() - t0;
    // (Mar: ▶ enabled as soon as the plan lands) the toolpaths handed to the simulating worker,
    // not planned there again: the play button ready within about a second of the toolpath
    const l0 = Date.now();
    let playMs = null;
    while (Date.now() - l0 < 60000 * SLACK) {
      if (await read("!document.getElementById('vtPlay').disabled")) {
        playMs = Date.now() - l0;
        break;
      }
      await sleep(50);
    }
    const ringAfter = await read(`(${VISIBLE})(document.querySelector('#tree .op-state.planning'))`);
    const gap = await read("(cancelAnimationFrame(window.__eyeRaf), window.__eyeGap)");
    await settled();
    const vr = await viewRect();
    const img = await shot(`indexed_${what.replace(/\W+/g, "_")}`);
    let px = 0;
    for (let y = vr[1]; y < vr[1] + vr[3]; y += 2)
      for (let x = vr[0]; x < vr[0] + vr[2]; x += 2) {
        const q = img.at(x, y);
        if (q[2] > 150 && q[2] > q[0] + 60 && q[2] > q[1] + 20) px++;
      }
    const status = await read("document.getElementById('status').textContent");
    const gcode = await read("(async () => { const t = document.getElementById('gcode').value; const b = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(t)); return [...new Uint8Array(b)].slice(0, 8).map((x) => x.toString(16).padStart(2, '0')).join('') + ' ' + t.length; })()");
    return { tab, ms, playMs, gap: Math.round(gap), px, status, gcode, fills, ringAfter };
  };
  const many = await indexedRun("workers");
  check("Indexed roughing (3+1) on a bladed part: its cuts on the screen within 60 s (the angles found across the workers)", !!many.tab && many.px > 400 && many.ms < 60000 * SLACK, JSON.stringify(many));
  check("the page answers while it plans (no frame held over 1 s)", many.gap < 1000 * SLACK, `${many.gap} ms`);
  check("the play button (▶) ready within about a second of the plan landing (its toolpaths handed to the simulation, not planned again)", many.playMs != null && many.playMs < 1500 * SLACK, `${many.playMs} ms`);
  const nums = many.fills.filter((f) => typeof f === "number");
  check("while it plans, the operation's progress wheel is on the screen and fills (angle by angle), gone when its toolpath shows",
    nums.length >= 3 && nums.every((f, i) => i === 0 || f > nums[i - 1]) && nums[nums.length - 1] > nums[0] && !many.ringAfter,
    `${JSON.stringify(many.fills)}; after ${many.ringAfter}`);
  await clean("indexed roughing across the workers");
  // (Mar: "the index roughing simulation playback is super chunky", "when you set the speed
  // slider high") its playback with the stock: Play, the speed slider dragged to the top
  // (x1000); it starts within seconds, the frames in budget, the stock at the end in time
  {
    const pressed = await read("(() => { const b = [...document.querySelectorAll('.view-toggles button')].find((b) => /Simulated stock/.test(b.title)); const r = b.getBoundingClientRect(); return [b.getAttribute('aria-pressed') === 'true', r.left + r.width / 2, r.top + r.height / 2]; })()");
    if (!pressed[0]) await drag([pressed[1], pressed[2]], [pressed[1], pressed[2]], 1);
    await waitFor(() => read("!document.getElementById('vtPlay').disabled"), "the play button", 120000 * SLACK);
    const p0 = Date.now();
    await click("#vtPlay");
    await waitFor(() => read("(() => { const m = window.__marcam.state.machining; return !!(m.open && m.stock && m.playing); })()"), "playing with the stock", 120000 * SLACK).catch(() => {});
    const startMs = Date.now() - p0;
    const sp = await read("(() => { const r = document.getElementById('simSpeed').getBoundingClientRect(); return [r.left, r.top + r.height / 2, r.width, +document.getElementById('simSpeed').value]; })()");
    await drag([sp[0] + 8 + ((sp[2] - 16) * sp[3]) / 3, sp[1]], [sp[0] + sp[2] - 2, sp[1]], 10);
    const run = await read(`(async () => { const m = window.__marcam.state.machining; const w0 = performance.now(); let worst = 0, last = w0, frames = 0; const pos0 = m.head ?? m.pos; await new Promise((r) => { const f = (t) => { worst = Math.max(worst, t - last); last = t; frames++; if ((m.pos < m.session.segments || m.busy) && performance.now() - w0 < ${120000 * SLACK}) requestAnimationFrame(f); else r(); }; requestAnimationFrame(f); }); return { speed: document.getElementById('simSpeedLabel').textContent, endMs: Math.round(performance.now() - w0), worst: Math.round(worst), frames, from: pos0, at: m.pos, of: m.session.segments }; })()`);
    await shot("indexed_playback_end");
    check("Indexed roughing's playback with the stock: Play to playing within 10 s, the speed at the top (x1000), the stock to the end within 60 s, no frame held over 100 ms",
      startMs < 10000 * SLACK && run.speed === "×1000" && run.at === run.of && run.endMs < 60000 * SLACK && run.worst < 100 * SLACK, JSON.stringify({ startMs, ...run }));
  }
  // a browser with one core: no workers; the planner finds the angles itself
  await S("Emulation.setHardwareConcurrencyOverride", { hardwareConcurrency: 1 });
  const one = await indexedRun("one core");
  await S("Emulation.setHardwareConcurrencyOverride", { hardwareConcurrency: 8 }).catch(() => {});
  check("with one core (no workers): planned too, the same G-code, on the screen, the page answering", !!one.tab && one.px > 400 && one.gcode === many.gcode && one.gap < 1000 * SLACK, `${JSON.stringify(one)} vs ${many.gcode}`);
  await clean("indexed roughing on one core");
  // ---------------------------------------------------------------- the cut piece carried on
  // (Mar: "a standing bar stock in 3axis. mill a square boss with pocket inside it. Then do the
  // outer diameter with some contours. Next, add a setup where you put that cut stock into a
  // 4 axis setup and then knurl the OD.") all of it as a person does it, in a fresh page
  {
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await click("#menuFile > summary");
    await sleep(200);
    await click("#projectNew");
    await settled();
    const pick = (id, v) => read(`(() => { const s = document.getElementById('${id}'); s.value = [...s.options].find((o) => o.value === '${v}')?.value ?? ''; s.dispatchEvent(new Event('change', { bubbles: true })); return s.value; })()`);
    await pick("treeAddKind", "vise");
    await click("#treeAddSetup");
    await settled();
    // a round bar standing up in the vise
    await click("#tree .tree-setup.active [data-act=stock]");
    await settled();
    await pick("stType", "round");
    await settled();
    await pick("stAlong", "z");
    await settled();
    const bar = await read("document.querySelector('#tree .tree-setup.active [data-act=stock]')?.textContent || ''");
    check("carry-through: a round bar standing in the vise (its Stock: Round bar, Axis Z)", /bar/.test(bar) && (await read("document.getElementById('stAlong').value")) === "z", bar.trim());
    // (Mar: "no human will ever do that [type path coordinates] ... you shouldn't use it for
    // your testing") the finished part imported, its features picked on it in the 3D view
    const fin = join(mkdtempSync(join(tmpdir(), "marcam-finished-")), "finished_part.stl");
    writeFileSync(fin, finishedPartStl());
    await click("#tree .tree-setup.active [data-act=part]");
    await sleep(800);
    const chooser = new Promise((resolve) => {
      const t0 = events.length;
      const tick = setInterval(() => {
        const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
        if (e) {
          clearInterval(tick);
          resolve(e.params);
        }
      }, 50);
    });
    await click("#ptReplace", "Import model");
    const fcp = await Promise.race([chooser, sleep(10000).then(() => null)]);
    if (!fcp) throw new Error("carry-through: no file chooser from Import model");
    await S("DOM.setFileInputFiles", { files: [fin], backendNodeId: fcp.backendNodeId });
    await waitFor(() => read("/finished_part/.test(document.querySelector('#tree .tree-setup.active [data-act=part]')?.textContent || '')"), "the finished part in the vise", 60000);
    await settled();
    // the bar fitted to it, no margin round it (a Ø20 bar)
    await click("#tree .tree-setup.active [data-act=stock]");
    await settled();
    // (the margin is under the Stock's More)
    await openMore("stockPane");
    await typeInto("#stMargin", "0");
    await click("#stFit", "Fit to part");
    await settled();
    const fitted = await read("document.querySelector('#tree .tree-setup.active [data-act=stock]')?.textContent || ''");
    check("carry-through: the finished part imported, the bar fitted to it (Ø20)", /Ø20 /.test(fitted), fitted.trim());
    const addOp = async (type, lib) => {
      await pick("treeAddType", type);
      await click("#treeAdd");
      if (lib) {
        const row = await waitFor(() => where("dialog[open] .tp-row", lib), `the library (${lib})`, 30000).catch(() => null);
        if (row) await click("dialog[open] .tp-row", lib);
      }
      await waitFor(() => read("!!document.getElementById('opForm')"), `the ${type} form`, 60000);
      await settled();
    };
    // a feature picked as a person does: (Mar: no "Done picking": picking is on while the
    // operation is selected) a click on it in the 3D view (turned to the iso view first if it
    // cannot be seen), nothing pressed after: its path planned and drawn on its own
    const faceAt = (pred) => `(() => { const t = window.__marcam.state.tree, v = window.__marcam.state.three; const target = t.pick?.list.find(${pred}); if (!target) return null; const want = new Set(target.faces); const tri = t.regions.tri; const el = v.renderer.domElement, rc = el.getBoundingClientRect(); const ray = new v.THREE.Raycaster(); for (let k = 0; k < t.regions.of.length; k++) { if (!want.has(t.regions.list[t.regions.of[k]].id)) continue; for (const w of [[1/3, 1/3, 1/3], [0.8, 0.1, 0.1], [0.1, 0.8, 0.1], [0.1, 0.1, 0.8]]) { const c = new v.THREE.Vector3(...[0, 1, 2].map((d) => w[0] * tri[9*k+d] + w[1] * tri[9*k+3+d] + w[2] * tri[9*k+6+d])); const p = c.clone().project(v.camera); if (Math.abs(p.x) > 0.9 || Math.abs(p.y) > 0.9) continue; const sx = ((p.x + 1) / 2) * rc.width, sy = ((1 - p.y) / 2) * rc.height; const on = (dx, dy) => { ray.setFromCamera(new v.THREE.Vector2(((sx + dx) / rc.width) * 2 - 1, -((sy + dy) / rc.height) * 2 + 1), v.camera); const h = ray.intersectObject(t.pickMesh)[0]; return h && want.has(t.regions.list[t.regions.of[h.faceIndex]].id) ? h : null; }; const hit = on(0, 0); if (!hit || hit.point.distanceTo(c) > 0.05) continue; if (![[3, 0], [-3, 0], [0, 3], [0, -3]].every(([dx, dy]) => on(dx, dy))) continue; return [Math.round(rc.left + sx), Math.round(rc.top + sy)]; } } return null; })()`;
    // (the view still: a turn of the camera animates; aimed mid-turn, the click landed where
    // the part had been)
    const still = () => waitFor(async () => { const a = await read("window.__marcam.state.three.camera.position.toArray().map((x) => x.toFixed(3)).join()"); await sleep(250); return a === (await read("window.__marcam.state.three.camera.position.toArray().map((x) => x.toFixed(3)).join()")); }, "the view still", 20000).catch(() => {});
    // the selected operation's path as planned: its points (none: not planned)
    const opPath = "(() => { const st = window.__marcam.state, id = st.tree.selected; for (const p of st.result?.programs || []) { const ops = (p.toolpath?.ops || []).filter((o) => o.start != null).sort((a, b) => a.start - b.start); const k = ops.findIndex((o) => o.id === id); if (k < 0) continue; const end = k + 1 < ops.length ? ops[k + 1].start : p.toolpath.points.length / 4; return [...p.toolpath.points.slice(4 * ops[k].start, 4 * end)].map((x) => +x.toFixed(3)).join(); } return ''; })()";
    const pathChanges = [];
    const pickOn = async (pred, what) => {
      const path0 = await read(opPath);
      const pressed0 = await read("document.querySelectorAll('#opEditor button').length");
      await waitFor(() => read("!!window.__marcam.state.tree.pickMesh && !!window.__marcam.state.tree.pick?.list?.length"), `${what}: picking`, 60000);
      // (aimed once the view is still, as a person does: a plan landing meanwhile (the new
      // operation's) can frame the view again, and the point aimed at was then empty)
      await settled();
      await still();
      let at = await read(faceAt(pred));
      if (!at) {
        await read("window.__marcam.setView('iso', {})");
        await sleep(300);
        await still();
        at = await read(faceAt(pred));
      }
      const scene = "(() => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; return { cam: v.camera.position.toArray().map((x) => +x.toFixed(2)).join(), mesh: t.pickMesh?.uuid?.slice(0, 8), mw: t.pickMesh ? [...t.pickMesh.matrixWorld.elements].map((x) => +x.toFixed(2)).join() : null, parent: t.pickMesh?.parent?.type, visible: t.pickMesh?.visible }; })()";
      const aimed = await read(scene);
      if (at) {
        await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: at[0], y: at[1] });
        await S("Input.dispatchMouseEvent", { type: "mousePressed", x: at[0], y: at[1], button: "left", clickCount: 1 });
        await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: at[0], y: at[1], button: "left", clickCount: 1 });
        await sleep(1000);
      }
      const pickedNow = () => read("(() => { const t = window.__marcam.state.tree; return [...(t.pick?.sel || [])].map((id) => t.pick.list.find((x) => x.id === id)?.label).join(' | '); })()");
      // (missed, the view moved under the click: aimed again and clicked once more, as a
      // person does; the first miss in the log)
      if (at && !(await pickedNow())) {
        console.log(`  ${what}: the first click picked nothing; aimed again`);
        await settled();
        await still();
        const again = await read(faceAt(pred));
        if (again) {
          await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: again[0], y: again[1] });
          await S("Input.dispatchMouseEvent", { type: "mousePressed", x: again[0], y: again[1], button: "left", clickCount: 1 });
          await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: again[0], y: again[1], button: "left", clickCount: 1 });
          await sleep(1000);
        }
      }
      const picked = await pickedNow();
      await shot(`carry_pick_${what.replace(/\W+/g, "_")}`);
      // (nothing picked by the click: what was under it, and whether the same point picks it
      // called directly, for the log; still a failure)
      if (!picked) {
        const diag = at ? await read(`(async () => { const v = window.__marcam.state.three, t = window.__marcam.state.tree; const e = document.elementFromPoint(${at[0]}, ${at[1]}); const r = v.renderer.domElement.getBoundingClientRect(); const pr = await window.__marcam.tree.pickAt(${at[0]} - r.left, ${at[1]} - r.top); const sel = [...t.pick.sel].length; if (sel) t.pick.sel.clear(); return { under: e ? (e.tagName + '#' + e.id + (e === v.renderer.domElement ? '(the view)' : '')) : 'none', notes: t.pick.notes, direct: pr ? (pr.pickable?.label || 'a face, nothing to pick') : 'no face', picking: !!t.picking, n: t.pick.list.length }; })()`) : { at: "no point of it seen" };
        console.log(`  ${what}: the click picked nothing: ${JSON.stringify(diag)}; aimed at ${JSON.stringify(at)} with ${JSON.stringify(aimed)}; now ${JSON.stringify(await read(scene))}; aimed now ${JSON.stringify(await read(faceAt(pred)))}`);
      }
      // nothing pressed: planned again on its own, its new path drawn
      await waitFor(async () => { const p = await read(opPath); return p && p !== path0; }, `${what}: planned again`, 120000 * SLACK).catch(() => {});
      await settled();
      const path1 = await read(opPath);
      // on the screen: the operation's own colour where its new path is
      const drawnAt = await read("(() => { const st = window.__marcam.state, v = st.three, id = st.tree.selected; const s = st.tree.doc.setups.find((x) => x.id === st.tree.setup); const PAL = ['#3b82f6', '#f59e0b', '#10b981', '#ef4444', '#a855f7', '#14b8a6', '#e11d48', '#84cc16']; const i0 = s.operations.findIndex((o) => o.id === id); const hex = s.operations[i0]?.color || PAL[i0 % PAL.length]; const p = st.result.programs[st.tab]; const ops = (p?.toolpath?.ops || []).filter((o) => o.start != null).sort((a, b) => a.start - b.start); const k = ops.findIndex((o) => o.id === id); if (k < 0) return { hex, pts: [] }; const end = k + 1 < ops.length ? ops[k + 1].start : p.toolpath.points.length / 4; const r = v.renderer.domElement.getBoundingClientRect(); const pts = []; const P = p.toolpath.points; for (let i = ops[k].start + 1; i < end && pts.length < 40; i += Math.max(1, Math.floor((end - ops[k].start) / 40))) { if (!(P[4 * i + 3] % 2)) continue; const q = new v.THREE.Vector3(P[4 * i], P[4 * i + 1], P[4 * i + 2]).project(v.camera); if (Math.abs(q.x) < 0.95 && Math.abs(q.y) < 0.95) pts.push([r.left + (q.x + 1) / 2 * r.width, r.top + (1 - q.y) / 2 * r.height]); } return { hex, pts }; })()");
      const img = await shot(`carry_picked_${what.replace(/\W+/g, "_")}`);
      const hs = ([r, g, b]) => { const mx = Math.max(r, g, b), d = mx - Math.min(r, g, b); if (!d) return [0, 0]; const h = mx === r ? ((g - b) / d) % 6 : mx === g ? (b - r) / d + 2 : (r - g) / d + 4; return [(h * 60 + 360) % 360, d / mx]; };
      const [h0] = hs([1, 3, 5].map((k) => parseInt(drawnAt.hex.slice(k, k + 2), 16)));
      let seen = 0;
      for (const [x, y] of drawnAt.pts) {
        let ok = false;
        for (let dy = -3; dy <= 3 && !ok; dy++) for (let dx = -3; dx <= 3 && !ok; dx++) { const q = img.at(Math.round(x + dx), Math.round(y + dy)); if (!q) continue; const [h, sat] = hs(q); if (sat > 0.2 && Math.min(Math.abs(h - h0), 360 - Math.abs(h - h0)) < 22) ok = true; }
        if (ok) seen++;
      }
      pathChanges.push({ seen: drawnAt.pts.length ? +(seen / drawnAt.pts.length).toFixed(2) : 0, n: drawnAt.pts.length, what, before: path0.length, after: path1.length, changed: !!path1 && path1 !== path0, picking: await read("!!window.__marcam.state.tree.picking"), buttons: [pressed0, await read("document.querySelectorAll('#opEditor button').length")] });
      return picked;
    };
    const val = (name) => read(`document.getElementById('opForm')?.elements['${name}']?.value ?? null`);
    // the boss: its outline (the edge round its top), cut down its walls
    await addOp("contour", "3\\.175 x 12");
    // (Mar's picking rule) before its pick: the one-line prompt only, nothing planned, no error
    const before = await read(`(() => { const vis = ${VISIBLE}; return { prompt: vis(document.getElementById('pickNone')) ? document.getElementById('pickNone').textContent : '', tabs: [...document.querySelectorAll('#tabs button')].filter((b) => vis(b)).length, error: vis(document.getElementById('error')) ? document.getElementById('error').textContent : '', issues: [...document.querySelectorAll('.issues, #summary .error')].filter((e) => vis(e)).map((e) => e.textContent.slice(0, 60)) }; })()`);
    check("carry-through: a contour added, nothing picked yet: only the 'Nothing picked' line, not planned, no error", /^Nothing picked\. Pick/.test(before.prompt) && before.tabs === 0 && !before.error && !before.issues.length, JSON.stringify(before));
    const bossPick = await pickOn("(x) => x.kind === 'edge_loop' && x.data.side === 'outside' && Math.abs(x.data.length - 48) < 0.5", "the boss");
    const bossDepth = +(await val("depth"));
    // the pocket in it
    await addOp("pocket");
    const pocketPick = await pickOn("(x) => x.kind === 'pocket'", "the pocket");
    const pocketDepth = +(await val("depth"));
    // the turned OD (Ø18): the edge round its top, down its wall to the step
    await addOp("contour");
    const odPick = await pickOn("(x) => x.kind === 'edge_loop' && x.data.side === 'outside' && Math.abs(x.data.length - Math.PI * 18) < 1", "the turned OD");
    const odDepth = +(await val("depth"));
    check("carry-through: the boss, the pocket and the turned OD picked on the part (no coordinates typed)",
      /outline, 48\.0/.test(bossPick) && /Pocket 6\.00 x 6\.00/.test(pocketPick) && /outline, 56\.5/.test(odPick) && Math.abs(bossDepth - 6.5) < 0.05 && Math.abs(pocketDepth - 3) < 0.05 && Math.abs(odDepth - 16.5) < 0.05,
      JSON.stringify({ bossPick, bossDepth, pocketPick, pocketDepth, odPick, odDepth }));
    const s1 = await read(`(() => { const vis = ${VISIBLE}; const t = [...document.querySelectorAll('#tabs button')].find((b) => vis(b)); return { tab: t?.textContent || '', ops: [...document.querySelectorAll('#tree .tree-setup.active .tree-op .tree-label')].map((e) => e.textContent.trim().split(' ')[0]), error: vis(document.getElementById('error')) ? document.getElementById('error').textContent : '' }; })()`);
    check("carry-through, setup 1: the boss's contour, the pocket, the OD's contour, planned clean", /OK/.test(s1.tab) && s1.ops.length === 3 && !s1.error, JSON.stringify(s1));
    // (Mar: "I don't want a done picking button ... as you pick the toolpaths should update")
    check("live picking: no Done picking button; each pick planned again with nothing pressed, its new path drawn in its colour",
      !(await read("!!document.getElementById('pickFaces')")) && pathChanges.length === 3 && pathChanges.every((c) => c.changed && c.picking && c.buttons[1] <= c.buttons[0] + 1 && c.n >= 3 && c.seen >= 0.4), JSON.stringify(pathChanges));
    // deselected (Escape): picking ends; a click on the part picks nothing more
    const before2 = await read("JSON.stringify(window.__marcam.state.tree.doc.setups.find((s) => s.id === window.__marcam.state.tree.setup).operations.map((o) => o.picks || []))");
    await S("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await S("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await settled();
    const vr0 = await viewRect();
    const mid = [Math.round(vr0[0] + vr0[2] / 2), Math.round(vr0[1] + vr0[3] / 2)];
    await drag(mid, mid, 1);
    await settled();
    const after2 = await read("JSON.stringify(window.__marcam.state.tree.doc.setups.find((s) => s.id === window.__marcam.state.tree.setup).operations.map((o) => o.picks || []))");
    const off = await read("({ picking: !!window.__marcam.state.tree.picking, selected: window.__marcam.state.tree.selected, form: !!document.getElementById('opForm') })");
    check("live picking: Escape deselects the operation; picking off, a click on the part changes no picks", !off.picking && !off.selected && !off.form && before2 === after2, JSON.stringify(off));
    await shot("carry_setup1");
    // + Add setup: 4th axis, continuing from setup 1
    await pick("treeAddKind", "rotary");
    await pick("treeAddFrom", "carry");
    await click("#treeAddSetup");
    await settled();
    const vr = await viewRect();
    const img = await shot("carry_setup2_piece");
    let px = 0;
    for (let y = vr[1]; y < vr[1] + vr[3]; y += 3)
      for (let x = vr[0]; x < vr[0] + vr[2]; x += 3) {
        const q = img.at(x, y);
        if (q[0] + q[1] + q[2] > 180) px++;
      }
    const s2 = await read(`(() => { const vis = ${VISIBLE}; return { error: vis(document.getElementById('error')) ? document.getElementById('error').textContent : '', stock: document.querySelector('#tree .tree-setup.active [data-act=stock]')?.textContent.trim() || '', tabs: [...document.querySelectorAll('#tabs button')].filter((b) => vis(b)).length }; })()`);
    const frame = await read("window.__marcam.engine('setupFrame', window.__marcam.state.tree.doc, { setup: window.__marcam.state.tree.setup }).then((r) => r.result.stock)");
    check("carry-through, setup 2 (4th axis, continuing): the cut piece on the screen, lying along A, no error, nothing of setup 1 drawn over it",
      !s2.error && /left/.test(s2.stock) && s2.tabs === 0 && px > 2000 && frame.length > 59, `${JSON.stringify(s2)}; ${JSON.stringify(frame)}; ${px} px`);
    // (Mar: "I can't get it to orient correctly ... the UI makes all of this very unclear") the
    // carried piece on the A axis by default, its length along X, nothing drawn beside it; the
    // quick buttons (no typing): end for end, a quarter turn about A, as a person clicks them
    await click("#tree .tree-setup.active [data-act=stock]");
    await settled();
    const pieceNow = () => read("(() => { const v = window.__marcam.state.three; const B = new v.THREE.Box3(); const out = { piece: null, extra: 0, part: null, guide: !!v.groups.stock.children.find((c) => c.userData.guide) }; for (const c of v.groups.stock.children) { if (c.userData.guide) continue; B.setFromObject(c); if (c.userData.carried) out.piece = [B.min.toArray(), B.max.toArray()].map((p) => p.map((x) => +x.toFixed(2))); else out.extra++; } const p = v.groups.part.children[0]; if (p) { B.setFromObject(p); out.part = [B.min.toArray(), B.max.toArray()].map((q) => q.map((x) => +x.toFixed(2))); } return out; })()");
    const p0 = await pieceNow();
    const onAxis = (b) => b && Math.abs(b[0][1] + b[1][1]) < 0.5 && Math.abs(b[0][2] + b[1][2]) < 0.5 && b[1][0] - b[0][0] >= Math.max(b[1][1] - b[0][1], b[1][2] - b[0][2]);
    const buttons = await read("[...document.querySelectorAll('#carryQuick button')].map((b) => b.textContent.trim())");
    check("carried onto the 4th axis: the piece centred on A (Y, Z), its length along X, nothing else drawn as stock; the A axis and its ends labelled; plain-word buttons, typed coordinates folded away",
      onAxis(p0.piece) && p0.extra === 0 && p0.guide && buttons.some((b) => /Flip end for end/.test(b)) && buttons.some((b) => /Turn 90/.test(b)) && !(await read("document.getElementById('stTyped')?.open")), JSON.stringify({ p0, buttons }));
    const imgA = await shot("carry_orient_before");
    await click("#carryQuick button", "Flip end for end");
    await settled();
    const p1 = await pieceNow();
    await click("#carryQuick button", "Turn 90");
    await settled();
    const p2 = await pieceNow();
    const imgB = await shot("carry_orient_after");
    const vrO = await viewRect();
    let changed = 0, seen = 0;
    for (let y = vrO[1]; y < vrO[1] + vrO[3]; y += 4)
      for (let x = vrO[0]; x < vrO[0] + vrO[2]; x += 4) {
        const p = imgA.at(x, y), q = imgB.at(x, y);
        seen++;
        if (Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 60) changed++;
      }
    // (the part goes with it: end for end, its end at the other end of X)
    const midX = (b) => (b[0][0] + b[1][0]) / 2;
    check("the carried piece's quick buttons: Flip end for end puts the part at the other end, Turn 90° about A turns it, the piece stays on A, the view changes",
      onAxis(p1.piece) && onAxis(p2.piece) && p0.part && p1.part && Math.abs(midX(p1.part) - midX(p0.part)) > 5 && changed > seen * 0.01, JSON.stringify({ p0: p0.part, p1: p1.part, p2: p2.part, changed, seen }));
    // back as it was (the knurl after this is picked on the part where it was)
    await click("#carryQuick button", "Turn 90");
    await settled();
    await click("#carryQuick button", "Turn 90");
    await settled();
    await click("#carryQuick button", "Turn 90");
    await settled();
    await click("#carryQuick button", "Flip end for end");
    await settled();
    // the knurl on the OD, picked on the part (the library for a V-bit)
    await addOp("helical_knurl", "90 deg V-bit");
    const odBand = await pickOn("(x) => x.kind === 'cylindrical_band' && Math.abs(x.data.r[1] - 10) < 0.05", "the OD to knurl");
    const body = [await val("body_back"), await val("body_front")];
    const [back, front] = body.map((t) => (t || "").split(",").map(Number));
    await settled();
    const k = await read(`(() => { const vis = ${VISIBLE}; const t = [...document.querySelectorAll('#tabs button')].find((b) => vis(b)); return { tab: t?.textContent || '', error: vis(document.getElementById('error')) ? document.getElementById('error').textContent : '' }; })()`);
    // (an operation on the 4th axis changed to indexed roughing and back: its Type list offers
    // it, as the Add list does)
    const idxOpt = await read("[...(document.getElementById('opForm')?.elements.__type?.options || [])].map((o) => o.textContent).join('|')");
    check("carry-through: on the 4th axis, an operation's Type list offers Indexed roughing (3+1)", /Indexed roughing \(3\+1\)/.test(idxOpt), idxOpt);
    // (the knurl's simulation: no gouge where the knurl never cut, the carried piece's own
    // resolution on the turned OD was counted as 3340 gouged cells)
    // (as a person does: the playback folded, the view's Simulated stock toggle on, the
    // simulation runs behind)
    if (await where("#simClose")) await click("#simClose");
    await settled();
    const stockBtn = await read("(() => { const b = [...document.querySelectorAll('.view-toggles button')].find((b) => /Simulated stock/.test(b.title)); const r = b.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2, b.getAttribute('aria-pressed') === 'true']; })()");
    // (on already: off and on again, its simulation started)
    if (stockBtn[2]) {
      await drag(stockBtn, stockBtn, 1);
      await sleep(500);
    }
    await drag(stockBtn, stockBtn, 1);
    const stockToggle = stockBtn[2] ? null : stockBtn;
    await waitFor(() => read("(window.__marcam.state.sim?.stocks || []).length > 0"), "the knurl's simulation", 180000 * SLACK).catch(() => {});
    await settled();
    const gouges = await read("({ error: document.getElementById('error').textContent, cells: (window.__marcam.state.sim?.stocks || []).map((s) => s.gouge_cells), summary: document.getElementById('summary').textContent.split('Simulated stock')[1]?.slice(0, 160) || '' })");
    check("carry-through: the knurl simulated: no gouge reported (the turned OD it never cut is not counted)", gouges.cells.length > 0 && !/gouged/.test(gouges.error) && gouges.cells.every((n) => n === 0), JSON.stringify(gouges));
    if (stockToggle) await drag(stockToggle, stockToggle, 1);
    check("carry-through: the knurl on the OD picked on the part (radius 10, clear of the chuck, behind the turned-down part), planned",
      /Cylinder 20\.00/.test(odBand) && back && front && Math.abs(back[1] - 10) < 0.3 && Math.abs(front[1] - 10) < 0.3 && back[0] > -frame.length + 1.9 && front[0] <= -15 && /OK/.test(k.tab) && !k.error, `${odBand}; ${JSON.stringify(body)} ${JSON.stringify(k)}`);
    // its playback starts from the piece setup 1 left: the boss end narrower than the bar
    await click("#vtPlay");
    await waitFor(() => read("!!window.__marcam.state.machining?.r && window.__marcam.state.machining.open"), "the playback", 120000).catch(() => {});
    await sleep(500);
    const ends = await read("(() => { const m = window.__marcam.state.machining; const g = m.session.grid; const at = (x) => { const i = Math.round((x - g.x0) / g.dx - 0.5); let mx = 0; for (let j = 0; j < g.na; j++) mx = Math.max(mx, m.r[i * g.na + j]); return +mx.toFixed(2); }; return { boss: at(-3), od: at(-10), bar: at(-40) }; })()");
    await shot("carry_setup2_playing");
    check("carry-through: setup 2's playback starts from what setup 1 left (the ring round the boss, the turned-down OD), not a fresh bar",
      ends.od < ends.bar - 0.5 && ends.bar > 9.5 && ends.bar < 10.5, JSON.stringify(ends));
    await click("#vtPlay").catch(() => {});
    await clean("the cut piece carried on");
  }
  // ---------------------------------------------------------------- the view's paths and setups
  // (Mar: "toolpath color glitch": every path drawn in op1's blue while picking; "the toolpaths
  // don't always update when something changes ... hide and unhide"; "the carried over stock
  // looks weird ... should be solid"; "need a way to edit the type of setup after it's been
  // added")
  {
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await read("(() => { window.confirm = () => true; const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
    await read("window.__marcam.setView('top', {})");
    await sleep(1500);
    // each operation's swatch colour, at points along its own path (projected to the screen)
    const opsAt = `(() => { const st = window.__marcam.state, v = st.three; const p = st.result.programs[st.tab]; const pts = p.toolpath.points, n = pts.length / 4; const starts = (p.toolpath.ops || []).filter((o) => o.id && o.start != null).sort((a, b) => a.start - b.start); const setup = st.tree.doc.setups.find((s) => s.id === st.tree.setup); const PAL = ["#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#a855f7", "#14b8a6", "#e11d48", "#84cc16"]; const r = v.renderer.domElement.getBoundingClientRect(); return starts.map((o, k) => { const end = k + 1 < starts.length ? starts[k + 1].start : n; const i0 = setup.operations.findIndex((x) => x.id === o.id); const hex = setup.operations[i0]?.color || PAL[i0 % PAL.length]; const at = []; for (let i = o.start + 1; i < end && at.length < 40; i += Math.max(1, Math.floor((end - o.start) / 40))) { if (!(pts[4 * i + 3] % 2)) continue; const q = new v.THREE.Vector3(pts[4 * i], pts[4 * i + 1], pts[4 * i + 2]).project(v.camera); if (Math.abs(q.x) < 0.95 && Math.abs(q.y) < 0.95) at.push([r.left + (q.x + 1) / 2 * r.width, r.top + (1 - q.y) / 2 * r.height]); } return { id: o.id, hex, at }; }); })()`;
    // a colour's hue (degrees) and saturation: a path under the stock's light tint keeps its
    // hue, paler
    const hueSat = ([r, g, b]) => {
      const mx = Math.max(r, g, b), mn = Math.min(r, g, b), d = mx - mn;
      if (!d) return [0, 0];
      const h = mx === r ? ((g - b) / d) % 6 : mx === g ? (b - r) / d + 2 : (r - g) / d + 4;
      return [(h * 60 + 360) % 360, d / mx];
    };
    const near = (img, hex, at) => {
      const [h0] = hueSat([parseInt(hex.slice(1, 3), 16), parseInt(hex.slice(3, 5), 16), parseInt(hex.slice(5, 7), 16)]);
      let hit = 0;
      for (const [x, y] of at) {
        let ok = false;
        for (let dy = -3; dy <= 3 && !ok; dy++)
          for (let dx = -3; dx <= 3 && !ok; dx++) {
            const q = img.at(Math.round(x + dx), Math.round(y + dy));
            if (!q) continue;
            const [h, sat] = hueSat(q);
            const dh = Math.min(Math.abs(h - h0), 360 - Math.abs(h - h0));
            if (sat > 0.2 && dh < 22) ok = true;
          }
        if (ok) hit++;
      }
      return at.length ? hit / at.length : 0;
    };
    const coloursSeen = async (tag) => {
      const ops = await read(opsAt);
      const img = await shot(`paths_colours_${tag}`);
      return ops.map((o) => ({ id: o.id, hex: o.hex, at: o.at.length, seen: +near(img, o.hex, o.at).toFixed(2) }));
    };
    const normal = await coloursSeen("normal");
    check("the view: each operation's path drawn in its own swatch colour, where its path is", normal.length >= 3 && normal.filter((o) => o.at >= 3).every((o) => o.seen >= 0.5), JSON.stringify(normal));
    // (Mar: "all the other toolpaths disappeared when I made this last one"; "the part is gone
    // and I can't get it back") letters typed with the page focused, the pointer off the view
    // (a form drawn again under the caret): the view's toggles stay as they are
    const view0 = await read("JSON.stringify(window.__marcam.state.machining.view)");
    await read("(() => { document.activeElement?.blur?.(); return true; })()");
    await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: 5, y: 5 });
    for (const ch of "optsr") await type(ch);
    await settled();
    const view1 = await read("JSON.stringify(window.__marcam.state.machining.view)");
    check("the view: letters typed off the view (o, p, t, s, r) toggle nothing", view0 === view1, `${view0} -> ${view1}`);
    // a new operation added (on another tool than some): every operation's path still drawn in
    // its own colour (none hidden by it being selected)
    await read("(() => { const s = document.getElementById('treeAddType'); s.value = 'face'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#treeAdd");
    const libRow = await waitFor(() => where("dialog[open] .tp-row", "3\.175"), "the library", 8000).catch(() => null);
    if (libRow) await click("dialog[open] .tp-row", "3\.175");
    await waitFor(() => read("!window.__marcam.state.tree.draft"), "the facing added", 60000).catch(() => {});
    await settled();
    await read("window.__marcam.setView('top', {})");
    await sleep(1500);
    const added = await coloursSeen("added");
    const tools = await read("[...new Set(window.__marcam.state.tree.doc.setups[0].operations.map((o) => o.tool))].length");
    check("the view: a new operation added and selected, ops on several tools: every operation's path still in its own colour at its path", tools >= 2 && added.length === normal.length + 1 && added.filter((o) => o.at >= 3).every((o) => o.seen >= 0.5), JSON.stringify({ tools, added }));
    // picking for the pocket: the same colours (were all op1's blue)
    await click('#tree .tree-op[data-op="op2"] [data-act="select"]');
    await settled();
    // (picking is on while it is selected)
    await waitFor(() => read("!!window.__marcam.state.tree.picking"), "picking", 30000).catch(() => {});
    await read("window.__marcam.setView('top', {})");
    await sleep(1500);
    const picking = await coloursSeen("picking");
    check("the view while picking: each operation's path still in its own swatch colour (not all op1's)", picking.filter((o) => o.at >= 3).every((o) => o.seen >= 0.5) && picking.length === normal.length, JSON.stringify(picking));
    // the pocket's step-down and its colour changed: both on the screen, nothing toggled
    await read("window.__marcam.setView('iso', {})");
    await sleep(1200);
    const segs = () => read("(() => { const st = window.__marcam.state; const p = st.result.programs[st.tab]; return p.toolpath.points.length / 4; })()");
    const s0 = await segs();
    await openMore("opEditor");
    await typeInto("#opForm [name=step_down]", "0.5");
    await settled();
    const s1 = await segs();
    const drawn = await read("(() => { const v = window.__marcam.state.three, m = window.__marcam.state.machining; return { still: v.cutLines?.geometry.getAttribute('position')?.count || 0, playback: (m.paths?.cut || []).reduce((a, l) => a + l.geometry.getAttribute('position').count, 0) }; })()");
    const sw = await read("(() => { const r = document.querySelector('#tree .tree-op[data-op=\"op2\"] input[type=color]').getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()");
    await read("(() => { const i = document.querySelector('#tree .tree-op[data-op=\"op2\"] input[type=color]'); i.value = '#ff00ff'; i.dispatchEvent(new Event('input', { bubbles: true })); i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await settled();
    await sleep(800);
    const after = await coloursSeen("recoloured");
    const pocket = after.find((o) => o.id === "op2");
    check("the view: the pocket's step-down changed: its new path drawn (no hide/show); its colour changed: drawn in it at once",
      s1 > s0 && (drawn.still >= 2 * (s1 - 2) * 0.5 || drawn.playback > 0) && pocket?.hex === "#ff00ff" && pocket.seen >= 0.5, JSON.stringify({ s0, s1, drawn, pocket, sw }));
    await clean("the paths in their colours");
    // the carried piece: a solid (as the setup before's simulation), no stripes of edges
    await read("(() => { for (const [id, v] of [['treeAddKind', 'vise'], ['treeAddFrom', 'carry']]) { const s = document.getElementById(id); s.value = v; s.dispatchEvent(new Event('change', { bubbles: true })); } return true; })()");
    await click("#treeAddSetup");
    await waitFor(() => read("window.__marcam.state.three.groups.stock.children.some((o) => o.userData.carried)"), "the carried piece", 120000 * SLACK).catch(() => {});
    await settled();
    await read("window.__marcam.setView('iso', {})");
    await sleep(1500);
    const box = await read("(() => { const v = window.__marcam.state.three; const g = v.groups.stock; const b = new v.THREE.Box3().setFromObject(g); const r = v.renderer.domElement.getBoundingClientRect(); let lo = [1e9, 1e9], hi = [-1e9, -1e9]; for (const x of [b.min.x, b.max.x]) for (const y of [b.min.y, b.max.y]) for (const z of [b.min.z, b.max.z]) { const q = new v.THREE.Vector3(x, y, z).project(v.camera); const sx = r.left + (q.x + 1) / 2 * r.width, sy = r.top + (1 - q.y) / 2 * r.height; lo = [Math.min(lo[0], sx), Math.min(lo[1], sy)]; hi = [Math.max(hi[0], sx), Math.max(hi[1], sy)]; } return { lo, hi, edges: g.children.some((o) => o.children.some((c) => c.isLineSegments)), opaque: g.children.every((o) => o.children.every((c) => !c.material?.transparent)) }; })()");
    const pimg = await shot("carried_piece_solid");
    // the middle of its outline: covered (not the background), and few sharp light/dark
    // alternations along rows (stripes of edge lines)
    const [x0, y0] = [box.lo[0] + (box.hi[0] - box.lo[0]) * 0.3, box.lo[1] + (box.hi[1] - box.lo[1]) * 0.3];
    const [x1, y1] = [box.lo[0] + (box.hi[0] - box.lo[0]) * 0.7, box.lo[1] + (box.hi[1] - box.lo[1]) * 0.7];
    let covered = 0, n = 0, flips = 0, steps = 0;
    for (let y = Math.round(y0); y < y1; y += 3) {
      let prev = null;
      for (let x = Math.round(x0); x < x1; x += 1) {
        const q = pimg.at(x, y);
        if (!q) continue;
        const l = q[0] + q[1] + q[2];
        n++;
        if (l > 120) covered++;
        if (prev != null) {
          steps++;
          if (Math.abs(l - prev) > 90) flips++;
        }
        prev = l;
      }
    }
    check("the carried piece: drawn solid (opaque, no edge lines), no stripes across it", !box.edges && box.opaque && covered > n * 0.9 && flips < steps * 0.05, JSON.stringify({ edges: box.edges, opaque: box.opaque, covered, n, flips, steps }));
    // the setup's type, from its icon cards at the top of its details (Mar: "just move up the
    // icon based one"): for the carried setup, the
    // plate's first (a part, operations), and a setup with no part
    const typeSwitch = async (label, from, to) => {
      await click("#tree .tree-setup.active [data-act=setup]");
      await settled();
      const vr = await viewRect();
      const img0 = await shot(`type_${label}_before`);
      const before = await read("(() => { const st = window.__marcam.state; const s = st.tree.doc.setups.find((x) => x.id === st.tree.setup); return { kind: s.kind || '', holding: s.holding?.kind, stock: s.stock?.shape, jaws: st.three.groups.jaws?.children.length || 0, row: (r => Math.round(r.top))(document.getElementById('setupType').getBoundingClientRect()), icons: document.querySelectorAll('#setupType .sd-kind svg').length, shown: [...document.querySelectorAll('.sd-kind')].filter((e) => e.getBoundingClientRect().height > 0).length }; })()");
      await click(`#setupType [data-setkind=${to}]`);
      await waitFor(() => read(`(() => { const st = window.__marcam.state; return st.tree.doc.setups.find((x) => x.id === st.tree.setup)?.kind === '${to}'; })()`), `${label}: the type changed`, 30000).catch(() => {});
      await settled();
      await sleep(1000);
      const now = await read("(() => { const st = window.__marcam.state; const s = st.tree.doc.setups.find((x) => x.id === st.tree.setup); return { kind: s.kind || '', holding: s.holding?.kind, stock: s.stock?.shape, jaws: st.three.groups.jaws?.children.length || 0 }; })()");
      const img1 = await shot(`type_${label}_after`);
      let diff = 0, m = 0;
      for (let y = vr[1]; y < vr[1] + vr[3]; y += 4)
        for (let x = vr[0]; x < vr[0] + vr[2]; x += 4) {
          const p = img0.at(x, y), q = img1.at(x, y);
          m++;
          if (Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 60) diff++;
        }
      check(`the setup's type (${label}): its icon cards at the top of its details (none further down); ${from} -> ${to} switches it, the stock and the fixture change on the screen`,
        before.row < 600 && before.icons === 3 && before.shown === 3 && now.kind === to && now.holding !== before.holding && (to === "vise" ? now.jaws > 0 : now.jaws === 0) && diff > m * 0.02, JSON.stringify({ before, now, diff, m }));
    };
    await typeSwitch("carried", "3-axis vise", "rotary");
    // the plate's first setup: a part and operations
    await click('#tree .tree-setup[data-setup="vise1"] [data-act=setup]');
    await settled();
    await typeSwitch("a part, operations", "3-axis vise", "rotary");
    await typeSwitch("back", "4th axis", "vise");
    // a setup with no part (a new project)
    await click("#menuFile > summary");
    await sleep(200);
    await click("#projectNew");
    await settled();
    await read("(() => { const s = document.getElementById('treeAddKind'); s.value = 'rotary'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#treeAddSetup");
    await settled();
    await typeSwitch("no part", "4th axis", "vise");
    await clean("the setup's type");
  }
  // ---------------------------------------------------------------- a custom toolpath
  // (Mar: "full custom toolpaths ... user generated gcode via python or similar") the plate:
  // Custom toolpath (G-code) added, a .nc made by a small Python script chosen in its form,
  // its lines read (what it can't read listed), its path on the screen, then simulated
  {
    const nc = join(mkdtempSync(join(tmpdir(), "marcam-custom-")), "custom_slot.nc");
    const py = spawnSync(process.env.PYTHON || "python", [join(dirname(fileURLToPath(import.meta.url)), "fixtures", "custom_slot.py"), nc], { encoding: "utf8" });
    if (py.status !== 0) throw new Error(`the custom toolpath's Python fixture: ${py.stderr || py.error}`);
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await read("(() => { window.confirm = () => true; const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
    const offered = await read("[...document.getElementById('treeAddType').options].some((o) => o.value === 'custom' && /Custom toolpath/.test(o.textContent))");
    check("a custom toolpath: the operation picker offers \"Custom toolpath (G-code)\"", offered);
    await read("(() => { const s = document.getElementById('treeAddType'); s.value = 'custom'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    const tabs0 = await read("JSON.stringify([...document.querySelectorAll('#tabs button')].map((b) => b.textContent))");
    await click("#treeAdd");
    const lib = await waitFor(() => where("dialog[open] .tp-row", "3\\.175"), "the library", 15000).catch(() => null);
    if (lib) await click("dialog[open] .tp-row", "3\\.175");
    await waitFor(() => where("#opForm [name=gcode]"), "the custom toolpath's form", 60000);
    await settled();
    // the program chosen as a file, as a person does (its file button)
    const chooser = new Promise((resolve) => {
      const t0 = events.length;
      const tick = setInterval(() => {
        const e = events.slice(t0).find((x) => x.method === "Page.fileChooserOpened");
        if (e) {
          clearInterval(tick);
          resolve(e.params);
        }
      }, 50);
    });
    await click("#opForm label.filebtn", "G-code file");
    const fc = await Promise.race([chooser, sleep(10000).then(() => null)]);
    if (!fc) throw new Error("a custom toolpath: no file chooser from its G-code file button");
    await S("DOM.setFileInputFiles", { files: [nc], backendNodeId: fc.backendNodeId });
    await waitFor(() => read("/feed moves read/.test(document.getElementById('gcodeLines')?.textContent || '')"), "the program read", 60000).catch(() => {});
    await settled();
    const lines = await read("document.getElementById('gcodeLines')?.textContent || ''");
    const prog = await read(`(() => { const vis = ${VISIBLE}; const d = JSON.parse(document.getElementById('job').value); const op = d.setups[0].operations.find((o) => o.type === 'custom'); return { op: !!op, lines: (op?.gcode || '').split('\\n').length, tabs: [...document.querySelectorAll('#tabs button')].filter((b) => vis(b)).map((b) => b.textContent), error: vis(document.getElementById('error')) ? document.getElementById('error').textContent : '' }; })()`);
    check("a custom toolpath: the .nc from a Python script chosen in its form: read (its feed moves counted, nothing it couldn't read), in the project, planned",
      /\d+ feed moves read/.test(lines) && /0 errors/.test(lines) && prog.op && prog.lines > 20 && prog.tabs.some((t) => /OK|issue/.test(t)) && !prog.error, `${lines.slice(0, 160)} | ${JSON.stringify(prog)} | before ${tabs0}`);
    // its path on the screen: the blue of its cuts where the slot and the pocket are
    await read("window.__marcam.setView('top', {})");
    await sleep(1200);
    const vr = await viewRect();
    const img = await shot("custom_toolpath_path");
    const at = (x, y) => read(`(() => { const v = window.__marcam.state.three; const p = new v.THREE.Vector3(${x}, ${y}, -0.5).project(v.camera); const r = v.renderer.domElement.getBoundingClientRect(); return [r.left + (p.x + 1) / 2 * r.width, r.top + (1 - p.y) / 2 * r.height]; })()`);
    const blueNear = async (x, y) => {
      const [px, py2] = await at(x, y);
      let n = 0;
      for (let dy = -6; dy <= 6; dy++)
        for (let dx = -6; dx <= 6; dx++) {
          const q = img.at(Math.round(px + dx), Math.round(py2 + dy));
          if (q && q[2] > 120 && q[2] > q[0] + 40) n++;
        }
      return n;
    };
    const onSlot = await blueNear(20, 8), inPocket = await blueNear(44, 24);
    check("a custom toolpath: its path on the screen (its cuts drawn at the slot and in the pocket)", onSlot > 3 && inPocket > 3, JSON.stringify({ onSlot, inPocket, view: vr }));
    // simulated: the stock as the program leaves it, on the screen (the view's Simulated stock
    // toggle on, as a person does)
    // (left on by a section before: off, then on, as a person does to see it again)
    const simState = await read("(() => { const b = [...document.querySelectorAll('.view-toggles button')].find((b) => /Simulated stock/.test(b.title)); const r = b.getBoundingClientRect(); return { on: b.getAttribute('aria-pressed') === 'true', at: [r.left + r.width / 2, r.top + r.height / 2] }; })()");
    const simToggle = simState.at;
    if (simState.on) {
      await drag(simToggle, simToggle, 1);
      await settled();
    }
    await drag(simToggle, simToggle, 1);
    await waitFor(() => read("/Simulated stock/.test(document.getElementById('summary').textContent)"), "the simulation", 180000 * SLACK).catch(() => {});
    await settled();
    const sim = await read(`(() => { const vis = ${VISIBLE}; return { status: document.getElementById('status').textContent.slice(0, 80), summary: [...document.querySelectorAll('#summary *')].filter((e) => vis(e)).map((e) => e.textContent).find((t) => /Simulated stock/.test(t))?.slice(0, 120) || '' }; })()`);
    await shot("custom_toolpath_simulated");
    check("a custom toolpath: simulated, the stock it leaves on the screen", /Simulated stock/.test(sim.summary), JSON.stringify(sim));
    // (left as it was)
    if (!simState.on) await drag(simToggle, simToggle, 1);
    await clean("a custom toolpath");
  }
  // ---------------------------------------------------------------- tree rows: the whole row
  // (Mar: "the setup line is hard to click on. you have to click on the bold word only. this
  // is different than the other lines in the tree where you can click anywhere on the row")
  // the plate: an operation selected, then each row clicked in its empty right-hand part (not
  // on its words): what it is for shown
  {
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await read("(() => { window.confirm = () => true; const s = document.getElementById('example'); s.value = 'plate'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
    // a point of the row right of its words (and left of its buttons), and what is there
    const emptyOf = (css) => read(`(() => { const row = document.querySelector(${JSON.stringify(css)}); if (!row) return null; row.scrollIntoView({ block: 'nearest' }); const r = row.getBoundingClientRect(); let right = r.right - 6; for (const b of row.querySelectorAll('button')) { const q = b.getBoundingClientRect(); if (q.width && q.left > r.left + r.width / 2) right = Math.min(right, q.left - 6); } let words = r.left; for (const e of row.querySelectorAll('.tree-label, .muted')) { const q = e.getBoundingClientRect(); for (const rr of e.getClientRects()) if (Math.abs(rr.top + rr.height / 2 - (r.top + r.height / 2)) < r.height / 2) words = Math.max(words, rr.right); } const x = Math.round((words + right) / 2), y = Math.round(r.top + r.height / 2); const hit = document.elementFromPoint(x, y); return { at: [x, y], room: Math.round(right - words), hit: hit ? hit.className || hit.tagName : null, onWords: !!hit?.closest('.tree-label') && hit.closest('.tree-label') !== hit.closest('.tree-op .tree-label') }; })()`);
    const rows = [
      ["the setup's line", ".tree-setup.active > .tree-setuphead", "!!document.querySelector('#setupDialog.embedded') && document.querySelector('#setupDialog.embedded').getBoundingClientRect().height > 0"],
      ["Stock", ".tree-stock[data-node=stock]", "!!document.getElementById('stockPane')?.getBoundingClientRect().height"],
      ["Part", ".tree-stock[data-node=part]", "!!document.getElementById('partPane')?.getBoundingClientRect().height"],
      ["Fixture", ".tree-stock[data-node=fixture]", "!!document.getElementById('fixturePane')?.getBoundingClientRect().height"],
      ["a tool", ".tree-tool", "!!document.querySelector('.tool-details')?.getBoundingClientRect().height"],
      ["the Tool library", ".tree-lib > .tree-setuphead", "!!document.querySelector('.tree-lib.sel')"],
    ];
    for (const [what, css, shown] of rows) {
      // (an operation selected first: its form shown, not this row's details)
      await click(".tree-op .tree-label");
      await settled();
      const before = await read(shown);
      const e = await emptyOf(css);
      if (!e) {
        check(`tree rows: ${what} clicked right of its words: its details shown`, false, `no ${css}`);
        continue;
      }
      await drag(e.at, e.at, 1);
      await settled();
      const after = await read(shown);
      check(`tree rows: ${what} clicked right of its words (not on them): what it is for shown`, !before && after && e.room > 20 && !e.onWords, JSON.stringify({ before, after, ...e }));
    }
    await shot("tree_row_clicked");
    await clean("tree rows");
  }
  // ---------------------------------------------------------------- playback: one clock
  // (Mar: "the simulation sliders [are] not very consistent. changing the speed ... causes
  // visual bugs and the wrong playback speed"; chunky when fast) the knob playing with its
  // stock; the speed slider dragged low, high, low: the machine clock runs at the speed shown,
  // frames in budget, the stock right behind; the scrubber dragged while playing: it goes on
  // from there; paused, the stock drawn is the one a fresh seek to that place draws
  {
    await S("Page.navigate", { url });
    await waitFor(() => read("!!document.getElementById('status') && !/Loading engine/i.test(document.getElementById('status').textContent) && document.body.dataset.state !== 'busy'"), "the app", 120000);
    await sleep(1000);
    await read("(() => { const s = document.getElementById('example'); s.value = 'knob'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()");
    await click("#loadExample");
    await settled();
    // the stock on (its toggle), then Play
    const stockBtn = await read("(() => { const b = [...document.querySelectorAll('.view-toggles button')].find((b) => /Simulated stock/.test(b.title)); const r = b.getBoundingClientRect(); return b.getAttribute('aria-pressed') === 'true' ? null : [r.left + r.width / 2, r.top + r.height / 2]; })()");
    if (stockBtn) await drag(stockBtn, stockBtn, 1);
    await sleep(800);
    await click("#vtPlay");
    await waitFor(() => read("document.body.dataset.playing === 'yes' && !!window.__marcam.state.machining?.session"), "the playback playing", 120000);
    await sleep(1500);
    const slider = await read("(() => { const r = document.getElementById('simSpeed').getBoundingClientRect(); return [r.left, r.top + r.height / 2, r.width]; })()");
    // the slider's place for a speed (×10^v, v 0..3)
    const at = (v) => [slider[0] + 8 + ((slider[2] - 16) * v) / 3, slider[1]];
    const thumb = async () => at(+(await read("document.getElementById('simSpeed').value")));
    // (the playback's machine clock itself, not the move the playhead is on: a move seconds
    // long held the move number still and the rate read low or high by where it fell)
    // (the clock against the frames' time, each counted to 0.1 s at most as the playback does:
    // a frame held longer slows it, never jumps it; CI's software GL drew few, long frames)
    const measure = () => read(`(async () => { const m = window.__marcam.state.machining; const tAt = (k) => (k <= 0 ? 0 : m.session.times[Math.min(k, m.session.times.length) - 1]); const h = () => (m.playing && m.head != null ? m.head : m.pos); const clk = () => (m.playing && m.clock != null ? m.clock : tAt(h())); const w0 = performance.now(), c0 = clk(); let worst = 0, last = w0, capped = 0; await new Promise((r) => { const f = (t) => { worst = Math.max(worst, t - last); capped += Math.min(100, t - last); last = t; if (performance.now() - w0 < 2500) requestAnimationFrame(f); else r(); }; requestAnimationFrame(f); }); const w = capped / 1000; const shown = document.getElementById('simSpeedLabel').textContent; return { shown, factor: Math.pow(10, +document.getElementById('simSpeed').value), rate: (clk() - c0) / w, worst, lag: h() - m.pos }; })()`);
    const runs = [];
    for (const v of [0.3, 2, 0.3]) {
      await drag(await thumb(), at(v), 10);
      await sleep(700);
      runs.push(await measure());
    }
    const SL = Number(process.env.MARCAM_PERF_SLACK || 1);
    const rateOk = runs.every((r) => Math.abs(r.rate / r.factor - 1) < 0.1);
    const framesOk = runs.every((r) => r.worst < 50 * SL);
    check("playback: the speed slider dragged low, high, low while playing: the machine clock runs at the speed shown (within 10%) each time", rateOk, JSON.stringify(runs.map((r) => ({ shown: r.shown, rate: +r.rate.toFixed(2) }))));
    check("playback: frames in budget at every speed (none held over 50 ms), the stock right behind the playhead", framesOk, JSON.stringify(runs.map((r) => ({ worst: Math.round(r.worst), lag: r.lag }))));
    // one loop: Pause and Play pressed quickly twice; the clock still at the speed shown
    await click("#simPlay");
    await click("#simPlay");
    await click("#simPlay");
    await click("#simPlay");
    // (measured once it is playing again: Play first draws the stock afresh, then moves)
    await waitFor(async () => { const a = await read("window.__marcam.state.machining.head ?? -1"); await sleep(300); return (await read("window.__marcam.state.machining.head ?? -1")) > a; }, "playing again", 30000).catch(() => {});
    // (and the stock drawn again for the Play pressed last, a build queued behind the first:
    // the frame it holds is capped at 0.1 s of clock, so measured over it the clock runs slow)
    await sleep(1500);
    const again = await measure();
    check("playback: Pause/Play pressed quickly twice: one loop, still at the speed shown (not twice it)", Math.abs(again.rate / again.factor - 1) < 0.1, JSON.stringify({ shown: again.shown, rate: +again.rate.toFixed(2) }));
    // the scrubber dragged while playing: on from where it was dropped
    const scrub = await read("(() => { const r = document.getElementById('simScrub').getBoundingClientRect(); return [r.left, r.top + r.height / 2, r.width, +document.getElementById('simScrub').max]; })()");
    const pos = () => read("(() => { const m = window.__marcam.state.machining; return m.playing && m.head != null ? m.head : m.pos; })()");
    const xOf = (k) => scrub[0] + 8 + ((scrub[2] - 16) * k) / scrub[3];
    const now = await pos();
    const goal = Math.round(scrub[3] * 0.25);
    await drag([xOf(now), scrub[1]], [xOf(goal), scrub[1]], 12);
    await sleep(1200);
    const after = await pos();
    check("playback: the scrubber dragged while playing: the playhead goes on from where it was dropped", Math.abs(after - goal) < scrub[3] * 0.05 && after >= goal - 5, JSON.stringify({ from: now, dropped: goal, after }));
    // paused: the stock drawn is what a fresh seek to that place draws
    await click("#simPlay");
    await waitFor(() => read("document.body.dataset.playing !== 'yes' && !window.__marcam.state.machining.busy"), "paused", 30000);
    await sleep(800);
    const here = await read("window.__marcam.state.machining.pos");
    const vr = await viewRect();
    const a = await shot("playback_paused");
    // (back to the start and to the same place again by the scrubber: a fresh seek)
    await drag([xOf(here), scrub[1]], [xOf(0), scrub[1]], 6);
    await waitFor(() => read("window.__marcam.state.machining.pos === 0 && !window.__marcam.state.machining.busy"), "the start", 60000).catch(() => {});
    await read(`(() => { const s = document.getElementById('simScrub'); s.value = '${here}'; s.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await waitFor(() => read(`window.__marcam.state.machining.pos === ${here} && !window.__marcam.state.machining.busy`), "the same place again", 120000).catch(() => {});
    await sleep(800);
    const b = await shot("playback_fresh_seek");
    let diff = 0, n = 0;
    for (let y = vr[1]; y < vr[1] + vr[3]; y += 2)
      for (let x = vr[0]; x < vr[0] + vr[2]; x += 2) {
        const p = a.at(x, y), q = b.at(x, y);
        n++;
        if (Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 60) diff++;
      }
    check("playback: paused after all that, the stock drawn is the same as a fresh seek to that place draws", diff < n * 0.01, `${diff} of ${n} px differ at move ${here}`);
    await clean("the playback's speed and scrubber");
  }
} catch (e) {
  check(`the path could be followed (${e.message})`, false);
  await shot("failure").catch(() => {});
}
console.log(`\n${fails.length ? `${fails.length} FAILED` : "all passed"} (${url}); screenshots in ${shots}`);
process.exit(fails.length ? 1 : 0);
