// Project workflow for the web app: New (a setup dialog: name, machine, setup type, part,
// stock), Open (also drag-and-drop), Save / Save as (one .marcam.json file with the part mesh
// embedded), Import STL (binary or ASCII, mm or inch, through the same dialog), editing a
// setup (its type, machine, stock, origin, holding: converting it when the type changes),
// adding a setup, unsaved-changes tracking and warnings, and an undo for setup changes.

import { setupDialog } from "./setupdialog.js";
import { revolveProfile } from "./meshes.js";

/** The file format Save writes: the project, and the mesh bytes it names, in one file. */
export const FILE_FORMAT = "marcam-project";
/** Project files saved before the rename (4cam -> marcam, `.4cam.json`) open too. */
export const LEGACY_FILE_FORMATS = ["4cam-project"];

/** The setup types, as the dialog and the tree name them. */
export const KINDS = {
  rotary: "Rotary 4th axis",
  indexed: "Indexed 3+1",
  vise: "3-axis vise",
};

const enc = new TextEncoder();
const dec = new TextDecoder();

// ------------------------------------------------------------------ STL

/** Triangles of an STL file (binary or ASCII), as 9 floats each. Throws on anything else. */
export function parseStl(bytes) {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  if (b.length >= 84) {
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
    const n = dv.getUint32(80, true);
    if (n > 0 && 84 + 50 * n === b.length) {
      const tris = new Float32Array(9 * n);
      for (let t = 0; t < n; t++) for (let k = 0; k < 9; k++) tris[9 * t + k] = dv.getFloat32(84 + 50 * t + 12 + 4 * k, true);
      return { tris, ascii: false };
    }
  }
  const head = dec.decode(b.subarray(0, Math.min(b.length, 512))).trimStart();
  if (head.startsWith("solid")) {
    const text = dec.decode(b);
    const vals = [];
    const re = /vertex\s+(\S+)\s+(\S+)\s+(\S+)/g;
    for (let m; (m = re.exec(text)); ) vals.push(+m[1], +m[2], +m[3]);
    if (vals.length && vals.length % 9 === 0 && vals.every(Number.isFinite)) return { tris: new Float32Array(vals), ascii: true };
    throw new Error("this ASCII STL has no complete triangles (facets of three vertices)");
  }
  throw new Error("not an STL file: neither a binary STL (its size does not match its triangle count) nor an ASCII one (it does not start with \"solid\")");
}

/**
 * A part file's triangles: an STL as it is, a STEP (.step/.stp) through the STEP module (fetched
 * the first time one is opened). A STEP also gives `step`: its own bytes (kept: the project saves
 * the STEP), the STL with its faces numbered (what the engine reads), and what was in it.
 */
export async function readPart(bytes, name = "") {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const { isStep, stepToStl } = await import("./step.js");
  if (!isStep(b, name)) return parseStl(b);
  const r = await stepToStl(b);
  return { ...parseStl(r.stl), ascii: false, step: { bytes: b, stl: r.stl, bodies: r.bodies, faces: r.faces, names: r.names, warnings: r.warnings } };
}

/** "binary STL, 1234 triangles" / "STEP, 2 bodies, 9 faces, 1234 triangles" */
export function partSummary(parsed) {
  const n = parsed.tris.length / 9;
  if (parsed.step) return `STEP, ${parsed.step.bodies} bod${parsed.step.bodies === 1 ? "y" : "ies"}, ${parsed.step.faces} faces, ${n} triangles`;
  return `${parsed.ascii ? "ASCII" : "binary"} STL, ${n} triangles`;
}

/** Binary STL bytes of triangles (9 floats each), with normals. */
export function encodeStl(tris, header = "marcam") {
  const n = tris.length / 9;
  const out = new Uint8Array(84 + 50 * n);
  out.set(enc.encode(header).subarray(0, 80));
  const dv = new DataView(out.buffer);
  dv.setUint32(80, n, true);
  for (let t = 0; t < n; t++) {
    const p = tris.subarray(9 * t, 9 * t + 9);
    const u = [p[3] - p[0], p[4] - p[1], p[5] - p[2]], v = [p[6] - p[0], p[7] - p[1], p[8] - p[2]];
    const nn = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    const len = Math.hypot(...nn) || 1;
    const o = 84 + 50 * t;
    for (let k = 0; k < 3; k++) dv.setFloat32(o + 4 * k, nn[k] / len, true);
    for (let k = 0; k < 9; k++) dv.setFloat32(o + 12 + 4 * k, p[k], true);
  }
  return out;
}

/** The engine's rotation (MeshSource.rotate): degrees about X, then Y, then Z; quarter turns
 *  exact (as the engine's). Returns a function of a point; null for none. */
export function rotation(rot) {
  if (!rot || rot.every((v) => !v)) return null;
  const sc = (deg) => {
    const q = deg / 90;
    if (Number.isInteger(q)) return [[0, 1], [1, 0], [0, -1], [-1, 0]][((q % 4) + 4) % 4];
    const r = (deg * Math.PI) / 180;
    return [Math.sin(r), Math.cos(r)];
  };
  const [[sx, cx], [sy, cy], [sz, cz]] = rot.map(sc);
  return (p) => {
    let q = [p[0], cx * p[1] - sx * p[2], sx * p[1] + cx * p[2]];
    q = [cy * q[0] + sy * q[2], q[1], -sy * q[0] + cy * q[2]];
    return [cz * q[0] - sz * q[1], sz * q[0] + cz * q[1], q[2]];
  };
}

/** The part's largest flat faces (after the axis mapping): their outward normals, the
 *  biggest first, for laying the part down on one (up to `n`). */
export function flatFaces(tris, axis, n = 6) {
  const by = new Map();
  for (let i = 0; i + 8 < tris.length; i += 9) {
    const a = mapAxis([tris[i], tris[i + 1], tris[i + 2]], axis);
    const b = mapAxis([tris[i + 3], tris[i + 4], tris[i + 5]], axis);
    const c = mapAxis([tris[i + 6], tris[i + 7], tris[i + 8]], axis);
    const u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]], v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    const nn = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    const l = Math.hypot(...nn);
    if (l < 1e-12) continue;
    const k = nn.map((x) => Math.round((x / l) * 200) / 200);
    const key = k.join(",");
    const e = by.get(key) || { n: k, area: 0 };
    e.area += l / 2;
    by.set(key, e);
  }
  return [...by.values()].sort((p, q) => q.area - p.area).slice(0, n).map((e) => {
    const l = Math.hypot(...e.n);
    return e.n.map((x) => x / l);
  });
}

/** The rotation [x, y, 0] (degrees) that turns the outward normal `nrm` straight down (the
 *  face it belongs to on the table or the vise's floor); near quarter turns made exact. */
export function layFlat(nrm) {
  const [nx, ny, nz] = nrm;
  const a = Math.atan2(ny, nz);
  const w = Math.hypot(ny, nz);
  const b = Math.PI - Math.atan2(nx, w);
  const snap = (r) => {
    let d = (r * 180) / Math.PI;
    d = ((d % 360) + 540) % 360 - 180;
    const q = Math.round(d / 90) * 90;
    return Math.abs(d - q) < 1e-4 ? (q === -180 ? 180 : q) : Math.round(d * 1e4) / 1e4;
  };
  return [snap(a), snap(b), 0];
}

/** The engine's axis mapping (MeshSource.axis): the mesh axis that becomes X. */
export function mapAxis(p, axis) {
  if (axis === "y") return [p[1], p[2], p[0]];
  if (axis === "z") return [p[2], p[0], p[1]];
  return [p[0], p[1], p[2]];
}

const ceilHalf = (x) => Math.ceil(x * 2) / 2;
const r3 = (x) => Math.round(x * 1000) / 1000;

/** The engine's axis mapping that makes `up` (x, y or z) the vise's Z. */
export const UP_AXIS = { z: "x", y: "z", x: "y" };

/** The stock shapes: a block in a vise, bars on the rotary axis. */
export const STOCK_SHAPES = {
  block: "Plate / block",
  round: "Round bar",
  square: "Square bar",
  rect: "Rectangular bar",
  hex: "Hex bar",
};

/** A bar section's corners and flats: { kind, sides | width/height, angle } -> radius at phi
 *  (from +Y towards +Z, as the engine's Section). */
export function sectionRadius(shape, d, phi) {
  const a = ((d.angle || 0) * Math.PI) / 180;
  if (shape === "rect") {
    const p = phi - a;
    const c = Math.abs(Math.cos(p)), s = Math.abs(Math.sin(p));
    return Math.min(c > 1e-12 ? d.width / 2 / c : Infinity, s > 1e-12 ? d.height / 2 / s : Infinity);
  }
  if (shape === "square" || shape === "hex") {
    const n = shape === "square" ? 4 : 6;
    const af = shape === "square" ? d.side : d.across_flats;
    const step = (2 * Math.PI) / n;
    let x = (((phi - Math.PI / 2 - a) % step) + step) % step;
    if (x > step / 2) x -= step;
    return af / 2 / Math.cos(x);
  }
  return d.radius;
}

/** The smallest bar of `shape` (turned by `angle`) round the part's section points (y, z about
 *  the axis), `margin` beyond it; sizes rounded up to 0.5 mm. */
export function fitBar(shape, pts, margin, angle) {
  const a = ((angle || 0) * Math.PI) / 180;
  if (shape === "round") {
    let r = 0;
    for (const [y, z] of pts) r = Math.max(r, Math.hypot(y, z));
    return { radius: ceilHalf(r + margin - 1e-4) };
  }
  if (shape === "rect") {
    let w = 0, h = 0;
    const c = Math.cos(a), sn = Math.sin(a);
    for (const [y, z] of pts) {
      w = Math.max(w, Math.abs(y * c + z * sn));
      h = Math.max(h, Math.abs(-y * sn + z * c));
    }
    return { width: ceilHalf(2 * (w + margin) - 1e-4), height: ceilHalf(2 * (h + margin) - 1e-4), angle: angle || 0 };
  }
  const n = shape === "square" ? 4 : 6;
  let apo = 0;
  for (let k = 0; k < n; k++) {
    const phi = Math.PI / 2 + a + (k * 2 * Math.PI) / n;
    const ny = Math.cos(phi), nz = Math.sin(phi);
    for (const [y, z] of pts) apo = Math.max(apo, y * ny + z * nz);
  }
  const af = ceilHalf(2 * (apo + margin) - 1e-4);
  return shape === "square" ? { side: af, angle: angle || 0 } : { across_flats: af, angle: angle || 0 };
}

