// The rotate handle (Mar: "Bambu Studio-style"): three thin rings, red, green and blue,
// each with a grab handle. Dragging one hides the others and shows a protractor on it:
// ticks every 5° (longer every 15° and 45°), the spokes of where the drag started and where
// it is, a coloured arc between them and the angle in words ("Y: 35.00"). With the pointer
// on the ticks' band the angle snaps to 5°; inside the ring it is free. On release the turn
// is one edit (the caller's).

const COLORS = [0xff3b3b, 0x3bd16f, 0x3b8bff];
const DEG = Math.PI / 180;
/** The ring's radius on the screen (px), and the band of the ticks round it (radius ratio). */
const RING_PX = 105;
const BAND_IN = 0.9;

/**
 * The handle for view `v` (its THREE, camera, renderer, controls). `begin(k)` when a ring is
 * taken (false: not now), `change(k, deg)` as it turns, `end(k, deg)` on release; `readout`
 * the element the angle is written in.
 */
export function makeProtractor(v, { begin, change, end, readout, busy = () => false }) {
  const THREE = v.THREE;
  const root = new THREE.Group();
  root.name = "protractor";
  root.renderOrder = 999;
  // each ring in its own plane: X's in YZ, Y's in ZX, Z's in XY (its 0° along the next axis)
  const planes = [
    new THREE.Matrix4().makeBasis(new THREE.Vector3(0, 1, 0), new THREE.Vector3(0, 0, 1), new THREE.Vector3(1, 0, 0)),
    new THREE.Matrix4().makeBasis(new THREE.Vector3(0, 0, 1), new THREE.Vector3(1, 0, 0), new THREE.Vector3(0, 1, 0)),
    new THREE.Matrix4().makeBasis(new THREE.Vector3(1, 0, 0), new THREE.Vector3(0, 1, 0), new THREE.Vector3(0, 0, 1)),
  ];
  const line = (pts, color, opacity = 1) => {
    const geo = new THREE.BufferGeometry().setFromPoints(pts);
    const m = new THREE.LineBasicMaterial({ color, depthTest: false, depthWrite: false, transparent: true, opacity });
    const o = new THREE.Line(geo, m);
    o.renderOrder = 999;
    return o;
  };
  const circle = (r, n = 128) => Array.from({ length: n + 1 }, (_, i) => new THREE.Vector3(r * Math.cos((i / n) * 2 * Math.PI), r * Math.sin((i / n) * 2 * Math.PI), 0));
  const rings = planes.map((M, k) => {
    const g = new THREE.Group();
    g.matrixAutoUpdate = false;
    g.matrix.copy(M);
    g.add(line(circle(1), COLORS[k]));
    // its grab handle, 45° round
    const knob = new THREE.Mesh(new THREE.SphereGeometry(0.055, 12, 8), new THREE.MeshBasicMaterial({ color: COLORS[k], depthTest: false, depthWrite: false, transparent: true }));
    knob.position.set(Math.cos(45 * DEG), Math.sin(45 * DEG), 0);
    knob.renderOrder = 999;
    g.add(knob);
    // what the pointer takes: a band round the ring (not drawn)
    const pick = new THREE.Mesh(new THREE.TorusGeometry(1, 0.09, 6, 96), new THREE.MeshBasicMaterial({ visible: false }));
    pick.userData.axis = k;
    g.add(pick);
    root.add(g);
    return { g, pick, shown: true };
  });
  // the protractor of the ring being turned (made at the start of a drag)
  let dial = null;

  /** World units per screen pixel where the handle is. */
  function unitPerPx() {
    const cam = v.camera;
    const h = v.renderer.domElement.clientHeight || 1;
    if (cam.isOrthographicCamera) return (cam.top - cam.bottom) / cam.zoom / h;
    const d = cam.position.distanceTo(root.getWorldPosition(new THREE.Vector3()));
    return (2 * Math.tan((cam.fov * DEG) / 2) * d) / h;
  }
  const radius = () => RING_PX * unitPerPx();
  root.onBeforeRender = () => {};
  // (kept the same size on the screen, whatever the zoom)
  const sizer = new THREE.Object3D();
  sizer.onBeforeRender = () => {};
  function resize() {
    const s = radius();
    root.scale.setScalar(s);
  }

  const api = {
    root,
    visible: false,
    enabled: false,
    dragging: false,
    showX: true,
    showY: true,
    showZ: true,
    /** (each frame) where the handle is, its rings shown as asked. */
    sync(position) {
      root.position.copy(position);
      root.visible = api.visible;
      const show = [api.showX, api.showY, api.showZ];
      rings.forEach((r, k) => (r.g.visible = show[k] && (!api.dragging || k === drag?.k)));
      resize();
      root.updateMatrixWorld(true);
    },
    /** (tests) Where a ring's point `ang` degrees round is on the screen, `rf` times its radius
     *  out from the middle. */
    screenAt(k, ang, rf = 1) {
      if (!api.visible) return null;
      resize();
      root.updateMatrixWorld(true);
      const p = new THREE.Vector3(rf * Math.cos(ang * DEG), rf * Math.sin(ang * DEG), 0).applyMatrix4(planes[k]).applyMatrix4(root.matrixWorld);
      const q = p.project(v.camera);
      const r = v.renderer.domElement.getBoundingClientRect();
      return [r.left + ((q.x + 1) / 2) * r.width, r.top + ((1 - q.y) / 2) * r.height];
    },
  };

  // ---- the drag
  let drag = null;
  const ray = new THREE.Raycaster();
  function rayAt(ev) {
    const r = v.renderer.domElement.getBoundingClientRect();
    ray.setFromCamera(new THREE.Vector2(((ev.clientX - r.left) / r.width) * 2 - 1, -((ev.clientY - r.top) / r.height) * 2 + 1), v.camera);
    return ray;
  }
  /** The pointer in ring `k`'s plane: [angle (deg), radius ratio], or null. */
  function inPlane(ev, k) {
    const rr = rayAt(ev);
    const M = new THREE.Matrix4().multiplyMatrices(root.matrixWorld, planes[k]);
    const n = new THREE.Vector3(0, 0, 1).transformDirection(M);
    const c = new THREE.Vector3().setFromMatrixPosition(root.matrixWorld);
    const plane = new THREE.Plane().setFromNormalAndCoplanarPoint(n, c);
    const p = rr.ray.intersectPlane(plane, new THREE.Vector3());
    if (!p) return null;
    const local = p.applyMatrix4(M.clone().invert());
    return [Math.atan2(local.y, local.x) / DEG, Math.hypot(local.x, local.y)];
  }

  function buildDial(k, a0) {
    const g = new THREE.Group();
    g.matrixAutoUpdate = false;
    g.matrix.copy(planes[k]);
    const segs = [];
    for (let a = 0; a < 360; a += 5) {
      const len = a % 45 === 0 ? 0.24 : a % 15 === 0 ? 0.16 : 0.09;
      const c = Math.cos(a * DEG), s = Math.sin(a * DEG);
      segs.push(new THREE.Vector3(c * 1.02, s * 1.02, 0), new THREE.Vector3(c * (1.02 + len), s * (1.02 + len), 0));
    }
    const ticks = new THREE.LineSegments(new THREE.BufferGeometry().setFromPoints(segs), new THREE.LineBasicMaterial({ color: 0xe6e6e6, depthTest: false, depthWrite: false, transparent: true, opacity: 0.85 }));
    ticks.renderOrder = 999;
    g.add(ticks);
    const spoke0 = line([new THREE.Vector3(0, 0, 0), new THREE.Vector3(Math.cos(a0 * DEG), Math.sin(a0 * DEG), 0)], 0xffffff, 0.8);
    const spoke1 = line([new THREE.Vector3(0, 0, 0), new THREE.Vector3(Math.cos(a0 * DEG), Math.sin(a0 * DEG), 0)], COLORS[k]);
    const arc = line([new THREE.Vector3(), new THREE.Vector3()], COLORS[k]);
    g.add(spoke0, spoke1, arc);
    root.add(g);
    return { g, spoke1, arc };
  }
  function drawDial(a0, deg) {
    if (!dial) return;
    const a1 = a0 + deg;
    dial.spoke1.geometry.setFromPoints([new THREE.Vector3(0, 0, 0), new THREE.Vector3(Math.cos(a1 * DEG), Math.sin(a1 * DEG), 0)]);
    const n = Math.max(2, Math.ceil(Math.abs(deg) / 2));
    const pts = Array.from({ length: n + 1 }, (_, i) => {
      const a = (a0 + (deg * i) / n) * DEG;
      return new THREE.Vector3(Math.cos(a) * 0.98, Math.sin(a) * 0.98, 0);
    });
    dial.arc.geometry.setFromPoints(pts);
  }

  const el = v.renderer.domElement;
  el.addEventListener(
    "pointerdown",
    (ev) => {
      if (!api.visible || !api.enabled || ev.button !== 0) return;
      root.updateMatrixWorld(true);
      const hits = rayAt(ev).intersectObjects(rings.filter((r) => r.g.visible).map((r) => r.pick), false);
      if (!hits.length) return;
      const k = hits[0].object.userData.axis;
      const at = inPlane(ev, k);
      if (!at) return;
      // (an arrow under the pointer takes the press: an arrow and a ring can cross)
      if (busy()) return;
      if (begin(k) === false) return;
      // (the ring takes the press: not the arrows, not the orbit)
      ev.stopImmediatePropagation();
      ev.preventDefault();
      el.setPointerCapture?.(ev.pointerId);
      drag = { k, a0: at[0], last: at[0], turned: 0, deg: 0 };
      api.dragging = true;
      v.controls.enabled = false;
      dial = buildDial(k, at[0]);
      api.sync(root.position);
    },
    true,
  );
  el.addEventListener("pointermove", (ev) => {
    if (!drag) return;
    const at = inPlane(ev, drag.k);
    if (!at) return;
    // (round and round: the turn kept as it goes)
    let step = at[0] - drag.last;
    if (step > 180) step -= 360;
    if (step < -180) step += 360;
    drag.turned += step;
    drag.last = at[0];
    // on the ticks' band (or past it): to 5°; inside the ring: free
    const deg = at[1] >= BAND_IN ? Math.round(drag.turned / 5) * 5 : Math.round(drag.turned * 100) / 100;
    drag.deg = deg;
    drawDial(drag.a0, deg);
    readout.textContent = `${"XYZ"[drag.k]}: ${deg.toFixed(2)}`;
    const r = el.getBoundingClientRect();
    readout.style.left = `${ev.clientX - r.left + 16}px`;
    readout.style.top = `${ev.clientY - r.top - 30}px`;
    readout.classList.remove("hidden");
    change(drag.k, deg);
    window.__marcam?.kickView?.();
  });
  const stop = (ev) => {
    if (!drag) return;
    const { k, deg } = drag;
    drag = null;
    api.dragging = false;
    v.controls.enabled = true;
    el.releasePointerCapture?.(ev.pointerId);
    if (dial) root.remove(dial.g);
    dial = null;
    readout.classList.add("hidden");
    end(k, deg);
  };
  el.addEventListener("pointerup", stop);
  el.addEventListener("pointercancel", stop);
  return api;
}
