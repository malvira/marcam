// A seeded, replayable monkey on the web app's viewer, tree and forms: random actions in a
// random rhythm (often without waiting for the last to settle) on the knob and the plate:
// the viewer's toggles (P S T R C O), play, pause, scrub, speed, to the end, fold the
// controls, the view cube and Home, orbiting and zooming, the tree's eyes, selecting
// operations, typing into an operation's form, edits that plan again, Generate.
//
// After every action and once things settle, invariants:
// - no page error, nothing left busy;
// - what is drawn is what the toggles say (part, stock, toolpaths, rapids, tool), one tool
//   glyph at most, never the playback and the view without it together;
// - the camera, the toggles, the selection and the playhead change only when the action
//   itself changes them (the playhead clamped when a plan made the job shorter);
// - the project and the open form's values change only when the action edits them (a
//   background result never writes into them);
// - every frame drawn between an action and its settling shows the part and the toolpaths
//   when they are on before and after it (no flicker).
// Long tasks over 50 ms are counted (failing with --strict).
//
//   node web/tests/viewmonkey.mjs                         (the CI seeds: 2 x 40 actions)
//   node web/tests/viewmonkey.mjs --seed 7 --actions 300   (one longer run; replay a seed)
//   node web/tests/viewmonkey.mjs --seeds 1,2,3 --strict
import { sleep, startApp, waitFor } from "./harness.mjs";

const arg = (name, dflt) => (process.argv.includes(name) ? process.argv[process.argv.indexOf(name) + 1] : dflt);
const SEEDS = arg("--seeds", null) ? arg("--seeds").split(",").map(Number) : arg("--seed", null) ? [Number(arg("--seed"))] : [1, 2];
const ACTIONS = Number(arg("--actions", 40));
const STRICT = process.argv.includes("--strict");
const VERBOSE = process.argv.includes("--verbose");

function rng(seed) {
  let s = BigInt(seed) & 0xffffffffffffffffn;
  const next = () => {
    s = (s + 0x9e3779b97f4a7c15n) & 0xffffffffffffffffn;
    let z = s;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & 0xffffffffffffffffn;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & 0xffffffffffffffffn;
    return Number((z ^ (z >> 31n)) >> 11n) / 2 ** 53;
  };
  const r = { f: next, int: (n) => Math.floor(next() * n), chance: (p) => next() < p };
  r.pick = (xs) => xs[r.int(xs.length)];
  return r;
}