/** A bar in a vise along `along` (x, y, or z: standing up; as the engine's
 *  Stock::lying_along): the block round it (X and Y from 0, its top at Z0) and where its axis
 *  is across it (along X: [Y, Z]; along Y: [X, Z]; standing: [X, Y]). */
export function lyingBar(st, along) {
  const b = lyingBarX(st);
  const w = b.box.max[1], h = -b.box.min[2];
  if (along === "y") return { axis: b.axis, box: { min: [0, 0, -h], max: [w, st.length, 0] }, along };
  if (along === "z") return { axis: [b.axis[0], h + b.axis[1]], box: { min: [0, 0, -st.length], max: [w, h, 0] }, along };
  return b;
}

/** A bar lying in a vise along X (as the engine's Stock::lying): its outline's extents and
 *  where its axis is (Y, Z) with its low Y side at 0 and its top at Z0. */
function lyingBarX(st) {
  const sp = stockSpecOf(st);
  let y0 = Infinity, y1 = -Infinity, z0 = Infinity, z1 = -Infinity;
  for (let k = 0; k < 1440; k++) {
    const phi = (k * 2 * Math.PI) / 1440;
    const r = sp.shape === "round" ? st.radius : sectionRadius(sp.shape, sp.dims, phi);
    const y = r * Math.cos(phi), z = r * Math.sin(phi);
    y0 = Math.min(y0, y);
    y1 = Math.max(y1, y);
    z0 = Math.min(z0, z);
    z1 = Math.max(z1, z);
  }
  return { axis: [-y0, -z1], box: { min: [0, 0, z0 - z1], max: [st.length, y1 - y0, 0] } };
}

/** The depth a vise's jaws grip (mm up from the stock's bottom): the setup's own, else 3 mm or
 *  a third of a block's height (the smaller), 40% of a bar's (as the engine's default). */
export function jawGrip(s) {
  const st = s?.stock;
  if (!st) return 0;
  if (s.holding?.jaws != null) return s.holding.jaws;
  if (st.shape === "box") return Math.min(3, (st.max[2] - st.min[2]) / 3);
  const b = lyingBar(st, s.holding?.along).box;
  const h = b.max[2] - b.min[2];
  // (standing up: as a block's)
  return s.holding?.along === "z" ? Math.min(3, h / 3) : 0.4 * h;
}

/** The project's stock for a given stock spec (no part to place: a block's sizes, a bar's). */
export function stockOfSpec(kind, spec) {
  if (spec.dims) {
    if (kind === "vise" || spec.shape === "block") return { shape: "box", min: [0, 0, -spec.dims.z], max: [spec.dims.x, spec.dims.y, 0] };
    return barStock(spec.shape, spec.dims, spec.length);
  }
  // (the older explicit sizes)
  return kind === "vise" ? { shape: "box", min: [0, 0, -spec.bz], max: [spec.bx, spec.by, 0] } : { shape: "cylinder", radius: spec.dia / 2, length: spec.len };
}

/** The project's stock for a bar of `shape` with `dims` and `length`. */
function barStock(shape, dims, length) {
  if (shape === "square") return { shape: "square", side: dims.side, length, angle: dims.angle || 0 };
  if (shape === "rect") return { shape: "rect", width: dims.width, height: dims.height, length, angle: dims.angle || 0 };
  if (shape === "hex") return { shape: "hex", across_flats: dims.across_flats, length, angle: dims.angle || 0 };
  return { shape: "cylinder", radius: dims.radius, length };
}

/** A bar's radius at its corners (a block: its height above the bottom). */
export function stockRadius(st) {
  if (!st) return 0;
  if (st.shape === "box") return st.max[2] - st.min[2];
  if (st.shape === "square") return (st.side / 2) * Math.SQRT2;
  if (st.shape === "rect") return Math.hypot(st.width / 2, st.height / 2);
  if (st.shape === "hex") return st.across_flats / Math.sqrt(3);
  return st.radius ?? 0;
}

/** The stock in a few words ("hex bar 22 mm across flats x 40 mm", "block 60 x 40 x 10 mm"). */
export function describeStock(st, chuck) {
  if (!st) return "stock from the previous setup";
  const f = (v) => +(+v).toFixed(3);
  const turned = st.angle ? ` turned ${f(st.angle)}°` : "";
  const len = st.length != null ? ` x ${f(st.length)} mm${chuck ? ` (${f(chuck)} in the chuck)` : ""}` : "";
  switch (st.shape) {
    case "box": return `block ${f(st.max[0] - st.min[0])} x ${f(st.max[1] - st.min[1])} x ${f(st.max[2] - st.min[2])} mm`;
    case "square": return `square bar ${f(st.side)} mm${turned}${len}`;
    case "rect": return `bar ${f(st.width)} x ${f(st.height)} mm${turned}${len}`;
    case "hex": return `hex bar ${f(st.across_flats)} mm across flats${turned}${len}`;
    case "from_previous": {
      const turn = { x180: ", turned over front to back", y180: ", turned over left to right", z180: ", turned end for end" }[st.turn] ?? "";
      const moved = st.offset?.some((v) => v) ? `, moved ${st.offset.map(f).join(", ")} mm` : "";
      return `stock from the previous setup${turn}${moved}`;
    }
    default: return st.radius ? `bar Ø${f(2 * st.radius)}${len}` : "stock from the previous setup";
  }
}

/** The stock's shape name (as the dialog's picker) and sizes, from the project's stock. */
export function stockSpecOf(stock) {
  if (!stock) return null;
  switch (stock.shape) {
    case "box":
      return { shape: "block", dims: { x: r3(stock.max[0] - stock.min[0]), y: r3(stock.max[1] - stock.min[1]), z: r3(stock.max[2] - stock.min[2]) } };
    case "square":
      return { shape: "square", dims: { side: stock.side, angle: stock.angle || 0 }, length: stock.length };
    case "rect":
      return { shape: "rect", dims: { width: stock.width, height: stock.height, angle: stock.angle || 0 }, length: stock.length };
    case "hex":
      return { shape: "hex", dims: { across_flats: stock.across_flats, angle: stock.angle || 0 }, length: stock.length };
    default:
      return { shape: "round", dims: { radius: stock.radius }, length: stock.length };
  }
}

/**
 * Where a part goes and the stock round it, for a setup `kind` (or the older `holding`).
 * - Bar ("rotary", "indexed"): `axis` (x, y or z) is the mesh axis turned onto X; the part is
 *   centred on it with its front (largest X) at X0 plus `origin`.
 * - Vise: `up` is the mesh axis that points up; the part's top `origin` (default 0.5 mm) below
 *   the block's top (Z0).
 * - `stock`: { shape: block | round | square | rect | hex, mode: "fit" | "given", ... }.
 *   Fit: a bar of the shape (turned by `angle`) `margin` (default 0.5 mm) round the part, its
 *   length the part's + `chuck` (default 12 mm) + 3 mm; a block `margins` { xl, xh, yl, yh }
 *   (default `margin`, 2 mm) beside the part and `bottom` (default 2 mm) below it. Given: the sizes in
 *   `dims` ({ radius } | { side } | { width, height } | { across_flats }, with `angle`;
 *   a block { x, y, z } with the part `at` [x, y] from its corner, else centred) and `length`.
 * - `explicit` (older): { dia, len } for a bar, { bx, by, bz } for a block.
 * Returns { axis, translate, stock, size } (size: the part's X, Y, Z extent after placing).
 */
