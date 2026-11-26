// Closed (watertight) display meshes for the 3D view, built without three.js so they can be
// tested in Node (web/tests/meshes.test.mjs). Every solid has end caps and the annular faces
// at steps, outward winding, and its own vertices per face group so normals stay crisp.
//
// Geometry: X is the rotary axis; a point at radius r and part angle phi is
// (x, r cos phi, r sin phi).

/** A growing indexed triangle list. */
class Builder {
  constructor() {
    this.pos = [];
    this.col = [];
    this.idx = [];
  }
  vertex(x, y, z, c) {
    this.pos.push(x, y, z);
    if (c) this.col.push(c[0], c[1], c[2]);
    return this.pos.length / 3 - 1;
  }
  tri(a, b, c) {
    this.idx.push(a, b, c);
  }
  done() {
    return {
      positions: new Float32Array(this.pos),
      colors: this.col.length ? new Float32Array(this.col) : null,
      indices: new Uint32Array(this.idx),
    };
  }
}

/** Ring of `n` vertices at axial position x; `radius(j)` and `color(j)` per angle index. */
function ring(b, x, n, radius, color) {
  const out = [];
  for (let j = 0; j < n; j++) {
    const phi = ((j + 0.5) * 2 * Math.PI) / n;
    const r = radius(j);
    out.push(b.vertex(x, r * Math.cos(phi), r * Math.sin(phi), color && color(j)));
  }
  return out;
}

/** Band between two rings, outward for increasing x (a -> b along +x, angle increasing). */
function band(b, ra, rb) {
  const n = ra.length;
  for (let j = 0; j < n; j++) {
    const k = (j + 1) % n;
    // (angle) x (+x) points outward: [a_j, a_k, b_j] and [a_k, b_k, b_j]
    b.tri(ra[j], ra[k], rb[j]);
    b.tri(ra[k], rb[k], rb[j]);
  }
}

/** Disc cap at x closing a ring: faces -x when `back`, +x otherwise. */
function cap(b, x, n, radius, back, color) {
  const rv = ring(b, x, n, radius, color);
  const c = b.vertex(x, 0, 0, color && color(0));
  for (let j = 0; j < n; j++) {
    const k = (j + 1) % n;
    if (back) b.tri(c, rv[k], rv[j]);
    else b.tri(c, rv[j], rv[k]);
  }
}

/**
 * Revolve a profile [[x, r], ...] (ascending x; equal x = a step) about X into a closed solid.
 * Steps become annular faces; both ends get caps.
 */
export function revolveProfile(points, segments = 120) {
  const pts = points.filter((p, i) => i === 0 || p[0] !== points[i - 1][0] || p[1] !== points[i - 1][1]);
  const b = new Builder();
  if (pts.length < 2) return b.done();
  for (let i = 0; i + 1 < pts.length; i++) {
    const [x0, r0] = pts[i];
    const [x1, r1] = pts[i + 1];
    // each face group gets its own vertices (crisp normals at shoulders)
    const ra = ring(b, x0, segments, () => r0);
    const rb = ring(b, x1, segments, () => r1);
    // a side, or at a step (x0 === x1) the annulus: its normal comes out -x when stepping up
    // and +x when stepping down, so the same winding is outward in every case
    band(b, ra, rb);
  }
  cap(b, pts[0][0], segments, () => pts[0][1], true);
  cap(b, pts[pts.length - 1][0], segments, () => pts[pts.length - 1][1], false);
  return b.done();
}

/** Closed cylinder of radius r from x0 to x1. */
export function cylinder(r, x0, x1, segments = 96) {
  return revolveProfile([[x0, r], [x1, r]], segments);
}

/**
 * Closed solid from a radial heightmap (grid {x0, dx, nx, na}, radii r[i * na + j]) with an
 * optional per-cell colour. Rings sit at cell centres; the ends are capped.
 */
