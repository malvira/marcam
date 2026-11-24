// The carry-through's finished part, as a binary STL (Mar: "Import a model of the finished
// part ... generate the STL as a test fixture"): a Ø20 bar standing along Z, 60 long, its top
// turned down to Ø18 for 10 mm, and on it a 12 mm square boss 6 tall with a 6 x 6 pocket 3
// deep. Z0 at the bar's foot; the boss's top at Z60. One closed, outward-facing surface.

const R_BAR = 10, R_OD = 9, LEN = 60, BOSS = 6, BOSS_H = 6, POCKET = 3, POCKET_D = 3, OD_H = 10;

/** The triangles, each [[x,y,z] x3], counter-clockwise seen from outside. */
export function finishedPartTriangles(n = 96) {
  const T = [];
  const tri = (a, b, c) => T.push([a, b, c]);
  const quad = (a, b, c, d) => (tri(a, b, c), tri(a, c, d));
  const zTop = LEN, zBoss = LEN - BOSS_H, zStep = LEN - BOSS_H - OD_H, zPocket = LEN - POCKET_D;
  // the angles: n even steps (n a multiple of 8, so the square's corners at 45° are among them)
  const ang = Array.from({ length: n }, (_, i) => (2 * Math.PI * i) / n);
  const circ = (r, a, z) => [r * Math.cos(a), r * Math.sin(a), z];
  // the square of half-side h at angle a: where the ray from the axis meets it
  const sq = (h, a, z) => {
    const c = Math.cos(a), s = Math.sin(a), k = h / Math.max(Math.abs(c), Math.abs(s));
    return [k * c, k * s, z];
  };
  for (let i = 0; i < n; i++) {
    const a = ang[i], b = ang[(i + 1) % n];
    // the bar's wall, Z0 to the step
    quad(circ(R_BAR, a, 0), circ(R_BAR, b, 0), circ(R_BAR, b, zStep), circ(R_BAR, a, zStep));
    // its foot (facing down)
    tri([0, 0, 0], circ(R_BAR, b, 0), circ(R_BAR, a, 0));
    // the step (facing up): the ring from Ø18 to Ø20
    quad(circ(R_OD, a, zStep), circ(R_BAR, a, zStep), circ(R_BAR, b, zStep), circ(R_OD, b, zStep));
    // the turned OD's wall, the step to the boss's foot
    quad(circ(R_OD, a, zStep), circ(R_OD, b, zStep), circ(R_OD, b, zBoss), circ(R_OD, a, zBoss));
    // the boss's foot (facing up): from the square to Ø18
    quad(sq(BOSS, a, zBoss), circ(R_OD, a, zBoss), circ(R_OD, b, zBoss), sq(BOSS, b, zBoss));
    // the boss's walls (the square's sides, a step at a time: straight between the samples)
    quad(sq(BOSS, a, zBoss), sq(BOSS, b, zBoss), sq(BOSS, b, zTop), sq(BOSS, a, zTop));
    // its top (facing up): from the pocket's square to the boss's
    quad(sq(POCKET, a, zTop), sq(BOSS, a, zTop), sq(BOSS, b, zTop), sq(POCKET, b, zTop));
    // the pocket's walls (facing in) and its floor (facing up)
    quad(sq(POCKET, a, zTop), sq(POCKET, b, zTop), sq(POCKET, b, zPocket), sq(POCKET, a, zPocket));
    tri([0, 0, zPocket], sq(POCKET, a, zPocket), sq(POCKET, b, zPocket));
  }
  return T;
}

/** As a binary STL (a Buffer). */
export function finishedPartStl(n = 96) {
  const T = finishedPartTriangles(n);
  const buf = Buffer.alloc(84 + 50 * T.length);
  buf.write("marcam test: finished carry-through part", 0, "ascii");
  buf.writeUInt32LE(T.length, 80);
  let o = 84;
  for (const [a, b, c] of T) {
    const u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]], v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    const nn = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    const l = Math.hypot(...nn) || 1;
    for (const x of [...nn.map((q) => q / l), ...a, ...b, ...c]) (buf.writeFloatLE(x, o), (o += 4));
    o += 2;
  }
  return buf;
}

/** Closed and outward: every edge used twice, once each way; the volume positive. */
export function checkClosed(T) {
  const key = (p) => p.map((x) => x.toFixed(5)).join(",");
  const edges = new Map();
  let vol = 0, degenerate = 0;
  for (const [a, b, c] of T) {
    const u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]], v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    if (Math.hypot(u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]) < 1e-9) degenerate++;
    for (const [p, q] of [[a, b], [b, c], [c, a]]) {
      const k = `${key(p)}>${key(q)}`;
      edges.set(k, (edges.get(k) || 0) + 1);
    }
    vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6;
  }
  let open = 0;
  for (const [k, n] of edges) {
    const [p, q] = k.split(">");
    if (n !== 1 || edges.get(`${q}>${p}`) !== 1) open++;
  }
  return { triangles: T.length, open, degenerate, volume: vol };
}