export function placePart(tris, opts = {}) {
  const kind = opts.kind ?? (opts.holding === "vise" ? "vise" : "rotary");
  const vise = kind === "vise";
  const ax = vise ? UP_AXIS[opts.up ?? opts.axis ?? "z"] : opts.axis ?? "x";
  // (the part's orientation: after the axis mapping, as the engine turns it)
  const rot = rotation(opts.rotate);
  const map = rot ? (p) => rot(mapAxis(p, ax)) : (p) => mapAxis(p, ax);
  const lo = [Infinity, Infinity, Infinity], hi = [-Infinity, -Infinity, -Infinity];
  for (let i = 0; i < tris.length; i += 3) {
    const q = map([tris[i], tris[i + 1], tris[i + 2]]);
    for (let k = 0; k < 3; k++) {
      lo[k] = Math.min(lo[k], q[k]);
      hi[k] = Math.max(hi[k], q[k]);
    }
  }
  const size = [0, 1, 2].map((k) => hi[k] - lo[k]);
  // (the older explicit sizes, as a given stock)
  let spec = opts.stock;
  if (!spec && opts.explicit) {
    const ex = opts.explicit;
    spec = vise ? { shape: "block", mode: "given", dims: { x: ex.bx, y: ex.by, z: ex.bz } } : { shape: "round", mode: "given", dims: { radius: ex.dia / 2 }, length: ex.len };
  }
  if (vise && spec?.shape && spec.shape !== "block" && (opts.along === "y" || opts.along === "z")) {
    // (Mar) a bar along Y, or standing up: the part centred in its section; along Y between
    // its ends (the margin each side), standing its top `origin` below the bar's top
    const y = opts.along === "y";
    // the section's two directions (its Y, its Z) and the length's, in the setup's axes
    const [su, sv, sl] = y ? [0, 2, 1] : [0, 1, 2];
    const cu = (lo[su] + hi[su]) / 2, cv = (lo[sv] + hi[sv]) / 2;
    const mx = spec.margins?.xl ?? 2;
    const top = opts.origin ?? 0.5;
    let dims, length;
    if (spec.mode === "given") {
      dims = spec.dims;
      length = spec.length;
    } else {
      const pts = [];
      const step = Math.max(1, Math.floor(tris.length / 3 / 200000));
      for (let i = 0; i < tris.length; i += 3 * step) {
        const q = map([tris[i], tris[i + 1], tris[i + 2]]);
        pts.push([q[su] - cu, q[sv] - cv]);
      }
      dims = fitBar(spec.shape, pts, spec.margin ?? opts.margin ?? 0.5, spec.dims?.angle ?? 0);
      length = Math.ceil(size[sl] + (y ? 2 * mx : top + (spec.margins?.bottom ?? 2)));
    }
    const stock = barStock(spec.shape, dims, length);
    const { axis } = lyingBar(stock, opts.along);
    const translate = y
      ? [axis[0] - cu, Math.max(0, (length - size[1]) / 2) - lo[1], axis[1] - cv]
      : [axis[0] - cu, axis[1] - cv, -top - hi[2]];
    return { axis: ax, translate, stock, size };
  }
  if (vise && spec?.shape && spec.shape !== "block") {
    // a bar lying along X: the part centred in its section, its front at the bar's end plus
    // the X margin
    const cy = (lo[1] + hi[1]) / 2, cz = (lo[2] + hi[2]) / 2;
    const mx = spec.margins?.xl ?? 2;
    let dims, length;
    if (spec.mode === "given") {
      dims = spec.dims;
      length = spec.length;
    } else {
      const pts = [];
      const step = Math.max(1, Math.floor(tris.length / 3 / 200000));
      for (let i = 0; i < tris.length; i += 3 * step) {
        const q = map([tris[i], tris[i + 1], tris[i + 2]]);
        pts.push([q[1] - cy, q[2] - cz]);
      }
      dims = fitBar(spec.shape, pts, spec.margin ?? opts.margin ?? 0.5, spec.dims?.angle ?? 0);
      length = Math.ceil(size[0] + 2 * mx);
    }
    const stock = barStock(spec.shape, dims, length);
    const { axis } = lyingBar(stock);
    const translate = [Math.max(0, (length - size[0]) / 2) - lo[0], axis[0] - cy, axis[1] - cz];
    return { axis: ax, translate, stock, size };
  }
  if (vise) {
    const top = opts.origin ?? 0.5;
    if (spec?.mode === "given") {
      const d = spec.dims;
      const at = spec.at && spec.at.every((v) => Number.isFinite(v)) ? spec.at : [(d.x - size[0]) / 2, (d.y - size[1]) / 2];
      const translate = [at[0] - lo[0], at[1] - lo[1], -top - hi[2]];
      return { axis: ax, translate, stock: { shape: "box", min: [0, 0, -d.z], max: [d.x, d.y, 0] }, size };
    }
    const side = opts.margin ?? 2;
    const mg = { xl: side, xh: side, yl: side, yh: side, bottom: 2 };
    for (const [k, v] of Object.entries(spec?.margins || {})) if (Number.isFinite(v)) mg[k] = v;
    const translate = [mg.xl - lo[0], mg.yl - lo[1], -top - hi[2]];
    const stock = { shape: "box", min: [0, 0, r3(-(size[2] + top + mg.bottom))], max: [r3(size[0] + mg.xl + mg.xh), r3(size[1] + mg.yl + mg.yh), 0] };
    return { axis: ax, translate, stock, size };
  }
  const cy = (lo[1] + hi[1]) / 2, cz = (lo[2] + hi[2]) / 2;
  const shift = opts.origin ?? 0;
  const translate = [-hi[0] + shift, -cy, -cz];
  const shape = spec?.shape && spec.shape !== "block" ? spec.shape : "round";
  const chuck = spec?.chuck ?? 12;
  let dims, length;
  if (spec?.mode === "given") {
    dims = spec.dims;
    length = spec.length;
  } else {
    // the part's section about the axis (every vertex, thinned on a large mesh)
    const pts = [];
    const step = Math.max(1, Math.floor(tris.length / 3 / 200000));
    for (let i = 0; i < tris.length; i += 3 * step) {
      const q = map([tris[i], tris[i + 1], tris[i + 2]]);
      pts.push([q[1] - cy, q[2] - cz]);
    }
    dims = fitBar(shape, pts, spec?.margin ?? opts.margin ?? 0.5, spec?.dims?.angle ?? spec?.angle ?? 0);
    // the older default (the part + 15 mm) unless a chuck length is given
    length = spec?.chuck != null ? Math.ceil(size[0] + chuck + 3 + Math.max(0, shift)) : Math.ceil(size[0] + 15 + Math.max(0, shift));
  }
  return { axis: ax, rotate: opts.rotate, translate, stock: barStock(shape, dims, length), size, chuck: spec?.chuck };
}

/** The setup's kind: its own, else from its stock (a block: vise; a bar: rotary). */
export function kindOf(setup) {
  return setup?.kind ?? (setup?.stock?.shape === "box" ? "vise" : "rotary");
}

/**
 * Whether an operation type (from opTypes) fits a setup kind, as the engine's
 * ops::fits_setup: rotary takes what turns the part (and engrave/V-carve, wrapped), indexed the
 * 3-axis types (at fixed A angles), vise everything that needs no rotary axis. A bar without a
 * kind takes everything (older projects mix rotary and indexed operations).
 */
export function fitsKind(type, kind, explicit = true) {
  if (!explicit && kind === "rotary") return true;
  if (kind === "rotary") return !type.machine_needs.includes("xyz");
  if (kind === "indexed") return type.machine_needs.includes("xyz");
  if (kind === "vise") return !type.machine_needs.includes("rotary_a");
  return true;
}

/** The operations a new setup of each kind starts with: [type, tool, A, output]. */
export const STARTERS = {
  rotary: [["rotary_rough", 1, null, "rough"], ["rotary_finish", 2, null, "finish"]],
  indexed: [["rough3d", 1, 0, "rough"], ["rough3d", 1, 180, "rough"], ["raster3d", 2, 0, "finish"], ["raster3d", 2, 180, "finish"]],
  vise: [["rough3d", 1, null, "rough"], ["raster3d", 2, null, "finish"]],
};

// ------------------------------------------------------------------ files

