// (Mar) Handles in the 3D view to move the part in its stock, or a carried piece (the cut
// result of an earlier setup, its part with it) in the vise or the chuck: arrows along X, Y
// and Z and rings about them, shown while the setup's Stock or Part is selected in the tree.
// Moves snap to 0.1 mm and turns to 90° (Shift: 1°), with the value shown beside the handle;
// each drag is one edit (Undo takes it back), planned again as any edit is. On the rotary
// axis only the turn about X and the slide along X apply.

import { kindOf, rotation } from "./project.js";
import { carriedFrom, meshModelOf, rangesFollow } from "./stockpane.js";
import { makeProtractor } from "./protractor.js";

const r3 = (x) => Math.round(x * 1000) / 1000;
const DEG = Math.PI / 180;

/** The engine's rotation (degrees about X, then Y, then Z) as a 3x3 matrix (rows). */
export function matOf(rot) {
  const f = rotation(rot) || ((p) => p);
  const c = [f([1, 0, 0]), f([0, 1, 0]), f([0, 0, 1])];
  return [0, 1, 2].map((i) => [c[0][i], c[1][i], c[2][i]]);
}
export const mul = (a, b) => [0, 1, 2].map((i) => [0, 1, 2].map((j) => a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]));
export const apply = (m, p) => [0, 1, 2].map((i) => m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2]);

/** The angles (degrees about X, then Y, then Z) of `m` (as the engine's carry_mesh::euler):
 *  quarter turns snapped exact. */
export function eulerOf(m) {
  const sy = Math.max(-1, Math.min(1, -m[2][0]));
  let x, y, z;
  if (Math.abs(sy) < 1 - 1e-12) {
    y = Math.asin(sy);
    x = Math.atan2(m[2][1], m[2][2]);
    z = Math.atan2(m[1][0], m[0][0]);
  } else {
    y = sy > 0 ? Math.PI / 2 : -Math.PI / 2;
    x = sy > 0 ? Math.atan2(m[0][1], m[1][1]) : Math.atan2(-m[0][1], m[1][1]);
    z = 0;
  }
  return [x, y, z].map((r) => {
    const d = r / DEG;
    const q = Math.round(d / 90) * 90;
    let v = Math.abs(d - q) < 1e-4 ? q : Math.round(d * 1e4) / 1e4;
    if (v === -180) v = 180;
    return v === 0 ? 0 : v;
  });
}

/** A turn of `deg` about axis k (0 X, 1 Y, 2 Z) as a matrix. */
function axisTurn(k, deg) {
  const r = [0, 0, 0];
  r[k] = deg;
  return matOf(r);
}

const TURNS = { none: [1, 1, 1], x180: [1, -1, -1], y180: [-1, 1, -1], z180: [-1, -1, 1] };

/**
 * Setup `s` of `doc` moved by `d` (mm) and/or turned by `deg` about axis `k` round `pivot`
 * (in place). Its part (a mesh model, in its stock), or a carried piece (the stock and its
 * part together, in the machine). Returns a label, or null.
 */
export function moveSetup(doc, s, { d = [0, 0, 0], k = null, deg = 0, pivot = [0, 0, 0] }) {
  const D = k != null && deg ? axisTurn(k, deg) : null;
  if (!D && d.every((v) => !v)) return null;
  const what = D ? `turned ${r3(deg)}° about ${"XYZ"[k]}` : `moved ${d.map(r3).join(", ")} mm`;
  if (carriedFrom(doc, s)) {
    const st = s.stock;
    if (d.some((v) => v)) {
      rangesFollow(s, r3(d[0]));
      const off = (st.offset || [0, 0, 0]).map((v, i) => r3(v + d[i]));
      if (off.some((v) => v)) st.offset = off;
      else delete st.offset;
    }
    if (D) {
      const sg = TURNS[st.turn || "none"] || TURNS.none;
      const T = [[sg[0], 0, 0], [0, sg[1], 0], [0, 0, sg[2]]];
      const M = mul(D, mul(T, matOf(st.rotate)));
      // (a turn over or end for end: kept as one, exactly as before; any other way, angles)
      const diag = M.every((row, i) => row.every((v, j) => (i === j ? Math.abs(Math.abs(v) - 1) < 1e-9 : Math.abs(v) < 1e-9)));
      const name = diag && Object.entries(TURNS).find(([, g]) => g.every((v, i) => Math.abs(M[i][i] - v) < 1e-9))?.[0];
      delete st.rotate;
      delete st.turn;
      if (name && name !== "none") st.turn = name;
      else if (!name) st.rotate = eulerOf(M);
    }
    return `Setup "${s.name || s.id}": the stock ${what}.`;
  }
  const m = meshModelOf(doc, s);
  if (!m) return moveStock(s, d, k, deg, what);
  const t = m.translate || [0, 0, 0];
  rangesFollow(s, r3(d[0]));
  let t2 = t.map((v, i) => v + d[i]);
  if (D) {
    // P = R A(p) + t, turned about the pivot: D (P - c) + c
    const rel = apply(D, t.map((v, i) => v - pivot[i]));
    t2 = rel.map((v, i) => v + pivot[i] + d[i]);
    const rot = eulerOf(mul(D, matOf(m.rotate)));
    if (rot.some((v) => v)) m.rotate = rot;
    else delete m.rotate;
  }
  m.translate = t2.map(r3);
  return `Setup "${s.name || s.id}": the part ${what}.`;
}

