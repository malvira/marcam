// Solid machining simulation: the stock as a closed solid that the tool cuts as the job plays.
// The engine (WebAssembly, in the worker) keeps the stock heightmap and the cut-by-operation
// tags; this module plays it back, updates the GPU buffers in place for the rows that changed,
// and colours each cut surface by the operation that cut it. Rotary setups play on a bar
// (rows = X cells, A turns the work); 3-axis (vise) setups on a block (rows = Y cells, the
// tool moves in XYZ).

import { blockSolid, heightmapSolid, stockObject, STOCK_LEFT, WALL_BAND } from "./meshes.js";
import { GpuStock } from "./gpu/gpusim.js";
import { colorNum, colorRgb } from "./colors.js";

const PALETTE = ["#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#a855f7", "#14b8a6", "#e11d48", "#84cc16"];
// stock not cut yet: the stock's tint (distinct from cuts and from the part)
// (View > Colours: the uncut stock's colour)
const uncut = () => colorRgb("uncut") || STOCK_LEFT;

// (the worker decodes the engine's base64 into bytes; text still decodes here)
const b64ToBytes = (s) => (typeof s === "string" ? Uint8Array.from(atob(s), (c) => c.charCodeAt(0)) : s);
const f32 = (s) => {
  if (s instanceof Float32Array) return s;
  const b = b64ToBytes(s);
  return new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);
};
const u16 = (s) => {
  if (s instanceof Uint16Array) return s;
  const b = b64ToBytes(s);
  return new Uint16Array(b.buffer, b.byteOffset, b.byteLength / 2);
};
const hexToRgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);

// The 3D display shows the engine grid every k cells (each display cell the deepest cut of
// its block) when the grid is large: at most DISPLAY_MAX cells at rest, PLAY_MAX while playing
// (it refines again on pause).
const DISPLAY_MAX = 1_000_000;
const PLAY_MAX = 250_000;

