// marcam web UI: job editor -> engine (WebAssembly, in a worker) -> G-code, checks, previews.

import { blockSolid, cylinder, heightmapSolid, revolveProfile, stockObject, STOCK_LEFT } from "./meshes.js";
import { gpuDevice, simulateOnGpu } from "./gpu/gpusim.js";
import { setupMachining } from "./machining.js";
import { addViewCube } from "./viewcube.js";
import { setupToolDialogs } from "./tooldialog.js";
import { splitter } from "./splitters.js";
import { setupTree } from "./tree.js";
import { jawGrip, lyingBar, meshFileOf, setupProject } from "./project.js";
import { setupHandles } from "./handles.js";
import { calcFeeds, feedsSummary } from "./feeds.js";
import { color, colorNum, colorRgb, COLOR_DEFAULTS, COLOR_LABELS, onColorsChange, resetColors, setColor } from "./colors.js";

// Settings saved before the rename (4cam -> marcam) carry over once: "4cam.x" -> "marcam.x".
try {
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k && k.startsWith("4cam.") && localStorage.getItem("marcam." + k.slice(5)) === null) localStorage.setItem("marcam." + k.slice(5), localStorage.getItem(k));
  }
} catch {
  // storage blocked: nothing to carry over
}

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- engine (worker RPC)
// Two workers, each with the engine: one plans and answers the editor (forms, picks, the
// part), the other simulates (the batch simulation, the playback), so a simulation running
// in the background never holds up an edit. A cancel ends the simulating one only.
const SIM_WORKER = new Set(["simulate", "simOpen", "simSeek", "simClose", "gpuOpen", "gpuStats"]);
const workers = { main: null, sim: null };
let nextId = 1;
const pending = new Map();
/** A worker: started at load, and the simulating one again after a cancel (which ends it,
 *  with whatever it was doing: the only way to stop a call in WebAssembly). */
function startWorker(which = "main") {
  const w = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  w.onmessage = (ev) => {
    const { id, ok, result, error, ms } = ev.data;
    const p = pending.get(id);
    if (!p) return;
    // (where the call is, as it goes: for the diagnostics)
    if (ev.data.phase) {
      p.phase = ev.data.phase;
      p.phaseT = performance.now();
      return;
    }
    pending.delete(id);
    simCallDone(p.cmd);
    ok ? p.resolve({ result, ms }) : p.reject(new Error(error));
  };
  w.onerror = (ev) => {
    // (kept for diagnostics: a worker that dies leaves its calls unanswered)
    (window.__marcam.workerErrors ||= []).push(`${which}: ${ev.message || "error"}`);
    setStatus(`Engine failed to start: ${ev.message || "see console"}`, "error");
  };
  workers[which] = w;
}
startWorker("main");
startWorker("sim");
/** Stop what the engine is simulating (a long simulation): its worker is ended and a new
 *  one started; every simulation call waiting on it fails with "cancelled". Its playback
 *  goes with it (made again when next needed); planning goes on in the other. */
function cancelEngine(why = "cancelled") {
  simLock.drop();
  workers.sim.terminate();
  const waiting = [...pending.entries()].filter(([, p]) => p.worker === "sim");
  for (const [id] of waiting) pending.delete(id);
  simRing.active = 0;
  showSimRing();
  startWorker("sim");
  state.epoch = (state.epoch || 0) + 1;
  window.__marcam.machining?.lost?.(why);
  // ("superseded": a project opened or started meanwhile; nothing to say about it)
  for (const [, p] of waiting) p.reject(new Error(why));
}

// ---- the simulation's progress: a ring in a corner of the 3D view while the engine
// simulates (from its time estimate); a click on it cancels
const SIM_CMDS = new Set(["simulate", "simOpen", "simSeek", "gpuOpen"]);
const simRing = { active: 0, t0: 0, estimate: 0, timer: 0 };
function simCallStart(cmd) {
  if (!SIM_CMDS.has(cmd)) return;
  if (!simRing.active++) simRing.t0 = performance.now();
  if (!simRing.timer) simRing.timer = setInterval(showSimRing, 200);
}
function simCallDone(cmd) {
  if (!SIM_CMDS.has(cmd) || !simRing.active) return;
  simRing.active--;
  showSimRing();
}
function showSimRing() {
  let el = $("simRing");
  if (!simRing.active) {
    el?.classList.add("hidden");
    clearInterval(simRing.timer);
    simRing.timer = 0;
    return;
  }
  const s = (performance.now() - simRing.t0) / 1000;
  // shown once it takes a moment
  if (s < 0.4) return;
  if (!el) {
    el = document.createElement("button");
    el.id = "simRing";
    el.type = "button";
    el.className = "sim-ring";
    el.addEventListener("click", () => {
      cancelEngine();
      setStatus("Simulation cancelled", "done");
    });
    $("view3d").appendChild(el);
  }
  // from the estimate (never quite full until done); without one, a turning quarter
  const est = simRing.estimate;
  const f = est > 0 ? Math.min(0.97, s / est) : null;
  el.classList.remove("hidden");
  el.classList.toggle("spin", f === null);
  el.style.setProperty("--p", f === null ? "0.25" : f.toFixed(3));
  const left = est > 0 ? Math.max(0, Math.ceil(est - s)) : null;
  const label = `Simulating${f !== null ? ` (${Math.round(f * 100)}%, about ${left} s left)` : ""}: click to cancel`;
  el.title = label;
  el.setAttribute("aria-label", label);
}
/** Engine calls by command, since the page loaded (window.__marcam.calls). */
const calls = {};
const WITH_LIBRARIES = new Set(["opDefaults", "setMaterial", "simulate", "meshTriangles", "carriedMesh", "simGrid", "simOpen", "gpuOpen"]);
function engine(cmd, ...args) {
  calls[cmd] = (calls[cmd] || 0) + 1;
  // (Mar) setups with their own parts: the part file and theirs, given together
  if (state.extraMeshes?.size) args = args.map((a) => (a && a === state.mesh ? meshBundle() : a));
  // the person's tool libraries: their tools' cutting data for new operations' defaults
  // (and every command that reads the project: one of their tools is found in them)
  if (WITH_LIBRARIES.has(cmd) && args[1] && typeof args[1] === "object" && !args[1].libraries) args[1] = { ...args[1], libraries: libraries() };
  const id = nextId++;
  simCallStart(cmd);
  const which = SIM_WORKER.has(cmd) ? "sim" : "main";
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject, cmd, worker: which, t0: performance.now() });
    workers[which].postMessage({ id, cmd, args });
  });
}

// ---------------------------------------------------------------- state
// (bumped when a colour changes: see View > Colours)
let colorsGen = 0;
const state = {
  mesh: null, // Uint8Array
  meshName: "",
  result: null, // planAndPost result
  sim: null, // simulate result
  tab: 0,
  three: null, // lazily loaded three.js view
};
/** The engine calls not answered yet: command, worker and seconds waited (for diagnostics). */
const inFlight = () => [...pending.values()].map((p) => ({ cmd: p.cmd, worker: p.worker, s: Math.round((performance.now() - p.t0) / 100) / 10, ...(p.phase ? { phase: p.phase, phaseS: Math.round((performance.now() - p.phaseT) / 100) / 10 } : {}) }));
window.__marcam = { state, engine, calls, cancelEngine, simRing, inFlight };

function setStatus(text, mode = "") {
  $("status").textContent = text;
  document.body.dataset.state = mode;
}

function showError(msg) {
  const el = $("error");
  el.textContent = msg || "";
  el.classList.toggle("hidden", !msg);
}

// ---------------------------------------------------------------- job editing
function readJob() {
  try {
    return JSON.parse($("job").value);
  } catch (e) {
    throw new Error(`Job JSON is not valid: ${e.message}`);
  }
}

/** Write the project into the editor. An edit (the default) marks the project unsaved and
 *  its results stale; `{ display: true }` (a colour, a name) only unsaved; `{ load: true }` is
 *  a project being opened or reset. */
function writeJob(job, opts = {}) {
  $("job").value = JSON.stringify(job, null, 2);
  // (an opened project: the undo history starts here)
  if (opts.load) window.__marcam.project?.resetHistory?.();
  else window.__marcam.project?.changed({ stale: !opts.display });
}

/** The Generate button asks for attention when the results are out of date or missing. */
/** Whether the project has an operation to plan (one that is enabled). */
function hasEnabledOps(j) {
  // (an operation in error is not planned: its picked features are gone)
  // (nor one whose tool the project no longer has)
  return j.schema_version === "0.2" ? !!j.setups?.some((s) => s.operations?.some((o) => o.enabled !== false && !o.error && (j.tools || []).some((x) => x.number === o.tool))) : !!j.programs?.some((p) => p.operations?.length);
}

/** Whether the project has a part (a model, a mesh or a profile). */
function hasPart(j) {
  return j.schema_version === "0.2" ? !!j.models?.length : !!(j.part?.profile || j.part?.mesh);
}

function updateGenerateButton() {
  let ops = false;
  let reason = "";
  try {
    const j = readJob();
    ops = hasEnabledOps(j);
    if (!ops) reason = hasPart(j) ? "No operation to generate: add one in the project tree (or enable one)" : "Import an STL first";
  } catch {}
  $("generate").classList.toggle("attention", ops && (state.stale || !state.result));
  $("generate").title = !ops ? reason : state.stale ? "The project changed: generate the toolpaths again" : "";
  // nothing to plan: disabled (unless a generate or simulation holds it)
  if (document.body.dataset.state !== "busy") $("generate").disabled = !ops && !!state.registry;
}

// ---------------------------------------------------------------- live updating
// On by default (Tools > Update automatically): an edit plans again 500 ms after the last
// one, never while a mouse button is held (a slider or a drag waits for its release); only
// the operations it touched are planned (the engine's plan cache), and a newer request
// supersedes one still running (its result is dropped). Each operation in the tree shows
// whether it is planned, waiting or being planned (no "out of date" marks), and exports wait
// for the current results.
const AUTO_DELAY = 500;
let autoTimer = null;
let autoPending = false;
let pointerHeld = false;
function autoOn() {
  return $("autoGen").checked;
}
function scheduleAuto() {
  if (!autoOn()) return;
  clearTimeout(autoTimer);
  if (pointerHeld) {
    autoPending = true;
    return;
  }
  autoPending = false;
  autoTimer = setTimeout(() => {
    autoTimer = null;
    generate({ auto: true });
  }, AUTO_DELAY);
  window.__marcam.tree?.renderStates?.();
}

// ---- when the results are current: nothing waiting, nothing being planned
let settleWaiters = [];
function planIdle() {
  return !autoTimer && !autoPending && !state.planning;
}
/** Resolves once the results match the project (at once when they do). */
function whenSettled() {
  return planIdle() ? Promise.resolve() : new Promise((r) => settleWaiters.push(r));
}
function checkSettled() {
  if (!planIdle()) return;
  const w = settleWaiters;
  settleWaiters = [];
  w.forEach((f) => f());
}

// ---- each operation's state, for the tree's badges
/** What of a setup decides its operations' toolpaths besides the operations themselves. */
function setupKey(doc, s) {
  if (!doc || !s) return "";
  const { operations, outputs, name, ...rest } = s;
  return JSON.stringify([rest, doc.tools, doc.models, doc.unindexed || [], outputs.map((o) => [o.name, o.operations, o.reset_a])]);
}
/** An operation as planned (display-only fields left out: a colour or a name plans nothing). */
function opKey(o) {
  if (!o) return "";
  const { name, color, ...rest } = o;
  return JSON.stringify(rest);
}
/** "planned", "pending" (changed: planned again in a moment), "planning", "error" or "none"
 *  (live updating off and never planned) for operation `opId` of setup `setupId`. */
function opState(setupId, opId) {
  let doc;
  try {
    doc = readJob();
  } catch {
    return "none";
  }
  const s = doc.setups?.find((x) => x.id === setupId);
  const op = s?.operations?.find((o) => o.id === opId);
  if (!op) return "none";
  if (op.enabled === false) return "off";
  const same = (planned) => {
    const ps = planned?.setups?.find((x) => x.id === setupId);
    return !!ps && setupKey(planned, ps) === setupKey(doc, s) && opKey(ps.operations.find((o) => o.id === opId)) === opKey(op);
  };
  if (state.planError && same(state.planError.doc)) return "error";
  if (state.planning && same(state.planning)) return "planning";
  if (state.result && same(state.job)) return "planned";
  return autoOn() ? "pending" : "none";
}
window.addEventListener("pointerdown", () => {
  pointerHeld = true;
}, true);
const released = () => {
  pointerHeld = false;
  if (autoPending) scheduleAuto();
};
window.addEventListener("pointerup", released, true);
window.addEventListener("pointercancel", released, true);

/** The results no longer match the project: say so on them until the next Generate. */
function markStale() {
  let none = false;
  try {
    const j = readJob();
    none = !hasEnabledOps(j) && hasPart(j);
  } catch {}
  if (none) {
    // nothing left to plan: the toolpaths of what was there go; the part and stock stay
    if (state.result || !$("emptyState")) clearResults(NO_OPS, []);
    drawModel();
    updateGenerateButton();
    return;
  }
  drawModel();
  if (state.result && !state.stale) {
    state.stale = true;
    // live: the tree's badges say what is being planned again (the results stay as they
    // are until the new ones come); otherwise the results are marked out of date
    if (!autoOn()) {
      document.body.classList.add("stale");
      selectTab(state.tab);
    }
  }
  updateGenerateButton();
  scheduleAuto();
  window.__marcam.tree?.renderStates?.();
}

const RESULT_GROUPS = ["part", "stock", "path", "sim", "tool"];

// ---------------------------------------------------------------- toolpath visibility
// View only: hidden operations' moves are left out of the 3D and side views. Nothing is
// planned or marked out of date, and the G-code is the same. Keys are "setup/operation";
// kept in the project file (its view) and in this browser.
state.hidden = new Set();
function hiddenStoreKey() {
  return `marcam.hidden:${window.__marcam?.project?.state?.name || "untitled"}`;
}
function setHidden(keys, opts = {}) {
  state.hidden = new Set(keys);
  if (opts.persist !== false) {
    try {
      localStorage.setItem(hiddenStoreKey(), JSON.stringify([...state.hidden]));
    } catch {
      // storage blocked: this session only
    }
  }
  const v = state.three;
  if (v && !v.failed) v.key = null;
  if (state.result) {
    updateView();
    drawSide();
  }
  window.__marcam.tree?.renderEyes?.();
  // the simulations leave hidden operations out: done again (nothing is planned again)
  window.__marcam.machining?.applyHidden?.();
  if (state.sim && opts.resimulate !== false) resimulate();
}