/** (Mar) A setup with no part to move in its stock (a revolved profile, or none): the stock
 *  itself moved (a block, in the vise) or turned a quarter at a time (a block: its sides
 *  swapped; a bar in the vise: which way it lies; on the rotary axis: a bar's flats round
 *  its axis). */
function moveStock(s, d, k, deg, what) {
  const st = s.stock;
  if (!st) return null;
  const vise = kindOf(s) === "vise";
  const q = Math.round(deg / 90);
  if (st.shape === "box") {
    if (d.some((v) => v)) {
      st.min = st.min.map((v, i) => r3(v + d[i]));
      st.max = st.max.map((v, i) => r3(v + d[i]));
    }
    if (k != null && q % 2) {
      // (a quarter turn: the two sides across the axis swapped, its low corner and top kept)
      const [a, b] = [[1, 2], [0, 2], [0, 1]][k];
      const size = st.max.map((v, i) => v - st.min[i]);
      [size[a], size[b]] = [size[b], size[a]];
      st.max = [0, 1].map((i) => r3(st.min[i] + size[i])).concat(st.max[2]);
      st.min[2] = r3(st.max[2] - size[2]);
    }
    return `Setup "${s.name || s.id}": the stock ${what}.`;
  }
  if (k == null) return null;
  if (!vise) {
    // on the rotary axis: a bar's flats turned about it
    if (k !== 0 || !("angle" in st || ["square", "hex", "rect"].includes(st.shape))) return null;
    st.angle = r3((((st.angle || 0) + deg) % 360 + 360) % 360);
    return `Setup "${s.name || s.id}": the bar turned ${r3(deg)}° about its axis.`;
  }
  if (!(q % 2)) return null;
  // a bar in the vise: turned a quarter, it lies another way (along X, Y, or standing up)
  const along = s.holding?.along || "x";
  const turned = { x: ["x", "z", "y"], y: ["z", "y", "x"], z: ["y", "x", "z"] }[along][k];
  s.holding ||= { kind: "vise" };
  if (turned === "x") delete s.holding.along;
  else s.holding.along = turned;
  return `Setup "${s.name || s.id}": the bar along ${turned.toUpperCase()}.`;
}