export function heightmapSolid(grid, r, color, uncut = null) {
  const { x0, dx, nx, na } = grid;
  const b = new Builder();
  if (nx < 1) return b.done();
  const c = color ? (i) => (j) => color(i * na + j) : () => null;
  let prev = null;
  for (let i = 0; i < nx; i++) {
    const cur = ring(b, x0 + (i + 0.5) * dx, na, (j) => r[i * na + j], c(i));
    if (prev) band(b, prev, cur);
    prev = cur;
  }
  // (the bar's ends: its faces as they came, cutting only lowers the radius; the cut's colour
  // on the round)
  const end = (i) => (uncut ? () => uncut : c(i));
  cap(b, x0 + 0.5 * dx, na, (j) => r[j], true, end(0));
  cap(b, x0 + (nx - 0.5) * dx, na, (j) => r[(nx - 1) * na + j], false, end(nx - 1));
  return b.done();
}

/**
 * Closed solid from a 3-axis block heightmap (grid {x0, y0, cell, nx, ny}, tops z[j * nx + i],
 * flat bottom at `bottom`) with an optional per-cell colour. The top vertices sit at cell
 * centres and come first (vertex k is cell k), and the side walls share them, so updating
 * vertex k's Z moves the surface and the walls together.
 */
export function blockSolid(grid, z, bottom, color, uncut = null) {
  const { x0, y0, cell, nx, ny } = grid;
  const b = new Builder();
  if (nx < 2 || ny < 2) return b.done();
  const cx = (i) => x0 + (i + 0.5) * cell, cy = (j) => y0 + (j + 0.5) * cell;
  const col = color ? (k) => color(k) : () => null;
  for (let j = 0; j < ny; j++) for (let i = 0; i < nx; i++) b.vertex(cx(i), cy(j), z[j * nx + i], col(j * nx + i));
  // top: counter-clockwise seen from above (normal +Z)
  for (let j = 0; j + 1 < ny; j++)
    for (let i = 0; i + 1 < nx; i++) {
      const a = j * nx + i, r = a + 1, d = a + nx, c = d + 1;
      b.tri(a, r, c);
      b.tri(a, c, d);
    }
  // boundary loop, counter-clockwise seen from above
  const loop = [];
  for (let i = 0; i < nx - 1; i++) loop.push(i);
  for (let j = 0; j < ny - 1; j++) loop.push(j * nx + nx - 1);
  for (let i = nx - 1; i > 0; i--) loop.push((ny - 1) * nx + i);
  for (let j = ny - 1; j > 0; j--) loop.push(j * nx);
  // (Mar: "this operation colored the wrong thing on the stock": a column cut at its top
  // tinted its whole side down to the vise) the sides: a thin band under each top in its
  // colour (the cut's edge), below that the stock as it came (cutting only lowers a top: its
  // side below was never cut). `wallMid` pairs each top vertex with the band's lower edge, to
  // move with it.
  const side = uncut ? () => uncut : col;
  const mid = loop.map((k) => b.vertex(b.pos[3 * k], b.pos[3 * k + 1], Math.max(bottom, b.pos[3 * k + 2] - WALL_BAND), side(k)));
  const low = loop.map((k) => b.vertex(b.pos[3 * k], b.pos[3 * k + 1], bottom, side(k)));
  for (let t = 0; t < loop.length; t++) {
    const u = (t + 1) % loop.length;
    const [p, q, P, Q] = [loop[t], loop[u], mid[t], mid[u]];
    b.tri(q, p, P);
    b.tri(q, P, Q);
    const [m0, m1, L0, L1] = [mid[t], mid[u], low[t], low[u]];
    b.tri(m1, m0, L0);
    b.tri(m1, L0, L1);
  }
  // bottom: fan from the centre (normal -Z)
  const centre = b.vertex(x0 + (nx * cell) / 2, y0 + (ny * cell) / 2, bottom, side(0));
  for (let t = 0; t < low.length; t++) b.tri(centre, low[(t + 1) % low.length], low[t]);
  const out = b.done();
  out.wallMid = new Int32Array(loop.length * 2);
  loop.forEach((k, t) => {
    out.wallMid[2 * t] = k;
    out.wallMid[2 * t + 1] = mid[t];
  });
  return out;
}
/** How deep a cut's colour runs down the block's sides (mm): its edge. */
export const WALL_BAND = 0.2;