// ---------------------------------------------------------------- in the page
// The audit: what each frame draws, against what the toggles say.
const INSTALL = `(() => {
  const M = window.__marcam, v = M.state.three;
  if (window.__monkey) return true;
  const shown = (o) => { for (let x = o; x; x = x.parent) { if (!x.visible) return false; if (x === v.scene) return true; } return false; };
  const drawables = (o) => { let n = 0; o?.traverse((x) => { if ((x.isMesh || x.isLine || x.isPoints) && x.geometry?.attributes?.position?.count && shown(x)) n++; }); return n; };
  const lineShown = (l) => !!l && shown(l) && l.geometry.drawRange.count > 0 && l.geometry.attributes.position.count > 0;
  function census() {
    const g = v.groups, m = M.machining.state;
    const mach = !!g.machining && shown(g.machining);
    const shared = mach && g.work ? g.work.children.filter((c) => c.userData.shared && drawables(c)).length : 0;
    // (the path lines in the scene: while a playback is made again its new lines are not in it
    // yet, the old ones are)
    const inScene = [];
    if (mach && g.work) g.work.traverse((o) => o.userData.path && inScene.push(o));
    const machPaths = { cut: inScene.some((o) => o.userData.path === 'cut' && lineShown(o)), rapid: inScene.some((o) => o.userData.path === 'rapid' && lineShown(o)) };
    const legacy = { part: drawables(g.part), stock: drawables(g.stock), sim: drawables(g.sim), path: lineShown(v.cutLines), rapid: lineShown(v.rapLines), tool: drawables(g.tool), draft: g.draft ? drawables(g.draft) : 0 };
    const machStock = mach && m.stockMesh && shown(m.stockMesh) ? 1 : 0;
    const simFinal = mach && g.simFinal ? drawables(g.simFinal) : 0;
    const toolModel = mach && g.toolModel && shown(g.toolModel) ? drawables(g.toolModel) : 0;
    const strays = v.scene.children.filter((c) => !Object.values(g).includes(c) && c.visible && !c.isLight && !c.isCamera && c.type !== 'AxesHelper' && drawables(c)).length;
    return {
      mach, legacy, shared, machPaths, machStock, simFinal, toolModel, strays,
      part: legacy.part > 0 || shared > 0,
      path: legacy.path || machPaths.cut,
      rapid: legacy.rapid || machPaths.rapid,
      stock: legacy.sim > 0 || machStock > 0 || simFinal > 0,
      tools: (legacy.tool > 0 ? 1 : 0) + (toolModel > 0 ? 1 : 0),
    };
  }
  /** The ways what is drawn disagrees with the toggles. */
  function audit() {
    const c = census(), view = M.machining.state.view, m = M.machining.state, out = [];
    const editing = !!(M.state.tree && (M.state.tree.picking || M.state.tree.draft || M.state.tree.tabMode));
    if (c.tools > 1) out.push('two tool glyphs drawn');
    if (!view.part && c.part) out.push('the part drawn with Part off');
    if (view.paths === 'off' && (c.path || c.legacy.draft)) out.push('toolpaths drawn with Toolpaths off');
    if (!view.rapids && c.rapid) out.push('rapids drawn with Rapids off');
    if (!view.stock && c.stock) out.push('the simulated stock drawn with Stock off');
    if (!view.tool && c.tools) out.push('a tool drawn with Tool off');
    if (c.mach && (c.legacy.part || c.legacy.sim || c.legacy.path || c.legacy.tool)) out.push('the playback and the view without it drawn together');
    if (c.legacy.draft && !editing) out.push('a draft preview left drawn with no draft');
    if (c.strays) out.push('objects drawn outside the view groups');
    if (c.part && c.stock) out.push('the part and the simulated stock drawn together (they z-fight)');
    // (Mar's scrub glitch) one 3D view; a vise's work never turned by A; its stock no taller
    // than its block
    if (document.querySelectorAll('#view3d canvas').length > 1) out.push('two 3D views drawn');
    // (while a new playback is built the old one stays on show, as it was: judged once swapped)
    if (c.mach && m.xyz && !m.swapping && Math.abs(v.groups.work?.rotation.x || 0) > 1e-9) out.push('the work turned (A) in a vise playback');
    const sb = m.session?.stock_box;
    if (c.mach && m.xyz && !m.swapping && sb && m.stockMesh && shown(m.stockMesh)) {
      const p = m.stockMesh.geometry.attributes.position.array;
      let top = -Infinity;
      for (let i = 2; i < p.length; i += 30) top = Math.max(top, p[i]);
      if (top > sb.max[2] + 0.05) out.push('the stock drawn taller than its block');
    }
    return out;
  }
  const frames = [];
  const r0 = v.renderer.render.bind(v.renderer);
  v.renderer.render = (s, cam) => {
    if (s === v.scene && window.__monkey.recording) {
      const c = census();
      const au = audit();
      const ms = M.machining.state;
      frames.push({ part: c.part, path: c.path, tools: c.tools, audit: au, census: au.length || !c.path ? JSON.stringify({ c, open: ms.open, swapping: !!ms.swapping, session: !!ms.session, editing: !!ms.editing, expanded: !!ms.expanded, pos: ms.pos, paths: ms.view.paths, range: ms.paths?.cut.map((l) => l.geometry.drawRange.count + '/' + l.visible).join(' ') }) : null });
    }
    return r0(s, cam);
  };
  const longs = [];
  try { new PerformanceObserver((l) => { for (const e of l.getEntries()) longs.push(Math.round(e.duration)); }).observe({ type: 'longtask', buffered: false }); } catch {}
  /** (Mar) the toolpaths drawn are the eyes that are on: the playback's operations against the
   *  planned ones of the setup shown, less those hidden. Null: nothing to judge now. */
  function eyes() {
    const m = M.machining.state, j = M.state.job, sid = M.state.tree?.setup;
    if (!m.open || m.swapping || M.state.stale || !M.state.result || j?.schema_version !== '0.2') return null;
    // (open with no playback, all settled: one stopped and never made again)
    if (!m.session) return { want: 'a playback', drawn: 'none (lost)', lines: 0, segments: 0, paths: m.view.paths };
    const s = j.setups?.find((x) => x.id === sid);
    if (!s) return null;
    const tools = new Set((j.tools || []).map((t) => t.number));
    const planned = s.outputs.flatMap((o) => o.operations).map((id) => s.operations.find((o) => o.id === id)).filter((o) => o && o.enabled !== false && !o.error && tools.has(o.tool)).map((o) => o.id);
    const want = planned.filter((id) => !M.state.hidden.has(sid + '/' + id));
    const drawn = [...new Set(m.session.ops.map((o) => o.project_id))];
    const cut = [];
    v.groups.work?.traverse((o) => o.userData.path === 'cut' && o.visible && cut.push(o.geometry.drawRange.count));
    return { want: want.join(','), drawn: drawn.join(','), lines: cut.reduce((a, b) => a + b, 0), segments: m.session.segments, paths: m.view.paths };
  }
  window.__monkey = { census, audit, eyes, frames, longs, recording: false };
  return true;
})()`;

