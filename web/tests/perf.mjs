// Playback smoothness: frame times, main-thread long tasks and how far the tool jumps per
// frame while the machining playback plays (with the stock simulated along), on the knob
// (rotary) and the plate (vise), at a few speeds. Prints a table; with --gate, fails when a
// frame budget is broken (see GATE).
//
//   node web/tests/perf.mjs                      (this checkout)
//   node web/tests/perf.mjs --root ../old --old  (another checkout, the older playback)
//   node web/tests/perf.mjs --gate               (fail on a broken budget)
//
// Needs web/pkg (scripts/build-wasm.sh) and Chrome, Edge or Chromium.
import { sleep, startApp, waitFor } from "./harness.mjs";

const arg = (name, dflt) => (process.argv.includes(name) ? process.argv[process.argv.indexOf(name) + 1] : dflt);
const ROOT = arg("--root", undefined);
const OLD = process.argv.includes("--old");
const GATE = process.argv.includes("--gate");
const SECONDS = Number(arg("--seconds", 4));
// the budget: no main-thread task over 50 ms, the median frame under 20 ms (slower CI
// runners: MARCAM_PERF_SLACK multiplies both)
const SLACK = Number(process.env.MARCAM_PERF_SLACK || 1);
// (with software GL, as on CI's runners, a frame's time is the renderer's, not the page's:
// only the page's long tasks are held to the budget there)
const SOFT_GL = !!process.env.MARCAM_E2E_SOFTWARE_GL;
const BUDGET = { longTask: 50 * SLACK, medianFrame: SOFT_GL ? Infinity : 20 * SLACK };

