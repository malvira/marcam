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

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";
import { findBrowser, freePort, sleep, waitFor } from "./harness.mjs";

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
    await waitFor(async () => { try { return (await fetch(url)).ok; } catch { return false; } }, "the server", 15000);
  }
  const profile = mkdtempSync(join(tmpdir(), "marcam-eye-"));
  const dport = await freePort();
  children.push(spawn(findBrowser(), ["--headless=new", `--remote-debugging-port=${dport}`, `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", ...(process.env.MARCAM_E2E_SOFTWARE_GL ? ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] : []), "about:blank"], { stdio: "ignore" }));
  const version = await waitFor(async () => { try { return await (await fetch(`http://127.0.0.1:${dport}/json/version`)).json(); } catch { return null; } }, "the browser", 20000);
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
    } else if (msg.method) events.push(msg);
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
  await S("Input.dispatchKeyEvent", { type: "keyDown", key: ch, text: ch, windowsVirtualKeyCode: ch.charCodeAt(0) });
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
/** The horizontal extent (px) of the stock's outline colour in the view. */
function stockSpan(img, [x0, y0, w, h]) {
  let lo = Infinity, hi = -Infinity;
  for (let y = y0; y < y0 + h; y += 2)
    for (let x = x0; x < x0 + w; x += 1) {
      const [r, g, b] = img.at(x, y);
      // the stock's outline (#a5d0ff) and its tint
      if (Math.abs(r - 165) < 30 && Math.abs(g - 208) < 30 && b > 225) {
        lo = Math.min(lo, x);
        hi = Math.max(hi, x);
      }
    }
  return hi > lo ? hi - lo : 0;
}
/** The centroid of the part's colour (#c8ccd2, lit: greys) in the view. */
function partAt(img, [x0, y0, w, h]) {
  let sx = 0, n = 0;
  for (let y = y0; y < y0 + h; y += 2)
    for (let x = x0; x < x0 + w; x += 2) {
      const [r, g, b] = img.at(x, y);
      if (r > 120 && Math.abs(r - g) < 12 && Math.abs(g - b) < 14 && r < 245) {
        sx += x;
        n++;
      }
    }
  return n ? sx / n : null;
}

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
  // the knob's STL through the file chooser
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
  await click("#importStl");
  const fc = await Promise.race([chooser, sleep(10000).then(() => null)]);
  if (!fc) throw new Error("no file chooser opened by Import STL");
  await S("DOM.setFileInputFiles", { files: [join(root, "examples", "knob", "knob.stl")], backendNodeId: fc.backendNodeId });
  await sleep(1500);
  await clean("Import STL");
  await waitFor(() => where("#tree .tree-setup [data-act=setup]"), "a setup in the tree", 60000);
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
  const span0 = stockSpan(img, r);
  const field = await where("#stX") || await where("#stLen");
  check("the stock's size is a field in the pane", !!field);
  const spans = [];
  if (field) {
    await click(await read("document.getElementById('stX') ? '#stX' : '#stLen'"));
    await key("a", 2);
    for (const ch of ["1", "5", "0"]) {
      await type(ch);
      await sleep(900);
      spans.push(stockSpan(await shot(`typed_${ch}`), r));
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
  const before = partAt(img, r);
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
    const from = near[Math.min(near.length - 1, 6)] || red;
    d[0] = from[0] - c[0];
    d[1] = from[1] - c[1];
    l = Math.hypot(...d) || 1;
    const shownMid = await drag(from, [from[0] + (60 * d[0]) / l, from[1] + (60 * d[1]) / l]);
    await sleep(2500);
    img = await shot("part_dragged");
    const after = partAt(img, r);
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
} catch (e) {
  check(`the path could be followed (${e.message})`, false);
  await shot("failure").catch(() => {});
}
console.log(`\n${fails.length ? `${fails.length} FAILED` : "all passed"} (${url}); screenshots in ${shots}`);
process.exit(fails.length ? 1 : 0);