/** Everything an action may or may not change, as one snapshot. */
const SNAP = `(() => {
  const M = window.__marcam, v = M.state.three, m = M.machining.state;
  const f = document.getElementById('opForm');
  return {
    cam: v.camera.matrixWorld.elements.map((x) => (Math.abs(x) < 5e-4 ? 0 : x).toFixed(3)).join(','),
    target: v.controls.target.toArray().map((x) => (Math.abs(x) < 5e-4 ? 0 : x).toFixed(3)).join(','),
    view: JSON.stringify(m.view),
    selected: M.state.tree?.selected ?? null,
    pos: m.busy ? m.target : m.pos, segments: m.session?.segments ?? null, playing: !!m.playing, expanded: !!m.expanded, open: !!m.open,
    project: document.getElementById('job').value,
    formOp: f ? (M.state.tree?.selected ?? null) : null,
    form: f ? [...f.querySelectorAll('input, select, textarea')].map((e) => e.name + '=' + (e.type === 'checkbox' ? e.checked : e.value)).join('|') : null,
    focus: document.activeElement ? (document.activeElement.name || document.activeElement.id || document.activeElement.tagName) : null,
    hidden: [...(M.state.hidden || [])].join(','),
    // every setting on show outside the tree (the simulation's resolution, the machine, ...)
    controls: [...document.querySelectorAll('input, select, textarea')].filter((e) => e.offsetParent && e.id !== 'job' && !e.closest('#tree') && !e.closest('#simControls') && e.type !== 'file' && e.type !== 'button').map((e) => (e.id || e.name) + '=' + (e.type === 'checkbox' ? e.checked : e.value)).join('|'),
  };
})()`;

const BUSY = "!(window.__marcam.planIdle() && document.body.dataset.state !== 'busy' && !window.__marcam.machining.state.autoOpening && !window.__marcam.machining.state.busy && !window.__marcam.machining.state.swapping && !window.__marcam.simLock.busy && !window.__marcam.viewCube?.busy?.() && !window.__marcam.inFlight().some((c) => c.worker === 'main') && !window.__marcam.resimPending?.())";