export function setupMachining({ engine, getThree, $, state, readJob, setStatus, showError, libraries, simOpts, confirmSimRes, simGpu, showBackend, simLock, machineArg, simMeshOf }) {
  const m = {
    open: false,
    session: null, // sim_open result (+ decoded arrays)
    r: null, // Float32Array radii, or block tops for 3-axis (mirror of the engine stock)
    xyz: false,
    op: null, // Uint16Array cut-by tags
    pos: 0,
    playing: false,
    busy: false,
    target: 0,
    last: 0,
    colors: {}, // tag -> [r, g, b]
    hiddenTags: new Set(), // operations hidden in the tree (view only): cuts in grey, no tool
  };
  state.machining = m;
  // bumped by reset(): an open() still waiting on the engine then drops its result
  let epoch = 0;

  /** Dispose an object tree's geometries and materials. */
  const disposeTree = (o) =>
    o.traverse((c) => {
      // (the part's copies share the main view's geometry)
      if (c.userData.shared) return;
      c.geometry?.dispose();
      for (const mat of [].concat(c.material || [])) mat.dispose();
    });

  // ---- the playback's controls: speed in machine time (×1 to ×1000, a log slider), the
  // stock toggle; both remembered
  const remember = (k, v) => {
    try {
      localStorage.setItem(k, String(v));
    } catch {}
  };
  const recall = (k) => {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  };
  const speedFactor = () => Math.pow(10, Math.max(0, Math.min(3, +$("simSpeed").value || 0)));
  function showSpeed() {
    const f = speedFactor();
    $("simSpeedLabel").textContent = `×${f < 10 ? f.toFixed(1).replace(/\.0$/, "") : Math.round(f)}`;
  }
  {
    const sp = recall("marcam.playSpeed");
    if (sp !== null && Number.isFinite(+sp)) $("simSpeed").value = sp;
    showSpeed();
  }

  // ---- what the viewer shows (Mar: "way too busy"): a row of toggles in the 3D view's
  // corner, each with a key, remembered. View only: nothing is planned or simulated again
  // for them, except the stock, which shows the background simulation's result (and runs
  // it if there is none yet).
  const VIEW_KEY = "marcam.view";
  const VIEW_DEFAULT = { part: true, stock: false, paths: "all", rapids: false, tool: true, only: false, chuck: false, tailstock: false, outline: true };
  /** The stock shown: its toggle; (Mar's carry-through) or, in a setup with no model (a cut
   *  piece carried in), the part's toggle: the piece is the part there. */
  const stockOn = () => {
    if (view.stock) return true;
    if (!view.part) return false;
    const t = state.tree;
    const s = t?.doc?.setups?.find((x) => x.id === t.setup);
    return !!s && !s.part?.length;
  };
  const view = { ...VIEW_DEFAULT };
  try {
    const saved = JSON.parse(recall(VIEW_KEY) || "null");
    if (saved && typeof saved === "object") for (const k of Object.keys(VIEW_DEFAULT)) if (k in saved && typeof saved[k] === typeof VIEW_DEFAULT[k]) view[k] = saved[k];
  } catch {}
  if (!["all", "playhead", "off"].includes(view.paths)) view.paths = "all";
  // (Only hides every other operation's path: a view of the moment, never brought back from
  // an earlier visit)
  view.only = false;
  m.view = view;
  // (the older switches stay as the state the rest of the app reads)
  const syncInputs = () => {
    $("showPart").checked = view.part;
    $("simStock").checked = view.stock;
    $("showRapids").checked = view.rapids;
    $("showStock").checked = view.outline;
  };
  syncInputs();
  const TOGGLES = [
    { k: "part", key: "p", title: "Part", svg: '<path d="M4 7l6-3 6 3v7l-6 3-6-3z M4 7l6 3 6-3 M10 10v7"/>' },
    { k: "stock", key: "s", title: "Simulated stock (the background simulation's result; while playing, simulated along)", svg: '<path d="M3 8h14v8H3z M3 8l2-3h10l2 3 M7 8v3h6V8"/>' },
    // (Mar) the stock the setup starts with (see-through, its edges; a carried piece solid): on or off
    { k: "outline", key: "b", title: "The stock as the setup starts with it (its outline)", svg: '<path d="M3 6h14v9H3z" stroke-dasharray="2 1.5"/>' },
    { k: "paths", key: "t", title: "Toolpaths: all, up to the playhead, or off", svg: '<path d="M3 15l3-9 3 9 3-9 3 9 2-6"/>' },
    { k: "rapids", key: "r", title: "Rapid moves", svg: '<path d="M3 10h9 M3 10h1 M6 10h2 M10 10h2 M12 6l5 4-5 4" stroke-dasharray="2 2"/>' },
    { k: "tool", key: "c", title: "The tool (during playback)", svg: '<path d="M8 2h4v8H8z M9 10h2v6l-1 2-1-2z"/>' },
    { k: "only", key: "o", title: "Only the selected operation's toolpath (others dimmed when off)", svg: '<circle cx="10" cy="10" r="6"/><circle cx="10" cy="10" r="2"/>' },
    // (Mar) the 4th axis's holding: off by default, shown on a 4th-axis setup only
    { k: "chuck", key: "k", title: "The chuck (4th axis)", rotary: true, svg: '<path d="M3 4h6v12H3z M9 6h4v2H9z M9 12h4v2H9z M13 9h4v2h-4z"/>' },
    { k: "tailstock", key: "l", title: "The tailstock (4th axis, when the setup has one)", rotary: true, tail: true, svg: '<path d="M17 6v8h-6l-4-4 4-4z M3 10h4"/>' },
  ];
  (window.__marcam ||= {}).renderViewToggles = () => renderToggles();
  function renderToggles() {
    let bar = $("viewToggles");
    const host = $("view3d");
    if (!host) return;
    if (!bar) {
      bar = document.createElement("div");
      bar.id = "viewToggles";
      bar.className = "view-toggles";
      bar.setAttribute("role", "toolbar");
      bar.setAttribute("aria-label", "What the 3D view shows");
      bar.innerHTML = TOGGLES.map((t) => `<button type="button" data-v="${t.k}" title="${t.title} (${t.key.toUpperCase()})" aria-label="${t.title}"><svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true">${t.svg}</svg><span class="vt-sub"></span></button>`).join("") +
        '<button type="button" id="vtPlay" title="Play the toolpaths (Space)" aria-label="Play"><svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true"><path d="M6 4l10 6-10 6z" class="fill"/></svg></button>';
      host.appendChild(bar);
      bar.addEventListener("click", (ev) => {
        const b = ev.target.closest("button");
        if (!b) return;
        if (b.id === "vtPlay") return playButton();
        toggle(b.dataset.v);
      });
    }
    // (the chuck and tailstock toggles: on a 4th-axis setup; the tailstock's when it has one)
    const t0 = state.tree;
    const s0 = t0?.doc?.setups?.find((x) => x.id === t0.setup);
    const rotarySetup = !!s0 && !(s0.kind === "vise" || s0.holding?.kind === "vise" || s0.stock?.shape === "box");
    for (const b of bar.querySelectorAll("button[data-v]")) {
      const tg = TOGGLES.find((x) => x.k === b.dataset.v);
      b.hidden = !!tg?.rotary && (!rotarySetup || (tg.tail && !s0?.holding?.tailstock));
    }
    for (const b of bar.querySelectorAll("button[data-v]")) {
      const k = b.dataset.v;
      const on = k === "paths" ? view.paths !== "off" : !!view[k];
      b.setAttribute("aria-pressed", String(on));
      b.dataset.state = k === "paths" ? view.paths : on ? "on" : "off";
      b.querySelector(".vt-sub").textContent = k === "paths" ? (view.paths === "playhead" ? "▸" : "") : "";
    }
    const play = $("vtPlay");
    play.disabled = !m.session;
    // (Mar: "the play stop and rewind controls are gone") hidden only while the playback's own
    // controls are on the screen: never both gone
    play.classList.toggle("hidden", !!m.expanded && !$("simControls").classList.contains("hidden"));
  }
  /** One toggle (paths cycles all, up to the playhead, off). */
  function toggle(k) {
    if (!(k in view)) return;
    if (k === "paths") view.paths = { all: "playhead", playhead: "off", off: "all" }[view.paths];
    else view[k] = !view[k];
    remember(VIEW_KEY, JSON.stringify(view));
    syncInputs();
    renderToggles();
    if (k === "stock") {
      applyLayers();
      window.__marcam.legacyLayers?.();
      return stockToggled();
    }
    applyLayers();
    showRange();
    m.selKey = null;
    syncFrame();
    window.__marcam.legacyLayers?.();
  }
  /** The stock shown or not: while playing, the playback again at the same place (the stock
   *  simulated along); otherwise the background simulation's result (run if missing). */
  async function stockToggled() {
    if (m.open && m.expanded) {
      const playing = m.playing;
      m.playing = false;
      await open();
      if (playing && m.session) $("simPlay").click();
      return;
    }
    if (stockOn() && !state.sim) window.__marcam.statsInBackground?.();
    m.finalKey = null;
    syncFrame();
  }
  /** The small play button: the playback's controls open (from the start if at the end) and
   *  it plays; again: pause. */
  async function playButton() {
    if (!m.session) return;
    if (!m.expanded || $("simControls").classList.contains("hidden")) {
      // (its opening dropped by a newer plan: the playback after it opens unfolded)
      try {
        await expand();
      } catch {
        return;
      }
    }
    $("simPlay").click();
  }
  /** The playback's controls (scrubber, speed, the end): shown, the stock simulated along if
   *  it is on. */
  async function expand() {
    m.expanded = true;
    // (at once: the final stock steps aside for the part while the playback is made again)
    showFinalStock();
    applyLayers();
    document.body.dataset.playback = "open";
    renderToggles();
    if (stockOn() && m.session && !m.stock) {
      m.keepAt = 0;
      m.open = false;
      await open();
    } else if (m.session) {
      $("simControls").classList.remove("hidden");
      if (m.pos >= m.session.segments) await seek(0);
    }
    renderToggles();
    showRange();
    syncFrame();
  }
  /** Back to the still view: the controls folded away, the whole toolpath drawn. */
  function collapse() {
    if (m.playing) $("simPlay").click();
    m.expanded = false;
    showFinalStock();
    applyLayers();
    document.body.dataset.playback = "";
    $("simControls").classList.add("hidden");
    renderToggles();
    showRange();
    syncFrame();
  }
  // (Mar: "all the other toolpaths disappeared", "the part is gone and I can't get it back":
  // a form drawn again under the caret left the page focused, and the letters typed next
  // switched Only on and the part off) the view's letters act with the pointer over the view
  let overView = false;
  $("view3d")?.addEventListener("pointerenter", () => (overView = true));
  $("view3d")?.addEventListener("pointerleave", () => (overView = false));
  document.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
    const t = ev.target;
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    if (!overView && !t?.closest?.("#view3d")) return;
    if (document.querySelector("dialog[open]:not(.embedded)")) return;
    const key = ev.key.toLowerCase();
    if (key === " " && (!t || t === document.body || t.closest?.("#view3d"))) {
      if (!m.session) return;
      ev.preventDefault();
      playButton();
      return;
    }
    const tg = TOGGLES.find((x) => x.key === key);
    if (!tg) return;
    // (the chuck's and tailstock's only where their buttons are shown)
    if (tg.rotary && document.querySelector(`#viewToggles button[data-v="${tg.k}"]`)?.hidden) return;
    ev.preventDefault();
    toggle(tg.k);
  });
  /** Machine time (s) when `n` segments have been played. */
  const timeAt = (n) => (n > 0 && m.session?.times ? m.session.times[Math.min(n, m.session.times.length) - 1] : 0);
  /** Segments played by machine time `t`. */
  function segmentsBy(t) {
    const ts = m.session.times;
    let lo = 0, hi = ts.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (ts[mid] <= t) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }
  const fmtClock = (t) => {
    const h = Math.floor(t / 3600), mi = Math.floor((t % 3600) / 60), se = Math.floor(t % 60);
    return h ? `${h}:${String(mi).padStart(2, "0")}:${String(se).padStart(2, "0")}` : `${mi}:${String(se).padStart(2, "0")}`;
  };

  const opKey = (o) => `${o.program}#${o.id}#${o.kind}`;
  const savedColors = () => {
    try {
      return JSON.parse(localStorage.getItem("marcam.opColors") || "{}");
    } catch {
      return {};
    }
  };

  function renderLegend() {
    const box = $("opLegend");
    box.innerHTML = "";
    if (!m.session) return;
    const saved = savedColors();
    const proj = m.session.project_ops;
    // colours follow the operation (its id), as the tree shows them: the same whatever is
    // hidden (hidden operations are not played, so the list has fewer entries)
    const setupOps = state.tree?.doc?.setups?.find((x) => x.id === state.tree?.setup)?.operations || [];
    m.session.ops.forEach((o, i) => {
      if (proj) o.project_id = proj.ids[i];
      const k = o.project_id != null ? setupOps.findIndex((x) => x.id === o.project_id) : -1;
      // (the tree's swatch: the project's colour, else the palette's by its place in the
      // setup; colours saved in this browser only for a 0.1 job, with no project's to follow)
      const own = k >= 0 ? setupOps[k].color : null;
      const hex = own || (proj && proj.colors[i]) || (proj ? null : saved[opKey(o)]) || PALETTE[(k >= 0 ? k : i) % PALETTE.length];
      m.colors[o.tag] = hexToRgb(hex);
      const row = document.createElement("label");
      row.className = "oprow";
      const hid = m.hiddenTags.has(o.tag);
      row.classList.toggle("op-hidden", hid);
      row.innerHTML = `<input type="color" value="${hex}" data-tag="${o.tag}" aria-label="Colour for ${o.program} ${o.kind}"> <span>${o.program}: ${o.kind.replace(/_/g, " ")}${o.project_id ? ` (${o.project_id})` : ""}${hid ? " (hidden)" : ""}</span>`;
      box.appendChild(row);
    });
    const un = document.createElement("div");
    un.className = "oprow small muted";
    un.innerHTML = `<span class="swatch" style="background: rgb(${uncut().map((c) => Math.round(c * 255)).join(",")})"></span> uncut stock`;
    box.appendChild(un);
  }

  // a hidden operation's cuts: grey (the material is still gone; it is just not highlighted)
  const HIDDEN_CUT = [0.5, 0.52, 0.56];
  const colorOf = (tag) => (tag ? (m.hiddenTags.has(tag) ? HIDDEN_CUT : m.colors[tag] || [1, 0, 1]) : uncut());

  /** Which playback operations the tree's eyes hide (by project id, or a 0.1 job's planning
   *  order). */
  function refreshHidden() {
    // (hidden operations are left out of the playback altogether: nothing played is hidden)
    m.hiddenTags = new Set();
  }

  /** The tree's eyes changed: the playback plays the visible operations, so it is opened
   *  again at the same place (nothing is planned again). */
  async function applyHidden() {
    // (open, its playback stopped by a newer plan: made again now with these eyes; else the
    // eyes changed nothing on it until a plan came: Mar's "can't get both to show")
    if (!m.session && !m.open) return;
    if (m.open) {
      // (a view action: opened again by itself, the status line keeps the job's times; at
      // the place last asked for, a scrub still under way included: the reopen keeps it)
      await open({ auto: true });
    } else {
      refreshHidden();
      renderLegend();
    }
  }

  // the engine grid as rows x cols (rotary: X rows of na cells; 3-axis: Y rows of nx cells)
  const dims = () => {
    const g = m.session.grid;
    return m.xyz ? { R: g.ny, C: g.nx } : { R: g.nx, C: g.na };
  };
  /** Smallest stride whose display grid has at most `limit` cells (rotary: dividing na). */
  function strideFor(limit) {
    const { R, C } = dims();
    let k = 1;
    for (;;) {
      if ((m.xyz || C % k === 0) && Math.ceil(R / k) * Math.ceil(C / k) <= limit) return k;
      k++;
      if (k > 64) return 64;
    }
  }
  function displayGrid(k) {
    const g = m.session.grid;
    return m.xyz
      ? { x0: g.x0, y0: g.y0, cell: g.cell * k, nx: Math.ceil(g.nx / k), ny: Math.ceil(g.ny / k) }
      : { x0: g.x0, dx: g.dx * k, nx: Math.ceil(g.nx / k), na: g.na / k };
  }
  /** Recompute display rows rd0..rd1 from the engine mirror (deepest cell of each block). */
  function fillRows(rd0, rd1) {
    const k = m.stride;
    const { R, C } = dims();
    const CD = Math.ceil(C / k);
    for (let rd = rd0; rd <= rd1; rd++) {
      for (let cd = 0; cd < CD; cd++) {
        let best = Infinity, tag = 0;
        for (let r = rd * k; r < Math.min(R, rd * k + k); r++)
          for (let c = cd * k; c < Math.min(C, cd * k + k); c++) {
            const v = m.r[r * C + c];
            if (v < best) {
              best = v;
              tag = m.op[r * C + c];
            }
          }
        m.dr[rd * CD + cd] = best;
        m.dop[rd * CD + cd] = tag;
      }
    }
  }

  /** Build the stock solid and the tool from the current arrays. */
  async function buildScene() {
    const v = await getThree();
    if (!v || v.failed) return;
    const { THREE } = v;
    if (!v.groups.machining) {
      v.groups.machining = new THREE.Group();
      // (hidden until shown: made anew after a reset while the view without it is drawn, it
      // was drawn with it for a frame, another setup chosen)
      v.groups.machining.visible = false;
      v.scene.add(v.groups.machining);
      v.groups.work = new THREE.Group(); // turns with A
      v.groups.machining.add(v.groups.work);
      v.groups.toolModel = new THREE.Group();
      v.groups.machining.add(v.groups.toolModel);
    }
    const work = v.groups.work;
    // built aside and swapped in at the end (the old one stays shown meanwhile, never half
    // a scene), in two parts with a pause between them: neither holds the page for long
    const gen = (m.buildGen = (m.buildGen || 0) + 1);
    const next = new THREE.Group();
    const s = m.session;
    const swap = () => {
      for (const c of [...work.children]) {
        work.remove(c);
        disposeTree(c);
      }
      for (const c of [...next.children]) work.add(c);
      m.selKey = null;
      applyLayers();
      showRange();
    };
    // the toolpath played so far, on the part (it turns with A)
    buildPath(THREE, next);
    await new Promise((r) => setTimeout(r, 0));
    if (gen !== m.buildGen || s !== m.session) {
      disposeTree(next);
      return;
    }
    if (!m.stock) {
      // no stock: the part itself turns under the tool
      for (const c of v.groups.part.children) {
        const k = c.clone(true);
        k.traverse((x) => (x.userData.shared = true));
        k.userData.src = c;
        next.add(k);
      }
      m.stockMesh = null;
      m.ghost = null;
      swap();
      m.toolNumber = null;
      showMachining(true);
      placeTool();
      return;
    }
    if (m.solid) {
      // the stock in 3D: a mesh a tile, made again as the tool goes through it
      const grp = new THREE.Group();
      m.tiles = new Map();
      solidTiles(THREE, grp, s.solid.tiles);
      s.solid.tiles = null;
      m.stockMesh = grp;
      next.add(grp);
      const { stock_radius: rr } = s;
      const len = (state.result?.stock?.length ?? 0) || 0;
      const ghost = stockObject(THREE, new THREE.CylinderGeometry(rr, rr, len || 1, 64, 1, false).rotateZ(-Math.PI / 2).translate(-(len || 1) / 2, 0, 0), 0.1);
      ghost.visible = $("simGhost").checked;
      m.ghost = ghost;
      next.add(ghost);
      swap();
      m.toolNumber = null;
      showMachining(true);
      placeTool();
      return;
    }
    const { R, C } = dims();
    m.dgrid = displayGrid(m.stride);
    m.dr = new Float32Array(Math.ceil(R / m.stride) * Math.ceil(C / m.stride));
    m.dop = new Uint16Array(m.dr.length);
    fillRows(0, Math.ceil(R / m.stride) - 1);
    const mesh = m.xyz ? blockSolid(m.dgrid, m.dr, s.stock_box.min[2], (k) => colorOf(m.dop[k]), uncut()) : heightmapSolid(m.dgrid, m.dr, (k) => colorOf(m.dop[k]), uncut());
    // (the block's sides: the cut's band under each top, moved with it)
    m.wallMid = mesh.wallMid || null;
    m.blockBottom = m.xyz ? s.stock_box.min[2] : null;
    const g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.BufferAttribute(mesh.positions, 3));
    g.setAttribute("color", new THREE.BufferAttribute(mesh.colors, 3));
    g.setIndex(new THREE.BufferAttribute(mesh.indices, 1));
    g.computeVertexNormals();
    m.stockMesh = new THREE.Mesh(g, new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.55, metalness: 0.15 }));
    next.add(m.stockMesh);
    // ghost of the original bar or block: shows how much has been removed
    let ghost;
    if (m.xyz) {
      const { min, max } = s.stock_box;
      ghost = stockObject(THREE, new THREE.BoxGeometry(max[0] - min[0], max[1] - min[1], max[2] - min[2]), 0.1);
      ghost.position.set((min[0] + max[0]) / 2, (min[1] + max[1]) / 2, (min[2] + max[2]) / 2);
    } else {
      const { x0, dx, nx } = s.grid;
      ghost = stockObject(THREE, new THREE.CylinderGeometry(s.stock_radius, s.stock_radius, nx * dx, 64, 1, false).rotateZ(-Math.PI / 2).translate(x0 + (nx * dx) / 2, 0, 0), 0.1);
    }
    ghost.visible = $("simGhost").checked;
    m.ghost = ghost;
    next.add(ghost);
    swap();
    m.toolNumber = null;
    showMachining(true);
    placeTool();
  }

  /** The toolpath as lines in the part's frame (machine tip turned back by A): the cuts in
   *  their operation's colour and the rapids red, each as what has been played (drawn up to
   *  the playback's position) and what is still to come (faint). The segments are indexed the
   *  same in both geometries (the other kind's collapsed to a point), so one position cuts
   *  both. */
  function buildPath(THREE, work) {
    const p = m.session.path;
    const n = m.session.segments;
    const geo = { cut: [new Float32Array(n * 6), new Float32Array(n * 6)], rapid: [new Float32Array(n * 6), new Float32Array(n * 6)] };
    const rapidRgb = colorRgb("rapid");
    let prev = null;
    for (let k = 0; k < n; k++) {
      const [x, y, z, a, rapid, , tag] = p.subarray(7 * k, 7 * k + 7);
      const r = (a * Math.PI) / 180;
      const c = Math.cos(r), sn = Math.sin(r);
      const q = [x, y * c + z * sn, -y * sn + z * c];
      const from = prev || q;
      const [mine, other] = rapid ? [geo.rapid, geo.cut] : [geo.cut, geo.rapid];
      mine[0].set(from, 6 * k);
      mine[0].set(q, 6 * k + 3);
      other[0].set(q, 6 * k);
      other[0].set(q, 6 * k + 3);
      const rgb = rapid ? rapidRgb : m.colors[tag] || colorRgb("cut");
      mine[1].set(rgb, 6 * k);
      mine[1].set(rgb, 6 * k + 3);
      prev = q;
    }
    const lines = ([pos, col], opacity) => {
      const g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.BufferAttribute(pos, 3));
      g.setAttribute("color", new THREE.BufferAttribute(col, 3));
      return new THREE.LineSegments(g, new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity }));
    };
    m.paths = {
      cut: [lines([geo.cut[0].slice(), geo.cut[1].slice()], 0.9), lines(geo.cut, 0.22)],
      rapid: [lines(geo.rapid, 0.6), lines(geo.rapid, 0.15)],
    };
    // (what each line is: for whoever looks at the scene, e.g. the tests)
    for (const [k, pair] of Object.entries(m.paths)) for (const o of pair) o.userData.path = k;
    m.pathLine = m.paths.cut[0];
    // (the selection's dimming and folding work from these)
    m.pathBase = { pos: geo.cut[0].slice(), col: geo.cut[1].slice() };
    m.selKey = null;
    for (const pair of Object.values(m.paths)) for (const o of pair) work.add(o);
    showRange();
    applyLayers();
  }

  /** Whether the playback is moving (playing, or scrubbed a moment ago): only then are the
   *  played and the coming toolpath told apart. */
  const moving = () => !!m.expanded && (m.playing || performance.now() - (m.scrubAt || 0) < 1500);
  /** The toolpath drawn: at rest all of it alike; while moving, what has been played and
   *  (faint) what is to come; "up to the playhead": only what has been played. */
  /** The playhead: the clock's place while playing (see frame), else the stock's. */
  function head() {
    return m.playing && m.head != null ? m.head : m.pos;
  }

  function showRange() {
    // (a new playback being built: the old one shown as it is, its toolpath drawn as it was)
    if (!m.paths || !m.session || m.swapping) return;
    const n = m.session.segments;
    const at = Math.min(head(), n);
    const upTo = m.expanded && view.paths === "playhead" ? at : moving() ? at : n;
    const ahead = m.expanded && view.paths === "all" && moving() ? n - at : 0;
    m.moving = moving();
    for (const [played, rest] of Object.values(m.paths)) {
      played.geometry.setDrawRange(0, 2 * upTo);
      rest.geometry.setDrawRange(2 * at, 2 * ahead);
    }
  }

  /** The viewer's switches: toolpaths, rapids, the part (it turns under the tool without
   *  stock). */
  function applyLayers() {
    const v = state.three;
    if (!v || v.failed || !v.groups.work) return;
    for (const o of m.paths?.cut || []) o.visible = view.paths !== "off";
    for (const o of m.paths?.rapid || []) o.visible = view.paths !== "off" && view.rapids;
    // the stock as the toggle says at once (while the playback is made again without it)
    if (m.stockMesh) m.stockMesh.visible = stockOn();
    if (m.ghost) m.ghost.visible = stockOn() && $("simGhost").checked;
    // at rest with the stock on: the background simulation's stock instead of the part
    // (and only while it is in the scene, shown)
    const inView = (o) => { for (let x = o; x; x = x.parent) if (!x.visible) return false; let p = o; while (p?.parent) p = p.parent; return p === v.scene; };
    const finalStock = !m.expanded && stockOn() && !!v.groups.simFinal?.children.length && !m.editing && inView(v.groups.machining);
    for (const c of v.groups.work.children) if (c.userData.shared) c.visible = view.part && !finalStock;
  }

  /** The selected operation's toolpath stands out: the others dimmed, or (Only) not drawn. */
  function applySelection() {
    if (!m.paths || !m.pathBase || !m.session) return;
    const sel = state.tree?.selected || null;
    const ids = {};
    for (const o of m.session.ops) ids[o.tag] = o.project_id;
    const p = m.session.path;
    const n = m.session.segments;
    const geo = m.paths.cut[0].geometry;
    const pos = geo.attributes.position.array, col = geo.attributes.color.array;
    const { pos: pos0, col: col0 } = m.pathBase;
    for (let k = 0; k < n; k++) {
      const tag = p[7 * k + 6];
      const other = sel && ids[tag] !== sel;
      for (let j = 0; j < 6; j++) {
        col[6 * k + j] = other ? col0[6 * k + j] * 0.3 + 0.08 : col0[6 * k + j];
        // (only the selected one: the others' segments folded to a point)
        pos[6 * k + j] = other && view.only ? pos0[6 * k + (j % 3) + 3] : pos0[6 * k + j];
      }
    }
    geo.attributes.position.needsUpdate = true;
    geo.attributes.color.needsUpdate = true;
  }

  /** At rest with the stock on: the background simulation's last stock, in the part's frame. */
  function showFinalStock() {
    const v = state.three;
    if (!v || v.failed || !v.groups.machining) return;
    if (!v.groups.simFinal) {
      v.groups.simFinal = new v.THREE.Group();
      v.groups.machining.add(v.groups.simFinal);
    }
    // (Mar: "the part is gone and I can't get it back") a playback made again since: the
    // stock goes with the one shown, never left on the old one (out of the scene, the part
    // stepping aside for a stock no one sees)
    if (v.groups.simFinal.parent !== v.groups.machining) v.groups.machining.add(v.groups.simFinal);
    const want = !m.expanded && stockOn() && state.sim ? state.sim : null;
    const key = want ? want : null;
    if (m.finalKey !== key || (want && !v.groups.simFinal.children.length)) {
      m.finalKey = key;
      for (const c of [...v.groups.simFinal.children]) {
        v.groups.simFinal.remove(c);
        disposeTree(c);
      }
      const last = want?.stocks?.[want.stocks.length - 1];
      if (last && simMeshOf) v.groups.simFinal.add(simMeshOf(v.THREE, want, last));
      applyLayers();
    }
    v.groups.simFinal.visible = !!want && !m.editing;
  }

  /** Only the machining view while it is open. */
  function showMachining(on) {
    const v = state.three;
    if (!v || v.failed) return;
    if (v.groups.machining) v.groups.machining.visible = on;
    // the view without the playback: as the corner toggles say (none of it while it shows)
    if (window.__marcam.legacyLayers) window.__marcam.legacyLayers();
    else for (const k of ["part", "stock", "sim", "path", "tool"]) v.groups[k].visible = !on;
  }

  /** Tool model: shank + cutting end, tip at the origin, axis +Z (the spindle). */
  function toolModel(THREE, t) {
    const grp = new THREE.Group();
    const mat = new THREE.MeshStandardMaterial({ color: colorNum("tool"), metalness: 0.6, roughness: 0.35 });
    const d = t?.diameter ?? 3.175;
    const R = d / 2;
    const flute = Math.max(t?.flute_length ?? 8, 2);
    const add = (geo, z) => {
      const mesh = new THREE.Mesh(geo.rotateX(Math.PI / 2), mat);
      mesh.position.z = z;
      grp.add(mesh);
    };
    if (t?.kind === "vbit") {
      const h = R / Math.tan(((t.angle ?? 90) * Math.PI) / 360);
      // ConeGeometry's apex is at +Y; turned -90deg about X it points down (-Z): tip at 0
      const cone = new THREE.Mesh(new THREE.ConeGeometry(R, h, 32).rotateX(-Math.PI / 2), mat);
      cone.position.z = h / 2;
      grp.add(cone);
      add(new THREE.CylinderGeometry(R, R, 20, 32), h + 10);
    } else if (t?.kind === "ball") {
      const ball = new THREE.Mesh(new THREE.SphereGeometry(R, 24, 16), mat);
      ball.position.z = R;
      grp.add(ball);
      add(new THREE.CylinderGeometry(R, R, flute + 20 - R, 32), R + (flute + 20 - R) / 2);
    } else {
      add(new THREE.CylinderGeometry(R, R, flute + 20, 32), (flute + 20) / 2);
    }
    return grp;
  }

  function placeTool() {
    // (as showRange: the old playback untouched while the new one is built)
    if (m.swapping) return;
    const v = state.three;
    // (every operation hidden: nothing to play, no tool to place; on a vise, the work square)
    if (v && !v.failed && m.session && !m.session.segments && m.xyz && v.groups.work) {
      m.angle = 0;
      v.groups.work.rotation.x = 0;
    }
    if (!v || v.failed || !m.session || !m.session.segments) return;
    const s = m.session;
    // (Mar: "the wrong playback speed", chunky when fast) the playhead: where the clock is
    // while playing, the stock catching up behind it; else the stock's place
    const at = head();
    const k = Math.max(0, Math.min(at, s.segments) - 1);
    const p = s.path;
    // machine tool tip, A, rapid, tool, operation tag
    let [x, y, z, a, , toolNo, opTag] = p.subarray(7 * k, 7 * k + 7);
    // playing: part of the way along the move under way (it does not jump move to move)
    const f = m.playing && m.fracAt === at ? m.frac || 0 : 0;
    if (f > 0 && at < s.segments && at > 0) {
      const q = p.subarray(7 * at, 7 * at + 7);
      if (q[5] === toolNo) {
        x += (q[0] - x) * f;
        y += (q[1] - y) * f;
        z += (q[2] - z) * f;
        a += (q[3] - a) * f;
      }
    }
    if (toolNo !== m.toolNumber) {
      const tm = v.groups.toolModel;
      for (const c of [...tm.children]) {
        tm.remove(c);
        disposeTree(c);
      }
      tm.add(toolModel(v.THREE, s.tools.find((t) => t.number === toolNo)));
      m.toolNumber = toolNo;
    }
    const op = s.ops.find((o) => o.tag === opTag);
    const where = m.xyz ? `X${x.toFixed(1)} Y${y.toFixed(1)} Z${z.toFixed(2)}` : y ? `A${a.toFixed(1)} · Y${y.toFixed(2)} Z${z.toFixed(2)}` : `A${a.toFixed(1)}`;
    // the work turns by A (right-handed about +X; 0 on a vise); the tool stands at its tip
    m.angle = (a * Math.PI) / 180;
    v.groups.work.rotation.x = m.angle;
    v.groups.toolModel.position.set(x, y, z);
    // the tool: during playback (Tool on), never at rest
    v.groups.toolModel.visible = !!m.expanded && view.tool;
    syncFrame();
    showRange();
    const clock = s.times?.length ? ` · ${fmtClock(timeAt(at))} / ${fmtClock(s.times[s.times.length - 1])}` : "";
    if (m.playing) $("simScrub").value = String(at);
    $("simLabel").textContent = `move ${at.toLocaleString("en")} / ${s.segments.toLocaleString("en")}${clock}${op ? ` · ${op.program}: ${op.kind.replace(/_/g, " ")}` : ""} · ${where} · ${m.stock ? (m.gpu ? "GPU" : "CPU") : "no stock"}${m.note ? ` · ${m.note}` : ""}`;
    renderSimProgress();
  }

  /** (Mar: "It's also very unclear as to what its displaying and when it's still rendering.
   *  I think the progress bar should be used to visualize what has been rendered / simulated
   *  and what is still to go") The playback's bar: the stock simulated so far filled, where
   *  it is still being simulated to striped, the rest plain; and a word of what is shown: the
   *  stock simulating (how far), the smooth 3D stock being made, or ready. */
  function renderSimProgress() {
    const bar = $("simScrub");
    const s = m.session;
    if (!bar) return;
    const n = s?.segments || 0;
    const pct = (k) => (n ? Math.max(0, Math.min(100, (100 * k) / n)) : 0);
    const done = m.stock ? pct(m.pos) : 0;
    const want = m.stock && m.busy ? pct(Math.max(m.target ?? m.pos, m.pos)) : done;
    bar.style.setProperty("--sim-done", `${done.toFixed(2)}%`);
    bar.style.setProperty("--sim-want", `${want.toFixed(2)}%`);
    bar.classList.toggle("sim-progress", !!m.stock);
    // what is shown, in words
    const ring = window.__marcam.simRing;
    const sim = state.sim;
    let say = "";
    if (m.stock && m.busy) say = `Simulating the stock along the playback: ${Math.round(done)}% of the moves, to ${Math.round(want)}%`;
    else if (ring?.active) {
      const t = (performance.now() - ring.t0) / 1000;
      say = ring.estimate > 0 ? `Simulating the stock: ${Math.min(97, Math.round((100 * t) / ring.estimate))}%` : "Simulating the stock...";
    } else if (sim?.surfacing) say = "The stock in 3D: being made (the stock shown meanwhile is the quick one)";
    else if (sim?.surface) say = "Ready: the stock in 3D";
    else if (sim) say = "Ready";
    let el = $("simProgress");
    if (!el) {
      el = document.createElement("div");
      el.id = "simProgress";
      el.className = "small sim-progress-label";
      el.setAttribute("role", "status");
      $("simLabel")?.after(el);
    }
    if (el.textContent !== say) el.textContent = say;
  }

  /** Apply a sim_seek result: rows changed -> update vertices and colours in place. */
  /** Tiles of the stock in 3D into `grp` (made or made again; an empty one gone). */
  function solidTiles(THREE, grp, tiles) {
    // (each vertex in the colour of the operation that last cut there, as the radial stock was)
    const mat = m.tileMat || (m.tileMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.55, metalness: 0.15 }));
    for (const t of tiles || []) {
      const key = t.key.join(",");
      const old = m.tiles.get(key);
      if (old) {
        grp.remove(old);
        old.geometry.dispose();
        m.tiles.delete(key);
      }
      const pos = f32(t.positions);
      if (!pos.length) continue;
      const ib = b64ToBytes(t.indices);
      const idx = new Uint32Array(ib.buffer, ib.byteOffset, ib.byteLength / 4);
      const g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.BufferAttribute(pos, 3));
      const ops = t.ops ? u16(t.ops) : null;
      const col = new Float32Array(pos.length);
      for (let k = 0; k < pos.length / 3; k++) col.set(colorOf(ops ? ops[k] : 0), 3 * k);
      g.setAttribute("color", new THREE.BufferAttribute(col, 3));
      g.setIndex(new THREE.BufferAttribute(idx, 1));
      g.computeVertexNormals();
      const mesh = new THREE.Mesh(g, mat);
      mesh.userData.tile = key;
      mesh.userData.ops = ops;
      grp.add(mesh);
      m.tiles.set(key, mesh);
    }
  }

  function applyRows(res) {
    m.pos = res.pos;
    if (res.tiles) {
      const v = state.three;
      if (m.solid && m.stockMesh && v && !v.failed) solidTiles(v.THREE, m.stockMesh, res.tiles);
      else if (m.solid && m.session?.solid) {
        // (no stock drawn yet, as on reopening: the tiles kept, each replacing its key, for the scene)
        const by = new Map((m.session.solid.tiles || []).map((t) => [t.key.join(","), t]));
        for (const t of res.tiles) by.set(t.key.join(","), t);
        m.session.solid.tiles = [...by.values()];
      }
      if (!m.playing) $("simScrub").value = String(m.pos);
      placeTool();
      return;
    }
    if (res.rows) {
      // the engine mirror first (rows of C cells), then the display rows they fall in; the
      // WASM playback sends base64, the GPU one typed arrays
      const { C } = dims();
      const [r0] = res.rows;
      const h = f32(m.xyz ? res.z : res.r);
      const op = u16(res.op);
      // (rows of a playback since replaced, another grid: dropped, never written past it)
      if (r0 * C + h.length > m.r.length || r0 * C + op.length > m.op.length) return;
      m.r.set(h, r0 * C);
      m.op.set(op, r0 * C);
    }
    const k = m.stride;
    if (res.rows && m.stockMesh && m.xyz) {
      // display rows are Y rows; vertex v is display cell v (the walls share those vertices)
      const [j0, j1] = res.rows.map((r) => Math.floor(r / k));
      fillRows(j0, j1);
      const { nx } = m.dgrid;
      const pos = m.stockMesh.geometry.attributes.position.array;
      const col = m.stockMesh.geometry.attributes.color.array;
      for (let v = j0 * nx; v < (j1 + 1) * nx; v++) {
        pos[3 * v + 2] = m.dr[v];
        col.set(colorOf(m.dop[v]), 3 * v);
      }
      if (m.wallMid) {
        const [lo, hi] = [j0 * nx, (j1 + 1) * nx];
        for (let t = 0; t < m.wallMid.length; t += 2) {
          const v = m.wallMid[t];
          if (v >= lo && v < hi) pos[3 * m.wallMid[t + 1] + 2] = Math.max(m.blockBottom, m.dr[v] - WALL_BAND);
        }
      }
      m.stockMesh.geometry.attributes.position.needsUpdate = true;
      m.stockMesh.geometry.attributes.color.needsUpdate = true;
      blockNormals(m.stockMesh.geometry, j0 - 1, j1 + 1);
    } else if (res.rows && m.stockMesh) {
      const [i0, i1] = res.rows.map((r) => Math.floor(r / k));
      fillRows(i0, i1);
      const { na, nx } = m.dgrid;
      const pos = m.stockMesh.geometry.attributes.position.array;
      const col = m.stockMesh.geometry.attributes.color.array;
      const setV = (vi, i, j, paint = true) => {
        const k = i * na + j;
        const phi = ((j + 0.5) * 2 * Math.PI) / na;
        pos[3 * vi + 1] = m.dr[k] * Math.cos(phi);
        pos[3 * vi + 2] = m.dr[k] * Math.sin(phi);
        if (!paint) return;
        const c = colorOf(m.dop[k]);
        col[3 * vi] = c[0];
        col[3 * vi + 1] = c[1];
        col[3 * vi + 2] = c[2];
      };
      for (let i = i0; i <= i1; i++) for (let j = 0; j < na; j++) setV(i * na + j, i, j);
      // end caps repeat the first and last rings
      // (their faces in the stock's own colour: only moved)
      if (i0 === 0) for (let j = 0; j < na; j++) setV(nx * na + j, 0, j, false);
      if (i1 === nx - 1) for (let j = 0; j < na; j++) setV(nx * na + na + 1 + j, nx - 1, j, false);
      m.stockMesh.geometry.attributes.position.needsUpdate = true;
      m.stockMesh.geometry.attributes.color.needsUpdate = true;
      barNormals(m.stockMesh.geometry, i0 - 1, i1 + 1);
    }
    if (!m.playing) $("simScrub").value = String(m.pos);
    placeTool();
  }

  /** The block's top normals over display rows j0..j1 only (from the neighbouring cells:
   *  the whole mesh's normals each frame took most of a frame on a large block). The walls
   *  and bottom keep theirs. */
  function blockNormals(g, j0, j1) {
    const { nx, ny, cell } = m.dgrid;
    const nrm = g.attributes.normal.array;
    const z = m.dr;
    j0 = Math.max(0, j0);
    j1 = Math.min(ny - 1, j1);
    for (let j = j0; j <= j1; j++) {
      const jm = Math.max(0, j - 1), jp = Math.min(ny - 1, j + 1);
      for (let i = 0; i < nx; i++) {
        // (the edge rows and columns border the walls: theirs stay as built)
        if (i === 0 || i === nx - 1 || j === 0 || j === ny - 1) continue;
        const k = j * nx + i;
        const zx = (z[k + 1] - z[k - 1]) / (2 * cell);
        const zy = (z[jp * nx + i] - z[jm * nx + i]) / ((jp - jm) * cell);
        const l = Math.hypot(zx, zy, 1);
        nrm[3 * k] = -zx / l;
        nrm[3 * k + 1] = -zy / l;
        nrm[3 * k + 2] = 1 / l;
      }
    }
    g.attributes.normal.needsUpdate = true;
  }

  /** The bar's normals over display rings i0..i1 only (a surface of revolution r(x, phi)). */
  function barNormals(g, i0, i1) {
    const { na, nx, dx } = m.dgrid;
    const nrm = g.attributes.normal.array;
    const r = m.dr;
    const dphi = (2 * Math.PI) / na;
    i0 = Math.max(0, i0);
    i1 = Math.min(nx - 1, i1);
    for (let i = i0; i <= i1; i++) {
      const im = Math.max(0, i - 1), ip = Math.min(nx - 1, i + 1);
      for (let j = 0; j < na; j++) {
        const k = i * na + j;
        const rr = r[k];
        const rx = ip > im ? (r[ip * na + j] - r[im * na + j]) / ((ip - im) * dx) : 0;
        const rp = (r[i * na + ((j + 1) % na)] - r[i * na + ((j + na - 1) % na)]) / (2 * dphi);
        const phi = (j + 0.5) * dphi;
        const c = Math.cos(phi), s = Math.sin(phi);
        // T_phi x T_x, T_x = (1, rx c, rx s), T_phi = (0, rp c - r s, rp s + r c): outward
        const ty = rp * c - rr * s, tz = rp * s + rr * c;
        const nX = ty * rx * s - tz * rx * c, nY = tz, nZ = -ty;
        const l = Math.hypot(nX, nY, nZ) || 1;
        nrm[3 * k] = nX / l;
        nrm[3 * k + 1] = nY / l;
        nrm[3 * k + 2] = nZ / l;
      }
    }
    g.attributes.normal.needsUpdate = true;
  }

  /** (Mar: "if you click a specific one, it should go to that location and time in the sim")
   *  The playback at move `move` of program `prog`: open and unfolded, played up to it (the
   *  stock as it was then), the tool there, and the view turned to look at it. Returns the
   *  segment, or null. */
  async function jumpTo(prog, move) {
    for (let i = 0; i < 300 && !m.session; i++) await new Promise((r) => setTimeout(r, 100));
    if (!m.session) return null;
    if (!m.expanded) await expand();
    const s = m.session;
    if (!s.moves) return null;
    const starts = s.program_starts || [0];
    const from = starts[prog] ?? 0;
    const to = prog + 1 < starts.length ? starts[prog + 1] : s.segments;
    let lo = from, hi = to;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (s.moves[mid] < move) lo = mid + 1;
      else hi = mid;
    }
    const k = Math.min(lo, to - 1);
    if (k < from) return null;
    if (m.playing) $("simPlay").click();
    await seek(k + 1);
    for (let i = 0; i < 600 && m.busy; i++) await new Promise((r) => setTimeout(r, 50));
    $("simScrub").value = String(m.pos);
    placeTool();
    // (the one camera move here, asked for: the view's centre onto the tool, as far away)
    const v = state.three;
    if (v && !v.failed && v.groups.toolModel) {
      const p = v.groups.toolModel.getWorldPosition(new v.THREE.Vector3());
      const d = p.clone().sub(v.controls.target);
      v.camera.position.add(d);
      v.controls.target.copy(p);
      v.controls.update();
      window.__marcam.kickView?.();
    }
    syncFrame();
    return { segment: k, pos: m.pos };
  }

  const SEEK_CHUNK = 4000;
  async function seek(n) {
    if (!m.session) return;
    // (the place asked for: a playback opened again meanwhile goes there)
    m.wanted = n;
    // no stock: only the tool and the path move
    if (!m.stock) {
      m.pos = Math.max(0, Math.min(n, m.session.segments));
      $("simScrub").value = String(m.pos);
      placeTool();
      return;
    }
    m.target = n;
    if (m.busy) return;
    m.busy = true;
    try {
      while (m.session && m.stock && m.target !== m.pos) {
        const session = m.session;
        // in steps of at most SEEK_CHUNK moves: a newer place asked for (a scrub during the
        // long run to the end) is taken up after the step under way, not after the whole run
        // (back: the engine starts again from the top)
        // (back, the slider still moving: a rewind runs the whole job again from the top, so
        // wait for it to rest a moment instead of chasing each place it passes)
        if (m.target < m.pos && performance.now() - (m.scrubAt || 0) < 150) {
          await new Promise((r) => setTimeout(r, 60));
          continue;
        }
        const goal = m.target;
        renderSimProgress();
        const from = goal < m.pos ? 0 : m.pos;
        // (back: the engine starts again from the top, run to the place in one go, so no
        // state on the way there is drawn; Mar's scrub glitch)
        const want = from === 0 && goal < m.pos ? goal : Math.min(goal, from + SEEK_CHUNK);
        const before = m.pos;
        let result;
        if (m.gpu) {
          // on the GPU: run the segments, read back only the rows they touched
          const gpu = m.gpu;
          const rows = gpu.seek(want);
          result = { pos: gpu.pos, rows: null };
          if (rows) {
            const { h, op } = gpu.decode(await gpu.readRows(rows[0], rows[1]));
            result = { pos: gpu.pos, rows, [m.xyz ? "z" : "r"]: h, op };
          }
        } else {
          result = (await engine("simSeek", want)).result;
        }
        // closed while the engine worked: nothing to show; another opened meanwhile (the
        // playback opened again after a plan): on to the place asked for in that one
        if (!m.session) break;
        if (m.session !== session) continue;
        if (!result) break;
        applyRows(result);
        // (the engine stopped short, at the end of the program: done; short of the place
        // otherwise, on with the next step: a scrub was left stopped half-way, the playhead
        // jumping there later by itself)
        if (m.pos === before || m.pos >= session.segments) break;
      }
    } catch (e) {
      // cancelled, or a newer plan (the playback went with the engine's worker, and a seek
      // still on its way found the new worker without one): nothing more to show here, the
      // playback made again goes to the place asked for
      if (!["cancelled", "superseded", "replanning"].includes(e.message) && !/no playback open/.test(e.message)) throw e;
    } finally {
      m.busy = false;
    }
  }

  /** Open (or open again, at the same place) the playback. `opts.auto`: by itself after a
   *  plan (nothing asked, the status left alone, at the end the first time). */
  async function open(opts = {}) {
    if (!opts.auto) showError("");
    let job;
    try {
      job = readJob();
    } catch (e) {
      return showError(e.message);
    }
    // one simulation at a time (the engine does one thing at a time, and two on the GPU at
    // once can stall it): after the one running
    if (!simLock) return openLocked(job, opts);
    return simLock.run("playback", () => {
      if (!opts.auto) setStatus("Opening machining simulation...", "busy");
      return openLocked(job, opts);
    }, { quiet: !!opts.auto });
  }

  async function openLocked(job, opts = {}) {
    try {
      return await openLocked1(job, opts);
    } finally {
      m.swapping = false;
    }
  }

  async function openLocked1(job, opts = {}) {
    // the stock simulated along while playing (Stock on); at rest only the tool and the path
    // (the stock shown then is the background simulation's)
    let stock = stockOn() && !!m.expanded;
    m.note = "";
    if (stock && opts.auto) {
      // opened by itself (after a plan): nothing asked; too long a simulation is not started
      try {
        const t = (await engine("simGrid", job, { ...(simOpts?.() || {}), libraries: libraries(), setup: job.schema_version === "0.2" ? state.tree?.setup : undefined, estimate: true }, state.mesh)).result.time;
        if (t?.seconds > 30) {
          stock = false;
          m.note = `stock not simulated: about ${Math.round(t.seconds)} s at this resolution (choose a coarser one, or turn stock off and on to start it anyway)`;
        }
      } catch {}
    } else if (stock && confirmSimRes && !(await confirmSimRes())) {
      setStatus("Machining simulation not started", "done");
      return;
    }
    const mine = epoch;
    // the same place when open again; the first time, the end (the whole job drawn)
    // (the place a seek under way is going to; else where it is: played on since a scrub, the
    // place it was scrubbed to is not where it is)
    const keep = m.open ? (m.busy && m.wanted != null ? m.wanted : m.pos) : (m.keepAt ?? Number.POSITIVE_INFINITY);
    m.keepAt = null;
    // open again (after a plan): the old playback stays shown, untouched, until the new one
    // is built at the same place, then swapped in (never a frame from its start)
    const reopening = m.open;
    try {
      const setup = job.schema_version === "0.2" ? state.tree?.setup : undefined;
      const machine = machineArg?.() ?? null;
      // (opened by itself after a plan: a newer plan makes it stale, and stops it)
      m.autoOpening = !!opts.auto;
      let result;
      try {
        ({ result } = await engine("simOpen", job, { ...(simOpts?.() || {}), libraries: libraries(), setup, machine: typeof machine === "string" ? machine : null, path_only: !stock, solid: stock }, state.mesh));
      } finally {
        m.autoOpening = false;
      }
      result.path = f32(result.path);
      result.times = result.times ? f32(result.times) : null;
      result.moves = result.moves ? f32(result.moves) : null;
      if (reopening) m.swapping = true;
      m.session = result;
      m.stock = stock;
      m.xyz = result.kind === "xyz";
      // (a vise's work never turned: no A left from a rotary playback before it, which with
      // nothing to play was never placed again; the knob in a vise drawn turned)
      if (m.xyz) m.angle = 0;
      m.pos = 0;
      m.clock = null;
      m.gpu?.destroy();
      m.gpu = null;
      // (the place, within the new job: clamped if it got shorter)
      const at = Math.min(keep, result.segments);
      if (!stock) {
        if (mine !== epoch) return void (m.session = null);
        m.backend = "";
        renderLegend();
        m.pos = at;
        await buildScene();
        m.swapping = false;
        // (the new one's toolpath drawn to its place, its tool there)
        showRange();
        placeTool();
        $("simScrub").max = String(result.segments);
        $("simScrub").value = String(m.pos);
        $("simControls").classList.toggle("hidden", !m.expanded);
        m.open = true;
        document.body.dataset.machining = "open";
        renderToggles();
        if (keep) await seek(Math.min(keep, result.segments));
        if (!opts.auto) setStatus(`Playback: ${result.segments.toLocaleString("en")} moves, ${result.ops.length} operations, ${fmtClock(result.times?.[result.times.length - 1] || 0)} of machining (no stock)`, "done");
        // (the stock shown, the playback shut: the background simulation's stock, started
        // without a click; a project opened or an example loaded showed none until one)
        if (stockOn()) window.__marcam.statsInBackground?.();
        return;
      }
      // (the stock in 3D: tiles of its surface from the engine, no radial grid here)
      m.solid = !!result.solid;
      const n = m.solid ? 0 : m.xyz ? result.grid.nx * result.grid.ny : result.grid.nx * result.grid.na;
      m.r = new Float32Array(n).fill(m.xyz ? result.stock_box?.max[2] ?? 0 : result.stock_radius);
      // (a square, hex or rectangular bar: its radius at each angle)
      if (!m.solid && !m.xyz && result.stock_ring) for (let i = 0; i < result.grid.nx; i++) m.r.set(result.stock_ring, i * result.grid.na);
      // (a bar lying in the vise: each row from the bar's top there)
      if (m.xyz && result.stock_rows) result.stock_rows.forEach((z, j) => m.r.fill(z, j * result.grid.nx, (j + 1) * result.grid.nx));
      // (Mar's carry-through: a setup taking the cut piece of the one before, or a bar standing
      // in the vise) every cell as the stock starts, not a fresh bar
      if (result.stock_start) {
        const st = f32(result.stock_start);
        if (st.length === n) m.r.set(st);
      }
      m.op = new Uint16Array(n);
      m.stride = m.solid ? 1 : strideFor(DISPLAY_MAX);
      // the stock on the GPU when there is one and the engine offers the job to it
      const { gpu, why } = simGpu && !m.solid ? await simGpu() : { gpu: null, why: m.solid ? "the stock in 3D" : "" };
      let backend = `CPU (${why})`;
      if (gpu) {
        try {
          const { result: g } = await engine("gpuOpen", job, { ...(simOpts?.() || {}), libraries: libraries(), setup }, state.mesh);
          const same = m.xyz ? g.grid.nx === result.grid.nx && g.grid.ny === result.grid.ny : g.grid.nx === result.grid.nx && g.grid.na === result.grid.na;
          if (!same || g.input.seg_start.length === 0) throw new Error("grid mismatch");
          m.gpu = new GpuStock(gpu, g);
          if (m.gpu.segments !== result.segments) throw new Error(`segments ${m.gpu.segments} vs ${result.segments}`);
          backend = `GPU (${gpu.name})`;
        } catch (e) {
          m.gpu?.destroy();
          m.gpu = null;
          backend = `CPU (${e.message})`;
        }
      }
      if (mine !== epoch) {
        // the project was replaced meanwhile
        m.gpu?.destroy();
        m.gpu = null;
        m.session = null;
        return;
      }
      m.backend = backend;
      showBackend?.(backend);
      await engine("simSeek", 0);
      refreshHidden();
      renderLegend();
      if (reopening && at > 0) {
        // the stock run to the place first (into the engine's mirror only: the old scene
        // stays as it was), then built there
        m.stockMesh = null;
        await seek(at);
        if (m.session !== result) return;
      }
      await buildScene();
      m.swapping = false;
      showRange();
      placeTool();
      $("simScrub").max = String(result.segments);
      $("simScrub").value = String(m.pos);
      $("simControls").classList.toggle("hidden", !m.expanded);
      m.open = true;
      document.body.dataset.machining = "open";
      renderToggles();
      if (keep) await seek(Math.min(keep, result.segments));
      if (!opts.auto) setStatus(`Machining simulation: ${result.segments.toLocaleString("en")} moves, ${result.ops.length} operations`, "done");
      // the stock's numbers (material left, gouges, rapids through it): the batch simulation,
      // in the background
      window.__marcam.statsInBackground?.();
    } catch (e) {
      // (the project went meanwhile, or a newer plan: nothing to say)
      if (e.message === "superseded" || e.message === "replanning") return;
      if (opts.auto) {
        // (opened by itself: the status line stays the person's)
        if (e.message !== "cancelled") showError(e.message);
        return;
      }
      if (e.message === "cancelled") return setStatus("Machining simulation cancelled", "done");
      showError(e.message);
      setStatus("Machining simulation failed", "error");
    }
  }

  function close() {
    m.playing = false;
    m.open = false;
    // (a playback cancelled from the ring in the view's corner, or lost with its worker, left
    // the controls folded away and the small play button hidden: both gone) folded, the play
    // button back to open it again
    m.expanded = false;
    document.body.dataset.playback = "";
    m.gpu?.destroy();
    m.gpu = null;
    $("simControls").classList.add("hidden");
    $("simPlay").textContent = "Play";
    document.body.dataset.machining = "";
    showMachining(false);
    renderToggles();
  }

  /** A new project: close the playback and forget its session, GPU stock and meshes. */
  function reset() {
    epoch++;
    close();
    Object.assign(m, { session: null, r: null, op: null, pos: 0, target: 0, wanted: null, busy: false, stockMesh: null, ghost: null, dgrid: null, dr: null, dop: null, toolNumber: null, colors: {} });
    // (the play button disabled with the session gone: close drew it while it was there)
    renderToggles();
    $("opLegend").innerHTML = "";
    const v = state.three;
    if (v && !v.failed && v.groups.machining) {
      v.scene.remove(v.groups.machining);
      disposeTree(v.groups.machining);
      delete v.groups.machining;
      delete v.groups.work;
      delete v.groups.toolModel;
    }
  }

  /** Rebuild the display at the stride for the current state (playing or at rest). */
  async function restride() {
    // (without stock there is no grid to show coarser while playing)
    if (!m.session || !m.stock || m.solid) return;
    const want = strideFor(m.playing ? PLAY_MAX : DISPLAY_MAX);
    if (want !== m.stride) {
      m.stride = want;
      await buildScene();
    }
  }

  /** Start the playing loop: one at a time (a Play pressed again, or Pause then Play within
   *  a frame, started a second one, and it played twice as fast). */
  function startLoop() {
    const id = (m.loopId = (m.loopId || 0) + 1);
    const step = (t) => {
      if (id !== m.loopId) return;
      if (frame(t)) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  /** One frame of playing: false when it stops. */
  function frame(t) {
    if (!m.playing) {
      m.head = null;
      // (paused: the tool where the stock is, at the move shown, not part of the way along
      // the next one; a fresh seek to that place draws the same)
      placeTool();
      restride();
      return false;
    }
    // (the playback being replaced after a plan: on once it is)
    if (!m.session || m.swapping) {
      m.last = t;
      return true;
    }
    const dt = Math.min(0.1, (t - (m.last || t)) / 1000);
    m.last = t;
    let next;
    if (m.session.times?.length) {
      // machine time runs `speed` times faster than the clock; the tool goes part of the way
      // along the move under way (placeTool), so a long move is seen, not jumped
      if (m.clock == null) m.clock = timeAt(m.pos);
      m.clock += dt * speedFactor();
      const ts = m.session.times;
      // a move that goes nowhere (a tool change, a dwell) passes in a third of a second at
      // most: watching nothing move for its real length looks like a hang
      {
        const k = segmentsBy(m.clock);
        if (k < ts.length) {
          const p = m.session.path;
          // (the first move carries the program's start: its tool change)
          const still = k === 0 || (p[7 * k] === p[7 * k - 7] && p[7 * k + 1] === p[7 * k - 6] && p[7 * k + 2] === p[7 * k - 5] && p[7 * k + 3] === p[7 * k - 4]);
          if (still) m.clock = Math.max(m.clock, ts[k] - 0.3 * speedFactor() + dt * speedFactor());
        }
      }
      next = segmentsBy(m.clock);
      // (a run of moves that take no time: one at least each frame)
      const from = m.head ?? m.pos;
      if (next < ts.length && next === from && ts[next] <= timeAt(from)) next++;
      const t0 = timeAt(next), t1 = next < ts.length ? ts[next] : t0;
      m.frac = t1 > t0 ? Math.max(0, Math.min(1, (m.clock - t0) / (t1 - t0))) : 0;
      m.fracAt = next;
    } else {
      next = Math.round(m.pos + Math.max(1, 300 * speedFactor() * dt));
      m.frac = 0;
    }
    next = Math.min(m.session.segments, next);
    // the playhead goes where the clock is, every frame; the stock follows it (as far as the
    // engine has got: it catches up in the frames after, never holding the playhead back)
    m.head = next;
    if (next !== m.target || (!m.busy && next !== m.pos)) seek(next);
    placeTool();
    if (next >= m.session.segments && m.pos >= m.session.segments) {
      m.playing = false;
      m.head = null;
      $("simPlay").textContent = "Play";
      document.body.dataset.playing = "";
      restride();
      return false;
    }
    return true;
  }

  // opened again by hand: not open until the new playback is (at the same place)
  // (Playback: the controls open, the playback made again at the same place)
  $("simOpen").addEventListener("click", () => {
    m.expanded = true;
    document.body.dataset.playback = "open";
    if (m.open) {
      m.keepAt = m.busy && m.wanted != null ? m.wanted : m.pos;
      m.open = false;
      document.body.dataset.machining = "";
    }
    open();
  });
  $("simClose").addEventListener("click", collapse);
  $("simPlay").addEventListener("click", () => {
    if (!m.session) return;
    m.playing = !m.playing;
    $("simPlay").textContent = m.playing ? "Pause" : "Play";
    document.body.dataset.playing = m.playing ? "yes" : "";
    if (m.playing) {
      if (m.pos >= m.session.segments) seek(0);
      m.last = 0;
      m.head = null;
      restride().then(() => m.playing && startLoop());
    }
  });
  $("simStep").addEventListener("click", () => {
    m.clock = null;
    if (m.session) seek(Math.min(m.session.segments, m.pos + 1));
  });
  $("simScrub").addEventListener("input", () => {
    const at = +$("simScrub").value;
    // (playing: the clock goes on from where it was dropped, not from where the stock was)
    m.clock = m.playing && m.session?.times?.length ? timeAt(at) : null;
    m.head = m.playing ? at : null;
    m.scrubAt = performance.now();
    seek(at);
  });
  $("simEnd").addEventListener("click", () => {
    m.clock = null;
    if (m.session) seek(m.session.segments);
  });
  $("simSpeed").addEventListener("input", () => {
    showSpeed();
    remember("marcam.playSpeed", $("simSpeed").value);
  });
  // the older switches (scripts set them): as their toggles
  $("simStock").addEventListener("change", () => {
    if ($("simStock").checked !== view.stock) toggle("stock");
  });
  $("simGhost").addEventListener("change", () => m.ghost && (m.ghost.visible = $("simGhost").checked));
  $("showRapids").addEventListener("change", () => {
    if ($("showRapids").checked !== view.rapids) toggle("rapids");
  });
  $("showPart").addEventListener("change", () => {
    if ($("showPart").checked !== view.part) toggle("part");
  });
  $("opLegend").addEventListener("input", (ev) => {
    const inp = ev.target.closest("input[type=color]");
    if (!inp) return;
    const tag = +inp.dataset.tag;
    m.colors[tag] = hexToRgb(inp.value);
    const o = m.session.ops.find((x) => x.tag === tag);
    const saved = savedColors();
    saved[opKey(o)] = inp.value;
    try {
      localStorage.setItem("marcam.opColors", JSON.stringify(saved));
    } catch {}
    // recolour every vertex from the tag mirror
    if (m.solid) {
      for (const t of m.tiles?.values() || []) {
        const col = t.geometry.attributes.color.array;
        const ops = t.userData.ops;
        for (let k = 0; k < col.length / 3; k++) col.set(colorOf(ops ? ops[k] : 0), 3 * k);
        t.geometry.attributes.color.needsUpdate = true;
      }
    } else if (m.stockMesh && m.xyz) {
      const col = m.stockMesh.geometry.attributes.color.array;
      for (let k = 0; k < m.dop.length; k++) col.set(colorOf(m.dop[k]), 3 * k);
      m.stockMesh.geometry.attributes.color.needsUpdate = true;
    } else if (m.stockMesh) {
      const { na, nx } = m.dgrid;
      const col = m.stockMesh.geometry.attributes.color.array;
      const paint = (vi, k) => col.set(colorOf(m.dop[k]), 3 * vi);
      for (let k = 0; k < nx * na; k++) paint(k, k);
      // (the ends: the stock's own colour)
      const u = uncut();
      for (let j = 0; j < na; j++) {
        col.set(u, 3 * (nx * na + j));
        col.set(u, 3 * (nx * na + na + 1 + j));
      }
      m.stockMesh.geometry.attributes.color.needsUpdate = true;
    }
  });

  /** The engine's worker was ended (a cancel): its playback is gone, so this one closes. */
  function lost(why) {
    // a newer plan (the playback being made for the last one stopped): what is shown stays
    // until the new playback replaces it, at the same place
    // (and one the person just unfolded, being opened again with the stock: closed here, its
    // controls folded and the playback after the plan came folded; ▶ pressed, nothing to see)
    if (why === "replanning" && (m.open || m.expanded)) {
      if (m.session) m.keepAt = m.busy && m.wanted != null ? m.wanted : m.pos;
      m.session = null;
      // (the play button as it is: nothing to play until the new playback; left enabled, a
      // press did nothing)
      renderToggles();
      return;
    }
    if (m.session || m.open) close();
    m.session = null;
    renderToggles();
  }

  /** The playback's copies of the part (no stock: the part turns under the tool) where the
   *  part is: (Mar's knob) dragged off the axis, the part stayed drawn on it, the copies made
   *  when the playback was; its plan failing, for good. Placed as the part each frame, made
   *  again when its meshes are new (cheap: a compare while nothing moved). */
  function syncPartCopies(v) {
    const work = v.groups.work, part = v.groups.part;
    if (!part || m.swapping) return;
    const copies = work.children.filter((c) => c.userData.src);
    if (!copies.length) return;
    const srcs = part.children;
    if (copies.length !== srcs.length || copies.some((k, i) => k.userData.src !== srcs[i])) {
      // (the part's meshes made anew: copies of them; the old ones' geometry is the old
      // part's, not disposed here)
      for (const k of copies) work.remove(k);
      for (const c of srcs) {
        const k = c.clone(true);
        k.traverse((x) => (x.userData.shared = true));
        k.userData.src = c;
        work.add(k);
      }
      applyLayers();
      return;
    }
    for (const k of copies) {
      const c = k.userData.src;
      if (!k.position.equals(c.position) || !k.quaternion.equals(c.quaternion) || !k.scale.equals(c.scale)) {
        k.position.copy(c.position);
        k.quaternion.copy(c.quaternion);
        k.scale.copy(c.scale);
      }
    }
  }

  /** Picking on the model, a new operation's preview, placing tabs: they are drawn in the
   *  part's own frame, so the work is shown at A0 (and the tool hidden) meanwhile. Called
   *  every frame (cheap). */
  function syncFrame() {
    const v = state.three;
    // the vise's jaws go with the part and the stock, a playback open or not
    if (v?.groups?.jaws) v.groups.jaws.visible = view.part || view.stock;
    if (!m.open || !v || v.failed || !v.groups.work) return;
    syncPartCopies(v);
    // (a test's chosen layers, View hooks: they stand until it clears them)
    if (state.viewShow) {
      if (v.groups.machining) v.groups.machining.visible = false;
      return;
    }
    const t = state.tree;
    const editing = !!(t && (t.picking || t.draft || t.tabMode));
    // the part and stock as they are (the playback steps aside), and back after (each
    // frame: other views set these too; while a new playback is built as well)
    const was = m.editing;
    m.editing = editing;
    showMachining(!editing);
    // (the part stands in for the simulated stock while that steps aside: as the toggles say)
    if (was !== editing) applyLayers();
    // (a new playback being built: the old one shown as it is meanwhile)
    if (m.swapping || !m.session) return;
    // at rest the part stands at A0 (the whole toolpath drawn on it); playing, it turns
    if (!editing) v.groups.work.rotation.x = m.expanded ? m.angle || 0 : 0;
    if (v.groups.toolModel) v.groups.toolModel.visible = !editing && !!m.expanded && view.tool;
    if (v.groups.jaws) v.groups.jaws.visible = view.part || view.stock;
    // the played/coming distinction ends a moment after the last scrub
    if (m.moving !== moving()) showRange();
    // the selection changed: its toolpath stands out
    const sel = `${t?.selected || ""}|${view.only}`;
    if (sel !== m.selKey) {
      m.selKey = sel;
      applySelection();
    }
    showFinalStock();
  }

  /** The colours changed (View > Colours): the open playback drawn again. */
  async function recolor() {
    if (!m.open || !m.session) return;
    m.toolNumber = null;
    // (the colours as the project has them now: an operation's swatch changed)
    renderLegend();
    await buildScene();
  }

  renderToggles();
  return { open, close, seek, jumpTo, renderSimProgress, reset, applyHidden, colorOf, lost, recolor, syncFrame, expand, collapse, toggle, renderToggles, state: m };
}