const app = await startApp(ROOT ? { root: ROOT } : {});
const { js } = app;
const rows = [];
let failed = false;
try {
  for (const ex of ["knob", "plate"]) {
    await js(`document.getElementById('example').value = '${ex}'; document.getElementById('loadExample').click(); true`);
    await sleep(500);
    if (OLD) {
      await js("document.getElementById('generate').click(); true");
    }
    await waitFor(() => js("document.body.dataset.state === 'done' && !!window.__marcam.state.result"), `${ex} planned`, 240000);
    await sleep(1000);
    // the stock simulated along with the playback
    if (!OLD) {
      await js("(() => { const m = window.__marcam.state.machining; if (window.__marcam.machining.toggle) { if (!m.view.stock) window.__marcam.machining.toggle('stock'); } else { const s = document.getElementById('simStock'); s.checked = true; s.dispatchEvent(new Event('change')); } return true; })()");
    }
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("(() => { const m = window.__marcam.state.machining; return !!(m.open && m.session && m.stockMesh && !m.busy); })()"), `${ex} playback with stock`, 240000);
    // (no background simulation competing: it is measured on its own elsewhere)
    await waitFor(() => js("!(window.__marcam.simLock?.busy) && !(window.__marcam.simRing?.active)"), "the engine idle", 240000).catch(() => {});
    const speeds = OLD ? ["300", "3000"] : ["1", "2", "3"];
    for (const sp of speeds) {
      await js(`document.getElementById('simSpeed').value = '${sp}'; document.getElementById('simSpeed').dispatchEvent(new Event('input')); document.getElementById('simSpeed').dispatchEvent(new Event('change')); true`);
      await js("window.__marcam.machining.seek(0).then(() => true)");
      await waitFor(() => js("!window.__marcam.state.machining.busy"), "the playback at its start", 60000);
      // the probe: each frame's time and where the tool is (and the work's turn)
      await js(`(() => {
        const v = window.__marcam.state.three, m = window.__marcam.state.machining;
        const P = (window.__probe = { frames: [], long: [], tool: [], stop: false });
        try { P.obs = new PerformanceObserver((l) => { for (const e of l.getEntries()) P.long.push(e.duration); }); P.obs.observe({ type: 'longtask' }); } catch {}
        const tick = (t) => {
          if (P.stop) return;
          P.frames.push(t);
          const tm = v.groups.toolModel, w = v.groups.work;
          P.tool.push([tm.position.x, tm.position.y, tm.position.z, w.rotation.x, m.pos]);
          requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
        return true; })()`);
      await js("document.getElementById('simPlay').click(); true");
      await sleep(SECONDS * 1000);
      await js("if (window.__marcam.state.machining.playing) document.getElementById('simPlay').click(); window.__probe.stop = true; window.__probe.obs?.disconnect(); true");
      const r = await js(`(() => {
        const P = window.__probe;
        const dts = P.frames.slice(1).map((t, i) => t - P.frames[i]).sort((a, b) => a - b);
        const q = (p) => dts.length ? dts[Math.min(dts.length - 1, Math.floor(p * dts.length))] : 0;
        const jumps = [], turns = [];
        let still = 0;
        for (let i = 1; i < P.tool.length; i++) {
          const [a, b] = [P.tool[i - 1], P.tool[i]];
          const d = Math.hypot(b[0] - a[0], b[1] - a[1], b[2] - a[2]);
          jumps.push(d);
          turns.push(Math.abs(b[3] - a[3]) * 180 / Math.PI);
          if (d === 0 && b[3] === a[3]) still++;
        }
        const s = (xs) => xs.slice().sort((a, b) => a - b);
        const sj = s(jumps), st = s(turns);
        const pct = (xs, p) => xs.length ? xs[Math.min(xs.length - 1, Math.floor(p * xs.length))] : 0;
        return { frames: dts.length, median: q(0.5), p95: q(0.95), worst: dts[dts.length - 1] || 0,
          long: P.long.length, longMax: Math.max(0, ...P.long),
          jumpP95: pct(sj, 0.95), jumpMax: sj[sj.length - 1] || 0, turnP95: pct(st, 0.95), stillShare: still / Math.max(1, jumps.length),
          moves: P.tool.length ? P.tool[P.tool.length - 1][4] - P.tool[0][4] : 0 };
      })()`);
      const label = OLD ? `${sp} moves/s` : `x${Math.round(10 ** +sp)}`;
      rows.push({ ex, speed: label, ...r });
      console.log(`${ex.padEnd(6)} ${label.padEnd(14)} frames ${String(r.frames).padStart(4)}  median ${r.median.toFixed(1).padStart(5)} ms  p95 ${r.p95.toFixed(1).padStart(5)}  worst ${r.worst.toFixed(0).padStart(4)}  long tasks ${r.long} (max ${r.longMax.toFixed(0)} ms)  tool jump p95 ${r.jumpP95.toFixed(2)} mm, max ${r.jumpMax.toFixed(1)}  turn p95 ${r.turnP95.toFixed(1)}°  still ${(100 * r.stillShare).toFixed(0)}%  moves ${r.moves}`);
      if (GATE && (r.longMax > BUDGET.longTask || r.median > BUDGET.medianFrame)) {
        failed = true;
        console.log(`FAIL  ${ex} ${label}: longest task ${r.longMax.toFixed(0)} ms (budget ${BUDGET.longTask}), median frame ${r.median.toFixed(1)} ms (budget ${BUDGET.medianFrame})`);
      }
    }
    await js("document.getElementById('simClose').click(); true");
  }
  // while the 3D stock's snapshots are built in the background (the knob's playback with its
  // stock open, nothing else running), the camera orbits every frame and the playback is
  // scrubbed every half second: the page smooth, each scrub answered within a step, every
  // snapshot built
  if (!OLD) {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.body.dataset.state === 'done' && !!window.__marcam.state.result && window.__marcam.planIdle()"), "the knob planned", 240000);
    await js("(() => { const m = window.__marcam.state.machining; if (!m.view.stock) window.__marcam.machining.toggle('stock'); return true; })()");
    // (the knob plays whole in about a second, too short for snapshots: a snapshot here every
    // 30 ms of playing, so that there are some to build)
    await js("window.__marcam.state.machining.snapshotSavesMs = 30; true");
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("(() => { const m = window.__marcam.state.machining; return !!(m.open && m.session && m.stockMesh && !m.busy && m.solid); })()"), "the knob's playback with its 3D stock", 240000);
    await js("(() => { const m = window.__marcam.state.machining; if (m.playing) document.getElementById('simPlay').click(); return true; })()");
    await waitFor(() => js("!!window.__marcam.state.machining.warming"), "the snapshots building", 240000);
    await js(`(() => {
      const v = window.__marcam.state.three, m = window.__marcam.state.machining;
      const P = (window.__probe = { frames: [], long: [], waits: [], stop: false });
      try { P.obs = new PerformanceObserver((l) => { for (const e of l.getEntries()) P.long.push(e.duration); }); P.obs.observe({ type: 'longtask' }); } catch {}
      let k = 0, asked = null;
      const tick = (t) => {
        if (P.stop) return;
        P.frames.push(t);
        const c = v.camera, tg = v.controls?.target || { x: 0, y: 0, z: 0 };
        const dx = c.position.x - tg.x, dy = c.position.y - tg.y;
        const a = 0.02, cs = Math.cos(a), sn = Math.sin(a);
        c.position.x = tg.x + dx * cs - dy * sn;
        c.position.y = tg.y + dx * sn + dy * cs;
        c.lookAt(tg.x, tg.y, tg.z);
        // (a scrub every 30 frames, and how long it waited for the engine)
        if (asked && !m.busy) { P.waits.push(t - asked); asked = null; }
        if (k % 30 === 0 && m.session && !asked) {
          const s = document.getElementById('simScrub');
          s.value = String(Math.round(m.session.segments * (0.2 + 0.6 * ((k / 30) % 2)) + k));
          s.dispatchEvent(new Event('input'));
          asked = t;
        }
        k++;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      return true; })()`);
    const t0 = Date.now();
    await waitFor(() => js("!window.__marcam.state.machining.warming"), "the snapshots built", 300000);
    const builtIn = (Date.now() - t0) / 1000;
    await js("window.__probe.stop = true; window.__probe.obs?.disconnect(); true");
    const r = await js(`(() => { const P = window.__probe, m = window.__marcam.state.machining; const d = P.frames.slice(1).map((t, i) => t - P.frames[i]).sort((a, b) => a - b);
      const q = (p) => d[Math.min(d.length - 1, Math.floor(p * d.length))] || 0;
      return { frames: d.length, median: q(0.5), p95: q(0.95), worst: d[d.length - 1] || 0, long: P.long.length, longMax: Math.max(0, ...P.long), waitMax: Math.max(0, ...P.waits), scrubs: P.waits.length,
        built: (m.snapshots || []).length, wanted: m.snapshotInfo?.wanted ?? -1, mb: m.snapshotInfo?.mb ?? 0 }; })()`);
    console.log(`knob, snapshots built in the background (${builtIn.toFixed(1)} s, ${r.built}/${r.wanted}, ${r.mb.toFixed(1)} MB), orbiting and scrubbing: frames ${r.frames}  median ${r.median.toFixed(1)} ms  p95 ${r.p95.toFixed(1)}  worst ${r.worst.toFixed(0)}  long tasks ${r.long} (max ${r.longMax.toFixed(0)} ms)  scrubs ${r.scrubs}, longest wait ${r.waitMax.toFixed(0)} ms`);
    if (GATE && (r.longMax > BUDGET.longTask || r.median > BUDGET.medianFrame || r.built !== r.wanted || r.wanted < 2)) {
      failed = true;
      console.log(`FAIL  snapshots in the background: longest task ${r.longMax.toFixed(0)} ms (budget ${BUDGET.longTask}), median frame ${r.median.toFixed(1)} ms (budget ${BUDGET.medianFrame}), ${r.built} of ${r.wanted} snapshots`);
    }
    await js("document.getElementById('simClose').click(); true");
  }
  // while the toolpaths are planned again and the stock simulated in the background (an
  // edit of the knob), the camera orbits and the playback is scrubbed every frame: the page
  // must stay smooth
  if (!OLD) {
    await js("document.getElementById('example').value = 'knob'; document.getElementById('loadExample').click(); true");
    await waitFor(() => js("document.body.dataset.state === 'done' && !!window.__marcam.state.result && window.__marcam.planIdle()"), "the knob planned", 240000);
    await js("document.getElementById('simOpen').click(); true");
    await waitFor(() => js("(() => { const m = window.__marcam.state.machining; return !!(m.open && m.session && !m.busy); })()"), "the playback", 240000);
    await js(`(() => {
      const v = window.__marcam.state.three, m = window.__marcam.state.machining;
      const P = (window.__probe = { frames: [], long: [], stop: false });
      try { P.obs = new PerformanceObserver((l) => { for (const e of l.getEntries()) P.long.push(e.duration); }); P.obs.observe({ type: 'longtask' }); } catch {}
      let k = 0;
      const tick = (t) => {
        if (P.stop) return;
        P.frames.push(t);
        // orbit: the camera round the target
        const c = v.camera, tg = v.controls?.target || { x: 0, y: 0, z: 0 };
        const dx = c.position.x - tg.x, dy = c.position.y - tg.y;
        const a = 0.02, cs = Math.cos(a), sn = Math.sin(a);
        c.position.x = tg.x + dx * cs - dy * sn;
        c.position.y = tg.y + dx * sn + dy * cs;
        c.lookAt(tg.x, tg.y, tg.z);
        // scrub
        const s = document.getElementById('simScrub');
        if (m.session) { s.value = String((k * 97) % m.session.segments); s.dispatchEvent(new Event('input')); }
        k++;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      return true; })()`);
    // the edit: every operation's feed (all planned again), then the stock simulated
    await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); for (const p of j.programs || []) for (const o of p.operations || []) o.feed = Math.round(o.feed * 0.9); for (const s of j.setups || []) for (const o of s.operations || []) o.feed = Math.round(o.feed * 0.9); t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    const t0 = Date.now();
    await sleep(800);
    await waitFor(() => js("window.__marcam.planIdle() && document.body.dataset.state !== 'busy'"), "the replan", 240000);
    await js("window.__marcam.statsInBackground?.(); true");
    await sleep(300);
    await waitFor(() => js("!window.__marcam.simLock.busy && !window.__marcam.simRing.active"), "the background simulation", 240000);
    const busyFor = (Date.now() - t0) / 1000;
    await js("window.__probe.stop = true; window.__probe.obs?.disconnect(); true");
    const r = await js(`(() => { const P = window.__probe; const d = P.frames.slice(1).map((t, i) => t - P.frames[i]).sort((a, b) => a - b);
      const q = (p) => d[Math.min(d.length - 1, Math.floor(p * d.length))] || 0;
      return { frames: d.length, median: q(0.5), p95: q(0.95), worst: d[d.length - 1] || 0, long: P.long.length, longMax: Math.max(0, ...P.long), longs: P.long.slice().sort((a, b) => b - a).slice(0, 5).map(Math.round) }; })()`);
    console.log(`knob, replan + simulation in the background (${busyFor.toFixed(1)} s), orbiting and scrubbing: frames ${r.frames}  median ${r.median.toFixed(1)} ms  p95 ${r.p95.toFixed(1)}  worst ${r.worst.toFixed(0)}  long tasks ${r.long} (max ${r.longMax.toFixed(0)} ms; ${r.longs.join(", ")})`);
    if (GATE && (r.longMax > BUDGET.longTask || r.median > BUDGET.medianFrame)) {
      failed = true;
      console.log(`FAIL  background work: longest task ${r.longMax.toFixed(0)} ms (budget ${BUDGET.longTask}), median frame ${r.median.toFixed(1)} ms (budget ${BUDGET.medianFrame})`);
    }
  }
} catch (e) {
  failed = true;
  console.log(`FAIL  ${e.message}`);
} finally {
  await app.close?.();
}
process.exit(failed ? 1 : 0);