/** The simulated stock again (the eyes changed): the toolpaths stay, only what is cut. */
let resimTimer = null;
function resimulate() {
  clearTimeout(resimTimer);
  resimTimer = setTimeout(() => {
    // (a view action: in the background, the status line keeps the job's times)
    if (document.body.dataset.state !== "busy" && state.result) simLock.run("batch", () => simulateLocked({ quiet: true }), { quiet: true });
  }, 200);
}
/** Each preview point of program `p`: whether its operation is hidden (null: none is). */
function hiddenMask(p) {
  const ops = p.toolpath.ops || [];
  if (!state.hidden.size || !ops.length) return null;
  const setupId = state.tree?.setup;
  // a 0.1 job shown as a project: operations by planning order
  const s = state.tree?.doc?.setups?.find((x) => x.id === setupId);
  const planned = s ? s.outputs.flatMap((o) => o.operations).filter((id) => s.operations.find((op) => op.id === id)?.enabled !== false) : [];
  const n = p.toolpath.points.length / 4;
  const mask = new Uint8Array(n);
  ops.forEach((o, k) => {
    const id = o.id ?? planned[o.index];
    if (!state.hidden.has(`${setupId}/${id}`)) return;
    const end = k + 1 < ops.length ? ops[k + 1].start : n;
    // the move into the operation's first point belongs to it too
    mask.fill(1, o.start, end);
  });
  return mask;
}
const NO_OPS = "No operations: add one in the project tree (or use a detected feature in the setup), then Generate.";

/**
 * The one reset that New, Open and the examples go through: nothing of the previous project
 * survives. A generate or simulation still running is dropped when it returns; the auto
 * regenerate is cancelled; the machining playback closes and its meshes, GPU stock and the
 * worker's sessions are freed; face picking stops and its highlight is disposed; the tree,
 * the results (G-code, estimates, side view, toolpaths, part, stock, sim), the warnings, the
 * undo and the mesh are cleared; the camera refits on the next project.
 */
async function resetProject(msg, actions) {
  // a simulation of the project going (the background one after a plan) is stopped: the
  // engine works for the new one at once (its part drawn, its plan) instead of after it
  if (simRing.active) cancelEngine("superseded");
  state.genRun = (state.genRun || 0) + 1;
  state.epoch = (state.epoch || 0) + 1;
  clearTimeout(autoTimer);
  autoTimer = null;
  autoPending = false;
  window.__marcam.machining?.reset();
  window.__marcam.tree?.reset();
  window.__marcam.project?.dropUndo();
  // (the setups' own part files go with the project)
  state.extraMeshes = new Map();
  state.meshesGen++;
  try {
    await engine("simClose");
  } catch {
    // an engine without sessions (not loaded yet): nothing to free
  }
  state.simGrid = null;
  state.hidden = new Set();
  setMesh(null, "");
  showError("");
  clearResults(msg, actions);
}

/** No results (a new or just opened project); `msg`, if given, says what to do. */
function clearResults(msg, actions = ["import", "open"]) {
  // (results cleared because the project needs planning: live, it is planned now)
  if (actions?.includes("generate") && autoOn()) setTimeout(liveLoaded, 0);
  // the viewer's playback goes with them
  window.__marcam.machining?.reset?.();
  state.result = null;
  state.sim = null;
  state.job = null;
  state.stale = false;
  document.body.classList.remove("stale");
  $("tabs").innerHTML = "";
  const btn = { import: '<button type="button" data-empty="import" class="primary">Import STL...</button>', open: '<button type="button" data-empty="open">Open project...</button>', generate: '<button type="button" data-empty="generate" class="primary" id="emptyGenerate">Generate</button>' };
  $("summary").innerHTML = msg ? `<div class="empty-state" id="emptyState">${msg}<br>${actions.map((k) => btn[k]).join("")}</div>` : "";
  $("gcode").value = "";
  $("codeInfo").textContent = "";
  $("download").disabled = true;
  $("scrub").max = 0;
  $("scrub").value = 0;
  $("scrub").disabled = true;
  $("scrubLabel").textContent = "";
  const v = state.three;
  if (v && !v.failed) {
    for (const k of RESULT_GROUPS) clear(v.groups[k]);
    v.key = null;
    v.modelKey = null;
    v.simKey = null;
    v.fitted = null;
    v.renderer.render(v.scene, v.camera);
  }
  const ctx = $("side").getContext("2d");
  ctx.clearRect(0, 0, $("side").width, $("side").height);
  updateGenerateButton();
  drawModel();
}

/** Everything that shows the project, again: tools, the tree, the grid. */
async function refreshAll() {
  renderTools();
  await window.__marcam.tree?.refresh();
  updateSimResInfo();
  updateGenerateButton();
  await drawModel();
}

async function loadExample(name) {
  showError("");
  // busy until it is in: Generate meanwhile would plan the project being replaced
  if (state.registry) setStatus(`Loading the ${name} example...`, "busy");
  $("generate").disabled = $("simulate").disabled = true;
  try {
    await loadExampleInto(name);
  } finally {
    if (state.registry) $("generate").disabled = $("simulate").disabled = false;
  }
}

/** A project or job file's text (TOML or JSON) as the document object the page keeps. */
async function parseProjectText(text) {
  const t = text.replace(/^﻿/, "").trimStart();
  if (t.startsWith("{")) return JSON.parse(t);
  // TOML: the engine reads it (the same document model)
  return (await engine("checkProject", text)).result.project;
}
window.__marcam.parseProjectText = parseProjectText;

async function loadExampleInto(name) {
  await resetProject("");
  if (name === "knob") {
    const { result } = await engine("exampleJob");
    writeJob(result, { load: true });
    setMesh(null, "");
  } else {
    const [file, mesh] = {
      plate: ["plate/plate.marcam.toml", "plate/dome.stl"],
      knob_indexed: ["knob/knob_indexed.marcam.toml", null],
      plaque: ["plaque/plaque.marcam.toml", "plaque/plaque.png"],
      ring: ["ring/ring.marcam.toml", null],
    }[name] || ["knob/knob_finish.job.json", "knob/knob.stl"];
    const [job, stl] = await Promise.all([
      fetch(`../examples/${file}`).then((r) => {
        if (!r.ok) throw new Error(`examples/${file} not found (serve the repository root)`);
        return r.text().then(parseProjectText);
      }),
      mesh ? fetch(`../examples/${mesh}`).then((r) => (r.ok ? r.arrayBuffer() : null)) : null,
    ]);
    writeJob(job, { load: true });
    const meshName = mesh?.split("/")[1];
    setMesh(stl ? new Uint8Array(stl) : null, stl ? `${meshName} (example)` : "");
  }
  // a 0.1 job names its machine; a project names one per setup
  const loaded = readJob();
  const jm = loaded.machine ?? loaded.setups?.[0]?.machine;
  if (jm) renderMachines(jm);
  renderTools();
  if (window.__marcam.project) {
    window.__marcam.project.state.fileName = null;
    window.__marcam.project.clean(loaded.name || `example: ${name}`);
  }
  clearResults("");
  await window.__marcam.tree?.refresh();
  updateSimResInfo();
  if (state.registry) setStatus(autoOn() ? `Loaded the ${name} example: planning it...` : `Loaded the ${name} example: Generate to plan it`, "ready");
  liveLoaded();
}

/** A project just in (opened, an example, a setup changed, undone): planned by itself when
 *  live updating is on. */
function liveLoaded() {
  let ops = false;
  try {
    ops = hasEnabledOps(readJob());
  } catch {}
  if (autoOn() && ops && state.registry) scheduleAuto();
}

/** The part files of setups with a part of their own (other than the project's part, the
 *  mesh below): file name -> { bytes (an STL), name (shown) }. */
state.extraMeshes = new Map();
state.meshesGen = 0;
let bundled = null;
/** The project's part file and the setups' own, as one argument for the engine (each model
 *  takes its file from it: see the engine's mesh bundle). */