function b64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}
function unb64(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** The saved file: the project and the mesh it names, embedded. */
export function projectFile(project, mesh, meshName, view) {
  const file = { format: FILE_FORMAT, version: 1, project };
  if (mesh) file.meshes = { [meshName]: b64(mesh) };
  // how it was shown (view only: which toolpaths were hidden)
  if (view?.hidden?.length) file.view = { hidden: view.hidden };
  return file;
}

/** Read a saved file, a 0.2 project or a 0.1 job: { project, mesh, meshName }. */
export function readProjectFile(text) {
  let obj;
  try {
    obj = JSON.parse(text);
  } catch (e) {
    throw new Error(`not a marcam project: the file is not JSON (${e.message})`);
  }
  if (obj && (obj.format === FILE_FORMAT || LEGACY_FILE_FORMATS.includes(obj.format))) {
    const [name, data] = Object.entries(obj.meshes || {})[0] || [];
    return { project: obj.project, mesh: data ? unb64(data) : null, meshName: name || "", view: obj.view || null };
  }
  if (obj && (obj.schema_version || obj.setups || obj.programs)) return { project: obj, mesh: null, meshName: "" };
  throw new Error("not a marcam project: no project, setups or programs in the file");
}

/** The mesh file a project names (its part's mesh model, or a 0.1 job's part.mesh). */
export function meshFileOf(project) {
  const m = project?.models?.find((x) => x.kind === "mesh" && (project.setups || []).some((s) => (s.part || []).includes(x.id)));
  return m?.file ?? project?.part?.mesh?.file ?? null;
}

const slug = (s) => (String(s).toLowerCase().replace(/\.[a-z0-9]+$/, "").replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "part");

// ------------------------------------------------------------------ the workflow

export function setupProject(ctx) {
  const { $, engine, state, readJob, writeJob, setMesh, showError, setStatus, generate, refreshAll, clearResults, resetProject, machineArg, currentMachine } = ctx;
  const p = { name: "untitled", dirty: false, fileName: null, pending: null };
  state.project = p;
  const dialog = setupDialog({ $, state, parseStl, readPart, partSummary, placePart, kindOf, KINDS, stockSpecOf, lyingBar, flatFaces, layFlat, UP_AXIS, rotation });

  function show() {
    $("projectName").textContent = p.name;
    $("projectDirty").classList.toggle("hidden", !p.dirty);
    $("projectDirty").title = p.dirty ? "unsaved changes" : "";
    document.title = `${p.dirty ? "* " : ""}${p.name} - marcam`;
  }
  /** Something in the project changed: unsaved, and (unless `stale: false`, a colour or a
   *  name) any results are out of date. Nothing is planned here (see auto-regenerate). */
  function changed({ stale = true, typing = false } = {}) {
    record({ typing });
    p.dirty = true;
    show();
    if (stale) ctx.markStale();
  }
  function clean(name) {
    if (name) p.name = name;
    p.dirty = false;
    show();
  }
  /** Ask before throwing away unsaved changes; true to go on. */
  function mayDiscard(what) {
    return !p.dirty || window.confirm(`"${p.name}" has unsaved changes. ${what} anyway and lose them?`);
  }
  const engineMachine = () => (typeof machineArg() === "string" && machineArg() !== "custom" ? machineArg() : null);

  // ------------------------------------------------------------ undo and redo
  // Every edit of the project (its text, and the part's mesh with it) is kept: Edit > Undo
  // (Ctrl+Z) goes back one, Redo (Ctrl+Shift+Z, Ctrl+Y) forward again. A snapshot is taken
  // as each change lands (typing in the JSON within a second is one step); what the view
  // shows (the eyes, the camera, a selection) is not an edit. Opening or starting a project
  // forgets the history.
  const TYPING_MS = 1000;
  const esc = (t) => String(t ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const h = { undo: [], redo: [], cur: null, loading: true, restoring: false, typingAt: 0, label: null };
  const snap = () => ({ text: $("job").value, mesh: state.mesh, meshName: state.meshName, meshSource: state.meshSource, extras: new Map(state.extraMeshes) });
  state.history = h;
  /** A project opened, made or reset: no history, this is where it starts. */
  function resetHistory() {
    Object.assign(h, { undo: [], redo: [], cur: snap(), loading: true, typingAt: 0, label: null });
    // (where it starts: what undoing everything comes back to)
    h.base = h.cur;
    renderHistory();
  }
  /** The mesh set while a project is being opened is part of where the history starts. */
  function meshSet() {
    if (h.loading && !h.restoring && h.cur) Object.assign(h.cur, { mesh: state.mesh, meshName: state.meshName, meshSource: state.meshSource, extras: new Map(state.extraMeshes) });
  }
  /** An edit is about to change the mesh (before the text): the snapshot keeps the old one. */
  function checkpoint() {
    h.loading = false;
  }
  function record({ typing = false } = {}) {
    if (h.restoring) return;
    if (!h.cur) h.cur = snap();
    h.loading = false;
    const now = snap();
    if (now.text === h.cur.text && now.mesh === h.cur.mesh) {
      // (nothing changed: the name was for nothing)
      h.label = null;
      return;
    }
    const t = performance.now();
    const merge = typing && h.typingAt && t - h.typingAt < TYPING_MS && h.undo.length && !h.redo.length;
    if (!merge) h.undo.push({ ...h.cur, label: h.label || (typing ? "Edited the JSON." : "") });
    h.label = null;
    h.typingAt = typing ? t : 0;
    h.redo = [];
    h.cur = now;
    renderHistory();
  }
  /** Name the change just made (or about to be): `job` is the project text from before it. */
  function keepUndo(label, job) {
    h.loading = false;
    const top = h.undo[h.undo.length - 1];
    if (job === h.cur?.text) h.label = label;
    else if (top && top.text === job) top.label = label;
    else {
      h.undo.push({ ...h.cur, text: job, label });
      h.redo = [];
    }
    $("undoBar").innerHTML = `${esc(label)} <button type="button" id="undoSetup">Undo</button>`;
    $("undoBar").classList.remove("hidden");
    renderHistory();
  }
  /** Name the edit about to be written (Edit > Undo says it). */
  function nameEdit(label) {
    h.label = label;
  }
  function dropUndo() {
    $("undoBar").classList.add("hidden");
  }
  function renderHistory() {
    const u = h.undo[h.undo.length - 1];
    const r = h.redo[h.redo.length - 1];
    const set = (id, on, what) => {
      const b = $(id);
      if (!b) return;
      b.disabled = !on;
      b.title = on ? what : "Nothing to " + id.slice(4).toLowerCase();
    };
    set("menuUndo", !!u, `Undo${u?.label ? `: ${u.label}` : " the last edit"} (Ctrl+Z)`);
    set("menuRedo", !!r, `Redo${r?.label ? `: ${r.label}` : " the edit undone"} (Ctrl+Shift+Z)`);
  }
  /** The project as it was in `e` (text and mesh), without recording it as an edit. */
  async function restore(e) {
    h.restoring = true;
    try {
      $("job").value = e.text;
      state.extraMeshes = new Map(e.extras || []);
      setMesh(e.mesh, e.meshName, e.meshSource);
      changed();
    } finally {
      h.restoring = false;
    }
    h.cur = snap();
    h.typingAt = 0;
    renderHistory();
    await refreshAll();
  }
  async function step(from, to, verb) {
    // (an edit not seen yet, such as typing: kept first)
    if (h.cur && $("job").value !== h.cur.text) record();
    const e = from.pop();
    if (!e) return false;
    to.push({ ...h.cur, label: e.label });
    dropUndo();
    await restore(e);
    const auto = $("autoGen")?.checked;
    setStatus(`${verb}${e.label ? `: ${e.label}` : "."}${auto ? "" : " Generate to see it again."}`, "ready");
    return true;
  }
  /** Back one edit (false: nothing to undo). */
  const undo = () => step(h.undo, h.redo, "Undone");
  /** Forward one undone edit. */
  const redo = () => step(h.redo, h.undo, "Redone");

  // ------------------------------------------------------------ new, open, save
  async function newProject() {
    if (!mayDiscard("Start a new project")) return false;
    // (Mar: no modal) a new, empty project at once: its name and setups made in the tree
    const cfg = await dialog.open("new", { name: "untitled", machine: currentMachine()?.id, kind: p.pending?.kind ?? "rotary" }, { auto: !settings.dialogs });
    if (!cfg || cfg.error) return false;
    await resetProject("New project: import an STL (button or drop it here) or open a project to start.");
    setStatus("New project", "ready");
    if (cfg.machine) ctx.selectMachine(cfg.machine);
    const doc = { schema_version: "0.2", name: cfg.name || "untitled", models: [], tools: [], setups: [] };
    writeJob(doc, { load: true });
    p.fileName = null;
    p.pending = cfg;
    clean(doc.name);
    await refreshAll();
    if (cfg.file) await importParsed(cfg);
    return true;
  }

  /** A bundle (.marcam): its project.toml, the part file it names, its view (view.json). */
  async function openBundle(bytes, fileName) {
    const { result } = await engine("bundleRead", bytes);
    const un = (b64s) => Uint8Array.from(atob(b64s), (c) => c.charCodeAt(0));
    const wanted = meshFileOf(result.project);
    const part = result.files.find((f) => f.name === wanted) ?? result.files.find((f) => /\.(stl|3mf|step|stp|png|pgm)$/i.test(f.name));
    const viewFile = result.files.find((f) => f.name === "view.json");
    let view = null;
    try {
      view = viewFile ? JSON.parse(new TextDecoder().decode(un(viewFile.bytes))) : null;
    } catch {}
    // the setups' own part files, besides the project's
    const others = new Set((result.project.models || []).filter((m) => m.kind === "mesh" && m.file && m.file !== part?.name).map((m) => m.file));
    const extras = result.files.filter((f) => others.has(f.name)).map((f) => [f.name, { bytes: un(f.bytes), name: f.name }]);
    await openParsed({ project: result.project, mesh: part ? un(part.bytes) : null, meshName: part?.name || "", view, extras }, fileName);
  }

  async function openText(text, fileName) {
    // TOML (a project as marcam writes it) or JSON (a .marcam.json, a project or a job)
    const t = text.replace(/^\uFEFF/, "").trimStart();
    if (!t.startsWith("{")) {
      const project = await window.__marcam.parseProjectText(text);
      return openParsed({ project, mesh: null, meshName: "", view: null }, fileName);
    }
    return openParsed(readProjectFile(text), fileName);
  }

  async function openParsed({ project, mesh, meshName, view, extras }, fileName) {
    const name = project.name || slug(fileName || "project");
    await resetProject("");
    if (extras?.length) state.extraMeshes = new Map(extras);
    writeJob(project, { load: true });
    const wanted = meshFileOf(project);
    // a STEP part: converted again (the same faces, the same ids)
    let part = null;
    if (mesh) {
      try {
        const { isStep } = await import("./step.js");
        if (isStep(mesh, meshName)) {
          setStatus(`Reading ${meshName} (STEP)...`, "busy");
          part = await readPart(mesh, meshName);
        }
      } catch (e) {
        showError(`${meshName}: ${e.message}`);
      }
    }
    if (part?.step) setMesh(part.step.stl, `${meshName} (in the project; ${partSummary(part)})`, { name: meshName, bytes: mesh });
    else setMesh(mesh, mesh ? `${meshName} (in the project)` : "");
    p.fileName = fileName || null;
    p.pending = null;
    clean(name);
    const sm = project.machine ?? project.setups?.[0]?.machine;
    if (sm) ctx.selectMachine(sm);
    if (wanted && !mesh) {
      clearResults("");
      await refreshAll();
      setStatus("Opened", "ready");
      showError(`This project uses the mesh "${wanted}", which is not in the file: import it (Import STL) to plan.`);
      return;
    }
    // what was hidden: the file's view, else this browser's memory of the project
    let hidden = view?.hidden;
    if (!hidden) {
      try {
        hidden = JSON.parse(localStorage.getItem(window.__marcam.hiddenStoreKey()) || "null");
      } catch {
        hidden = null;
      }
    }
    window.__marcam.setHidden(hidden || [], { persist: !!view });
    // opened, not planned: Generate when ready (it can take a while)
    clearResults(`Opened ${name}. Generate to plan its toolpaths.`, ["generate"]);
    await refreshAll();
    setStatus(`Opened ${name}`, "ready");
  }

  async function openFile(file) {
    if (!mayDiscard("Open another project")) return;
    showError("");
    setStatus(`Opening ${file.name}...`, "busy");
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      // a bundle: a zip (project.toml and its part files)
      if (bytes[0] === 0x50 && bytes[1] === 0x4b) await openBundle(bytes, file.name);
      else await openText(new TextDecoder().decode(bytes), file.name);
    } catch (e) {
      showError(`${file.name}: ${e.message}`);
      setStatus("Open failed", "error");
    }
  }

  async function save(asNew) {
    showError("");
    let project;
    try {
      project = readJob();
    } catch (e) {
      return showError(e.message);
    }
    let name = p.name;
    if (asNew || !p.fileName) {
      const asked = window.prompt("Save the project as:", p.fileName && /\.marcam$/i.test(p.fileName) ? p.fileName : `${slug(p.name)}.marcam`);
      if (!asked) return;
      // one file: the project (TOML) and its part, zipped (.marcam)
      p.fileName = /\.marcam$/i.test(asked) ? asked : `${asked.replace(/(\.marcam)?\.(json|toml)$/i, "")}.marcam`;
      name = p.fileName.replace(/\.marcam$/i, "");
      if (project.schema_version === "0.2") project.name = name;
      writeJob(project, { load: true });
    }
    const src = state.meshSource && state.meshSource.name === meshFileOf(project) ? state.meshSource : null;
    const files = [];
    const partBytes = src ? src.bytes : state.mesh;
    if (partBytes) files.push({ name: meshFileOf(project) || state.meshName.split(" ")[0] || "part.stl", bytes: partBytes });
    // (and the setups' own part files)
    for (const m of project.models || []) {
      const e = m.kind === "mesh" && state.extraMeshes.get(m.file);
      if (e && !files.some((f) => f.name === m.file)) files.push({ name: m.file, bytes: e.bytes });
    }
    // which toolpaths are hidden (the view): beside the project
    if (state.hidden?.size) files.push({ name: "view.json", bytes: new TextEncoder().encode(JSON.stringify({ hidden: [...state.hidden] })) });
    let zip;
    try {
      zip = (await engine("bundleWrite", project, files)).result.bytes;
    } catch (e) {
      return showError(`Save: ${e.message}`);
    }
    const blob = new Blob([Uint8Array.from(atob(zip), (c) => c.charCodeAt(0))], { type: "application/zip" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = p.fileName;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    clean(name);
    setStatus(`Saved ${p.fileName}`, "ready");
  }

  // ------------------------------------------------------------ setups
  /** The tools starter operations use: T1 flat and T2 ball, added when missing. */
  function ensureTools(doc, stock) {
    const reach = stockRadius(stock);
    const flute = Math.max(12, Math.ceil(reach + 1));
    doc.tools = doc.tools || [];
    // the starter tools (marcam's own, by their description) follow the stock's depth, so the
    // holder clears it; tools the user defined are left alone (the holder check reports them)
    for (const t of doc.tools) {
      const m = /^3\.175 mm (flat|ball) end mill, \d+ mm flute$/.exec(t.description ?? "");
      if (m && t.kind === m[1] && t.flute_length !== flute) {
        t.flute_length = flute;
        t.description = `3.175 mm ${m[1]} end mill, ${flute} mm flute`;
      }
    }
    if (!doc.tools.some((t) => t.number === 1))
      doc.tools.push({ number: 1, description: `3.175 mm flat end mill, ${flute} mm flute`, kind: "flat", diameter: 3.175, flute_length: flute });
    if (!doc.tools.some((t) => t.number === 2))
      doc.tools.push({ number: 2, description: `3.175 mm ball end mill, ${flute} mm flute`, kind: "ball", diameter: 3.175, flute_length: flute });
  }

  const nextOpId = (doc) => {
    const used = new Set(doc.setups.flatMap((s) => s.operations.map((o) => o.id)));
    for (let k = 1; ; k++) if (!used.has(`op${k}`)) return `op${k}`;
  };

  /** Starter operations are added only when asked for (Mar: a new project starts with none;
   *  the person adds them): `settings.starters`, or "marcam.starters" = "1" in the browser's
   *  storage (the tests that import parts and plan them ask for them that way). */
  const settings = {
    starters: (() => { try { return localStorage.getItem("marcam.starters") === "1"; } catch { return false; } })(),
    // (the older setup dialogs, for the tests written with them: "marcam.dialogs" = "1"; a
    // person never sees them, Mar)
    dialogs: (() => { try { return localStorage.getItem("marcam.dialogs") === "1"; } catch { return false; } })(),
  };

  /** A setup's starter operations (roughing, then finishing), every field from the engine. */
  async function addStarters(doc, s) {
    const base = `${slug(doc.name)}${doc.setups.length > 1 ? `_${slug(s.id)}` : ""}`;
    const outs = {};
    const added = [];
    // (a setup with no model, or none of the kind a type works from: not that type)
    const types = state.tree?.types?.length ? state.tree.types : (await engine("opTypes")).result;
    const has = (k) => s.part.some((id) => doc.models.find((x) => x.id === id)?.kind === k);
    const fits = (type) => {
      const g = types.find((t) => t.kind === type)?.geometry || [];
      const need = g.filter((k) => k === "mesh" || k === "profile");
      return !need.length || need.some(has);
    };
    for (const [type, tool, a] of STARTERS[kindOf(s)].map((x) => x.slice(0, 3)).filter(([type]) => fits(type))) {
      const what = STARTERS[kindOf(s)].find((x) => x[0] === type)[3];
      const { result } = await engine("opDefaults", doc, { type, setup: s.id, tool, machine: engineMachine() }, state.mesh);
      const op = { id: nextOpId(doc), ...result };
      if (a !== null) {
        op.index_a = a;
        op.name = `${type === "rough3d" ? "Rough" : "Finish"} at A${a}`;
      }
      s.operations.push(op);
      added.push(`${op.id} (${type}${a !== null ? ` at A${a}` : ""})`);
      if (!outs[what]) {
        outs[what] = { name: `${base}_${Object.keys(outs).length + 1}_${what}`, title: `${doc.name}: ${what}`, operations: [] };
        s.outputs.push(outs[what]);
      }
      outs[what].operations.push(op.id);
    }
    return added;
  }

  /** The mesh model a setup uses, its own: one shared with another setup is copied first. */
  function ownModel(doc, s) {
    const id = s.part.find((m) => doc.models.find((x) => x.id === m)?.kind === "mesh");
    if (!id) return null;
    const shared = doc.setups.some((o) => o !== s && o.part.includes(id));
    if (!shared) return doc.models.find((x) => x.id === id);
    const copy = { ...doc.models.find((x) => x.id === id), id: `${s.id}_part` };
    doc.models = doc.models.filter((x) => x.id !== copy.id).concat(copy);
    s.part = s.part.map((m) => (m === id ? copy.id : m));
    return copy;
  }

  /**
   * Apply a setup configuration from the dialog to setup `s`: kind, machine, holding, the
   * part placed again and the stock sized (when there is a mesh), the operations that do not
   * fit the kind removed and the rest given the engine's defaults for it again, starters added
   * when none are left. Returns { removed, added, recomputed }.
   */
  async function applySetup(doc, s, cfg, tris) {
    const kind = cfg.kind;
    s.kind = kind;
    // the material first: the operations' defaults below are for it
    if ("material" in cfg) {
      if (cfg.material) s.material = cfg.material;
      else delete s.material;
    }
    if (cfg.machine) s.machine = cfg.machine;
    if (cfg.setupName) s.name = cfg.setupName;
    if (cfg.post && Object.keys(cfg.post).length) s.post = cfg.post;
    else if (cfg.post) delete s.post;
    s.holding = { kind: kind === "vise" ? "vise" : "rotary_chuck", tailstock: kind !== "vise" && !!cfg.tailstock };
    const model = ownModel(doc, s);
    if (cfg.carry) {
      // what the setup before left: the engine turns the part and the stock together
      s.stock = { shape: "from_previous" };
      // (another setup than the one just before: named; the one just before is the default)
      const at = doc.setups.indexOf(s);
      if (cfg.carry.from && at > 0 && cfg.carry.from !== doc.setups[at - 1].id) s.stock.from = cfg.carry.from;
      if (cfg.carry.turn && cfg.carry.turn !== "none") s.stock.turn = cfg.carry.turn;
      if (cfg.carry.offset?.some((v) => v)) s.stock.offset = cfg.carry.offset.map(r3);
      // (Mar) turned any way: the cut result goes on as a mesh
      if (cfg.carry.rotate?.some((v) => v)) s.stock.rotate = cfg.carry.rotate.map((v) => Math.round(v * 1e4) / 1e4);
      // the starter tools, for the stock it started from (the first setup back with its own)
      let k = doc.setups.indexOf(s) - 1;
      while (k > 0 && doc.setups[k].stock?.shape === "from_previous") k--;
      if (k >= 0) ensureTools(doc, doc.setups[k].stock);
    } else if (model && tris) {
      const place = placePart(tris, { kind, axis: cfg.axis, up: cfg.axis, margin: cfg.margin, explicit: cfg.stock ? undefined : cfg.explicit, stock: cfg.stock, origin: cfg.origin, rotate: cfg.rotate });
      model.axis = place.axis;
      // the part's orientation (Mar): none, unless turned
      if (cfg.rotate?.some((v) => v)) model.rotate = cfg.rotate.map((v) => Math.round(v * 1e4) / 1e4);
      else delete model.rotate;
      model.translate = place.translate.map(r3);
      s.stock = place.stock;
      ensureTools(doc, place.stock);
    } else if (cfg.explicit) {
      s.stock = stockOfSpec(kind, cfg.explicit);
    }
    // the length of a bar held in the chuck; the depth the vise's jaws grip
    if (kind !== "vise" && cfg.stock?.chuck > 0) s.holding.chuck = r3(cfg.stock.chuck);
    if (kind === "vise" && cfg.jaws != null) s.holding.jaws = r3(cfg.jaws);
    // operations: keep what fits, with its defaults for the new part and kind
    const types = state.tree?.types?.length ? state.tree.types : (await engine("opTypes")).result;
    const removed = [], recomputed = [];
    const keep = [];
    const hasProfile = s.part.some((id) => doc.models.find((x) => x.id === id)?.kind === "profile");
    for (const op of s.operations) {
      const ty = types.find((t) => t.kind === op.type);
      if (!ty || !fitsKind(ty, kind)) {
        removed.push(`${op.id} (${op.type})`);
        continue;
      }
      // turning works from a revolved profile only
      if (!hasProfile && ty.geometry.length === 1 && ty.geometry[0] === "profile") {
        removed.push(`${op.id} (${op.type}: needs a revolved profile, and the part is a mesh)`);
        continue;
      }
      keep.push(op);
    }
    s.operations = keep;
    // (the output files name only what is left: the defaults below read the project, and an
    // output naming a removed operation made every later one fail, Mar's knob replaced)
    const left = new Set(keep.map((o) => o.id));
    for (const o of s.outputs) o.operations = o.operations.filter((id) => left.has(id));
    const next = [...s.operations];
    for (const [i, op] of [...s.operations.entries()]) {
      try {
        const { result } = await engine("opDefaults", doc, { type: op.type, setup: s.id, tool: op.tool, machine: engineMachine() }, state.mesh);
        const a = kind === "vise" ? 0 : op.index_a;
        next[i] = { ...result, id: op.id, enabled: op.enabled, ...(op.name ? { name: op.name } : {}), ...(op.color ? { color: op.color } : {}), ...(a !== undefined && result.index_a !== undefined ? { index_a: a } : {}) };
        recomputed.push(op.id);
      } catch (e) {
        removed.push(`${op.id} (${op.type}: ${e.message})`);
        next[i] = null;
      }
    }
    s.operations = next.filter(Boolean);
    const ids = new Set(s.operations.map((o) => o.id));
    for (const o of s.outputs) o.operations = o.operations.filter((id) => ids.has(id));
    s.outputs = s.outputs.filter((o) => o.operations.length);
    const added = s.operations.length || !settings.starters ? [] : await addStarters(doc, s);
    return { removed, added, recomputed };
  }

  /** The parsed STL of the dialog's file, in mm, or (no file) the project's mesh. */
  function trisOf(cfg) {
    if (cfg.tris) return cfg.units === "inch" && !cfg.step ? cfg.tris.map((v) => v * 25.4) : cfg.tris;
    return state.mesh ? parseStl(state.mesh).tris : null;
  }

  /** A new part from the dialog (its file already parsed): into a project without a setup,
   *  one is made; otherwise the dialog's setup is converted round it. */
  async function importParsed(cfg) {
    setStatus(`Importing ${cfg.file.name}...`, "busy");
    let tris = trisOf(cfg);
    let doc;
    try {
      doc = readJob();
    } catch {
      doc = { schema_version: "0.2", name: "untitled", models: [], tools: [], setups: [] };
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    const fresh = !doc.setups?.length || !doc.setups.some((s) => s.operations.length);
    if (state.mesh && meshFileOf(doc) && !fresh && !window.confirm(`Replace the part with ${cfg.file.name}? (OK: replace it; Cancel: add ${cfg.file.name} to the part)`)) {
      const old = parseStl(state.mesh).tris;
      const both = new Float32Array(old.length + tris.length);
      both.set(old);
      both.set(tris, old.length);
      tris = both;
    }
    // a STEP on its own: the engine reads the STL with its faces numbered, the project keeps the
    // STEP itself (merged with another part, its triangles alone)
    const step = cfg.step && tris === cfg.tris ? cfg.step : null;
    const meshFile = `${slug(cfg.file.name)}.${step ? "step" : "stl"}`;
    const bytes = step ? step.stl : encodeStl(tris, `marcam: ${cfg.file.name}`);
    const what = step ? partSummary({ tris, step }) : `${cfg.ascii ? "ASCII" : "binary"} STL${cfg.units === "inch" ? ", inch" : ""}, ${tris.length / 9} triangles`;
    checkpoint();
    setMesh(bytes, `${cfg.file.name} (${what})`, step ? { name: meshFile, bytes: step.bytes } : null);
    let report;
    if (fresh) {
      const name = doc.name && doc.name !== "untitled" ? doc.name : cfg.name && cfg.name !== "untitled" ? cfg.name : slug(cfg.file.name);
      doc = {
        schema_version: "0.2",
        name,
        models: [{ id: "part", name: cfg.file.name, kind: "mesh", file: meshFile, axis: "x", translate: [0, 0, 0] }],
        tools: [],
        setups: [{ id: "setup1", name: KINDS[cfg.kind], stock: { shape: "cylinder", radius: 1, length: 1 }, clearance: 3, part: ["part"], spindle_rpm: 12000, operations: [], outputs: [] }],
      };
      p.name = name;
      report = await applySetup(doc, doc.setups[0], cfg, tris);
    } else {
      // the new part for every mesh model; the dialog's setup converted, the others re-placed
      for (const m of doc.models) if (m.kind === "mesh") Object.assign(m, { name: cfg.file.name, file: meshFile });
      if (!doc.models.some((m) => m.kind === "mesh")) {
        doc.models = doc.models.filter((m) => m.kind !== "profile").concat({ id: "part", name: cfg.file.name, kind: "mesh", file: meshFile, axis: "x", translate: [0, 0, 0] });
        for (const s of doc.setups) s.part = ["part"];
      }
      const active = doc.setups.find((s) => s.id === state.tree?.setup) ?? doc.setups[0];
      report = await applySetup(doc, active, cfg, tris);
      for (const s of doc.setups.filter((x) => x !== active)) {
        await applySetup(doc, s, { kind: kindOf(s), machine: s.machine, axis: kindOf(s) === "vise" ? "z" : "x", tailstock: s.holding?.tailstock }, tris);
      }
    }
    // an import is a request for toolpaths: planned once
    await commitSetupChange(doc, report, null, true);
    return true;
  }

  /** Write a changed project and regenerate; say what the change removed or added. */
  async function commitSetupChange(doc, report, undoOf, planNow = false) {
    writeJob(doc);
    if (undoOf) keepUndo(undoOf.label, undoOf.job);
    const s0 = doc.setups.find((s) => s.id === state.tree?.setup) ?? doc.setups[0];
    const place = state.mesh && s0 ? placeInfo(doc, s0) : "";
    $("meshInfo").textContent = `${state.meshName}${place ? `: ${place}` : ""}`;
    const notes = [];
    if (report?.removed?.length) notes.push(`removed (they do not fit this setup): ${report.removed.join(", ")}`);
    if (report?.added?.length) notes.push(`added: ${report.added.join(", ")}`);
    if (planNow && doc.setups.some((s) => s.operations.some((o) => o.enabled !== false))) {
      clearResults("");
      await refreshAll();
      await generate();
    } else if (planNow) {
      // (nothing to plan yet: the person adds the operations)
      clearResults("");
      await refreshAll();
      setStatus("Add operations in the project tree, then Generate", "ready");
    } else {
      // (Mar: no Generate) planned again by itself, as any edit is (none to plan: a quiet
      // hint to add one)
      await refreshAll();
      ctx.markStale();
      setStatus("Setup changed", "ready");
    }
    if (notes.length) showError(`Setup changed: ${notes.join("; ")}`);
  }

  /** "21.70 x 20.00 x 20.00 mm, on the rotary axis ..." for a setup's placed part. */
  function placeInfo(doc, s) {
    const m = doc.models.find((x) => s.part.includes(x.id) && x.kind === "mesh");
    if (!m || !state.mesh) return "";
    const tris = parseStl(state.mesh).tris;
    const lo = [Infinity, Infinity, Infinity], hi = [-Infinity, -Infinity, -Infinity];
    for (let i = 0; i < tris.length; i += 3) {
      const q = mapAxis([tris[i], tris[i + 1], tris[i + 2]], m.axis ?? "x");
      for (let k = 0; k < 3; k++) {
        lo[k] = Math.min(lo[k], q[k]);
        hi[k] = Math.max(hi[k], q[k]);
      }
    }
    const size = [0, 1, 2].map((k) => (hi[k] - lo[k]).toFixed(2)).join(" x ");
    const st = s.stock;
    const kind = kindOf(s);
    const where = kind === "vise" ? `in the vise (${KINDS.vise}), ${describeStock(st)}${st.shape !== "box" ? " lying along X" : ""}` : `on the rotary axis (${KINDS[kind]}, front at X0), ${describeStock(st)}`;
    return `${size} mm, ${where}`;
  }

  /** Import STL: the setup dialog (its type pre-selected from the current setup), then in. */
  async function importMesh(file) {
    showError("");
    let parsed;
    try {
      if (/\.(step|stp)$/i.test(file.name)) setStatus(`Reading ${file.name} (STEP)...`, "busy");
      parsed = await readPart(new Uint8Array(await file.arrayBuffer()), file.name);
    } catch (e) {
      showError(`${file.name}: ${e.message}`);
      setStatus("Import failed", "error");
      return false;
    }
    let s = null;
    try {
      const doc = readJob();
      s = doc.setups?.find((x) => x.id === state.tree?.setup) ?? doc.setups?.[0] ?? null;
    } catch {}
    const preset = s
      ? { kind: kindOf(s), machine: s.machine ?? currentMachine()?.id, setupName: s.name, tailstock: s.holding?.tailstock }
      : { kind: p.pending?.kind ?? "rotary", machine: p.pending?.machine ?? currentMachine()?.id, margin: p.pending?.margin, explicit: p.pending?.explicit, units: p.pending?.units };
    // (Mar: no modal) placed at once with the setup's (or the usual) defaults; its type,
    // stock and placement are changed in the tree's Setup, Stock and Part
    const cfg = await dialog.open("import", { ...preset, file, parsed }, { auto: !settings.dialogs });
    if (cfg?.error) {
      showError(`${file.name}: ${cfg.error}`);
      setStatus("Import failed", "error");
      return false;
    }
    if (!cfg) {
      setStatus("Import cancelled", "ready");
      return false;
    }
    if (cfg.machine) ctx.selectMachine(cfg.machine);
    await importParsed(cfg);
    return true;
  }

  /**
   * Replace the part's model with a new STL or STEP: its placement, the setups, tools and
   * operations stay; every operation's picked features are found again on it (the engine);
   * those not found are put in error (kept, not planned) and named. Undo brings the old model
   * back. Then the toolpaths are planned again.
   */
  async function replaceModel(file) {
    showError("");
    let doc;
    try {
      doc = readJob();
    } catch (e) {
      return showError(e.message);
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    const wanted = meshFileOf(doc);
    // (the setup worked on's own part first)
    const here = doc.setups.find((x) => x.id === state.tree?.setup) ?? doc.setups[0];
    const model = doc.models?.find((m) => m.kind === "mesh" && here?.part?.includes(m.id)) ?? doc.models?.find((m) => m.kind === "mesh" && m.file === wanted) ?? doc.models?.find((m) => m.kind === "mesh");
    setStatus(`Reading ${file.name}...`, "busy");
    let part;
    try {
      part = await readPart(new Uint8Array(await file.arrayBuffer()), file.name);
    } catch (e) {
      setStatus("Replace failed", "error");
      return showError(`${file.name}: ${e.message}`);
    }
    const name = `${slug(file.name)}.${part.step ? "step" : "stl"}`;
    const bytes = part.step ? part.step.stl : encodeStl(part.tris, `marcam: ${file.name}`);
    if (!model && here) {
      // (Mar: "the replace model button doesn't work" on the knurled knob: its part is a
      // revolved profile, not a mesh) the file becomes the setup's part: a mesh model in the
      // profile's place, placed on the setup as an import is (its stock fitted round it); the
      // operations that need the profile say so. Undo brings the profile back.
      const before = JSON.stringify(doc, null, 2);
      const taken = new Set(doc.models.map((x) => x.id));
      let id = "part";
      for (let k = 2; taken.has(id); k++) id = `part${k}`;
      const kind = kindOf(here);
      doc.models.push({ id, name: file.name, kind: "mesh", file: name, axis: "x", translate: [0, 0, 0] });
      const was = here.part || [];
      here.part = [id];
      // (a profile no setup uses any more: gone with the edit)
      const used = new Set(doc.setups.flatMap((x) => x.part || []));
      doc.models = doc.models.filter((x) => used.has(x.id) || !was.includes(x.id));
      setMesh(bytes, `${file.name} (${partSummary(part)})`, part.step ? { name, bytes: part.step.bytes } : null);
      // (its machine as it is: the one chosen, when the setup names none)
      const report = await applySetup(doc, here, { kind, axis: kind === "vise" ? "z" : "x", tailstock: here.holding?.tailstock, rotate: [0, 0, 0], machine: here.machine || engineMachine() || undefined }, part.tris);
      await commitSetupChange(doc, report, { label: `Setup "${here.name || here.id}": its part is ${file.name} (was its revolved profile).`, job: before });
      setStatus(`Replaced the model with ${file.name}.`, "done");
      return { project: doc, operations: [] };
    }
    if (!model) return showError("Replace model: the project has no setup (import a part instead)");
    let result;
    try {
      ({ result } = await engine("replaceModel", doc, { model: model.id, file: name }, bytes));
    } catch (e) {
      setStatus("Replace failed", "error");
      return showError(`Replace model: ${e.message}`);
    }
    // undo: the project and the model as they were
    keepUndo(`Replaced the model with ${file.name}.`, JSON.stringify(doc, null, 2));
    setMesh(bytes, `${file.name} (${partSummary(part)})`, part.step ? { name, bytes: part.step.bytes } : null);
    const bad = result.operations.filter((o) => !o.ok);
    const found = result.operations.length - bad.length;
    await commitSetupChange(result.project, null, null, true);
    const msg = `Replaced the model with ${file.name}: ${found} operation(s) found their picks again${bad.length ? `; ${bad.length} not planned until picked again: ${bad.map((o) => `${o.op} (${o.error})`).join("; ")}` : ""}.`;
    if (bad.length) showError(msg);
    else setStatus(msg, "done");
    return result;
  }

  /** Edit a setup: type, machine, stock, origin, holding. A new type converts the setup. */
  /** The setup's settings in the details below the tree (`host`): the form put back as it was
   *  when it is already open there for that setup, else opened there anew. */
  function hostSetup(host, id, focus) {
    // (Add setup's dialog is up: the details wait, not taking the form from it; CI caught
    // the setup's own details reopening it as an edit of the first setup)
    if (dialog.modal) return;
    if (!host) return dialog.park();
    if (dialog.embeddedFor(id)) return dialog.reattach(host, focus);
    editSetup(id, { host }).catch((e) => showError(e.message));
  }

  async function editSetup(id, opts = {}) {
    let doc;
    try {
      doc = readJob();
    } catch (e) {
      return showError(e.message);
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    const s = doc.setups.find((x) => x.id === (id ?? state.tree?.setup)) ?? doc.setups[0];
    if (!s) return showError("No setup yet: import an STL first.");
    const m = doc.models.find((x) => s.part.includes(x.id) && x.kind === "mesh");
    // a revolved profile: its surface for the dialog's preview and Fit (not placed again)
    const prof = !m ? doc.models.find((x) => s.part.includes(x.id) && x.kind === "profile") : null;
    let previewTris = null;
    if (prof?.points?.length > 1) {
      const rm = revolveProfile(prof.points, 72);
      previewTris = new Float32Array(rm.indices.length * 3);
      rm.indices.forEach((v, i) => previewTris.set(rm.positions.subarray(3 * v, 3 * v + 3), 3 * i));
    }
    const kind = kindOf(s);
    const axis = kind === "vise" ? Object.entries(UP_AXIS).find(([, v]) => v === (m?.axis ?? "x"))?.[0] ?? "z" : m?.axis ?? "x";
    // the stock's and the part's boxes, as the setup has them (the work zero's presets)
    let stockBox = null, partBox = null;
    try {
      const { result: fr } = await engine("setupFrame", doc, { setup: s.id });
      stockBox = fr.stock_box ?? null;
      if (m && window.__marcam.meshFor(m.file)) {
        const { result: mt } = await engine("meshTriangles", doc, { setup: s.id, max_triangles: 20000 }, state.mesh);
        const t = mt.triangles;
        const lo = [Infinity, Infinity, Infinity], hi = [-Infinity, -Infinity, -Infinity];
        for (let i = 0; i < t.length; i += 3) for (let k = 0; k < 3; k++) { lo[k] = Math.min(lo[k], t[i + k]); hi[k] = Math.max(hi[k], t[i + k]); }
        if (lo[0] <= hi[0]) partBox = { min: lo, max: hi };
      }
    } catch {}
    // (the details' form, asked for before Add setup's dialog came up: not over it; the
    // details show it again when that one is done)
    if (opts.host && dialog.modal) return false;
    if (opts.host && !opts.host.isConnected) {
      // (the details drawn again meanwhile: into their new place, if it is this setup's)
      const again = document.getElementById("setupHost");
      if (!again || state.tree?.setup !== s.id) return false;
      opts = { ...opts, host: again };
    }
    const cfg = await dialog.open("edit", {
      stockBox, partBox,
      kind, machine: s.machine ?? currentMachine()?.id, setupName: s.name, axis, tailstock: !!s.holding?.tailstock, material: s.material,
      stock: s.stock, chuck: s.holding?.chuck, jaws: s.holding?.jaws, hasMesh: !!(m && window.__marcam.meshFor(m.file)), tris: m && window.__marcam.meshFor(m.file) ? parseStl(window.__marcam.meshFor(m.file)).tris : null, previewTris, post: s.post,
      canCarry: doc.setups.indexOf(s) > 0, setupId: s.id, rotate: s.stock?.shape === "from_previous" ? s.stock.rotate : m?.rotate,
      // (Mar) this setup's part: its own, another setup's, or another file
      modelChoices: m ? [{ value: "own", label: `its own: ${m.file}` }]
        .concat(doc.setups.filter((o) => o !== s).map((o) => [o, doc.models.find((x) => o.part.includes(x.id) && x.kind === "mesh")]).filter(([, om]) => om && om.file !== m.file && window.__marcam.meshFor(om.file)).map(([o, om]) => ({ value: `setup:${o.id}`, label: `as ${o.name || o.id}: ${om.file}` })))
        .concat([{ value: "none", label: "no model" }, { value: "file", label: "another file..." }]) : [],
      trisOf: (id) => {
        const o = doc.setups.find((x) => x.id === id);
        const om = o && doc.models.find((x) => o.part.includes(x.id) && x.kind === "mesh");
        const b = om && window.__marcam.meshFor(om.file);
        return b ? parseStl(b).tris : null;
      },
      carrySources: doc.setups.slice(0, doc.setups.indexOf(s)).map((x) => ({ id: x.id, name: x.name || x.id })),
    }, { host: opts.host });
    if (!cfg) return false;
    // nothing about the placement changed (only the name or the material): the operations
    // keep their values; a new material changes only the ones not typed over
    const machine0 = s.machine ?? currentMachine()?.id;
    if (cfg.kind === kind && cfg.axis === axis && !!cfg.tailstock === !!s.holding?.tailstock && !cfg.stockChanged && !cfg.explicit && !cfg.edited?.length && (cfg.machine ?? null) === (machine0 ?? null)) {
      const before = $("job").value;
      let changed = [];
      if ((cfg.material ?? null) !== (s.material ?? null)) {
        const { result } = await engine("setMaterial", doc, { setup: s.id, material: cfg.material, machine: engineMachine() });
        doc = result.project;
        changed = result.changed;
      }
      const s1 = doc.setups.find((x) => x.id === s.id);
      if (cfg.setupName) s1.name = cfg.setupName;
      // its G-code options (nothing planned changes: the G-code is written again)
      if (cfg.postChanged) {
        if (Object.keys(cfg.post).length) s1.post = cfg.post;
        else delete s1.post;
      }
      // the jaws' grip (nothing moves: the warnings change)
      if (kind === "vise") {
        if (cfg.jaws != null) s1.holding = { ...s1.holding, jaws: r3(cfg.jaws) };
        else if (s1.holding) delete s1.holding.jaws;
      }
      const what = (cfg.material ?? null) !== (s.material ?? null) ? `material ${cfg.material ?? "not set"}` : cfg.postChanged ? "G-code options" : "renamed";
      await commitSetupChange(doc, { removed: [], added: [] }, { label: `Setup "${s1.name || s1.id}": ${what}.`, job: before });
      if (changed.length) setStatus(`Material ${cfg.material ?? "not set"}: ${changed.length} cutting value${changed.length > 1 ? "s" : ""} updated (typed ones kept). Generate to plan it again`, "ready");
      return { removed: [], added: [], recomputed: [], material: changed };
    }
    setStatus(`Changing the setup to ${KINDS[cfg.kind]}...`, "busy");
    const before = $("job").value;
    const probe = structuredClone(doc);
    const ps = probe.setups.find((x) => x.id === s.id);
    const types = state.tree?.types?.length ? state.tree.types : (await engine("opTypes")).result;
    const going = ps.operations.filter((op) => !fitsKind(types.find((t) => t.kind === op.type) ?? { machine_needs: [] }, cfg.kind)).map((op) => `${op.id} (${op.type})`);
    if (going.length && !window.confirm(`Changing "${s.name || s.id}" to ${KINDS[cfg.kind]} removes ${going.join(", ")}: they do not fit that setup.${settings.starters ? " Starter operations are added if none are left." : ""} Go on? (You can undo it.)`)) {
      setStatus("Setup unchanged", "ready");
      return false;
    }
    // this setup's part: its own (from its file), another setup's, or a new file (its picks
    // found again on it; only this setup's operations)
    const ownBytes = m && window.__marcam.meshFor(m.file);
    let tris = ownBytes ? parseStl(ownBytes).tris : null;
    let found = null;
    if (cfg.model?.kind === "none") {
      // (Mar) no model: its stock and its operations on the stock only
      const s1 = doc.setups.find((x) => x.id === s.id);
      s1.part = [];
      const used = new Set(doc.setups.flatMap((x) => x.part || []));
      doc.models = doc.models.filter((x) => used.has(x.id));
      tris = null;
    } else if (cfg.model?.kind === "setup") {
      const o = doc.setups.find((x) => x.id === cfg.model.id);
      const om = o && doc.models.find((x) => o.part.includes(x.id) && x.kind === "mesh");
      if (om) {
        const own = ownModel(doc, doc.setups.find((x) => x.id === s.id));
        own.file = om.file;
        tris = parseStl(window.__marcam.meshFor(om.file)).tris;
      }
    } else if (cfg.model?.kind === "file") {
      const part = cfg.model.parsed;
      const taken = new Set([...doc.models.map((x) => x.file), ...state.extraMeshes.keys()]);
      let name = `${slug(cfg.model.file.name)}.stl`;
      for (let k = 2; taken.has(name); k++) name = `${slug(cfg.model.file.name)}_${k}.stl`;
      const bytes = part.step ? part.step.stl : encodeStl(part.tris, `marcam: ${cfg.model.file.name}`);
      const own = ownModel(doc, doc.setups.find((x) => x.id === s.id));
      const { result } = await engine("replaceModel", doc, { model: own.id, file: name }, bytes);
      doc = result.project;
      state.extraMeshes.set(name, { bytes, name: cfg.model.file.name });
      state.meshesGen++;
      tris = part.tris;
      found = result.operations;
    }
    const report = await applySetup(doc, doc.setups.find((x) => x.id === s.id), cfg, tris);
    // (the walkthrough: "Rotary 4th axis" stayed the name of a setup made a vise) a name that
    // was only its type's follows the type
    const named = doc.setups.find((x) => x.id === s.id);
    if (named && Object.values(KINDS).includes(named.name) && cfg.kind && KINDS[cfg.kind]) named.name = KINDS[cfg.kind];
    await commitSetupChange(doc, report, { label: `Setup "${s.name || s.id}" changed to ${KINDS[cfg.kind]}.`, job: before });
    if (cfg.machine) ctx.selectMachine(cfg.machine);
    const bad = (found || []).filter((o) => !o.ok);
    if (bad.length) showError(`The new part: ${bad.length} operation(s) not planned until picked again: ${bad.map((o) => `${o.op} (${o.error})`).join("; ")}`);
    return report;
  }

  /** Add a setup: the dialog's type, the same part, its own placement and starters. */
  async function addSetup(opts = {}) {
    let doc;
    try {
      doc = readJob();
    } catch (e) {
      return showError(e.message);
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    // (Mar) its model: the part's mesh, another setup's, a new file, or none
    const meshModel = doc.models.find((x) => x.kind === "mesh" && window.__marcam.meshFor(x.file));
    const meshOf = (o) => doc.models.find((x) => o.part.includes(x.id) && x.kind === "mesh" && window.__marcam.meshFor(x.file));
    const others = doc.setups.map((o) => [o, meshOf(o)]).filter(([, om]) => om && om.file !== meshModel?.file);
    const seen = new Set();
    const modelChoices = (meshModel ? [{ value: "own", label: `the part: ${meshModel.file}` }] : [])
      .concat(others.filter(([, om]) => !seen.has(om.file) && seen.add(om.file)).map(([o, om]) => ({ value: `setup:${o.id}`, label: `as ${o.name || o.id}: ${om.file}` })))
      .concat([{ value: "none", label: "no model" }, { value: "file", label: "another file..." }]);
    let n = doc.setups.length + 1;
    while (doc.setups.some((s) => s.id === `setup${n}`)) n++;
    const cfg = await dialog.open("add", { kind: opts.kind || "vise", machine: currentMachine()?.id, setupName: `Setup ${n}`, hasMesh: !!meshModel, tris: meshModel ? parseStl(window.__marcam.meshFor(meshModel.file)).tris : null, canCarry: doc.setups.length > 0 && !opts.independent,
      // (no model, nothing before it: a plate, or a bar on the 4th axis, of a usual size)
      ...(!meshModel && (!doc.setups.length || opts.independent) ? { stockSpec: (opts.kind || "vise") === "vise" ? { shape: "block", mode: "given", dims: { x: 60, y: 40, z: 20 } } : { shape: "round", mode: "given", dims: { radius: 12.5 }, length: 60 } } : {}),
      modelChoices,
      trisOf: (id) => {
        const o = doc.setups.find((x) => x.id === id);
        const om = o && meshOf(o);
        return om ? parseStl(window.__marcam.meshFor(om.file)).tris : null;
      },
      carrySources: doc.setups.map((x) => ({ id: x.id, name: x.name || x.id })) }, { auto: !settings.dialogs });
    // (Mar: no modal) made at once: what the setup before left as its stock (when there is
    // one), its Stock selected to change it
    if (!cfg || cfg.error) {
      if (cfg?.error) showError(`Add setup: ${cfg.error}`);
      return false;
    }
    setStatus(`Adding a ${KINDS[cfg.kind]} setup...`, "busy");
    const before = $("job").value;
    let model = null, tris = null;
    if (cfg.model?.kind === "setup") {
      const om = meshOf(doc.setups.find((x) => x.id === cfg.model.id));
      model = { ...om, id: `setup${n}_part` };
      tris = parseStl(window.__marcam.meshFor(om.file)).tris;
    } else if (cfg.model?.kind === "file") {
      const part = cfg.model.parsed;
      const taken = new Set([...doc.models.map((x) => x.file), ...state.extraMeshes.keys()]);
      let name = `${slug(cfg.model.file.name)}.stl`;
      for (let k = 2; taken.has(name); k++) name = `${slug(cfg.model.file.name)}_${k}.stl`;
      const bytes = part.step ? part.step.stl : encodeStl(part.tris, `marcam: ${cfg.model.file.name}`);
      state.extraMeshes.set(name, { bytes, name: cfg.model.file.name });
      state.meshesGen++;
      model = { id: `setup${n}_part`, name: cfg.model.file.name, kind: "mesh", file: name };
      tris = part.tris;
    } else if (cfg.model?.kind !== "none" && meshModel) {
      model = { ...meshModel, id: `setup${n}_part` };
      tris = parseStl(window.__marcam.meshFor(meshModel.file)).tris;
    }
    if (model) doc.models.push(model);
    const s = { id: `setup${n}`, name: cfg.setupName || KINDS[cfg.kind], stock: { shape: "cylinder", radius: 1, length: 1 }, clearance: 3, part: model ? [model.id] : [], spindle_rpm: 12000, operations: [], outputs: [] };
    doc.setups.push(s);
    const report = await applySetup(doc, s, cfg, tris);
    // (the new setup's Stock shown, its handles in the view)
    if (state.tree) Object.assign(state.tree, { setup: s.id, focus: settings.dialogs ? "setup" : "stock", selected: null });
    await commitSetupChange(doc, report, { label: `Setup "${s.name}" added.`, job: before });
    if (cfg.machine) ctx.selectMachine(cfg.machine);
    return report;
  }

  // ------------------------------------------------------------ wiring
  $("projectNew").addEventListener("click", () => newProject());
  $("projectOpen").addEventListener("click", () => $("projectOpenFile").click());
  $("projectOpenFile").addEventListener("change", async () => {
    const f = $("projectOpenFile").files[0];
    $("projectOpenFile").value = "";
    if (f) await openFile(f);
  });
  $("projectSave").addEventListener("click", () => save(false));
  $("projectSaveAs").addEventListener("click", () => save(true));
  $("importStl").addEventListener("click", () => $("importStlFile").click());
  $("importStlFile").addEventListener("change", async () => {
    const f = $("importStlFile").files[0];
    $("importStlFile").value = "";
    if (f) await importMesh(f);
  });
  $("undoBar").addEventListener("click", (ev) => {
    if (ev.target.id === "undoSetup") undo();
  });
  // drop a mesh to import it, or a project to open it, anywhere on the page
  document.addEventListener("dragover", (ev) => {
    if (ev.dataTransfer?.types?.includes("Files")) {
      ev.preventDefault();
      document.body.classList.add("dropping");
    }
  });
  document.addEventListener("dragleave", (ev) => {
    if (!ev.relatedTarget) document.body.classList.remove("dropping");
  });
  document.addEventListener("drop", async (ev) => {
    const f = ev.dataTransfer?.files?.[0];
    document.body.classList.remove("dropping");
    if (!f) return;
    ev.preventDefault();
    if (/\.(stl|step|stp)$/i.test(f.name)) await importMesh(f);
    else if (/\.(marcam|toml|json)$/i.test(f.name)) await openFile(f);
    else showError(`${f.name}: drop an .stl or a .step to import it, or a .marcam (or .marcam.toml, .json) project to open it`);
  });
  window.addEventListener("beforeunload", (ev) => {
    if (p.dirty) {
      ev.preventDefault();
      ev.returnValue = "";
    }
  });
  show();

  $("replaceModelFile").addEventListener("change", async (ev) => {
    const f = ev.target.files?.[0];
    ev.target.value = "";
    if (f) await replaceModel(f);
  });

  /**
   * (Mar: no import dialog) The part of setup `id` scaled by `k` (an inch file: 25.4): its
   * mesh file written again at that scale, its stock fitted round it again. Undo brings it
   * back.
   */
  async function scalePart(id, k) {
    let doc;
    try {
      doc = readJob();
    } catch (e) {
      return showError(e.message);
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    const s = doc.setups.find((x) => x.id === id);
    const m = s && doc.models.find((x) => s.part.includes(x.id) && x.kind === "mesh");
    const bytes = m && window.__marcam.meshFor(m.file);
    if (!bytes) return;
    const before = $("job").value;
    const tris = parseStl(bytes).tris.map((v) => v * k);
    const out = encodeStl(tris, `marcam: ${m.file} x${k}`);
    // (the part's file changes with the edit: the snapshot before keeps the old one, for Undo)
    checkpoint();
    // (the part's file: the project's own, or a setup's own)
    if (state.extraMeshes.has(m.file)) {
      state.extraMeshes.set(m.file, { ...state.extraMeshes.get(m.file), bytes: out });
      state.meshesGen++;
    } else {
      ctx.setMesh(out, state.meshName, state.meshSource);
    }
    m.translate = (m.translate || [0, 0, 0]).map((v) => r3(v * k));
    await applySetup(doc, s, { kind: kindOf(s), axis: kindOf(s) === "vise" ? Object.entries(UP_AXIS).find(([, v]) => v === (m.axis ?? "x"))?.[0] ?? "z" : m.axis ?? "x", tailstock: s.holding?.tailstock, rotate: m.rotate, machine: s.machine }, tris);
    await commitSetupChange(doc, { removed: [], added: [] }, { label: `Setup "${s.name || s.id}": its part ${k > 1 ? "in inches (x25.4)" : `scaled x${+k.toFixed(4)}`}.`, job: before });
  }

  /** (Mar) Setup `id` continuing from the setup before (`carry`: its cut result, the part with
   *  it) or independent (`own`: its own stock fitted round its own part where it now sits).
   *  One edit; planned again. */
  async function setSource(id, how) {
    let doc;
    try {
      doc = readJob();
    } catch (e) {
      return showError(e.message);
    }
    if (doc.schema_version !== "0.2") doc = (await engine("checkProject", doc)).result.project;
    const s = doc.setups.find((x) => x.id === id);
    const at = doc.setups.indexOf(s);
    if (!s || at < 1) return;
    const before = $("job").value;
    if (how === "carry") {
      if (s.stock?.shape === "from_previous") return;
      s.stock = { shape: "from_previous" };
    } else {
      if (s.stock?.shape !== "from_previous") return;
      // its own part: the model it had (or the setup before's), placed as it is drawn now
      let m = doc.models.find((x) => s.part.includes(x.id) && x.kind === "mesh");
      if (!m) {
        const pm = doc.models.find((x) => doc.setups[at - 1].part.includes(x.id) && x.kind === "mesh");
        if (pm) {
          m = { ...structuredClone(pm), id: `${s.id}_part` };
          doc.models = doc.models.filter((x) => x.id !== m.id).concat(m);
          s.part = [m.id];
        }
      }
      const bytes = m && window.__marcam.meshFor(m.file);
      const kind = kindOf(s) === "vise" || s.kind === "vise" ? "vise" : "rotary";
      s.stock = kind === "vise" ? { shape: "box", min: [0, 0, -20], max: [60, 40, 0] } : { shape: "cylinder", radius: 12.5, length: 60 };
      if (bytes) {
        const { fitStock } = await import("./stockpane.js");
        fitStock(doc, s, parseStl(bytes).tris, kind === "vise" ? 2 : 0.5);
      }
    }
    await commitSetupChange(doc, { removed: [], added: [] }, { label: `Setup "${s.name || s.id}": ${how === "carry" ? "continues from the setup before" : "independent"}.`, job: before });
  }

  return { changed, clean, mayDiscard, newProject, openText, openBundle, openFile, save, importMesh, replaceModel, editSetup, hostSetup, addSetup, scalePart, setSource, undo, redo, dropUndo, keepUndo, nameEdit, resetHistory, meshSet, history: h, dialog, state: p, settings };
}
