// Runs the marcam WebAssembly engine off the main thread.
// Messages in:  { id, cmd, args }   (cmd: version | exampleJob | machine | planAndPost | simulate | meshTriangles)
// Messages out: { id, ok, result } or { id, ok: false, error }
//
// The large arrays a result carries (base64 text, or long lists of numbers) are turned into
// typed arrays here and handed over without a copy (transferred): decoding or cloning them
// on the main thread took over a second at a time, and the view stalled meanwhile.
import { load } from "./pkg/marcam.js";

// (where a long call is: told to the page as it goes, for its diagnostics)
let current = null;
const ready = load(new URL("./pkg/marcam.wasm", import.meta.url), { onPhase: (phase) => current != null && self.postMessage({ id: current, phase }) });

/** base64 text -> bytes (a plain loop: Uint8Array.from with a mapper is far slower). */
function bytes(s) {
  const bin = atob(s);
  const n = bin.length;
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** The fields of each command's result that are base64 arrays, and those that are long
 *  lists of numbers (kept as float32: drawing and simulation display only). */
const BASE64 = {
  gpuOpen: [["input", "words"], ["input", "seg_start"], ["input", "seg_rapid"], ["keys"]],
  simOpen: [["path"], ["times"], ["moves"]],
  simSurface: [["positions"], ["indices"], ["left"]],
  simSeek: [["r"], ["z"], ["op"]],
  simExact: [["pieces"], ["nodes"], ["prims"]],
  faceRegions: [["triangles"], ["region_of"]],
  carriedMesh: [["stl"]],
};

/** A mesh's vertex normals (each the sum of its triangles' area-weighted normals, unit), as
 *  three.js's computeVertexNormals makes them. */
function vertexNormals(p, ix) {
  const n = new Float32Array(p.length);
  for (let t = 0; t < ix.length; t += 3) {
    const a = 3 * ix[t], b = 3 * ix[t + 1], c = 3 * ix[t + 2];
    const ux = p[b] - p[a], uy = p[b + 1] - p[a + 1], uz = p[b + 2] - p[a + 2];
    const vx = p[c] - p[a], vy = p[c + 1] - p[a + 1], vz = p[c + 2] - p[a + 2];
    const nx = uy * vz - uz * vy, ny = uz * vx - ux * vz, nz = ux * vy - uy * vx;
    for (const k of [a, b, c]) {
      n[k] += nx;
      n[k + 1] += ny;
      n[k + 2] += nz;
    }
  }
  for (let k = 0; k < n.length; k += 3) {
    const l = Math.hypot(n[k], n[k + 1], n[k + 2]) || 1;
    n[k] /= l;
    n[k + 1] /= l;
    n[k + 2] /= l;
  }
  return n;
}

function prepare(cmd, result, transfer) {
  // (the playback's stock in 3D: its tiles decoded and their normals made here, not on the
  // main thread, where a scrub's tiles held the view 100 ms and more)
  const tiles = cmd === "simSeek" ? result.tiles : cmd === "simOpen" ? result.solid?.tiles : null;
  for (const t of tiles || []) {
    if (typeof t.positions === "string") {
      const b = bytes(t.positions);
      t.positions = new Float32Array(b.buffer, 0, b.byteLength / 4);
    }
    if (typeof t.indices === "string") {
      const b = bytes(t.indices);
      t.indices = new Uint32Array(b.buffer, 0, b.byteLength / 4);
    }
    if (typeof t.ops === "string") {
      const b = bytes(t.ops);
      t.ops = new Uint16Array(b.buffer, 0, b.byteLength / 2);
    }
    t.normals = vertexNormals(t.positions, t.indices);
    // (the cut surface's own normals where the engine kept them: the tool's, not the grid's
    // facets; the triangles' where it sent zero)
    if (typeof t.shade === "string") {
      const b = bytes(t.shade);
      const s = new Float32Array(b.buffer, 0, b.byteLength / 4);
      if (s.length === t.normals.length) {
        for (let k = 0; k < s.length; k += 3) {
          if (s[k] !== 0 || s[k + 1] !== 0 || s[k + 2] !== 0) t.normals.set(s.subarray(k, k + 3), k);
        }
      }
      delete t.shade;
    }
    for (const a of [t.positions, t.indices, t.ops, t.normals]) if (a?.buffer && !transfer.includes(a.buffer)) transfer.push(a.buffer);
  }
  const at = (path) => {
    let o = result;
    for (const k of path.slice(0, -1)) o = o?.[k];
    return [o, path[path.length - 1]];
  };
  for (const path of BASE64[cmd] || []) {
    const [o, k] = at(path);
    if (o && typeof o[k] === "string") {
      o[k] = bytes(o[k]);
      transfer.push(o[k].buffer);
    }
  }
  const floats = (o, k) => {
    const a = o?.[k];
    if (!Array.isArray(a) || a.length < 1024) return;
    for (let i = 0; i < a.length; i++) if (typeof a[i] !== "number") return;
    o[k] = Float32Array.from(a);
    transfer.push(o[k].buffer);
  };
  if (cmd === "meshTriangles" || cmd === "carriedMesh") floats(result, "triangles");
  if (cmd === "simulate" || cmd === "gpuStats") {
    for (const s of result.stocks || []) {
      floats(s, "r");
      floats(s, "z");
    }
    floats(result, "part");
  }
  if (cmd === "planAndPost")
    for (const p of result.programs || []) {
      floats(p.toolpath, "points");
      // (each move's G-code line, base64)
      if (typeof p.move_lines === "string") {
        p.move_lines = bytes(p.move_lines);
        transfer.push(p.move_lines.buffer);
      }
    }
}

self.onmessage = async (ev) => {
  const { id, cmd, args } = ev.data;
  try {
    const cam = await ready;
    if (typeof cam[cmd] !== "function") throw new Error(`unknown command ${cmd}`);
    const t0 = performance.now();
    current = id;
    let result;
    try {
      result = cam[cmd](...(args || []));
    } finally {
      current = null;
    }
    if (result && result.error) {
      self.postMessage({ id, ok: false, error: result.error });
    } else {
      const transfer = [];
      if (result && typeof result === "object") prepare(cmd, result, transfer);
      self.postMessage({ id, ok: true, result, ms: performance.now() - t0 }, transfer);
    }
  } catch (e) {
    self.postMessage({ id, ok: false, error: String(e && e.message ? e.message : e) });
  }
};