function meshBundle() {
  if (bundled?.gen === state.meshesGen && bundled.mesh === state.mesh) return bundled.bytes;
  let main = "part.stl";
  try {
    main = meshFileOf(readJob()) || main;
  } catch {}
  const files = [[main, state.mesh], ...[...state.extraMeshes].map(([n, e]) => [n, e.bytes])].filter(([, b]) => b);
  const enc = new TextEncoder();
  const parts = [enc.encode("MARCAM-MESHES\n")];
  for (const [n, b] of files) {
    const nb = enc.encode(n);
    const len = (x) => new Uint8Array(new Uint32Array([x]).buffer);
    parts.push(len(nb.length), nb, len(b.length), b);
  }
  const out = new Uint8Array(parts.reduce((k, p) => k + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  bundled = { gen: state.meshesGen, mesh: state.mesh, bytes: out };
  return out;
}
/** The bytes of part file `file`: a setup's own, else the project's part. */
window.__marcam.meshFor = (file) => (file && state.extraMeshes.get(file)?.bytes) || state.mesh;

/** The part's mesh (an STL, with face numbers when it came from a STEP); `source`: the file it
 *  was made from, which the project saves instead ({ name, bytes }: a STEP). */
function setMesh(bytes, name, source = null) {
  state.meshesGen++;
  state.mesh = bytes;
  state.meshName = name;
  state.meshSource = source;
  window.__marcam.project?.meshSet?.();
  $("meshInfo").textContent = bytes ? `${name}: ${(bytes.length / 1024).toFixed(0)} KiB` : "No mesh loaded";
}

/** The machine for the engine: a built-in id, an imported profile object, or custom JSON. */
function machineArg() {
  const v = $("machine").value;
  if (v === "custom") return $("machineJson").value;
  if (v.startsWith("user:")) return userMachines().find((m) => `user:${m.id}` === v) || null;
  return v;
}

function currentMachine() {
  const v = $("machine").value;
  if (v.startsWith("user:")) return userMachines().find((m) => `user:${m.id}` === v);
  return state.registry.machines.find((m) => m.id === v);
}

// ---------------------------------------------------------------- generate
/** "1 h 02 min", "12 min 05 s", "48 s" (as the engine's CLI prints them). */
export function fmtTime(sec) {
  const s = Math.max(0, Math.round(sec));
  if (s >= 3600) return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600 + 30) / 60)).padStart(2, "0")} min`;
  if (s >= 60) return `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, "0")} s`;
  return `${s} s`;
}
window.__marcam = Object.assign(window.__marcam || {}, { fmtTime, setMesh, clearResults });

async function generate(opts = {}) {
  // (a click passes an event)
  if (opts instanceof Event) opts = {};
  showError("");
  let job;
  try {
    job = readJob();
  } catch (e) {
    showError(e.message);
    return;
  }
  if (!hasEnabledOps(job)) {
    showError(
      hasPart(job)
        ? "Nothing to generate: the project has no enabled operation. Add one in the project tree (or use a detected feature), or enable one."
        : "Nothing to generate yet: import an STL and add an operation in the project tree.",
    );
    return;
  }
  if (job.part?.mesh && !state.mesh) {
    showError(`This job uses the mesh "${job.part.mesh.file}". Choose it under "Part mesh".`);
    return;
  }
  $("generate").disabled = $("simulate").disabled = true;
  // the playback being made for the last plan by itself is stale now: stopped, so it does not
  // hold the machine while this one plans (a slow one took minutes on a slow PC)
  if (window.__marcam.machining?.state?.autoOpening) cancelEngine("replanning");
  setStatus(opts.auto ? "Updating the toolpaths..." : "Generating...", "busy");
  const run = (state.genRun = (state.genRun || 0) + 1);
  const hadSim = !!state.sim;
  state.planning = job;
  state.planError = null;
  window.__marcam.tree?.renderStates?.();
  try {
    const setupId = state.tree?.setup;
    const { result, ms } = await engine("planAndPost", job, machineArg(), state.mesh, { max_points: 150000, libraries: libraries(), setup: job.schema_version === "0.2" ? setupId : undefined });
    // an edit since then (auto-regenerate) has asked for a newer run: this one is dropped
    if (run !== state.genRun) return;
    state.result = result;
    state.sim = null;
    state.job = job;
    state.stale = false;
    document.body.classList.remove("stale");
    renderTabs();
    selectTab(0);
    const issues = result.programs.reduce((n, p) => n + p.issues.length, 0);
    // the tree shows each operation's and file's estimated time
    await window.__marcam.tree?.refresh();
    updateSimResInfo();
    // done only once everything is shown and the buttons work again
    $("generate").disabled = $("simulate").disabled = false;
    updateGenerateButton();
    // what was planned (operations unchanged since the last plan are reused)
    const pc = result.plan_cache;
    const what = pc && pc.reused ? ` (planned ${pc.planned} operation${pc.planned === 1 ? "" : "s"}, ${pc.reused} unchanged)` : "";
    setStatus(`${opts.auto ? "Updated" : "Generated"} ${result.programs.length} program(s) in ${(ms / 1000).toFixed(1)} s${what}, ${issues} issue(s); estimated machining time ${fmtTime(result.time_total_s)}`, "done");
    // the simulated stock was shown: simulated again in the background, coarse first
    // (once this run is over: after the `finally` below)
    if (hadSim && opts.auto) setTimeout(backgroundSimulate, 0);
    // the viewer (the playback) shows the new toolpaths, at the same place
    setTimeout(() => {
      if (run === state.genRun) window.__marcam.machining?.open({ auto: true });
    }, 0);
  } catch (e) {
    if (run === state.genRun) {
      state.planError = { doc: job, message: e.message };
      showError(e.message);
      setStatus("Failed", "error");
    }
  } finally {
    if (run === state.genRun) state.planning = null;
    $("generate").disabled = $("simulate").disabled = false;
    window.__marcam.tree?.renderStates?.();
    checkSettled();
  }
}

/** After a live update with the stock shown: simulated again, at Coarse at once, then at the
 *  chosen resolution when it is quick (no question asked in the background). */
async function backgroundSimulate() {
  // (the resolution passed along: the setting itself is the person's, never changed here)
  if ($("simRes").value !== "coarse") await simLock.run("batch", () => simulateQuiet(Infinity, "coarse"), { quiet: true });
  await simLock.run("batch", () => simulateQuiet(SIM_ASK_SECONDS), { quiet: true });
}

// ---------------------------------------------------------------- simulation resolution
/** The grid asked for: a preset, or custom cell sizes (rotary mm x degrees, 3-axis mm). */
function simOpts(res) {
  const v = res || $("simRes").value;
  // operations hidden in the tree are not simulated (their material stays)
  const hide = hiddenPlanned();
  // (the setup shown: without it the engine simulated the first setup, and its stock was
  // drawn in another's view; Mar's knob on end)
  const at = state.job?.schema_version === "0.2" && state.tree?.setup ? { setup: state.tree.setup } : {};
  if (v !== "custom") return { res: v, hide, ...at };
  const deg = Number($("simDeg").value) || 1.5;
  return { dx: Number($("simDx").value) || 0.1, na: Math.max(8, Math.round(360 / deg)), cell: Number($("simCell").value) || 0.25, hide, ...at };
}

/** The planning numbers of the operations hidden in the tree (those the engine planned: enabled,
 *  not in error, their tool in the project), in the setup the results are for. */
function hiddenPlanned() {
  if (!state.hidden?.size) return [];
  const j = state.job;
  const setupId = state.tree?.setup;
  const s = j?.schema_version === "0.2" ? j.setups?.find((x) => x.id === setupId) ?? j.setups?.[0] : null;
  if (!s) return [];
  const tools = new Set((j.tools || []).map((t) => t.number));
  const planned = s.outputs.flatMap((o) => o.operations).map((id) => s.operations.find((o) => o.id === id)).filter((o) => o && o.enabled !== false && !o.error && tools.has(o.tool));
  return planned.map((o, i) => (state.hidden.has(`${s.id}/${o.id}`) ? i : -1)).filter((i) => i >= 0);
}

function saveSimRes() {
  try {
    localStorage.setItem("marcam.simRes", JSON.stringify({ v: $("simRes").value, dx: $("simDx").value, deg: $("simDeg").value, cell: $("simCell").value, gpu: $("simGpu").checked }));
  } catch {}
}

/** The GPU to simulate on (null: the WASM CPU simulation) and why not, for the label. */
async function simGpu() {
  if (!$("simGpu").checked) return { gpu: null, why: "GPU off" };
  if (!navigator.gpu) return { gpu: null, why: "no WebGPU in this browser" };
  const gpu = await gpuDevice();
  return gpu ? { gpu, why: "" } : { gpu: null, why: "no GPU adapter" };
}

function showBackend(text) {
  state.simBackend = text;
  $("simBackend").textContent = text ? `last run: ${text}` : "";
}

function loadSimRes() {
  try {
    const s = JSON.parse(localStorage.getItem("marcam.simRes") || "null");
    if (s) {
      $("simRes").value = s.v;
      $("simDx").value = s.dx;
      $("simDeg").value = s.deg;
      $("simCell").value = s.cell;
      // (the GPU is not a saved setting: it is used when there is one; see ?gpu=off)
    }
  } catch {}
}

/** Show the grid the setting gives for this job: cells, memory, time against Normal. */
async function updateSimResInfo() {
  $("simResCustom").classList.toggle("hidden", $("simRes").value !== "custom");
  let job;
  try {
    job = readJob();
  } catch {
    return;
  }
  const base = { libraries: libraries(), setup: job.schema_version === "0.2" ? state.tree?.setup : undefined };
  try {
    const [{ result: g }, { result: n }] = await Promise.all([
      engine("simGrid", job, { ...base, ...simOpts() }, state.mesh),
      engine("simGrid", job, { ...base, res: "normal" }, state.mesh),
    ]);
    state.simGrid = g;
    const ratio = g.cells / Math.max(1, n.cells);
    const what = g.kind === "xyz" ? `${g.nx} x ${g.ny} cells of ${g.cell.toFixed(3)} mm` : `${g.nx} x ${g.na} cells (${g.dx} mm x ${(360 / g.na).toFixed(2)}\u00b0)`;
    const warn = g.too_large ? " - too fine (limit 40 million cells)" : g.cells > 5e6 ? " - very large: it may take minutes and a lot of memory" : "";
    $("simResInfo").textContent = `${what} = ${g.cells.toLocaleString("en")} cells, about ${Math.max(1, Math.round(g.engine_mb + g.display_mb))} MB, roughly ${ratio < 1.5 ? ratio.toFixed(1) : Math.round(ratio)}x the Normal time${warn}`;
    $("simResInfo").classList.toggle("error", !!warn);
  } catch {
    state.simGrid = null;
    $("simResInfo").textContent = "";
  }
}

const fmtSeconds = (s) => (s < 90 ? `${Math.max(1, Math.ceil(s))} s` : s < 5400 ? `${Math.round(s / 60)} min` : `${(s / 3600).toFixed(1)} h`);
/** Simulations expected to take longer than this (seconds) are asked about first. */
const SIM_ASK_SECONDS = 30;

/** Before a simulation: refuse grids over the limits (cells, time), ask about long ones
 *  (naming a quicker preset). Sets the progress ring's estimate. */
async function confirmSimRes(batch = false) {
  await updateSimResInfo();
  const g = state.simGrid;
  simRing.estimate = 0;
  if (!g) return true;
  if (g.too_large) {
    showError(`The simulation grid would have ${g.cells.toLocaleString("en")} cells (limit 40 million): choose a coarser setting.`);
    return false;
  }
  // how long: the engine's estimate for this job (the GPU is much quicker: not asked)
  let t = null;
  try {
    const job = readJob();
    const base = { libraries: libraries(), setup: job.schema_version === "0.2" ? state.tree?.setup : undefined, estimate: true };
    t = (await engine("simGrid", job, { ...base, ...simOpts() }, state.mesh)).result.time;
  } catch {}
  // (the batch simulation takes longer than a playback: the engine says by how much)
  const k = batch ? t?.batch_factor || 1 : 1;
  const s = t?.seconds * k;
  if (!(s > 0)) return true;
  simRing.estimate = s;
  const gpu = $("simGpu")?.checked && !!navigator.gpu;
  const res = $("simRes").value;
  const what = res === "custom" ? "this resolution" : res;
  const quicker = ["fine", "normal", "coarse"].find((r) => r !== res && t.presets?.[r] > 0 && k * t.presets[r] < Math.min(s, SIM_ASK_SECONDS));
  const alt = quicker ? ` (${quicker[0].toUpperCase() + quicker.slice(1)}: about ${fmtSeconds(k * t.presets[quicker])})` : "";
  if (s > t.max_seconds && !gpu) {
    showError(`Simulating this at ${what} would take about ${fmtSeconds(s)}: too long. Choose a coarser setting${alt}.`);
    return false;
  }
  if (s > SIM_ASK_SECONDS && !gpu) return window.confirm(`Simulating at ${what} will take about ${fmtSeconds(s)}${alt}. The ring in the corner of the 3D view shows it going; click it to cancel. Continue?`);
  return true;
}

for (const id of ["simRes", "simDx", "simDeg", "simCell", "simGpu"]) {
  $(id).addEventListener("change", () => {
    saveSimRes();
    updateSimResInfo();
    resimulateSoon();
  });
}
/** Another resolution (Mar: "the simulation should regenerate"): the stock simulated again at
 *  it in the background, nothing planned; the one shown stays until the new one is in, and a
 *  playback with the stock opens again at the same place. */
let resRunTimer = 0, resimPending = false;
function resimulateSoon() {
  clearTimeout(resRunTimer);
  resimPending = true;
  resRunTimer = setTimeout(async () => {
    try {
      if (!state.result || state.stale) return;
      const mach = window.__marcam.machining;
      if (state.sim || mach?.state?.view?.stock) await simLock.run("batch", () => simulateQuiet(), { quiet: true });
      if (mach?.state?.open && mach.state.expanded && mach.state.stock) await mach.open({ auto: true });
    } finally {
      resimPending = false;
    }
  }, 300);
}
window.__marcam.resimPending = () => resimPending;
loadSimRes();

/** One simulation at a time (the batch one, a playback opening: see machining.js): the next
 *  waits for the one running; a cancel drops the waiting ones too. */
const simLock = {
  busy: false,
  waiting: 0,
  gen: 0,
  tail: Promise.resolve(),
  /** Run `fn` once no other simulation runs (not at all if a cancel comes first). */
  run(kind, fn, opts = {}) {
    const gen = this.gen;
    // (the person is told they wait; the background is not shown waiting. What the status
    // said before comes back if the run says nothing itself.)
    const WAIT = "Waiting for the simulation that is running (click the ring in the 3D view to cancel it)";
    const before = [$("status").textContent, document.body.dataset.state];
    const told = this.busy && !opts.quiet;
    if (told) setStatus(WAIT, "busy");
    const restore = () => {
      if (told && $("status").textContent === WAIT) setStatus(before[0], before[1] === "busy" ? "done" : before[1]);
    };
    this.waiting++;
    const p = this.tail.then(async () => {
      this.waiting--;
      if (gen !== this.gen) return restore();
      this.busy = kind;
      try {
        return await fn();
      } finally {
        this.busy = false;
        restore();
      }
    });
    this.tail = p.catch(() => {});
    return p;
  },
  /** Drop the waiting ones (a cancel). */
  drop() {
    this.gen++;
  },
};

function simulate() {
  return simLock.run("batch", simulateLocked);
}

/** The batch simulation without asking (background): skipped when estimated longer than
 *  `max` seconds, or when the project changed meanwhile. */
async function simulateQuiet(max = Infinity, res) {
  // (the results' own job: an edit since then is not simulated, nor planned, here)
  if (!state.result || !state.job || state.stale || !planIdle()) return;
  try {
    const job = state.job;
    const base = { libraries: libraries(), setup: job.schema_version === "0.2" ? state.tree?.setup : undefined, estimate: true };
    const t = (await engine("simGrid", job, { ...base, ...simOpts(res) }, state.mesh)).result.time;
    const s = (t?.seconds || 0) * (t?.batch_factor || 1);
    if (s > max || s > (t?.max_seconds ?? Infinity)) return;
    simRing.estimate = s;
  } catch {
    return;
  }
  return simulateLocked({ quiet: true, res });
}

async function simulateLocked(opts = {}) {
  // (in the background, never a plan: the results went meanwhile, so nothing to simulate)
  if (!state.result && opts.quiet) return;
  if (!state.result) await generate();
  if (!state.result) return;
  // busy at once (the grid check below is asynchronous); not in the background
  if (!opts.quiet) setStatus("Checking the simulation grid...", "busy");
  if (!opts.quiet && !(await confirmSimRes(true))) {
    setStatus("Simulation not started", "done");
    return;
  }
  // (in the background: the buttons and the status line are left to what the person does)
  // the same simulation already done (in the background, after the plan): shown, not run again
  const simKey = JSON.stringify([simOpts(opts.res), $("simGpu")?.checked]);
  if (!opts.quiet && state.sim && state.simKey === simKey) {
    selectTab(state.tab);
    const s = state.sim.stocks[state.tab];
    setStatus("Simulated (done in the background after the plan)", "done");
    const hits = state.sim.stocks.reduce((n, st) => n + (st.collisions?.count || 0), 0);
    if (s && (s.rapid_hits || s.gouge_cells || hits)) showError(`Simulation: ${s.rapid_hits} rapid(s) through stock, ${s.gouge_cells} gouged cell(s), ${hits} holder/shank collision(s)`);
    return;
  }
  if (!opts.quiet) $("generate").disabled = $("simulate").disabled = true;
  const say = opts.quiet ? () => {} : setStatus;
  say("Simulating stock...", "busy");
  try {
    const t0 = performance.now();
    let result = null;
    const epoch = state.epoch;
    // (a plan landing meanwhile, of another setup or an edit: this one is not its stock)
    const forJob = state.job;
    const { gpu, why } = await simGpu();
    let backend = `CPU (${why})`;
    if (gpu) {
      try {
        const { result: open } = await engine("gpuOpen", state.job, { ...simOpts(opts.res), libraries: libraries() }, state.mesh);
        // (gpuOpen plans through the same cache and leaves the hidden operations out)
        result = await simulateOnGpu(gpu, open, async (o, bytes) => (await engine("gpuStats", o, bytes)).result);
        backend = `GPU (${gpu.name})`;
      } catch (e) {
        // the engine refuses some jobs on the GPU (indexed 3+1 moves): the CPU does them
        backend = `CPU (${e.message})`;
      }
    }
    if (!result) result = (await engine("simulate", state.job, simOpts(opts.res), state.mesh)).result;
    const ms = performance.now() - t0;
    if (epoch !== state.epoch || forJob !== state.job) return;
    showBackend(backend);
    result.stocks.forEach((st) => (st.hidden = hiddenPlanned().length));
    state.sim = result;
    state.simKey = simKey;
    selectTab(state.tab);
    const s = result.stocks[state.tab];
    say(`Simulated in ${(ms / 1000).toFixed(1)} s`, "done");
    const hits = result.stocks.reduce((n, st) => n + (st.collisions?.count || 0), 0);
    if (s && (s.rapid_hits || s.gouge_cells || hits)) showError(`Simulation: ${s.rapid_hits} rapid(s) through stock, ${s.gouge_cells} gouged cell(s), ${hits} holder/shank collision(s)`);
  } catch (e) {
    if (e.message === "superseded" || e.message === "replanning") {
      // (the project went, or a newer plan: its simulation with it)
    } else if (e.message === "cancelled") {
      if (!opts.quiet) setStatus("Simulation cancelled", "done");
    } else if (opts.quiet) {
      // (a background run that failed: the person asked for nothing, so nothing is shown)
      console.warn(`background simulation: ${e.message}`);
    } else {
      showError(e.message);
      setStatus("Simulation failed", "error");
    }
  } finally {
    if (!opts.quiet) $("generate").disabled = $("simulate").disabled = false;
  }
}

// ---------------------------------------------------------------- results
function collisionHtml(c) {
  if (!c) return "";
  if (!c.count) return `<div class="small" id="holderCheck">Holder/shank check: clear</div>`;
  const rows = c.collisions.slice(0, 20).map((k) =>
    `<li>line ${k.line ?? "?"}: T${k.tool} ${k.contact} ${k.depth.toFixed(3)} mm into the stock${k.rapid ? " (rapid)" : ""}</li>`).join("");
  return `<div class="issues bad" id="holderCheck">Holder/shank collision: ${c.count}${c.more ? "+" : ""} move(s), deepest ${c.max_depth.toFixed(3)} mm<ul>${rows}</ul></div>`;
}

function renderTabs() {
  const tabs = $("tabs");
  tabs.innerHTML = "";
  state.result.programs.forEach((p, i) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "tab");
    b.dataset.index = i;
    const n = p.issues.length + (p.collisions?.count ? 1 : 0);
    b.innerHTML = `${p.name}<span class="badge ${n ? "bad" : "ok"}">${n ? `${n} issue${n > 1 ? "s" : ""}` : "OK"}</span>`;
    b.addEventListener("click", () => selectTab(i));
    tabs.appendChild(b);
  });
}

function selectTab(i) {
  state.tab = i;
  const p = state.result.programs[i];
  for (const b of $("tabs").children) b.setAttribute("aria-selected", String(+b.dataset.index === i));
  // (a setup with no operation yet: nothing planned for it)
  if (!p) {
    $("summary").innerHTML = '<div class="muted">This setup has no operations yet: add one in the project tree.</div>';
    return;
  }
  const g = p.gouge;
  const sim = state.sim?.stocks?.[i];
  const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
  $("summary").innerHTML = `
    ${state.stale ? '<div class="stale-banner" id="staleBanner">Out of date: the project changed since these results were made. <button type="button" id="staleRegen">Generate again</button></div>' : ""}
    <div><strong>${esc(p.title)}</strong> &middot; <span id="lineCount">${p.lines.toLocaleString("en")}</span> lines
      &middot; ${state.result.machine}${p.tool ? ` &middot; T${p.tool[0]} ${esc(p.tool[1])}` : ""}</div>
    <div class="issues ${p.issues.length ? "bad" : "ok"}">${
      p.issues.length
        ? `${p.issues.length} verifier issue(s):<ul>${p.issues.slice(0, 200).map((s) => `<li>${esc(s)}</li>`).join("")}</ul>`
        : "Verifier: 0 issues (no fast plunges, no low rapids)"
    }</div>
    ${limitsHtml(p.name, esc)}
    ${p.warnings?.length ? `<div class="limits" id="toolWarnings">${p.warnings.map((w) => `<div>Warning: ${esc(w)}</div>`).join("")}</div>` : ""}
    ${p.time ? `<div class="small" id="timeEst">Estimated time: <strong>${fmtTime(p.time.total_s)}</strong> (cutting ${fmtTime(p.time.feed_s)}, rapids ${fmtTime(p.time.rapid_s)}, tool changes/dwells ${fmtTime(p.time.other_s)})${state.result.programs.length > 1 ? ` &middot; all files ${fmtTime(state.result.time_total_s)}` : ""}</div>` : ""}
    ${collisionHtml(p.collisions)}
    ${g && g.moves_checked ? `<div class="small">Gouge check: ${g.moves_checked.toLocaleString("en")} cut moves, deepest ${g.max_depth.toFixed(4)} mm into the part</div>` : ""}
    ${sim && !sim.samples ? `<div class="small muted">Not simulated: its tool has no full shape (a V-bit needs a diameter). The stock shown is from the programs before it.</div>` : ""}
    ${sim && sim.hidden ? `<div class="small muted" id="simHidden">${sim.hidden} operation${sim.hidden === 1 ? "" : "s"} hidden: not simulated (their material is left)</div>` : ""}
    ${sim ? `<div class="small">Simulated stock: material left max ${sim.max_left.toFixed(3)} mm, mean ${sim.mean_left.toFixed(3)} mm; deepest gouge ${sim.max_gouge.toFixed(3)} mm; ${sim.rapid_hits} rapid(s) through stock${sim.feature_cells ? `; ${sim.feature_cells.toLocaleString("en")} cells cut below the part on purpose (engraving, knurl, indexed features) not counted` : ""}</div>` : ""}`;
  $("gcode").value = p.gcode;
  $("codeInfo").textContent = `${p.name}.nc, ${(p.gcode.length / 1024).toFixed(0)} KiB`;
  $("download").disabled = false;
  const n = p.toolpath.points.length / 4;
  const sc = $("scrub");
  sc.max = String(n);
  sc.value = String(n);
  sc.disabled = n === 0;
  updateScrubLabel();
  drawSide();
  updateView();
}

async function download() {
  // the G-code of the project as it is: after any update waiting or running
  if (!planIdle()) {
    setStatus("Waiting for the toolpaths to update before saving the G-code...", "busy");
    await whenSettled();
  }
  const p = state.result?.programs[state.tab];
  if (!p) return;
  const url = URL.createObjectURL(new Blob([p.gcode], { type: "text/plain" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `${p.name}.nc`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function updateScrubLabel() {
  const p = state.result?.programs[state.tab];
  if (!p) return;
  const n = +$("scrub").value;
  const pts = p.toolpath.points;
  const q = 4 * (n - 1);
  const at = n <= 0 ? "" : pts[q + 3] >= 2
    ? `X${pts[q].toFixed(2)} Y${pts[q + 1].toFixed(2)} Z${pts[q + 2].toFixed(2)}`
    : `X${pts[q].toFixed(2)} Z${pts[q + 1].toFixed(2)} A${pts[q + 2].toFixed(1)}`;
  const thin = p.toolpath.thinned ? ` (thinned from ${p.toolpath.total.toLocaleString("en")})` : "";
  $("scrubLabel").textContent = `${n.toLocaleString("en")} / ${(pts.length / 4).toLocaleString("en")}${thin} ${at}`;
}

// ---------------------------------------------------------------- 2D side view
function drawSide() {
  const cv = $("side");
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth, h = cv.clientHeight;
  cv.width = w * dpr;
  cv.height = h * dpr;
  const ctx = cv.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const res = state.result;
  if (!res) return;
  const p = res.programs[state.tab];
  if (!p) return;
  const pts = p.toolpath.points;
  const n = +$("scrub").value;
  const sb = res.stock_box;
  const hide = hiddenMask(p);
  if (sb) return drawSideXYZ(ctx, w, h, p, pts, n, sb, hide);
  let x0 = Infinity, x1 = -Infinity;
  for (let i = 0; i < pts.length; i += 4) if (pts[i + 3] % 2) { x0 = Math.min(x0, pts[i]); x1 = Math.max(x1, pts[i]); }
  const prof = res.profile?.points || [];
  for (const [x] of prof) { x0 = Math.min(x0, x); x1 = Math.max(x1, x); }
  if (!Number.isFinite(x0)) { x0 = -1; x1 = 1; }
  const rTop = Math.max(p.safe_r, res.stock.radius) + 0.5;
  const pad = 30, sx = (x) => pad + ((x - x0) / (x1 - x0 || 1)) * (w - 2 * pad), sy = (r) => h - 18 - (r / rTop) * (h - 30);
  const css = getComputedStyle(document.body);
  ctx.font = "11px system-ui";
  ctx.fillStyle = css.getPropertyValue("--muted");
  ctx.fillText("side view: X vs radius (grey: part, line: stock)", 6, 12);
  if (prof.length) {
    ctx.fillStyle = "rgba(150,150,150,0.45)";
    ctx.beginPath();
    ctx.moveTo(sx(prof[0][0]), sy(0));
    for (const [x, r] of prof) ctx.lineTo(sx(x), sy(r));
    ctx.lineTo(sx(prof[prof.length - 1][0]), sy(0));
    ctx.fill();
  }
  ctx.strokeStyle = "rgba(120,120,120,0.8)";
  ctx.beginPath();
  ctx.moveTo(sx(x0), sy(res.stock.radius));
  ctx.lineTo(sx(x1), sy(res.stock.radius));
  ctx.stroke();
  ctx.lineWidth = 1;
  for (let i = 1; i < Math.min(n, pts.length / 4); i++) {
    const cut = pts[4 * i + 3];
    if (hide && hide[i]) continue;
    if (!cut && !$("showRapids").checked) continue;
    ctx.strokeStyle = cut ? "rgba(37,99,235,0.35)" : "rgba(220,38,38,0.35)";
    ctx.beginPath();
    ctx.moveTo(sx(pts[4 * (i - 1)]), sy(pts[4 * (i - 1) + 1]));
    ctx.lineTo(sx(pts[4 * i]), sy(pts[4 * i + 1]));
    ctx.stroke();
  }
}

/** Side view of a 3-axis program: X vs Z, with the stock block. */
function drawSideXYZ(ctx, w, h, p, pts, n, sb, hide) {
  const x0 = sb.min[0] - 3, x1 = sb.max[0] + 3;
  const z0 = sb.min[2] - 1, z1 = Math.max(p.safe_r, sb.max[2]) + 1;
  const pad = 30, sx = (x) => pad + ((x - x0) / (x1 - x0)) * (w - 2 * pad), sz = (z) => h - 18 - ((z - z0) / (z1 - z0)) * (h - 30);
  ctx.font = "11px system-ui";
  ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted");
  ctx.fillText("side view: X vs Z (box: stock)", 6, 12);
  ctx.fillStyle = "rgba(150,150,150,0.25)";
  ctx.fillRect(sx(sb.min[0]), sz(sb.max[2]), sx(sb.max[0]) - sx(sb.min[0]), sz(sb.min[2]) - sz(sb.max[2]));
  ctx.lineWidth = 1;
  for (let i = 1; i < Math.min(n, pts.length / 4); i++) {
    const cut = pts[4 * i + 3] % 2;
    if (hide && hide[i]) continue;
    if (!cut && !$("showRapids").checked) continue;
    ctx.strokeStyle = cut ? "rgba(37,99,235,0.35)" : "rgba(220,38,38,0.35)";
    ctx.beginPath();
    ctx.moveTo(sx(pts[4 * (i - 1)]), sz(pts[4 * (i - 1) + 2]));
    ctx.lineTo(sx(pts[4 * i]), sz(pts[4 * i + 2]));
    ctx.stroke();
  }
}

// ---------------------------------------------------------------- 3D view (three.js)
async function getThree() {
  if (state.three) return state.three;
  try {
    const THREE = await import("three");
    const { OrbitControls } = await import("three/addons/controls/OrbitControls.js");
    const host = $("view3d");
    const renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true });
    renderer.setPixelRatio(window.devicePixelRatio || 1);
    host.appendChild(renderer.domElement);
    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(40, 1, 0.1, 2000);
    camera.up.set(0, 0, 1);
    const controls = new OrbitControls(camera, renderer.domElement);
    scene.add(new THREE.AmbientLight(0xffffff, 0.6));
    const sun = new THREE.DirectionalLight(0xffffff, 1.2);
    sun.position.set(20, -30, 40);
    scene.add(sun);
    const axes = new THREE.AxesHelper(5);
    scene.add(axes);
    const groups = { part: new THREE.Group(), stock: new THREE.Group(), path: new THREE.Group(), sim: new THREE.Group(), tool: new THREE.Group(), draft: new THREE.Group() };
    for (const g of Object.values(groups)) scene.add(g);
    const resize = () => {
      const w = host.clientWidth, h = host.clientHeight;
      renderer.setSize(w, h);
      camera.aspect = w / Math.max(h, 1);
      camera.updateProjectionMatrix();
    };
    new ResizeObserver(resize).observe(host);
    resize();
    // the view cube (upper right): view only, it turns the camera
    const cube = addViewCube({ THREE, renderer, camera, controls }, host);
    // Drawn when something may have changed, not every frame regardless: at rest an idle
    // view took a CPU's worth of drawing (all of it with software WebGL), which the engine's
    // workers then lacked. At full rate a moment after any input, while the camera moves or
    // the playback plays, and whenever what is shown changed (its signature below); twice a
    // second otherwise, for anything the signature does not see.
    let activeUntil = 0, lastDraw = 0, lastSig = "";
    const kick = () => (activeUntil = performance.now() + 1500);
    for (const ev of ["pointerdown", "pointermove", "wheel", "keydown", "resize"]) window.addEventListener(ev, kick, { passive: true, capture: true });
    controls.addEventListener("change", kick);
    window.__marcam.kickView = kick;
    kick();
    const signature = () => {
      const m = state.machining, v = state.three, parts = [];
      for (const g of Object.values(groups)) parts.push(g.visible ? g.children.length : -1);
      for (const k of ["machining", "work", "toolModel", "simFinal"]) {
        const g = v?.groups?.[k];
        parts.push(g ? `${g.visible ? g.children.length : -1}:${g.position.x},${g.position.y},${g.position.z}:${g.rotation.x}` : "");
      }
      return [camera.position.toArray().join(), camera.quaternion.toArray().join(), camera.zoom, camera.aspect, controls.target.toArray().join(), state.tree?.selected, $("simGhost")?.checked, m?.pos, m?.open, m?.expanded, m?.swapping, JSON.stringify(m?.view), m?.stockMesh?.id, m?.paths?.cut[0]?.id, m?.paths?.cut[0]?.geometry.drawRange.count, v?.key, v?.modelKey, v?.simKey, v?.cutLines?.geometry.drawRange.count, groups.draft.children[0]?.id, colorsGen, document.body.className, parts.join()].join("|");
    };
    const loop = (t) => {
      requestAnimationFrame(loop);
      controls.update();
      window.__marcam.machining?.syncFrame?.();
      const sig = signature();
      const busy = t < activeUntil || state.machining?.playing || cube.busy();
      if (!busy && sig === lastSig && t - lastDraw < 500) return;
      lastSig = sig;
      lastDraw = t;
      renderer.render(scene, camera);
      cube.render();
    };
    requestAnimationFrame(loop);
    state.three = { THREE, renderer, scene, camera, controls, groups, axes, fitted: null, cube };
    window.__marcam.viewCube = cube;
    $("viewNote").textContent = "Drag to orbit, scroll to zoom. Blue: cuts, red: rapids. The part turns, so the path is drawn around it (A rotates about X).";
  } catch (e) {
    state.three = { failed: true };
    $("viewNote").textContent = `3D preview unavailable (three.js could not be loaded from cdn.jsdelivr.net: ${e.message}). Everything else works.`;
  }
  return state.three;
}

function clear(group) {
  for (const c of [...group.children]) {
    group.remove(c);
    // (an object may be a group: the stock is its solid and its outline)
    c.traverse((o) => {
      o.geometry?.dispose();
      for (const mat of [].concat(o.material || [])) mat.dispose();
    });
  }
}

/** Polyline point i -> xyz: 3-axis points (kind 2/3) are already work XYZ. */
function pointXYZ(pts, i) {
  return pts[4 * i + 3] >= 2 ? [pts[4 * i], pts[4 * i + 1], pts[4 * i + 2]] : toXYZ(pts[4 * i], pts[4 * i + 1], pts[4 * i + 2]);
}

// ---------------------------------------------------------------- colours (View > Colours)
// Every colour the views draw with (colors.js), changed here and kept in this browser.
function applyBackground() {
  const bg = color("background");
  $("view3d").style.background = bg || "";
}
function openColors() {
  let dlg = $("colorsDialog");
  if (!dlg) {
    dlg = document.createElement("dialog");
    dlg.id = "colorsDialog";
    dlg.className = "tool-dialog";
    document.body.appendChild(dlg);
    dlg.addEventListener("input", (ev) => {
      const k = ev.target.dataset?.color;
      if (k) setColor(k, ev.target.value);
    });
    dlg.addEventListener("click", (ev) => {
      if (ev.target.id === "colorsReset") {
        resetColors();
        fill();
      }
      if (ev.target.dataset?.reset) {
        resetColors(ev.target.dataset.reset);
        fill();
      }
      if (ev.target.id === "colorsClose") dlg.close();
    });
  }
  const fill = () => {
    const bgDefault = getComputedStyle(document.body).getPropertyValue("--panel").trim() || "#1e2430";
    dlg.innerHTML = `<h3>Colours</h3><div class="muted small">Kept in this browser.</div><table class="tn-table">${Object.keys(COLOR_DEFAULTS).filter((k) => k in COLOR_LABELS)
      .map((k) => {
        const v = color(k) || (/^#[0-9a-f]{6}$/i.test(bgDefault) ? bgDefault : "#1e2430");
        return `<tr><td>${COLOR_LABELS[k]}</td><td><input type="color" data-color="${k}" value="${v}" aria-label="${COLOR_LABELS[k]}"></td><td><button type="button" data-reset="${k}" aria-label="${COLOR_LABELS[k]}: default">default</button></td></tr>`;
      })
      .join("")}</table><div class="row"><button type="button" id="colorsReset">Reset all</button> <button type="button" id="colorsClose" class="primary">Close</button></div>`;
  };
  fill();
  dlg.showModal();
}
onColorsChange(async () => {
  colorsGen++;
  applyBackground();
  const v = state.three;
  if (v && !v.failed) {
    v.key = null;
    v.modelKey = null;
    v.simKey = null;
  }
  await drawModel();
  if (state.result) await updateView();
  window.__marcam.machining?.recolor?.();
});
applyBackground();
window.__marcam.colors = { color, setColor, resetColors, open: openColors };

// ---------------------------------------------------------------- the draft's live preview
// An operation being set up (added, not applied yet) is planned in the background as it is
// picked and edited, and its toolpath drawn in amber over the others: what Apply would give.
let draftRun = 0;
let draftTimer = null;
/** Plan `doc` (the project with the draft operation `opId` in setup `setupId`) and show that
 *  operation's toolpath; `doc` null clears the preview. */
function previewDraft(doc, setupId, opId) {
  clearTimeout(draftTimer);
  const run = ++draftRun;
  if (!doc) {
    drawDraft(null);
    return;
  }
  draftTimer = setTimeout(async () => {
    try {
      const { result } = await engine("planPreview", doc, machineArg(), state.mesh, { max_points: 60000, libraries: libraries(), setup: setupId });
      if (run !== draftRun) return;
      let pts = null;
      for (const p of result.programs) {
        const ops = p.toolpath.ops || [];
        const k = ops.findIndex((o) => o.id === opId);
        if (k < 0) continue;
        const n = p.toolpath.points.length / 4;
        // (from its own first point: the one before is where the operation before ended, and
        // a line from it drew a link at depth that the program does not make)
        const start = Math.max(0, ops[k].start);
        const end = k + 1 < ops.length ? ops[k + 1].start : n;
        pts = p.toolpath.points.slice(4 * start, 4 * end);
        break;
      }
      state.draftPreview = { opId, points: pts ? pts.length / 4 : 0, error: null };
      drawDraft(pts);
    } catch (e) {
      if (run !== draftRun) return;
      // (an incomplete draft does not plan: no preview, the form says what is missing)
      state.draftPreview = { opId, points: 0, error: e.message };
      drawDraft(null);
    }
  }, 300);
}
async function drawDraft(pts) {
  const v = await getThree();
  if (!v || v.failed) return;
  const g = v.groups.draft;
  clear(g);
  if (!pts || pts.length < 8) return;
  const cut = [];
  for (let i = 1; i < pts.length / 4; i++) cut.push(...pointXYZ(pts, i - 1), ...pointXYZ(pts, i));
  const geo = new v.THREE.BufferGeometry();
  geo.setAttribute("position", new v.THREE.Float32BufferAttribute(cut, 3));
  g.add(new v.THREE.LineSegments(geo, new v.THREE.LineBasicMaterial({ color: colorNum("draft"), transparent: true, opacity: 0.95 })));
}
window.__marcam.previewDraft = previewDraft;
/** The stock's numbers (material left, gouges, rapids through it) when the viewer shows the
 *  stock: the batch simulation in the background, nothing asked (skipped when long). */
window.__marcam.statsInBackground = () => {
  if (!state.result || state.sim) return;
  setTimeout(() => simLock.run("batch", () => simulateQuiet(SIM_ASK_SECONDS), { quiet: true }), 0);
};

/** Machine (x, r, a) -> part frame xyz: the tool at A sits over part angle 90 - A. */
function toXYZ(x, r, aDeg) {
  const phi = ((90 - aDeg) * Math.PI) / 180;
  return [x, r * Math.cos(phi), r * Math.sin(phi)];
}

/**
 * The current setup's part and stock, from the project itself: shown with or without
 * results (a project with no operation, one just opened, one whose results were cleared).
 */
async function drawModel() {
  const v = await getThree();
  if (!v || v.failed) return;
  let doc;
  try {
    doc = readJob();
  } catch {
    return;
  }
  const { THREE, groups } = v;
  const v2 = doc.schema_version === "0.2";
  const setup = v2 ? doc.setups?.find((s) => s.id === state.tree?.setup) || doc.setups?.[0] : null;
  // (a setup with no model: none, as the engine has it)
  const ids = v2 ? (Array.isArray(setup?.part) ? setup.part : doc.models?.map((m) => m.id) || []) : [];
  const models = v2 ? (doc.models || []).filter((m) => ids.includes(m.id)) : [];
  // (with the colours' generation: a draw begun before a colour changed is dropped)
  // (a setup whose stock the one before left: that one's stock and part too)
  const carried = v2 && setup?.stock?.shape === "from_previous";
  const key = JSON.stringify([setup?.id, setup?.stock, setup?.kind, setup?.holding, models, v2 ? null : [doc.stock, doc.part], carried ? [doc.setups.map((s) => [s.id, s.stock, s.part, s.kind, s === setup ? null : s.operations]), doc.models, doc.tools] : null, state.mesh?.length || 0, colorsGen]);
  // the work zero's triad: where X0 Y0 Z0 of the programs is (a vise setup's own)
  const z0 = setup?.post?.origin;
  if (v.axes) v.axes.position.set(...(Array.isArray(z0) && (setup?.stock?.shape === "box" || setup?.kind === "vise") ? z0 : [0, 0, 0]));
  if (v.modelKey === key) return;
  v.modelKey = key;
  // built aside and swapped in at once when complete: the old part stays shown meanwhile
  const partNew = [], stockNew = [];
  // the part: a mesh model (as the engine places it) or a revolved profile
  const partMat = new THREE.MeshStandardMaterial({ color: colorNum("part"), metalness: 0.3, roughness: 0.5 });
  const mesh = v2 ? models.find((m) => m.kind === "mesh") : doc.part?.mesh;
  // carried: the stock and the part as the engine turns them from the setup before
  let frame = null;
  if (carried) {
    try {
      ({ result: frame } = await engine("setupFrame", doc, { setup: setup.id }));
    } catch (e) {
      console.warn(`the carried stock is not drawn: ${e?.message}`);
    }
    if (v.modelKey !== key) return;
  }
  // (carried: the part as the engine turns it, if it goes on at all)
  const prof = frame ? frame.profile?.points : v2 ? models.find((m) => m.kind === "profile")?.points : doc.part?.profile?.points;
  let bounds = null;
  if (mesh && state.mesh && (!frame || frame.mesh)) {
    try {
      // (a part being drawn: tests wait for it)
      v.drawing = (v.drawing || 0) + 1;
      let result;
      try {
        ({ result } = await engine("meshTriangles", doc, { max_triangles: 150000, setup: setup?.id }, state.mesh));
      } finally {
        v.drawing--;
      }
      if (v.modelKey !== key) return; // superseded meanwhile
      const g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.Float32BufferAttribute(result.triangles, 3));
      g.computeVertexNormals();
      const meshMat = partMat.clone();
      meshMat.side = THREE.DoubleSide; // a user's mesh may have inconsistent winding
      partNew.push(new THREE.Mesh(g, meshMat));
      bounds = result.bounds;
    } catch (e) {
      // the engine restarted under the draw (a cancel, a project opened meanwhile): drawn
      // again once it is back, not left without its part
      if (e?.message === "cancelled" || e?.message === "superseded") {
        if (v.modelKey === key) v.modelKey = null;
        setTimeout(drawModel, 50);
        return;
      }
      // a mesh the engine cannot read: the stock alone
      console.warn(`the part is not drawn: ${e?.message}`);
    }
  } else if (prof && prof.length > 1) {
    partNew.push(new THREE.Mesh(toGeometry(THREE, revolveProfile(prof, 120)), partMat));
    bounds = [[Math.min(...prof.map((q) => q[0])), 0, 0], [Math.max(...prof.map((q) => q[0])), 0, 0]];
  }
  // the stock: the setup's block or bar (a 0.1 job's bar)
  // (a light tint with its edges drawn: see meshes.js stockObject)
  const st0 = frame ? (frame.stock_box ? { shape: "box", ...frame.stock_box } : { shape: "cylinder", ...frame.stock }) : v2 ? setup?.stock : doc.stock ? { shape: "cylinder", ...doc.stock } : null;
  // a bar lying in a vise: drawn along X at its axis, the block round it for the view
  const viseBar = v2 && st0 && st0.shape !== "box" && (setup?.kind === "vise" || setup?.holding?.kind === "vise") ? lyingBar(st0, setup?.holding?.along) : null;
  const st = viseBar ? { shape: "box", ...viseBar.box } : st0;
  const sb = st?.shape === "box" ? st : null;
  let fitKey = null;
  if (viseBar) {
    const corners = barCorners(st0);
    const geo = corners ? prismGeometry(THREE, corners, 0, st0.length) : toGeometry(THREE, cylinder(st0.radius, 0, st0.length, 96));
    // (Mar) along Y: its section's Y along X, its length along Y; standing: its section's Y
    // along X, its Z along Y, its length down from Z0 (as the engine's)
    const L = st0.length;
    if (viseBar.along === "y") {
      geo.applyMatrix4(new THREE.Matrix4().set(0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1));
      flipWinding(geo);
      geo.translate(viseBar.axis[0], 0, viseBar.axis[1]);
    } else if (viseBar.along === "z") {
      geo.applyMatrix4(new THREE.Matrix4().set(0, 1, 0, 0, 0, 0, 1, 0, 1, 0, 0, -L, 0, 0, 0, 1));
      geo.translate(viseBar.axis[0], viseBar.axis[1], 0);
    } else {
      geo.translate(0, viseBar.axis[0], viseBar.axis[1]);
    }
    stockNew.push(stockObject(THREE, geo));
    fitKey = JSON.stringify(sb);
    v.bounds = { xmin: sb.min[0], xmax: sb.max[0], R: Math.max(sb.max[1] - sb.min[1], sb.max[2] - sb.min[2]) / 2 };
  } else if (sb) {
    const size = [0, 1, 2].map((k) => sb.max[k] - sb.min[k]);
    const box = stockObject(THREE, new THREE.BoxGeometry(...size));
    box.position.set(...[0, 1, 2].map((k) => (sb.min[k] + sb.max[k]) / 2));
    stockNew.push(box);
    fitKey = JSON.stringify(sb);
    v.bounds = { xmin: sb.min[0], xmax: sb.max[0], R: Math.max(sb.max[1] - sb.min[1], sb.max[2] - sb.min[2]) / 2 };
  } else if (st?.radius || ["square", "rect", "hex"].includes(st?.shape)) {
    // the bar: from the part's front face back by its length (square, rectangular and hex
    // bars as prisms from their corners)
    const front = Math.max(0, bounds ? bounds[1][0] : 0);
    const xmin = front - (st.length || 30) - 1, xmax = front + 1;
    const corners = barCorners(st);
    const R = corners ? Math.max(...corners.map(([y, z]) => Math.hypot(y, z))) : st.radius;
    stockNew.push(stockObject(THREE, corners ? prismGeometry(THREE, corners, xmin, xmax) : toGeometry(THREE, cylinder(st.radius, xmin, xmax, 96))));
    v.bounds = { xmin, xmax, R };
    fitKey = JSON.stringify([R, Math.round(xmin), Math.round(xmax)]);
  }
  // (Mar) carried: what the setup before left, as this setup starts with it (the box or bar
  // above until it is worked out, and if it cannot be)
  if (carried && frame) {
    try {
      v.drawing = (v.drawing || 0) + 1;
      let r;
      try {
        ({ result: r } = await engine("carriedMesh", doc, { setup: setup.id }, state.mesh));
      } finally {
        v.drawing--;
      }
      if (v.modelKey !== key) return;
      v.carriedError = r?.triangles?.length ? null : "no triangles";
      if (r?.triangles?.length) {
        const g = new THREE.BufferGeometry();
        g.setAttribute("position", new THREE.Float32BufferAttribute(r.triangles, 3));
        g.computeVertexNormals();
        const o = stockObject(THREE, g);
        o.userData.carried = true;
        stockNew.length = 0;
        stockNew.push(o);
      }
    } catch (e) {
      if (e?.message === "cancelled" || e?.message === "superseded") {
        if (v.modelKey === key) v.modelKey = null;
        setTimeout(drawModel, 50);
        return;
      }
      console.warn(`the carried stock is drawn as its box: ${e?.message}`);
      v.carriedError = e?.message || String(e);
    }
  }
  clear(groups.stock);
  clear(groups.part);
  setTimeout(() => window.__marcam.handles?.refresh?.(), 0);
  for (const o of partNew) groups.part.add(o);
  for (const o of stockNew) groups.stock.add(o);
  // a vise's jaws: beside the stock's Y faces, up to what they grip
  if (!groups.jaws) {
    groups.jaws = new THREE.Group();
    v.scene.add(groups.jaws);
  }
  clear(groups.jaws);
  if (sb && v2 && (setup?.kind === "vise" || setup?.holding?.kind === "vise" || setup?.stock?.shape === "box")) {
    // (carried: the block as the engine has it)
    const grip = jawGrip(frame?.stock_box ? { ...setup, stock: { shape: "box", ...frame.stock_box } } : setup);
    const top = sb.min[2] + grip, low = sb.min[2] - 4;
    const len = sb.max[0] - sb.min[0] + 20, thick = 8;
    for (const y of [sb.min[1] - thick / 2, sb.max[1] + thick / 2]) {
      // (steel, not the stock's colour: Mar)
      const geo = new THREE.BoxGeometry(len, thick, top - low);
      const jaw = new THREE.Group();
      jaw.add(new THREE.Mesh(geo, new THREE.MeshStandardMaterial({ color: colorNum("fixture"), metalness: 0.55, roughness: 0.5 })));
      jaw.add(new THREE.LineSegments(new THREE.EdgesGeometry(geo, 25), new THREE.LineBasicMaterial({ color: colorNum("fixtureEdge") })));
      jaw.position.set((sb.min[0] + sb.max[0]) / 2, y, (top + low) / 2);
      jaw.userData.jaw = true;
      groups.jaws.add(jaw);
    }
  }
  // frame the view when the stock changes; keep the person's orbit otherwise
  if (fitKey && v.fitted !== fitKey && v.fitted !== "picking") {
    if (sb) {
      // (a block standing tall, as a plate stood on its end: its height counts too)
      const c = [0, 1, 2].map((k) => (sb.min[k] + sb.max[k]) / 2), span = Math.max(sb.max[0] - sb.min[0], sb.max[1] - sb.min[1], sb.max[2] - sb.min[2]);
      v.camera.position.set(c[0] - span * 0.3, c[1] - span * 1.2, c[2] + span * 0.9);
      v.controls.target.set(...c);
    } else {
      const { xmin, xmax, R } = v.bounds;
      const span = xmax - xmin;
      v.camera.position.set((xmin + xmax) / 2 - span * 0.2, -span * 1.3, R * 2.2);
      v.controls.target.set((xmin + xmax) / 2, 0, 0);
    }
    v.controls.update();
    v.fitted = fitKey;
  }
  $("viewNote").textContent = sb
    ? "Drag to orbit, scroll to zoom. Blue: cuts, red: rapids. Vise setup: the block stays put and the tool moves in X, Y and Z (Z0 = top of the stock)."
    : "Drag to orbit, scroll to zoom. Blue: cuts, red: rapids. The part turns, so the path is drawn around it (A rotates about X).";
  legacyLayers();
  v.renderer.render(v.scene, v.camera);
}

async function updateView() {
  const v = await getThree();
  if (!v || v.failed || !state.result) return;
  const { THREE, groups } = v;
  const res = state.result;
  const p = res.programs[state.tab];
  if (!p) {
    // (a setup with no operation yet: no toolpath, its part and stock as they are)
    clear(groups.path);
    clear(groups.tool);
    v.cutLines = v.rapLines = null;
    v.key = null;
    await drawModel();
    legacyLayers();
    return;
  }
  const key = `${state.tab}:${res.programs.length}:${p.lines}:${[...state.hidden].join(",")}`;
  const hide = hiddenMask(p);

  if (v.key !== key) {
    v.key = key;
    // toolpath as two line-segment sets with prefix counts for the scrubber
    clear(groups.path);
    const pts = p.toolpath.points;
    const n = pts.length / 4;
    const cut = [], rap = [];
    const cutUpTo = new Uint32Array(n + 1), rapUpTo = new Uint32Array(n + 1);
    for (let i = 1; i < n; i++) {
      if (hide && hide[i]) {
        cutUpTo[i + 1] = cut.length / 6;
        rapUpTo[i + 1] = rap.length / 6;
        continue;
      }
      const a = pointXYZ(pts, i - 1);
      const b = pointXYZ(pts, i);
      (pts[4 * i + 3] % 2 ? cut : rap).push(...a, ...b);
      cutUpTo[i + 1] = cut.length / 6;
      rapUpTo[i + 1] = rap.length / 6;
    }
    const lines = (arr, color, opacity) => {
      const g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.Float32BufferAttribute(arr, 3));
      return new THREE.LineSegments(g, new THREE.LineBasicMaterial({ color, transparent: true, opacity }));
    };
    v.cutLines = lines(cut, colorNum("cut"), 0.85);
    v.rapLines = lines(rap, colorNum("rapid"), 0.5);
    v.cutUpTo = cutUpTo;
    v.rapUpTo = rapUpTo;
    groups.path.add(v.cutLines, v.rapLines);

    // the part and the stock come from the project (drawModel), results or not
    v.simKey = null;
    await drawModel();
  }

  // simulated stock surface for this program
  const simKey = state.sim ? `${key}:sim` : null;
  if (v.simKey !== simKey) {
    v.simKey = simKey;
    clear(groups.sim);
    const s = state.sim?.stocks?.[state.tab];
    if (s) groups.sim.add(simMesh(THREE, state.sim, s));
  }
  legacyLayers();

  // scrubber
  const k = +$("scrub").value;
  v.cutLines.geometry.setDrawRange(0, 2 * v.cutUpTo[Math.min(k, v.cutUpTo.length - 1)]);
  v.rapLines.geometry.setDrawRange(0, 2 * v.rapUpTo[Math.min(k, v.rapUpTo.length - 1)]);
  clear(groups.tool);
  const pts = p.toolpath.points;
  if (k > 0) {
    const [x, y, z] = pointXYZ(pts, k - 1);
    const tip = new THREE.Mesh(new THREE.SphereGeometry(0.4, 16, 12), new THREE.MeshBasicMaterial({ color: 0xf59e0b }));
    tip.position.set(x, y, z);
    groups.tool.add(tip);
  }
}

/** What the view without a playback shows (the part, the stock, the simulated stock, the
 *  toolpath and its tip, an operation's live preview): as the corner toggles say, and none of
 *  it while the playback shows (it draws its own). The one place their visibility is set. */
function legacyLayers() {
  const v = state.three;
  if (!v || v.failed) return;
  const { groups } = v;
  const view = state.machining?.view;
  const m = state.machining;
  // (no toggles yet, at the very start: the older switches)
  const part = view ? view.part : $("showPart").checked;
  const sim = view ? view.stock : $("showSim").checked;
  const paths = view ? view.paths !== "off" : true;
  const rapids = view ? view.rapids : $("showRapids").checked;
  const tool = view ? view.tool : true;
  const simShown = !!groups.sim.children.length && sim;
  groups.sim.visible = sim;
  // (picking: the faces to pick on stand in for the part)
  groups.part.visible = part && !simShown && !state.tree?.pickMesh;
  groups.stock.visible = $("showStock").checked && (view ? view.part || view.stock : true) && !simShown;
  groups.path.visible = paths;
  if (v.rapLines) v.rapLines.visible = rapids;
  groups.tool.visible = tool;
  // (an operation's live preview: while it is being made, not after)
  if (groups.draft) groups.draft.visible = paths && !!state.tree?.draft;
  // the playback shown (not stepped aside for picking or a new operation): it draws them
  const t = state.tree;
  const editing = !!(t && (t.picking || t.draft || t.tabMode));
  // (shown while it opens, too: its scene is drawn before it counts as open)
  if (!editing && groups.machining?.visible) for (const k of ["part", "stock", "sim", "path", "tool"]) groups[k].visible = false;
}
window.__marcam.legacyLayers = legacyLayers;

function ramp(t) {
  t = Math.max(0, Math.min(1, t));
  const stops = [[40, 60, 200], [30, 190, 210], [240, 210, 40], [210, 40, 40]];
  const f = t * 3, i = Math.min(2, Math.floor(f)), u = f - i;
  return stops[i].map((c, k) => (c * (1 - u) + stops[i + 1][k] * u) / 255);
}

/** A square, rectangular or hex bar's corners (y, z) about the axis, counter-clockwise; null
 *  for a round one. Angles as the engine's Section (degrees from +Y towards +Z). */
/** Its triangles turned the other way round (after a mirroring transform). */
function flipWinding(geo) {
  if (geo.index) {
    const ix = geo.index.array;
    for (let i = 0; i < ix.length; i += 3) [ix[i + 1], ix[i + 2]] = [ix[i + 2], ix[i + 1]];
    geo.index.needsUpdate = true;
  } else {
    const p = geo.attributes.position.array;
    for (let i = 0; i < p.length; i += 9) for (let k = 0; k < 3; k++) [p[i + 3 + k], p[i + 6 + k]] = [p[i + 6 + k], p[i + 3 + k]];
    geo.attributes.position.needsUpdate = true;
  }
  geo.computeVertexNormals();
}

function barCorners(st) {
  const a = ((st.angle || 0) * Math.PI) / 180;
  if (st.shape === "rect") {
    const c = Math.cos(a), s = Math.sin(a);
    return [[1, 1], [-1, 1], [-1, -1], [1, -1]].map(([u, w]) => {
      const y = (u * st.width) / 2, z = (w * st.height) / 2;
      return [y * c - z * s, y * s + z * c];
    });
  }
  if (st.shape === "square" || st.shape === "hex") {
    const n = st.shape === "square" ? 4 : 6;
    const af = st.shape === "square" ? st.side : st.across_flats;
    const rc = af / 2 / Math.cos(Math.PI / n);
    return Array.from({ length: n }, (_, k) => {
      const phi = Math.PI / 2 + a + Math.PI / n + (k * 2 * Math.PI) / n;
      return [rc * Math.cos(phi), rc * Math.sin(phi)];
    });
  }
  return null;
}

/** A prism along X from xmin to xmax with the section `pts` (y, z), closed. */
function prismGeometry(THREE, pts, xmin, xmax) {
  const pos = [];
  const quad = (a, b, c, d) => pos.push(...a, ...b, ...c, ...a, ...c, ...d);
  const n = pts.length;
  for (let k = 0; k < n; k++) {
    const [y0, z0] = pts[k], [y1, z1] = pts[(k + 1) % n];
    quad([xmin, y0, z0], [xmax, y0, z0], [xmax, y1, z1], [xmin, y1, z1]);
  }
  for (let k = 1; k + 1 < n; k++) {
    pos.push(xmax, ...pts[0], xmax, ...pts[k], xmax, ...pts[k + 1]);
    pos.push(xmin, ...pts[0], xmin, ...pts[k + 1], xmin, ...pts[k]);
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3));
  g.computeVertexNormals();
  return g;
}

/** Indexed geometry from a web/meshes.js mesh, with normals (and colours if present). */
function toGeometry(THREE, m) {
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(m.positions, 3));
  if (m.colors) g.setAttribute("color", new THREE.BufferAttribute(m.colors, 3));
  g.setIndex(new THREE.BufferAttribute(m.indices, 1));
  g.computeVertexNormals();
  return g;
}

/** Simulated stock -> closed solid (a bar around X, or a 3-axis block), coloured by material
 * left over the part (grey where there is no part). */
function simMesh(THREE, sim, s) {
  const r = sim.kind === "xyz" ? s.z : s.r;
  const has = (k) => (sim.kind === "xyz" ? sim.part[k] != null : sim.part[k] > 0);
  // where there is no part: remaining stock, tinted (not the part's grey)
  const uncut = colorRgb("uncut");
  const color = (k) => (has(k) ? ramp((r[k] - sim.part[k]) / 0.5) : uncut);
  const m = sim.kind === "xyz" ? blockSolid(sim.grid, r, sim.stock_box.min[2], color) : heightmapSolid(sim.grid, r, color);
  return new THREE.Mesh(toGeometry(THREE, m), new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.7 }));
}

// ---------------------------------------------------------------- wiring
$("loadExample").addEventListener("click", () => {
  if (!window.__marcam.project.mayDiscard("Load the example")) return;
  loadExample($("example").value).catch((e) => {
    console.error(e);
    showError(e.message);
    setStatus("Loading failed", "error");
  });
});
$("job").addEventListener("input", () => window.__marcam.project.changed({ typing: true }));
// Edit > Undo / Redo, and their keys (not inside a field or an open dialog: those keep the
// browser's own undo of the text being typed)
$("menuUndo").addEventListener("click", () => window.__marcam.project.undo());
$("menuRedo").addEventListener("click", () => window.__marcam.project.redo());
document.addEventListener("keydown", (ev) => {
  if (!(ev.ctrlKey || ev.metaKey) || ev.altKey) return;
  const k = ev.key.toLowerCase();
  const which = k === "z" ? (ev.shiftKey ? "redo" : "undo") : k === "y" && !ev.shiftKey ? "redo" : null;
  if (!which) return;
  const t = ev.target;
  if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
  if (document.querySelector("dialog[open]:not(.embedded)")) return;
  ev.preventDefault();
  window.__marcam.project[which]();
});
$("summary").addEventListener("click", (ev) => {
  if (ev.target.id === "staleRegen") generate();
  if (ev.target.dataset?.empty === "import") $("importStl").click();
  if (ev.target.dataset?.empty === "open") $("projectOpen").click();
  if (ev.target.dataset?.empty === "generate") generate();
});
$("generate").addEventListener("click", generate);
$("simulate").addEventListener("click", simulate);
$("download").addEventListener("click", download);
$("machine").addEventListener("change", () => {
  const custom = $("machine").value === "custom";
  $("machineJson").classList.toggle("hidden", !custom);
  if (custom && !$("machineJson").value) {
    const z1 = state.registry.machines.find((m) => m.id === "makera-z1-4axis");
    $("machineJson").value = JSON.stringify(z1, null, 2);
  }
  store("marcam.machineSel", $("machine").value);
  renderMachineInfo();
  // the project remembers a built-in machine on its setup
  try {
    const doc = readJob();
    const id = $("machine").value;
    const s = doc.setups?.find((x) => x.id === state.tree?.setup) ?? doc.setups?.[0];
    if (s && state.registry.machines.some((m) => m.id === id) && s.machine !== id) {
      s.machine = id;
      writeJob(doc);
      window.__marcam.tree?.refresh();
    } else window.__marcam.project?.changed();
  } catch {
    window.__marcam.project?.changed();
  }
});
$("mesh").addEventListener("change", async () => {
  const f = $("mesh").files[0];
  if (f) {
    const bytes = new Uint8Array(await f.arrayBuffer());
    try {
      const { isStep } = await import("./step.js");
      if (isStep(bytes, f.name)) {
        const { readPart } = await import("./project.js");
        const part = await readPart(bytes, f.name);
        setMesh(part.step.stl, f.name, { name: f.name, bytes });
      } else setMesh(bytes, f.name);
    } catch (e) {
      return showError(`${f.name}: ${e.message}`);
    }
    window.__marcam.project.changed();
  }
});
$("scrub").addEventListener("input", () => {
  updateScrubLabel();
  drawSide();
  updateView();
});
for (const id of ["showRapids", "showStock", "showPart", "showSim"]) $(id).addEventListener("change", () => { drawSide(); updateView(); legacyLayers(); });
window.addEventListener("resize", drawSide);

(async () => {
  try {
    const { result } = await engine("version");
    $("version").textContent = `v${result.version}`;
    state.registry = (await engine("profileList")).result;
    initLibraries();
    renderMachines(load("marcam.machineSel", "makera-z1-4axis"));
    renderLibraries();
    await loadExample("knob");
    setMesh(null, "");
    $("generate").disabled = $("simulate").disabled = false;
    setStatus("Ready", "ready");
  } catch (e) {
    setStatus("Engine failed to load", "error");
    showError(`${e.message}\nBuild it first: scripts/build-wasm.sh (or .ps1)`);
  }
})();

// ---------------------------------------------------------------- machines and tool libraries
function load(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : JSON.parse(v);
  } catch {
    return fallback;
  }
}
function store(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {}
}
const userMachines = () => load("marcam.machines", []);
const libraries = () => load("marcam.libraries", []);

/** First run: "My tools" starts as a copy of the Makera starter library. */
function initLibraries() {
  if (libraries().length) return;
  const starter = state.registry.libraries.find((l) => l.id === "makera-starter");
  store("marcam.libraries", [{ ...structuredClone(starter), id: "my-tools", name: "My tools (from Makera starter)" }]);
}

function renderMachines(selected) {
  const sel = $("machine");
  sel.innerHTML = "";
  const opt = (value, text) => {
    const o = document.createElement("option");
    o.value = value;
    o.textContent = text;
    sel.appendChild(o);
  };
  for (const m of state.registry.machines) opt(m.id, m.name);
  for (const m of userMachines()) opt(`user:${m.id}`, `${m.name} (imported)`);
  opt("custom", "Custom JSON...");
  const alias = { "makera-z1": "makera-z1-4axis", z1: "makera-z1-4axis", carvera: "makera-carvera-4axis", "carvera-air": "makera-carvera-air-4axis", linuxcnc: "linuxcnc-4axis" };
  const want = alias[selected] || selected;
  const values = [...sel.options].map((o) => o.value);
  sel.value = values.includes(want) ? want : values.includes(`user:${want}`) ? `user:${want}` : "makera-z1-4axis";
  $("machineJson").classList.toggle("hidden", sel.value !== "custom");
  renderMachineInfo();
}

function renderMachineInfo() {
  const m = currentMachine();
  if (!m) {
    $("machineInfo").textContent = "";
    return;
  }
  const tr = (l) => {
    const a = m.kinematics.linear.find((x) => x.letter === l);
    return a && a.min != null && a.max != null ? `${l} ${a.max - a.min} mm` : null;
  };
  const parts = [
    [tr("X"), tr("Y"), tr("Z")].filter(Boolean).join(", "),
    m.spindle ? `spindle ${m.spindle.min_rpm}-${m.spindle.max_rpm} rpm` : "",
    m.max_feed ? `feed cap ${m.max_feed} mm/min` : "",
    m.rotary_envelope ? `4th axis ${m.rotary_envelope.max_diameter} x ${m.rotary_envelope.max_length} mm` : "",
    m.tool_changer ? (m.tool_changer.kind === "atc" ? `ATC ${m.tool_changer.slots ?? ""} slots` : "manual tool change") : "",
    `feeds: ${m.post.feed_mode.replace(/_/g, " ")}`,
  ].filter(Boolean);
  $("machineInfo").textContent = parts.join(" · ");
}

function limitsHtml(program, esc) {
  const lim = (state.result.limits || []).filter((i) => !i.program || i.program === program);
  if (!lim.length) return `<div class="small" id="limits">Machine limits: OK for ${esc(state.result.machine)}</div>`;
  const bad = lim.some((i) => i.severity === "error");
  return `<div id="limits" class="limits ${bad ? "bad" : ""}">Machine limits (${esc(state.result.machine)}):<ul>${lim
    .map((i) => `<li>${i.severity}: ${esc(i.message)}</li>`)
    .join("")}</ul></div>`;
}

let editing = null; // index of the tool being edited, or -1 for a new one

function currentLibrary() {
  const libs = libraries();
  return libs.find((l) => l.id === $("library").value) || libs[0];
}

function saveLibrary(lib) {
  store("marcam.libraries", libraries().map((l) => (l.id === lib.id ? lib : l)));
}

function renderLibraries(selected) {
  const sel = $("library");
  const keep = selected || sel.value || load("marcam.libSel", "my-tools");
  sel.innerHTML = "";
  for (const l of libraries()) {
    const o = document.createElement("option");
    o.value = l.id;
    o.textContent = `${l.name} (${l.tools.length})`;
    sel.appendChild(o);
  }
  if ([...sel.options].some((o) => o.value === keep)) sel.value = keep;
  renderTools();
}

const shapeText = (s) =>
  s.kind === "vbit" ? `V ${s.angle}° Ø${s.diameter}`
  : s.kind === "bull" ? `bull Ø${s.diameter} r${s.corner_radius}`
  : s.kind === "tapered_ball" ? `tapered ball tip Ø${s.tip_diameter} ${s.taper_angle}°/side Ø${s.diameter}`
  : `${s.kind} Ø${s.diameter}`;

function renderTools() {
  const lib = currentLibrary();
  const tbl = $("toolTable");
  tbl.innerHTML = "<tr><th>Tool</th><th>Shape</th><th>#</th><th></th></tr>";
  if (!lib) return;
  let jobTools = [];
  try {
    jobTools = (readJob().tools || []).map((t) => t.number);
  } catch {}
  lib.tools.forEach((t, i) => {
    const tr = document.createElement("tr");
    tr.dataset.tool = t.id;
    const esc = (x) => String(x).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    const use = `<select data-use="${i}" aria-label="Use ${esc(t.name)} as"><option value="">use as...</option>${jobTools
      .map((n) => `<option value="${n}">T${n}</option>`)
      .join("")}<option value="new">new T</option></select>`;
    tr.innerHTML = `<td>${esc(t.name)}</td><td>${esc(shapeText(t.shape))}</td><td>${t.number ?? ""}</td>
      <td>${use} <button type="button" data-edit="${i}">Edit</button> <button type="button" data-del="${i}" aria-label="Remove ${esc(t.name)}">✕</button></td>`;
    tbl.appendChild(tr);
  });
}

/** Library tool -> job tool (the shapes the strategies use). */
function toJobTool(t, number) {
  const s = t.shape;
  const base = { number, description: t.name };
  if (t.flute_length) base.flute_length = t.flute_length;
  if (t.shank_diameter) base.shank_diameter = t.shank_diameter;
  if (t.stickout) base.stickout = t.stickout;
  if (s.kind === "flat" || s.kind === "bull") return { ...base, kind: "flat", diameter: s.diameter };
  if (s.kind === "ball") return { ...base, kind: "ball", diameter: s.diameter };
  if (s.kind === "vbit") return { ...base, kind: "vbit", angle: s.angle, diameter: s.diameter };
  if (s.kind === "tapered_ball")
    return { ...base, kind: "tapered_ball", tip_diameter: s.tip_diameter, taper_angle: s.taper_angle, diameter: s.diameter };
  throw new Error(`${t.name}: ${s.kind} tools are not used by the strategies yet`);
}

function useTool(i, target) {
  showError("");
  const t = currentLibrary().tools[i];
  try {
    const job = readJob();
    job.tools = job.tools || [];
    const number = target === "new" ? Math.max(0, ...job.tools.map((x) => x.number)) + 1 : +target;
    const jt = toJobTool(t, number);
    const k = job.tools.findIndex((x) => x.number === number);
    if (k >= 0) job.tools[k] = jt;
    else job.tools.push(jt);
    writeJob(job);
    renderTools();
    $("importInfo").textContent = `T${number} is now ${t.name}`;
  } catch (e) {
    showError(e.message);
  }
}

// ---- a library tool's cutting data: rows of material × operation type × values
const CUT_FIELDS = [
  ["rpm", "rpm", "Spindle speed the feeds are for (scaled to the setup's)"],
  ["feed", "feed", "Cutting feed, mm/min"],
  ["plunge", "plunge", "Plunge feed, mm/min"],
  ["stepdown", "depth", "Depth per pass, mm"],
  ["stepover", "stepover", "Stepover, mm"],
  ["ramp_angle", "ramp °", "Ramp angle, degrees"],
];
let opTypeNames = null; // [[kind, title]]

async function loadOpTypeNames() {
  if (!opTypeNames) {
    const types = state.tree?.types?.length ? state.tree.types : (await engine("opTypes")).result;
    opTypeNames = types.map((t) => [t.kind, t.title || t.kind]);
  }
  return opTypeNames;
}

function cutRowHtml(p = {}) {
  const esc = (x) => String(x ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const ops = (opTypeNames || []).map(([k, t]) => `<option value="${k}" ${p.op === k ? "selected" : ""}>${esc(t)}</option>`).join("");
  const val = (k) => (p[k] == null || (["rpm", "feed", "plunge"].includes(k) && !p[k]) ? "" : p[k]);
  return `<tr class="cut-row"><td><input class="cut-material" list="tfMaterials" placeholder="any material" aria-label="Material" value="${esc(p.material)}"></td>
    <td><select class="cut-op" aria-label="Operation type"><option value="">any operation</option>${ops}</select></td>
    ${CUT_FIELDS.map(([k, , help]) => `<td><input class="cut-${k}" type="number" step="any" min="0" title="${help}" aria-label="${help}" value="${val(k)}"></td>`).join("")}
    <td class="nowrap"><button type="button" class="cut-calc" title="Calculate: speeds, feeds and depths for this material and operation (chip load, surface speed, chip thinning), within the machine's limits">Calc</button> <button type="button" class="cut-del" aria-label="Remove this row">✕</button></td></tr>`;
}

function renderCutting(presets) {
  $("tfMaterials").innerHTML = (state.registry?.materials ?? []).map((m) => `<option value="${m}">`).join("");
  $("tfCutTable").innerHTML = `<tr><th>Material</th><th>Operation</th>${CUT_FIELDS.map(([, l, help]) => `<th title="${help}">${l}</th>`).join("")}<th></th></tr>${presets.map(cutRowHtml).join("")}`;
}

/** The cutting data rows as library presets (rows with nothing in them left out). */
function readCutting() {
  const out = [];
  for (const tr of $("tfCutTable").querySelectorAll("tr.cut-row")) {
    const p = { material: tr.querySelector(".cut-material").value.trim() };
    const op = tr.querySelector(".cut-op").value;
    if (op) p.op = op;
    let any = false;
    for (const [k] of CUT_FIELDS) {
      const v = tr.querySelector(`.cut-${k}`).value;
      // (a depth or stepover of 0 is no value)
      if (v === "" || !(+v >= 0) || (["stepdown", "stepover"].includes(k) && !(+v > 0))) {
        if (["rpm", "feed", "plunge"].includes(k)) p[k] = 0;
        continue;
      }
      p[k] = +v;
      any = true;
    }
    if (any) out.push(p);
  }
  return out;
}

function openForm(i) {
  editing = i;
  const t = i >= 0 ? currentLibrary().tools[i] : { name: "", shape: { kind: "flat", diameter: 3.175 } };
  $("tfName").value = t.name;
  $("tfKind").value = ["flat", "ball", "bull", "vbit", "tapered_ball", "drill"].includes(t.shape.kind) ? t.shape.kind : "ball";
  $("tfTip").value = t.shape.tip_diameter ?? "";
  $("tfTaper").value = t.shape.taper_angle ?? "";
  $("tfDiameter").value = t.shape.diameter;
  $("tfAngle").value = t.shape.angle ?? "";
  $("tfCorner").value = t.shape.corner_radius ?? "";
  $("tfFlute").value = t.flute_length ?? "";
  $("tfFlutes").value = t.flutes ?? "";
  $("tfCalcNotes").textContent = "";
  $("tfNumber").value = t.number ?? "";
  renderCutting(t.presets || []);
  // (the operation names once, on the first open)
  if (!opTypeNames) loadOpTypeNames().then(() => editing === i && renderCutting(t.presets || [])).catch(() => {});
  $("toolForm").classList.remove("hidden");
  $("tfName").focus();
}

function saveForm(ev) {
  ev.preventDefault();
  showError("");
  const lib = currentLibrary();
  const num = (id) => ($(id).value === "" ? undefined : +$(id).value);
  const kind = $("tfKind").value;
  const shape = { kind, diameter: num("tfDiameter") };
  if (kind === "vbit") shape.angle = num("tfAngle") ?? 90;
  if (kind === "bull") shape.corner_radius = num("tfCorner") ?? 0;
  if (kind === "drill") shape.point_angle = 118;
  if (kind === "tapered_ball") {
    shape.tip_diameter = num("tfTip") ?? shape.diameter;
    shape.taper_angle = num("tfTaper") ?? 0;
    if (!(shape.tip_diameter > 0 && shape.tip_diameter <= shape.diameter && shape.taper_angle >= 0 && shape.taper_angle < 90))
      return showError("Tapered ball: 0 < tip Ø <= diameter, 0 <= taper < 90°");
  }
  if (!(shape.diameter > 0)) return showError("Diameter must be > 0");
  if (kind === "vbit" && !(shape.angle > 0 && shape.angle < 180)) return showError("V-bit angle must be between 0 and 180");
  const old = editing >= 0 ? lib.tools[editing] : {};
  const name = $("tfName").value.trim() || "tool";
  const tool = { ...old, id: old.id || `${name.toLowerCase().replace(/[^a-z0-9.]+/g, "-")}-${Date.now() % 100000}`, name, shape };
  const flute = num("tfFlute");
  const number = num("tfNumber");
  if (flute) tool.flute_length = flute;
  else delete tool.flute_length;
  const flutes = num("tfFlutes");
  if (flutes >= 1) tool.flutes = Math.round(flutes);
  else delete tool.flutes;
  if (number) tool.number = number;
  else delete tool.number;
  const presets = readCutting();
  const bad = presets.find((p) => p.ramp_angle > 89);
  if (bad) return showError("Ramp angle: 0 to 89 degrees");
  if (presets.length) tool.presets = presets;
  else delete tool.presets;
  if (editing >= 0) lib.tools[editing] = tool;
  else lib.tools.push(tool);
  saveLibrary(lib);
  $("toolForm").classList.add("hidden");
  renderLibraries(lib.id);
}

async function importBytes(bytes, name) {
  showError("");
  const { result } = await engine("importProfile", bytes, name);
  const n = result.notes || [];
  const notes = n.length ? ` (${n.length} note${n.length > 1 ? "s" : ""}: ${n.slice(0, 3).join("; ")})` : "";
  if (result.kind === "tools") {
    const libs = libraries().filter((l) => l.id !== result.profile.id);
    libs.push(result.profile);
    store("marcam.libraries", libs);
    renderLibraries(result.profile.id);
    store("marcam.libSel", result.profile.id);
    $("importInfo").textContent = `Imported tool library "${result.profile.name}": ${result.profile.tools.length} tools${notes}`;
  } else {
    const ms = userMachines().filter((m) => m.id !== result.profile.id);
    ms.push(result.profile);
    store("marcam.machines", ms);
    renderMachines(result.profile.id);
    store("marcam.machineSel", $("machine").value);
    $("importInfo").textContent = `Imported machine "${result.profile.name}"${notes}`;
  }
}

function downloadJson(obj, file) {
  const url = URL.createObjectURL(new Blob([JSON.stringify(obj, null, 2) + "\n"], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = file;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

$("library").addEventListener("change", () => {
  store("marcam.libSel", $("library").value);
  renderTools();
});
$("toolTable").addEventListener("click", (ev) => {
  const b = ev.target.closest("button");
  if (!b) return;
  if (b.dataset.edit) openForm(+b.dataset.edit);
  if (b.dataset.del) {
    const lib = currentLibrary();
    lib.tools.splice(+b.dataset.del, 1);
    saveLibrary(lib);
    renderLibraries(lib.id);
  }
});
$("toolTable").addEventListener("change", (ev) => {
  const s = ev.target.closest("select[data-use]");
  if (s && s.value) useTool(+s.dataset.use, s.value);
});
$("addTool").addEventListener("click", () => openForm(-1));
$("tfCutAdd").addEventListener("click", () => $("tfCutTable").insertAdjacentHTML("beforeend", cutRowHtml({})));
$("tfCutTable").addEventListener("click", async (ev) => {
  ev.target.closest(".cut-del")?.closest("tr").remove();
  const calc = ev.target.closest(".cut-calc");
  if (!calc) return;
  // the row's material and operation, the tool being edited, the machine chosen
  const tr = calc.closest("tr");
  const num = (id) => ($(id).value === "" ? undefined : +$(id).value);
  const shape = { kind: $("tfKind").value, diameter: num("tfDiameter"), angle: num("tfAngle"), tip_diameter: num("tfTip") };
  try {
    const f = await calcFeeds(engine, {
      material: tr.querySelector(".cut-material").value.trim(),
      tool: shape,
      flutes: num("tfFlutes"),
      flute_length: num("tfFlute"),
      op: tr.querySelector(".cut-op").value || undefined,
      machine: typeof machineArg() === "string" ? machineArg() : machineArg()?.id ? machineArg() : null,
    });
    for (const [k] of CUT_FIELDS) tr.querySelector(`.cut-${k}`).value = f.preset[k] ?? "";
    $("tfCalcNotes").textContent = feedsSummary(f);
  } catch (e) {
    $("tfCalcNotes").textContent = e.message;
  }
});
$("toolForm").addEventListener("submit", saveForm);
$("tfCancel").addEventListener("click", () => $("toolForm").classList.add("hidden"));
$("exportLibrary").addEventListener("click", () => {
  const l = currentLibrary();
  if (l) downloadJson(l, `${l.id}.tools.json`);
});
$("importFile").addEventListener("change", async () => {
  const f = $("importFile").files[0];
  if (!f) return;
  try {
    await importBytes(new Uint8Array(await f.arrayBuffer()), f.name);
  } catch (e) {
    showError(e.message);
  }
});
$("importUrlBtn").addEventListener("click", async () => {
  const url = $("importUrl").value.trim();
  if (!url) return;
  try {
    const r = await fetch(url);
    if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
    await importBytes(new Uint8Array(await r.arrayBuffer()), decodeURIComponent(url.split("/").pop().split("?")[0]));
  } catch (e) {
    showError(`Import failed: ${e.message}`);
  }
});
$("job").addEventListener("change", renderTools);

// ---------------------------------------------------------------- resizable panes
window.__marcam.splitters = {
  left: splitter({ bar: $("splitLeft"), target: $("left"), axis: "x", min: 300, max: () => window.innerWidth - 520, id: "left" }),
  code: splitter({ bar: $("splitCode"), target: $("codeWrap"), axis: "x", after: true, min: 200, max: () => $("views").clientWidth - 288, id: "code" }),
  side: splitter({ bar: $("split3d"), target: $("side"), axis: "y", after: true, min: 80, max: () => Math.max(80, $("viewWrap").clientHeight - 260), id: "side" }),
  tools: splitter({ bar: $("splitTools"), target: $("toolTableWrap"), axis: "y", min: 80, max: 900, id: "tools" }),
};
// the 2D side view redraws at its new size (the 3D view has its own ResizeObserver)
new ResizeObserver(() => drawSide()).observe($("side"));

// ---------------------------------------------------------------- project tree (0.2)
window.__marcam.handles = setupHandles({ state, edit: (fn) => window.__marcam.tree?.edit(fn) });
window.__marcam.tree = setupTree({
  engine, $, state, readJob, writeJob, showError, getThree,
  machineArg: () => (typeof machineArg() === "string" && machineArg() !== "custom" ? machineArg() : null),
  onChange: () => { renderTools(); },
});
$("job").addEventListener("change", () => window.__marcam.tree.refresh());

// ---------------------------------------------------------------- auto-regenerate setting
try {
  $("autoGen").checked = localStorage.getItem("marcam.autoGen") !== "0";
} catch {
  $("autoGen").checked = true;
}
$("autoGen").addEventListener("change", () => {
  try {
    localStorage.setItem("marcam.autoGen", $("autoGen").checked ? "1" : "0");
  } catch {}
  if ($("autoGen").checked && state.stale) scheduleAuto();
  // off: an update waiting is dropped (the results are marked out of date instead)
  if (!$("autoGen").checked) {
    clearTimeout(autoTimer);
    autoTimer = null;
    autoPending = false;
    if (state.stale) {
      document.body.classList.add("stale");
      if (state.result) selectTab(state.tab);
    }
    checkSettled();
  }
  window.__marcam.tree?.renderStates?.();
});
window.__marcam.autoDelay = AUTO_DELAY;
window.__marcam.opState = opState;
window.__marcam.liveLoaded = liveLoaded;
window.__marcam.whenSettled = whenSettled;
window.__marcam.planIdle = planIdle;
window.__marcam.setHidden = setHidden;
window.__marcam.hiddenStoreKey = hiddenStoreKey;
// what Generate plans with (the tests plan the saved project again with it)
window.__marcam.planArgs = () => ({ machine: machineArg(), libraries: libraries() });

// ---------------------------------------------------------------- project workflow
window.__marcam.project = setupProject({
  $, engine, state, readJob, writeJob, setMesh, showError, setStatus, generate, refreshAll, clearResults, markStale, resetProject,
  machineArg, currentMachine, selectMachine: (id) => renderMachines(id),
});

// ---------------------------------------------------------------- machining simulation
window.__marcam.machining = setupMachining({ engine, getThree, $, state, readJob, setStatus, showError, libraries, simOpts, confirmSimRes, simGpu, showBackend, simLock, machineArg, simMeshOf: (THREE, sim, s) => simMesh(THREE, sim, s) });
window.__marcam.updateSimResInfo = updateSimResInfo;
window.__marcam.simLock = simLock;

// ---------------------------------------------------------------- view hooks (used by the e2e test)
/** Camera presets: "iso", "front" (looking down the axis from +X), "back" (from -X). `show`
 *  picks layers: { path, part, stock, sim }. Renders immediately. */
window.__marcam.setView = async (name, show = {}) => {
  const v = await getThree();
  if (!v || v.failed || !v.bounds) return false;
  const { xmin, xmax, R } = v.bounds;
  const cx = (xmin + xmax) / 2, span = xmax - xmin;
  if (name === "front") v.camera.position.set(xmax + 6 * R, 0, 0);
  else if (name === "back") v.camera.position.set(xmin - 6 * R, 0, 0);
  else v.camera.position.set(cx - span * 0.2, -span * 1.3, R * 2.2);
  v.controls.target.set(cx, 0, 0);
  v.controls.update();
  // (layers chosen: the playback steps aside until a call without them)
  state.viewShow = show.path === false ? show : null;
  if (state.viewShow && v.groups.machining) v.groups.machining.visible = false;
  for (const [k, on] of Object.entries(show)) if (v.groups[k]) v.groups[k].visible = on;
  if (show.path === false) v.groups.tool.visible = false;
  v.axes.visible = show.axes ?? true;
  v.renderer.render(v.scene, v.camera);
  return true;
};

/** "r,g,b" at a fraction (fx, fy) of the 3D canvas, after a fresh render. */
window.__marcam.pixelAt = (fx, fy) => {
  const v = state.three;
  v.renderer.render(v.scene, v.camera);
  const gl = v.renderer.getContext();
  const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
  const buf = new Uint8Array(4);
  gl.readPixels(Math.floor(fx * w), Math.floor((1 - fy) * h), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, buf);
  return `${buf[0]},${buf[1]},${buf[2]}`;
};

// ---------------------------------------------------------------- developer switches
// ?gpu=off: simulate on the CPU (the GPU is used automatically otherwise); ?json or
// Ctrl+Shift+J: show the project as text
{
  const q = new URLSearchParams(location.search);
  if (q.get("gpu") === "off") $("simGpu").checked = false;
  if (q.has("json")) $("jsonPanel").classList.remove("hidden");
  document.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey && ev.shiftKey && (ev.key === "J" || ev.key === "j")) {
      ev.preventDefault();
      $("jsonPanel").classList.toggle("hidden");
    }
  });
}

// ---------------------------------------------------------------- the side view (optional)
// Off by default (the view cube does most of what it did), remembered in this browser. View
// only: nothing is planned.
{
  const apply = (on) => {
    $("side").classList.toggle("hidden", !on);
    $("split3d").classList.toggle("hidden", !on);
    if (on) drawSide();
  };
  let on = false;
  try {
    on = localStorage.getItem("marcam.sideView") === "1";
  } catch {}
  $("showSide").checked = on;
  apply(on);
  $("showSide").addEventListener("change", () => {
    try {
      localStorage.setItem("marcam.sideView", $("showSide").checked ? "1" : "0");
    } catch {}
    apply($("showSide").checked);
  });
}

// ---------------------------------------------------------------- the menus
// File: what the toolbar's buttons did; a menu closes once something in it is chosen.
$("menuImport").addEventListener("click", () => $("importStl").click());
$("menuReplace").addEventListener("click", () => $("replaceModelFile").click());
$("menuDownload").addEventListener("click", () => $("download").click());
$("menuColors").addEventListener("click", () => openColors());
for (const d of document.querySelectorAll("details.menu")) {
  d.addEventListener("click", (ev) => {
    if (ev.target.closest("button")) d.open = false;
  });
}
document.addEventListener("click", (ev) => {
  for (const d of document.querySelectorAll("details.menu[open]")) if (!d.contains(ev.target)) d.open = false;
});

// ---------------------------------------------------------------- tools from the libraries
window.__marcam.tools = setupToolDialogs({
  $, state, readJob, engine, machineArg,
  writeJob: (doc) => {
    writeJob(doc);
    renderTools();
  },
  userLibraries: libraries,
});
// (Mar) the tool library is a node of the project tree: its panel is shown in the tree's
// details when it is selected, and kept (hidden) in the left pane otherwise
{
  const panel = $("libraryPanel");
  const park = document.createElement("div");
  park.id = "libraryParked";
  park.hidden = true;
  panel.before(park);
  park.appendChild(panel);
  window.__marcam.tools.host = (host) => {
    if (host) {
      if (panel.parentElement !== host) host.appendChild(panel);
      panel.open = true;
      panel.classList.add("hosted");
    } else if (panel.parentElement !== park) {
      park.appendChild(panel);
      panel.classList.remove("hosted");
    }
  };
  window.__marcam.tools.summary = () => {
    const lib = currentLibrary();
    return lib ? `${lib.tools.length} tool${lib.tools.length === 1 ? "" : "s"} · ${lib.name || lib.id}` : "";
  };
}
$("menuToolNumbers").addEventListener("click", () => window.__marcam.tools.openNumbers());
$("menuAddTool").addEventListener("click", async () => {
  if (await window.__marcam.tools.pick()) await window.__marcam.tree?.refresh();
});
