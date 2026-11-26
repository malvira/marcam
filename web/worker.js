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
  simSeek: [["r"], ["z"], ["op"]],
  faceRegions: [["triangles"], ["region_of"]],
  carriedMesh: [["stl"]],
};

function prepare(cmd, result, transfer) {
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