// ---------------------------------------------------------------- the actions
// Each: what it may change (cam, view, selected, pos, project, form, hidden), and how.
function actions(r, app) {
  const { js, S } = app;
  const key = (k) => js(`document.body.dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', bubbles: true })); true`);
  // a real click (the mouse: focus moves, a field typed into commits on its way out)
  const click = async (expr) => {
    const at = await js(`(() => { const e = ${expr}; if (!e || !e.offsetParent || e.disabled) return null; e.scrollIntoView({ block: 'nearest' }); const b = e.getBoundingClientRect(); const x = b.left + b.width / 2, y = b.top + b.height / 2; const hit = document.elementFromPoint(x, y); return hit && (hit === e || e.contains(hit)) ? [x, y] : 'covered by ' + (hit ? hit.tagName + '#' + hit.id + '.' + hit.className : 'nothing'); })()`);
    if (!at) return false;
    if (typeof at === "string") throw new Error(at);
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x: at[0], y: at[1], button: "left", clickCount: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: at[0], y: at[1], button: "left", clickCount: 1 });
    return true;
  };
  const center = async () => js("(() => { const b = document.querySelector('#view3d canvas').getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2]; })()");
  return [
    { name: "toggle", w: 6, may: ["view"], run: async () => { const k = r.pick(["p", "s", "t", "r", "c", "o"]); await key(k); return k; } },
    { name: "play", w: 3, may: ["pos", "playing", "expanded"], run: async () => { const b = await js("!!document.getElementById('vtPlay') && !document.getElementById('vtPlay').disabled && !document.getElementById('vtPlay').classList.contains('hidden')"); await click(b ? "document.getElementById('vtPlay')" : "document.getElementById('simPlay')"); return b ? "vtPlay" : "simPlay"; } },
    { name: "scrub", w: 3, may: ["pos"], run: async () => { const f = r.f(); await js(`(() => { const s = document.getElementById('simScrub'); if (s.offsetParent === null) return false; s.value = String(Math.round(+s.max * ${f})); s.dispatchEvent(new Event('input')); return true; })()`); return f.toFixed(2); } },
    { name: "scrubfast", w: 2, may: ["pos"], run: async () => {
      // (Mar) a fast drag back and forth on the playback slider: every frame one place's state
      const xs = Array.from({ length: 12 }, () => r.f());
      await js(`(async () => { const s = document.getElementById('simScrub'); if (s.offsetParent === null) return false; for (const f of ${JSON.stringify(xs)}) { s.value = String(Math.round(+s.max * f)); s.dispatchEvent(new Event('input')); await new Promise((r) => requestAnimationFrame(() => r())); } const want = +s.value, m = window.__marcam.machining.state; for (let k = 0; k < 400 && !(m.pos === Math.min(want, m.session?.segments ?? want) && !m.busy && !m.swapping); k++) await new Promise((r) => setTimeout(r, 50)); return true; })()`);
      return xs.length;
    } },
    { name: "speed", w: 1, may: [], run: async () => { const f = r.f(); await js(`(() => { const s = document.getElementById('simSpeed'); s.value = String(+s.min + (+s.max - +s.min) * ${f}); s.dispatchEvent(new Event('input')); return true; })()`); return f.toFixed(2); } },
    { name: "end", w: 1, may: ["pos"], run: async () => { await click("document.getElementById('simEnd')"); } },
    { name: "fold", w: 1, may: ["expanded", "playing"], run: async () => { await click("document.getElementById('simClose')"); } },
    { name: "cube", w: 2, may: ["cam"], run: async () => { const d = r.pick([[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [1, -1, 1], [-1, -1, 0.5]]); await js(`window.__marcam.viewCube.goTo(${JSON.stringify(d)}); true`); return d.join(","); } },
    { name: "home", w: 1, may: ["cam"], run: async () => { await click("document.getElementById('viewHome')"); } },
    { name: "orbit", w: 2, may: ["cam"], run: async () => {
      const [x, y] = await center();
      const dx = (r.f() - 0.5) * 300, dy = (r.f() - 0.5) * 200;
      await S("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
      for (let i = 1; i <= 4; i++) await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: x + (dx * i) / 4, y: y + (dy * i) / 4, button: "left", buttons: 1 });
      await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: x + dx, y: y + dy, button: "left", clickCount: 1 });
      return `${dx.toFixed(0)},${dy.toFixed(0)}`;
    } },
    { name: "zoom", w: 1, may: ["cam"], run: async () => { const [x, y] = await center(); const d = (r.f() - 0.5) * 600; await S("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: 0, deltaY: d }); return d.toFixed(0); } },
    { name: "eye", w: 2, may: ["hidden", "pos"], run: async () => { const k = r.f(); const id = await js(`(() => { const e = [...document.querySelectorAll('#tree .tree-op [data-act="eye"]')]; return e.length ? e[Math.floor(${k} * e.length)].closest('.tree-op').dataset.op : null; })()`); if (id) await click(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="eye"]')`); return id; } },
    { name: "select", w: 3, may: ["selected", "form"], run: async () => { const k = r.f(); const id = await js(`(() => { const e = [...document.querySelectorAll('#tree .tree-op [data-act="select"]')]; return e.length ? e[Math.floor(${k} * e.length)].closest('.tree-op').dataset.op : null; })()`); if (id) await click(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="select"]')`); return id; } },
    { name: "type", w: 3, may: ["project", "form", "pos"], run: async () => {
      // into a number field of the open form: select it, type a value, Tab away (or not)
      const field = await js(`(() => { const f = document.getElementById('opForm'); if (!f) return null; const xs = [...f.querySelectorAll('input[type=number], input:not([type])')].filter((e) => e.offsetParent && /feed|step|depth|stepover|allowance/.test(e.name)); if (!xs.length) return null; const e = xs[Math.floor(${r.f()} * xs.length)]; e.focus(); e.select(); return e.name; })()`);
      if (!field) return null;
      const val = await js(`(() => { const e = document.activeElement; const v = parseFloat(e.value) || 1; return String(Math.round(v * ${0.8 + 0.4 * r.f()} * 1000) / 1000); })()`);
      await S("Input.insertText", { text: val });
      if (r.chance(0.6)) await S("Input.dispatchKeyEvent", { type: "keyDown", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 }), await S("Input.dispatchKeyEvent", { type: "keyUp", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
      return `${field}=${val}`;
    } },
    { name: "edit", w: 2, may: ["project", "form", "pos"], run: async () => { const d = 1 + r.int(9); await js(`(() => { const t = document.getElementById('job'); const j = JSON.parse(t.value); const ops = (j.setups ? j.setups.flatMap((s) => s.operations) : j.programs.flatMap((p) => p.operations)); const o = ops[${r.int(100)} % ops.length]; for (const k of ['feed', 'rough_feed', 'finish_feed']) if (o[k]) o[k] += ${d}; t.value = JSON.stringify(j, null, 2); t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`); return d; } },
    { name: "add", w: 1, may: ["selected", "form"], run: async () => {
      // a new operation of a random type: its form, filled by the engine (often while typing)
      const k = r.f();
      const kind = await js(`(() => { const s = document.getElementById('treeAddType'); if (!s) return null; const o = [...s.options]; s.value = o[Math.floor(${k} * o.length)].value; s.dispatchEvent(new Event('change', { bubbles: true })); return s.value; })()`);
      if (kind) await click("document.getElementById('treeAdd')");
      return kind;
    } },
    { name: "apply", w: 1, may: ["project", "form", "selected", "pos", "hidden"], run: async () => (await click("document.querySelector('#opForm button[type=submit]')")) || null },
    { name: "cancel", w: 1, may: ["form", "selected"], run: async () => (await click("document.getElementById('cancelDraft')")) || null },
    { name: "resolution", w: 1, may: ["controls"], run: async () => {
      // another simulation resolution: the stock simulated again at it (Mar), nothing planned
      const want = r.pick(["coarse", "normal", "fine"]);
      await js(`(() => { const s = document.getElementById('simRes'); s.value = '${want}'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
      return want;
    } },
    { name: "setup", w: 1, may: ["selected", "form"], run: async () => (await click("document.querySelector('#tree .tree-setup.active [data-act=setup]')")) || null },
    { name: "library", w: 1, may: ["selected", "form"], run: async () => (await click("document.querySelector('#tree [data-act=library]')")) || null },
    { name: "tool", w: 1, may: ["selected", "form"], run: async () => (await click("document.querySelector('#tree .tree-tool [data-act=tool]')")) || null },
    { name: "delsetup", w: 1, may: ["project", "selected", "form", "cam", "pos", "hidden"], run: async () => (await click("document.querySelector('#tree .tree-setup.active [data-act=delsetup]')")) || null },
    { name: "foldtree", w: 1, may: [], run: async () => (await click("document.querySelector('#tree .tree-setup.active [data-act=fold]')")) || null },
    { name: "carry", w: 1, may: ["project", "selected", "form", "pos", "hidden", "cam"], run: async () => {
      // (Mar) a setup taking what the last one left: a vise or the rotary, maybe stood on end
      if (!(await click("document.getElementById('treeAddSetup')"))) return null;
      const modal = "document.getElementById('setupDialog').open && !document.getElementById('setupDialog').classList.contains('embedded')";
      for (let i = 0; i < 50 && !(await js(modal)); i++) await sleep(100);
      if (!(await js(modal))) return "(no dialog)";
      const kind = r.pick(["vise", "rotary"]), rot = r.pick([0, 0, 90]);
      await js(`(() => { const p = document.querySelector('input[name="sdStock"][value="previous"]'); if (p.closest('label').classList.contains('hidden')) return false; p.checked = true; p.dispatchEvent(new Event('change', { bubbles: true }));
        const k = document.querySelector('.sd-kind[data-kind="${kind}"] input'); if (!k.disabled) document.querySelector('.sd-kind[data-kind="${kind}"]').click();
        const y = document.getElementById('sdRotY'); y.value = '${rot}'; y.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
      await js("document.getElementById('sdOk').click(); true");
      for (let i = 0; i < 100 && (await js(modal)); i++) await sleep(100);
      if (await js(modal)) await js("document.getElementById('sdCancel').click(); true");
      return `${kind} ${rot}`;
    } },
    { name: "duplicate", w: 1, may: ["project", "selected", "form", "pos"], run: async () => { const k = r.f(); const id = await js(`(() => { const e = [...document.querySelectorAll('#tree .tree-op [data-act="dup"]')]; return e.length ? e[Math.floor(${k} * e.length)].closest('.tree-op').dataset.op : null; })()`); if (id) await click(`document.querySelector('#tree .tree-op[data-op="${id}"] [data-act="dup"]')`); return id; } },
    { name: "stocknode", w: 1, may: ["selected", "form"], run: async () => { const k = r.pick(["stock", "part"]); return (await click(`document.querySelector('#tree [data-act=${k}]')`)) ? k : null; } },
    { name: "handle", w: 2, may: ["project", "form", "pos", "cam"], run: async () => {
      // (Mar) a drag on a handle (the Stock or Part selected): an arrow or a ring
      const mode = r.pick(["translate", "rotate"]), axis = r.pick(["X", "Y", "Z"]);
      const p = await js(`JSON.stringify(window.__marcam.handles?.screenOf('${mode}', '${axis}') ?? null)`).then(JSON.parse);
      if (!p) return null;
      const dx = (r.f() - 0.5) * 300, dy = (r.f() - 0.5) * 300;
      await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: p[0], y: p[1] });
      await S("Input.dispatchMouseEvent", { type: "mousePressed", x: p[0], y: p[1], button: "left", clickCount: 1 });
      for (let i = 1; i <= 6; i++) await S("Input.dispatchMouseEvent", { type: "mouseMoved", x: p[0] + (dx * i) / 6, y: p[1] + (dy * i) / 6, button: "left", buttons: 1 });
      await S("Input.dispatchMouseEvent", { type: "mouseReleased", x: p[0] + dx, y: p[1] + dy, button: "left", clickCount: 1 });
      return `${mode} ${axis} ${dx.toFixed(0)},${dy.toFixed(0)}`;
    } },
    { name: "othersetup", w: 1, may: ["selected", "form", "pos", "hidden", "cam"], run: async () => { const k = r.f(); const id = await js(`(() => { const e = [...document.querySelectorAll('#tree .tree-setup:not(.active) [data-act=setup]')]; return e.length ? e[Math.floor(${k} * e.length)].closest('.tree-setup').dataset.setup : null; })()`); if (id) await click(`document.querySelector('#tree .tree-setup[data-setup="${id}"] [data-act=setup]')`); return id; } },
    { name: "generate", w: 1, may: ["pos"], run: async () => { await click("document.getElementById('generate')"); } },
  ];
}

// ---------------------------------------------------------------- the run
const app = await startApp();
const { js } = app;
let failures = 0;
let lateNote = "";
const found = [];
function fail(seed, i, what) {
  if (lateNote) what += ` (still landing: ${lateNote})`;
  failures++;
  found.push(`seed ${seed} #${i}: ${what}`);
  console.log(`FAIL  seed ${seed} #${i}: ${what}`);
}
try {
  for (const seed of SEEDS) {
    const r = rng(seed);
    const example = r.pick(["knob", "plate"]);
    await js(`document.getElementById('example').value = '${example}'; document.getElementById('loadExample').click(); true`);
    await sleep(500);
    await waitFor(async () => !(await js(BUSY)) && (await js("!!window.__marcam.state.result && !!window.__marcam.state.three?.renderer && window.__marcam.machining.state.open")), `${example} planned and shown`, 180000).catch(async (e) => {
      throw new Error(`${e.message}: ${await js("JSON.stringify({ result: !!window.__marcam.state.result, open: window.__marcam.machining.state.open, state: document.body.dataset.state, status: document.getElementById('status').textContent, error: document.getElementById('error').textContent, inFlight: window.__marcam.inFlight(), stale: window.__marcam.state.stale, planIdle: window.__marcam.planIdle() })")} ${app.pageErrors().join(" | ").slice(0, 900)}`);
    });
    await js(INSTALL);
    const acts = actions(r, app);
    const total = acts.reduce((n, a) => n + a.w, 0);
    const pickAct = () => { let k = r.f() * total; for (const a of acts) { k -= a.w; if (k < 0) return a; } return acts[0]; };
    let errs0 = app.pageErrors().length;
    let camQuietAt = 0;
    let lastCam = "none";
    const carry = new Set();
    console.log(`seed ${seed}: ${example}, ${ACTIONS} actions`);
    for (let i = 0; i < ACTIONS; i++) {
      const a = pickAct();
      // (a camera move still running from an action before: this one's camera not judged)
      const camSteady = Date.now() > camQuietAt;
      const before = await js(SNAP);
      await js("window.__monkey.frames.length = 0; window.__monkey.recording = true; true");
      const t0 = Date.now();
      let what;
      try {
        what = await a.run();
      } catch (e) {
        what = `(threw: ${e.message.slice(0, 80)})`;
      }
      if (VERBOSE) console.log(`  #${i} ${a.name} ${what ?? ""}`);
      // (a setup switched to, added or gone: the view frames its stock once it is drawn, later)
      if (a.may.includes("cam")) { camQuietAt = Date.now() + (["othersetup", "carry", "delsetup"].includes(a.name) ? 5000 : 900); lastCam = `#${i} ${a.name}`; }
      // at once (mid-update: often the next action comes before this one settles)
      await sleep(80);
      const now = await js("window.__monkey.audit()");
      if (now.length) {
        const c = await js("JSON.stringify({ c: window.__monkey.census(), open: window.__marcam.machining.state.open, swapping: !!window.__marcam.machining.state.swapping, session: !!window.__marcam.machining.state.session, tree: { draft: !!window.__marcam.state.tree?.draft, picking: !!window.__marcam.state.tree?.picking, tab: !!window.__marcam.state.tree?.tabMode } })");
        for (const x of now) fail(seed, i, `${a.name}: ${x} (at once) ${c}`);
      }
      // (not waited for: what it may still change is the next one's to change too)
      if (r.chance(0.35)) {
        for (const k of a.may) carry.add(k);
        carry.add(`#${i} ${a.name}`);
        continue;
      }
      // settled
      try {
        await waitFor(async () => !(await js(BUSY)), "settling", 120000);
      } catch (e) {
        fail(seed, i, `${a.name}: never settled: ${await js("JSON.stringify({ state: document.body.dataset.state, status: document.getElementById('status').textContent, inFlight: window.__marcam.inFlight(), m: { busy: window.__marcam.machining.state.busy, swapping: window.__marcam.machining.state.swapping, autoOpening: window.__marcam.machining.state.autoOpening }, lock: window.__marcam.simLock.busy })")}`);
        break;
      }
      await sleep(Math.max(0, camQuietAt - Date.now()));
      await sleep(200);
      await js("window.__monkey.recording = false; true");
      const after = await js(SNAP);
      for (const x of await js("window.__monkey.audit()")) fail(seed, i, `${a.name}: ${x} (settled)`);
      // (Mar) after any eye toggles: the toolpaths drawn are exactly those whose eyes are on
      const ey = await js("window.__monkey.eyes()");
      if (ey && (ey.want !== ey.drawn || (ey.want && ey.paths === 'all' && !ey.lines))) fail(seed, i, `${a.name}: the toolpaths drawn (${ey.drawn || "none"}, ${ey.lines} line points) are not the eyes on (${ey.want || "none"}) ${JSON.stringify(ey)}`);
      // what the action did not touch stays as it was
      const may = new Set([...a.may, ...carry]);
      const late = [...carry].filter((k) => k.startsWith("#")).join(", ");
      lateNote = late;
      carry.clear();
      const playing = before.playing || after.playing;
      if (!may.has("cam") && camSteady && Date.now() > camQuietAt && (before.cam !== after.cam || before.target !== after.target)) {
        const d = Math.max(...before.cam.split(",").map((x, k) => Math.abs(+x - +after.cam.split(",")[k])));
        fail(seed, i, `${a.name}: the camera moved (by up to ${d.toFixed(3)}; target ${before.target} -> ${after.target}; last camera action ${lastCam})`);
      }
      if (!may.has("view") && before.view !== after.view) fail(seed, i, `${a.name}: the toggles changed ${before.view} -> ${after.view}`);
      if (!may.has("selected") && before.selected !== after.selected) fail(seed, i, `${a.name}: the selection changed ${before.selected} -> ${after.selected}`);
      if (!may.has("pos") && !playing && after.open && before.open && after.pos !== Math.min(before.pos, after.segments ?? before.pos)) fail(seed, i, `${a.name}: the playhead moved ${before.pos} -> ${after.pos} (of ${after.segments})`);
      if (!may.has("project") && before.project !== after.project) fail(seed, i, `${a.name}: the project changed`);
      // (the panels that show the project follow it: changed with it, by an edit)
      if (!may.has("project") && !may.has("controls") && before.controls !== after.controls) fail(seed, i, `${a.name}: a setting changed by itself: ${diff(before.controls, after.controls)}`);
      if (!may.has("form") && before.formOp && before.formOp === after.formOp && before.form !== after.form) fail(seed, i, `${a.name}: the form's values changed: ${diff(before.form, after.form)}`);
      if (a.name === "type" && what && before.formOp && after.formOp === before.formOp && !after.form?.includes(what.replace(/^(\w+)=/, "$1="))) {
        // (the value typed stays in its field, a plan landing or not)
        const [k, v] = what.split("=");
        if (!after.form?.split("|").some((x) => x.startsWith(`${k}=`) && Math.abs(parseFloat(x.slice(k.length + 1)) - parseFloat(v)) < 1e-9)) fail(seed, i, `type: the value typed (${what}) is not in its field: ${after.form?.split("|").find((x) => x.startsWith(`${k}=`))}`);
      }
      // a resolution chosen: the simulated stock (when there is one, or it is shown) is at it
      if (a.name === "resolution" && what) {
        const sim = await js("(() => { const M = window.__marcam; return { has: !!M.state.sim, shown: !!M.machining.state.view.stock, key: M.state.simKey || null, stale: !!M.state.stale, result: !!M.state.result?.programs?.length }; })()");
        if (sim.result && !sim.stale && (sim.has || sim.shown) && !(sim.key && JSON.parse(sim.key)[0].res === what)) fail(seed, i, `resolution ${what}: the stock was not simulated again at it (${sim.key})`);
      }
      // no flicker: the part and the toolpaths in every frame when on before and after
      const frames = await js("window.__monkey.frames.map((f) => [f.part ? 1 : 0, f.path ? 1 : 0, f.tools, f.audit.join(';')])");
      if (frames.length > 2) {
        const [p0, t0f] = frames[0], [p1, t1f] = frames[frames.length - 1];
        const lostPart = p0 && p1 && frames.some((f) => !f[0]);
        const lostPath = t0f && t1f && before.view === after.view && frames.some((f) => !f[1]) && !playing && !may.has("pos");
        if (lostPart && !may.has("view")) fail(seed, i, `${a.name}: the part vanished in ${frames.filter((f) => !f[0]).length} of ${frames.length} frames`);
        if (lostPath) fail(seed, i, `${a.name}: the toolpaths vanished in ${frames.filter((f) => !f[1]).length} of ${frames.length} frames ${await js("window.__monkey.frames.find((f) => !f.path)?.census")}`);
        const bad = frames.filter((f) => f[3]).map((f) => f[3]);
        if (bad.length) fail(seed, i, `${a.name}: ${bad.length} of ${frames.length} frames drawn against the toggles (${[...new Set(bad)].join(" | ")}) ${await js("window.__monkey.frames.find((f) => f.census)?.census")}`);
      }
      const errs = app.pageErrors();
      if (errs.length > errs0) {
        fail(seed, i, `${a.name}: page error ${errs.slice(errs0).join(" | ").slice(0, 300)}`);
        errs0 = errs.length;
      }
      if (VERBOSE) console.log(`    ok in ${Date.now() - t0} ms`);
    }
    const longs = await js("window.__monkey.longs.slice()");
    const over = longs.filter((d) => d > 50);
    console.log(`seed ${seed}: long tasks over 50 ms: ${over.length}${over.length ? ` (max ${Math.max(...over)} ms)` : ""}`);
    if (STRICT && over.length) fail(seed, "-", `${over.length} long tasks over 50 ms (max ${Math.max(...over)} ms)`);
    await js("window.__monkey.longs.length = 0; true");
  }
} finally {
  await app.close();
}

function diff(a, b) {
  const x = (a || "").split("|"), y = (b || "").split("|");
  return x.map((v, i) => (v !== y[i] ? `${v} -> ${y[i]}` : null)).filter(Boolean).slice(0, 4).join(", ");
}

const uniq = [...new Set(found.map((f) => f.replace(/^seed \d+ #[\d-]+: /, "").replace(/\d+ of \d+/g, "n of m").replace(/-?\d+(\.\d+)?/g, "#")))];
console.log(`\n${failures ? `${failures} failure(s), ${uniq.length} kind(s):\n  ${uniq.join("\n  ")}` : "monkey: all invariants held"}`);
process.exit(failures ? 1 : 0);