/** The handles, in the view `state.three` (made once it is there). */
export function setupHandles({ state, edit }) {
  const h = { tc: null, rc: null, proxy: null, ghost: null, readout: null, start: null, key: null, shift: false };

  /** What the handles act on now: null (none shown), else { kind: "part" | "carried",
   *  rotary, setup }. */
  function target() {
    const t = state.tree;
    const v = state.three;
    // (Mar) a setup, its Stock or its Part selected (a setup selected or just added: its
    // handles on show, nothing to find first)
    if (!t?.doc || !v || v.failed || t.selected || t.picking || !(t.focus === "stock" || t.focus === "part" || t.focus === "setup" || !t.focus)) return null;
    const s = t.doc.setups?.find((x) => x.id === t.setup);
    if (!s) return null;
    const rotary = kindOf(s) !== "vise";
    if (carriedFrom(t.doc, s)) return { kind: "carried", rotary, setup: s };
    // (the Part selected: its part; the Stock: its part too, moved in it, unless there is
    // no part to move: then the stock itself)
    if (meshModelOf(t.doc, s)) return { kind: "part", rotary, setup: s };
    if (s.stock && s.stock.shape !== "from_previous") return { kind: "stock", rotary, setup: s };
    return null;
  }

  /** The box of what the handles move, in the view: the part drawn, or (carried) the stock. */
  function boxOf(kind) {
    const v = state.three;
    const THREE = v.THREE;
    const b = new THREE.Box3();
    const add = (g) => g?.children.forEach((c) => b.expandByObject(c));
    if (kind === "carried" || kind === "stock") add(v.groups.stock);
    if (kind !== "stock") add(v.groups.part);
    return b;
  }

  async function make() {
    const v = state.three;
    const { TransformControls } = await import("three/addons/controls/TransformControls.js");
    const THREE = v.THREE;
    if (!v.groups.handles) {
      v.groups.handles = new THREE.Group();
      v.scene.add(v.groups.handles);
    }
    h.proxy = new THREE.Object3D();
    v.groups.handles.add(h.proxy);
    const mk = (mode) => {
      const c = new TransformControls(v.camera, v.renderer.domElement);
      c.setMode(mode);
      c.setSpace("world");
      c.attach(h.proxy);
      c.addEventListener("dragging-changed", (e) => {
        v.controls.enabled = !e.value;
        if (e.value) begin(c);
        else end(c);
      });
      c.addEventListener("objectChange", () => during(c));
      v.groups.handles.add(c);
      return c;
    };
    h.tc = mk("translate");
    h.readout = document.createElement("div");
    h.readout.className = "handle-readout hidden";
    h.readout.id = "handleReadout";
    document.getElementById("view3d").appendChild(h.readout);
    // (Mar: Bambu Studio's rotate handle) the rings: on the ticks' band 5° at a time, inside
    // the ring free; one edit on release
    h.rc = makeProtractor(v, {
      readout: h.readout,
      busy: () => h.tc.axis != null || h.tc.dragging,
      begin: () => {
        if (h.start || h.tc.dragging) return false;
        begin({ mode: "rotate" });
        return !!h.start;
      },
      change: (k, deg) => {
        if (!h.start) return;
        h.custom = { k, deg };
        const ax = new THREE.Vector3(...[0, 1, 2].map((i) => (i === k ? 1 : 0)));
        h.proxy.quaternion.copy(h.start.quat).premultiply(new THREE.Quaternion().setFromAxisAngle(ax, deg * DEG));
        during();
      },
      end: async () => {
        await end();
        h.custom = null;
      },
    });
    v.groups.handles.add(h.rc.root);
    // (kept on what it turns, every frame)
    const tick = () => {
      h.rc.sync(h.proxy.position);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    h.tc.setTranslationSnap(0.1);
  }

  function begin(c) {
    const v = state.three;
    const tg = target();
    if (!tg || h.start) return;
    h.start = { tg, pos: h.proxy.position.clone(), quat: h.proxy.quaternion.clone(), mode: c.mode };
    // a ghost of what moves, following the handle (the view itself is drawn again after)
    const THREE = v.THREE;
    h.ghost = new THREE.Group();
    const mat = new THREE.MeshBasicMaterial({ color: 0xffb020, transparent: true, opacity: 0.35, depthWrite: false });
    const src = [...(tg.kind !== "part" ? v.groups.stock.children : []), ...(tg.kind !== "stock" ? v.groups.part.children : [])];
    for (const o of src) o.traverse((x) => x.isMesh && h.ghost.add(Object.assign(new THREE.Mesh(x.geometry, mat), { matrixAutoUpdate: false, matrix: x.matrixWorld.clone() })));
    const holder = new THREE.Group();
    holder.add(h.ghost);
    h.ghost.position.copy(h.proxy.position).negate();
    h.holder = holder;
    v.groups.handles.add(holder);
    holder.position.copy(h.proxy.position);
  }

  /** The drag so far: { d, k, deg } snapped (a turn about one axis, or a move). */
  function delta() {
    const v = state.three;
    const THREE = v.THREE;
    const st = h.start;
    if (!st) return null;
    if (st.mode === "translate") {
      const d = h.proxy.position.clone().sub(st.pos).toArray().map((x) => Math.round(x * 100) / 100);
      // (on the rotary axis: a bar of its own slides along it only; a part, or a carried
      // piece, also across it, off the axis: Mar's radial handles)
      if (st.tg.rotary && st.tg.kind === "stock") (d[1] = 0), (d[2] = 0);
      return { d, k: null, deg: 0 };
    }
    // (the rings: their own angle, snapped or free as the pointer was)
    if (h.custom) return { d: [0, 0, 0], k: h.custom.k, deg: h.custom.deg };
    const q = st.quat.clone().invert().premultiply(h.proxy.quaternion);
    const e = new THREE.Euler().setFromQuaternion(q, "XYZ");
    const a = [e.x, e.y, e.z].map((r) => r / DEG);
    let k = a.map((x) => Math.abs(x)).indexOf(Math.max(...a.map((x) => Math.abs(x))));
    if (st.tg.rotary) k = 0;
    const deg = Math.round(a[k] * (h.shift ? 1 : 1e4)) / (h.shift ? 1 : 1e4);
    return { d: [0, 0, 0], k, deg };
  }

  function during() {
    if (!h.start) return;
    const dl = delta();
    h.holder.position.copy(h.proxy.position);
    h.holder.quaternion.copy(h.proxy.quaternion).multiply(h.start.quat.clone().invert());
    const v = state.three;
    const p = h.proxy.position.clone().project(v.camera);
    const r = v.renderer.domElement.getBoundingClientRect();
    // (the rings write their own: at the pointer, "Y: 35.00")
    if (h.custom) return void window.__marcam.kickView?.();
    h.readout.style.left = `${((p.x + 1) / 2) * r.width + 14}px`;
    h.readout.style.top = `${((1 - p.y) / 2) * r.height - 28}px`;
    h.readout.textContent = dl.k == null ? `X ${fmt(dl.d[0])}  Y ${fmt(dl.d[1])}  Z ${fmt(dl.d[2])} mm` : `${"XYZ"[dl.k]} ${fmt(dl.deg)}°`;
    h.readout.classList.remove("hidden");
    window.__marcam.kickView?.();
  }
  const fmt = (x) => `${x >= 0 ? "+" : ""}${+x.toFixed(2)}`;

  async function end() {
    const st = h.start;
    if (!st) return;
    const dl = delta();
    h.start = null;
    h.readout.classList.add("hidden");
    state.three.groups.handles.remove(h.holder);
    h.holder = null;
    h.ghost = null;
    const pivot = st.pos.toArray();
    // (on the rotary axis: turned about the axis itself)
    if (st.tg.rotary) pivot[1] = pivot[2] = 0;
    // back where it was until the edit lands (then placed on what moved)
    h.proxy.position.copy(st.pos);
    h.proxy.quaternion.copy(st.quat);
    if (dl && (dl.k != null ? Math.abs(dl.deg) > 1e-6 : dl.d.some((x) => x))) {
      h.key = null;
      await edit((doc) => {
        const s = doc.setups.find((x) => x.id === st.tg.setup.id);
        return s ? moveSetup(doc, s, { ...dl, pivot }) : null;
      });
    }
  }

  /** Shown on what they act on (or hidden): called after each draw of the view. */
  async function refresh() {
    const v = state.three;
    if (!v || v.failed) return;
    const tg = target();
    if (!tg) {
      if (h.tc) h.tc.visible = h.rc.visible = h.tc.enabled = h.rc.enabled = false;
      return;
    }
    // (made once: refreshes come together while three.js's module loads)
    if (!h.tc) {
      try {
        await (h.making ||= make());
      } catch (e) {
        // (three.js's handles could not be loaded: said, not silent)
        h.making = null;
        window.__marcam.showError?.(`The 3D handles could not be loaded (${e?.message || e}): type the values under the Stock or Part's More instead.`);
        return;
      }
    }
    if (h.start) return;
    const box = boxOf(tg.kind);
    if (box.isEmpty()) {
      h.tc.visible = h.rc.visible = h.tc.enabled = h.rc.enabled = false;
      return;
    }
    const c = box.getCenter(new v.THREE.Vector3());
    h.proxy.position.copy(c);
    h.proxy.quaternion.identity();
    h.tc.visible = h.rc.visible = h.tc.enabled = h.rc.enabled = true;
    // on the rotary axis: the slide along X and the turn about X only
    h.tc.showY = h.tc.showZ = !tg.rotary || tg.kind !== "stock";
    h.rc.showY = h.rc.showZ = !tg.rotary;
    h.rc.showX = true;
    window.__marcam.kickView?.();
  }

  /** (tests) Where a handle is on the screen: `mode` translate | rotate, `axis` X | Y | Z
   *  (a ring: the point `ang` degrees round it). */
  function screenOf(mode, axis, ang = 45, rf = 1) {
    const v = state.three;
    // (the rings: a point `ang` degrees round, `rf` times its radius out)
    if (mode === "rotate") return h.rc?.visible ? h.rc.screenAt("XYZ".indexOf(axis), ang, rf) : null;
    const c = h.tc;
    if (!c?.visible) return null;
    c.updateMatrixWorld(true);
    const THREE = v.THREE;
    let best = null;
    // its picker (the invisible shapes the pointer hits)
    const picker = c.children[0]?.picker?.[mode] || c._gizmo?.picker?.[mode];
    picker?.traverse((o) => {
      if (o.name !== axis || !o.geometry) return;
      const rr = o.geometry.parameters?.radius;
      if (mode === "rotate" && rr) {
        // a point on its ring, 45° round (on no other ring)
        best = new THREE.Vector3(rr * Math.cos(ang * DEG), rr * Math.sin(ang * DEG), 0).applyMatrix4(o.matrixWorld);
        return;
      }
      o.geometry.computeBoundingBox();
      best = o.geometry.boundingBox.clone().applyMatrix4(o.matrixWorld).getCenter(new THREE.Vector3());
    });
    if (!best) return null;
    const p = best.project(v.camera);
    const r = v.renderer.domElement.getBoundingClientRect();
    return [r.left + ((p.x + 1) / 2) * r.width, r.top + ((1 - p.y) / 2) * r.height];
  }

  return { refresh, target, screenOf, state: h };
}