/**
 * Check a mesh is a closed, consistently oriented surface: after welding equal positions,
 * every edge is used exactly twice, once in each direction. Returns { closed, volume,
 * badEdges, unpaired, triangles }: `unpaired` counts only holes and misoriented faces (an edge four
 * faces share, where two surfaces of a solid touch, is bad but paired).
 */
export function checkClosed({ positions, indices }) {
  const key = (i) => `${positions[3 * i].toFixed(5)},${positions[3 * i + 1].toFixed(5)},${positions[3 * i + 2].toFixed(5)}`;
  const weld = new Map();
  const id = new Int32Array(positions.length / 3);
  for (let i = 0; i < id.length; i++) {
    const k = key(i);
    if (!weld.has(k)) weld.set(k, weld.size);
    id[i] = weld.get(k);
  }
  const edges = new Map();
  let volume = 0;
  for (let t = 0; t < indices.length; t += 3) {
    const v = [indices[t], indices[t + 1], indices[t + 2]];
    const w = v.map((i) => id[i]);
    if (w[0] === w[1] || w[1] === w[2] || w[0] === w[2]) continue; // degenerate
    for (let e = 0; e < 3; e++) {
      const a = w[e], c = w[(e + 1) % 3];
      const k = `${a}>${c}`;
      edges.set(k, (edges.get(k) || 0) + 1);
    }
    const p = v.map((i) => [positions[3 * i], positions[3 * i + 1], positions[3 * i + 2]]);
    volume += (p[0][0] * (p[1][1] * p[2][2] - p[1][2] * p[2][1]) - p[0][1] * (p[1][0] * p[2][2] - p[1][2] * p[2][0]) + p[0][2] * (p[1][0] * p[2][1] - p[1][1] * p[2][0])) / 6;
  }
  let bad = 0;
  // (unpaired: an edge not met as often the other way round, a hole or a face turned the
  // wrong way; an edge four faces share, where two surfaces of a solid touch, is paired)
  let unpaired = 0;
  for (const [k, n] of edges) {
    const [a, c] = k.split(">");
    if (n !== 1 || edges.get(`${c}>${a}`) !== 1) bad++;
    if (edges.get(`${c}>${a}`) !== n) unpaired++;
  }
  return { closed: bad === 0, volume, badEdges: bad, unpaired, triangles: indices.length / 3 };
}

// ---------------------------------------------------------------- the stock's look
import { colorNum } from "./colors.js";
/** Stock colours: a light blue tint (readable on the dark background and over the grey part)
 *  and a brighter edge outline (the defaults: View > Colours changes them, see colors.js).
 *  Remaining stock in simulations uses STOCK_LEFT. */
export const STOCK_TINT = 0x7fb2e5;
export const STOCK_EDGE = 0xa5d0ff;
export const STOCK_LEFT = [0.58, 0.7, 0.86];

/** The stock as it is shown: the tinted solid (see-through) with its edges drawn. */
export function stockObject(THREE, geometry, opacity = 0.32) {
  const grp = new THREE.Group();
  grp.add(new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({ color: colorNum("stock"), transparent: true, opacity, depthWrite: false, roughness: 0.6 })));
  // outline: the box's edges, a bar's two end circles and its silhouette seams
  grp.add(new THREE.LineSegments(new THREE.EdgesGeometry(geometry, 25), new THREE.LineBasicMaterial({ color: colorNum("stockEdge"), transparent: true, opacity: 0.85 })));
  return grp;
}
