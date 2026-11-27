// The playback's stock on screen: drawn exact (web/gpu/exact.js) or as the tri-dexel's tiles
// (--tiles: as before it), each at the same cameras, as PNGs; and how long the exact one took
// to come and to draw (ms).
//
//   node web/tests/exact_view.mjs --project <file.marcam | example name> --out <dir> [--tiles]
//        [--name <prefix>] [--views normal,close,far] [--frames 20]
//
// Views: "normal" the playback's own framing; "close" 4x nearer its target; "far" 16x nearer.
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { sleep, startApp, waitFor } from "./harness.mjs";

const arg = (k, d) => (process.argv.includes(k) ? process.argv[process.argv.indexOf(k) + 1] : d);
const project = arg("--project", "knob");
const out = resolve(arg("--out", "."));
const tiles = process.argv.includes("--tiles");
const name = arg("--name", tiles ? "before" : "after");
const views = arg("--views", "normal,close,far").split(",");
const frames = Number(arg("--frames", 20));
// a camera of its own: "px,py,pz,tx,ty,tz" (position, target), for the same view every time
const cam = arg("--camera", null);
mkdirSync(out, { recursive: true });

const T0 = Date.now();
const say = (what) => process.stderr.write(`[${((Date.now() - T0) / 1000).toFixed(0)} s] ${what}
`);
// (--root: another checkout served, as an older build for the "before")
const rootArg = arg("--root", null);
const app = await startApp(rootArg ? { root: resolve(rootArg) } : {});
say("app up");
const { js, S } = app;
const report = { project, tiles, views: {} };
try {
  if (tiles) await js("window.__marcam.noExact = true").catch(() => {});
  if (existsSync(project)) {
    const doc = await S("DOM.getDocument");
    const { nodeId } = await S("DOM.querySelector", { nodeId: doc.root.nodeId, selector: "#projectOpenFile" });
    await S("DOM.setFileInputFiles", { files: [resolve(project)], nodeId });
    await sleep(1500);
  } else {
    await js(`document.getElementById('example').value = '${project}'; document.getElementById('loadExample').click(); true`);
    await sleep(500);
  }
  const t0 = Date.now();
  say("project asked for");
  await waitFor(() => js("document.body.dataset.state === 'done' && !!window.__marcam.state.result"), "planned", 400000);
  say("planned");
  report.planMs = Date.now() - t0;
  await js("(() => { const m = window.__marcam.state.machining; if (!m.view.stock) window.__marcam.machining.toggle('stock'); return true; })()");
  if (!(await js("window.__marcam.state.machining.open"))) await js("document.getElementById('simOpen').click(); true");
  const t1 = Date.now();
  await waitFor(() => js("(() => { const m = window.__marcam.state.machining; return !!(m.open && m.session && m.stockMesh && !m.busy); })()"), "the playback with its stock", 900000);
  report.openMs = Date.now() - t1;
  say("playback open");
  if (!tiles) {
    await waitFor(() => js("!!window.__marcam.state.machining.exact?.ready"), "the exact stock", 300000);
    report.exactMs = Date.now() - t1;
    report.compileMs = await js("Math.round(window.__marcam.state.machining.exact.readyMs)");
    report.pieces = await js("window.__marcam.state.machining.exact.count");
  }
  // (the toolpath hidden: the stock is what is compared)
  if (!process.argv.includes("--paths")) await js("(() => { const m = window.__marcam.state.machining; for (let i = 0; i < 3 && m.view.paths !== 'off'; i++) window.__marcam.machining.toggle('paths'); return m.view.paths; })()");
  // (the engine idle: no background simulation drawing meanwhile)
  await waitFor(() => js("!(window.__marcam.simLock?.busy)"), "the engine idle", 30000).catch(() => {});
  await sleep(1000);
  if (cam) {
    const [px, py, pz, tx, ty, tz] = cam.split(",").map(Number);
    await js(`(() => { const v = window.__marcam.state.three; v.camera.position.set(${px}, ${py}, ${pz}); v.controls.target.set(${tx}, ${ty}, ${tz}); v.controls.update(); return true; })()`);
  }
  let base = await js("(() => { const v = window.__marcam.state.three; return [...v.camera.position.toArray(), ...v.controls.target.toArray()]; })()");
  // --aim x,y (the view's fraction across and down): the nearer views close in on the stock's
  // surface there (the tiles met by a ray), not on a target inside the bar
  const aim = arg("--aim", null);
  if (aim) {
    const [ax, ay] = aim.split(",").map(Number);
    const hit = await js(`(() => { const v = window.__marcam.state.three, m = window.__marcam.state.machining; const rc = new v.THREE.Raycaster(); rc.setFromCamera(new v.THREE.Vector2(${2 * ax - 1}, ${1 - 2 * ay}), v.camera); const was = m.stockMesh.visible; m.stockMesh.visible = true; const h = rc.intersectObject(m.stockMesh, true)[0]; m.stockMesh.visible = was; return h ? h.point.toArray() : null; })()`);
    if (hit) base = [...base.slice(0, 3), ...hit];
    report.aim = hit;
  }
  report.camera = base;
  for (const vw of views) {
    const k = { normal: 1, close: 4, far: 16, extreme: 64 }[vw] ?? 1;
    await js(`(() => { const v = window.__marcam.state.three; const b = ${JSON.stringify(base)}; const t = new v.THREE.Vector3(b[3], b[4], b[5]); const p = new v.THREE.Vector3(b[0], b[1], b[2]).sub(t).multiplyScalar(${1 / k}).add(t); v.camera.position.copy(p); v.controls.target.copy(t); v.controls.update(); return true; })()`);
    // the exact stock drawn whole for this view (the view at rest): how long it took
    const ms = {};
    if (!tiles) {
      await waitFor(() => js("!!window.__marcam.state.machining.exact?.complete"), "the exact view drawn", 120000);
      Object.assign(ms, await js("(() => { const e = window.__marcam.state.machining.exact; return { settleMs: Math.round(e.settleMs), drawMs: Math.round(e.lastMs) }; })()"));
    } else await sleep(1500);
    // orbiting: frames drawn as the camera turns (each one waits for the GPU: a pixel read)
    Object.assign(ms, await js(`(() => { const v = window.__marcam.state.three; const gl = v.renderer.getContext(); const px = new Uint8Array(4); const t = []; const T = v.controls.target.clone(); const p0 = v.camera.position.clone(); for (let i = 0; i < ${frames}; i++) { const a = performance.now(); const q = p0.clone().sub(T).applyAxisAngle(new v.THREE.Vector3(0, 0, 1), 0.01 * (i + 1)).add(T); v.camera.position.copy(q); v.camera.lookAt(T); window.__marcam.machining.exactStep?.(); v.renderer.render(v.scene, v.camera); gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px); t.push(performance.now() - a); } v.camera.position.copy(p0); v.camera.lookAt(T); v.controls.update(); t.sort((x, y) => x - y); return { orbitMedianMs: +t[t.length >> 1].toFixed(1), orbitMaxMs: +t[t.length - 1].toFixed(1) }; })()`));
    if (!tiles) await waitFor(() => js("!!window.__marcam.state.machining.exact?.complete"), "the exact view drawn again", 120000);
    await sleep(300);
    const { data } = await S("Page.captureScreenshot", { format: "png" });
    // (--zoom-names: the files named by their zoom, 1x 4x 16x 64x)
    const label = process.argv.includes("--zoom-names") ? { normal: "1x", close: "4x", far: "16x", extreme: "64x" }[vw] : vw;
    writeFileSync(join(out, `${name}_${label}.png`), Buffer.from(data, "base64"));
    report.views[vw] = ms;
  }
  report.errors = app.pageErrors();
  report.gl = await js("(() => { const v = window.__marcam.state.three; const gl = v.renderer.getContext(); return { lost: gl.isContextLost(), size: [gl.drawingBufferWidth, gl.drawingBufferHeight], calls: v.renderer.info.render.calls, programs: v.renderer.info.programs?.length }; })()");
} catch (e) {
  report.error = e.message;
  report.status = await js("JSON.stringify({ state: document.body.dataset.state, title: document.title, status: document.getElementById('status')?.textContent, error: document.getElementById('error')?.textContent, playback: (() => { const m = window.__marcam.state.machining; return { open: m.open, stock: m.stock, solid: m.solid, busy: m.busy, note: m.note, mesh: !!m.stockMesh, progress: document.getElementById('simProgress')?.textContent }; })() })").catch(() => "");
  await app.shot("exact_view_error").catch(() => {});
  report.errors = app.pageErrors();
} finally {
  console.log(JSON.stringify(report));
  await app.close();
  process.exit(report.error ? 1 : 0);
}
