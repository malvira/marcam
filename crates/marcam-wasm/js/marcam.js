// marcam WebAssembly wrapper (ES module, browser and Node). No build step and no dependencies.
//
//   import { load } from "./marcam.js";
//   const cam = await load(new URL("./marcam.wasm", import.meta.url));   // or the bytes
//   const result = cam.planAndPost(jobObject, "makera-z1");
//
// Every function returns plain JSON-able objects; failures come back as { error: "..." }.

const enc = new TextEncoder();
const dec = new TextDecoder();

async function instantiate(source, imports) {
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
    return (await WebAssembly.instantiate(source, imports)).instance;
  }
  const resp = await fetch(source);
  if (!resp.ok) throw new Error(`could not load ${source}: ${resp.status}`);
  return (await WebAssembly.instantiate(await resp.arrayBuffer(), imports)).instance;
}

/** `opts.onPhase(text)`: told where a long call is as it goes (the operation being planned,
 *  the checks), for diagnostics. */
export async function load(source, opts = {}) {
  let mem = null;
  const imports = {
    env: {
      marcam_phase: (ptr, len) => {
        if (opts.onPhase && mem) opts.onPhase(dec.decode(new Uint8Array(mem.buffer, ptr, len).slice()));
      },
    },
  };
  const inst = await instantiate(source, imports);
  const { memory, fc_alloc, fc_free, fc_call } = inst.exports;
  mem = memory;

  function put(bytes) {
    if (!bytes || bytes.length === 0) return [0, 0];
    const ptr = fc_alloc(bytes.length);
    new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);
    return [ptr, bytes.length];
  }

  function arg(v) {
    if (v === undefined || v === null) return new Uint8Array(0);
    if (v instanceof Uint8Array) return v;
    if (v instanceof ArrayBuffer) return new Uint8Array(v);
    if (typeof v === "string") return enc.encode(v);
    return enc.encode(JSON.stringify(v));
  }

  function call(name, a, b, c) {
    const args = [enc.encode(name), arg(a), arg(b), arg(c)].map(put);
    const res = fc_call(...args.flat());
    for (const [p, n] of args) if (p) fc_free(p, n);
    const len = new DataView(memory.buffer).getUint32(res, true);
    const text = dec.decode(new Uint8Array(memory.buffer, res + 4, len));
    fc_free(res, len + 4);
    return JSON.parse(text);
  }

  return {
    version: () => call("version"),
    exampleJob: () => call("example_job"),
    machine: (name) => call("machine", name || ""),
    // machine: built-in id/alias ("makera-z1-4axis", "carvera", ...) or a machine profile
    // object; opts.libraries: extra tool libraries for {"library", "tool"} references
    planAndPost: (job, machine, meshBytes, opts = {}) =>
      call("plan_and_post_opts", job, { ...opts, machine: machine ?? null }, meshBytes),
    // planAndPost for a preview (an operation being set up, not in the project yet): the same
    // call, under its own name so a host can tell previews from plans
    planPreview: (job, machine, meshBytes, opts = {}) =>
      call("plan_and_post_opts", job, { ...opts, machine: machine ?? null }, meshBytes),
    simulate: (job, opts, meshBytes) => call("simulate", job, opts || {}, meshBytes),
    simSurface: (job, opts, meshBytes) => call("sim_surface", job, opts || {}, meshBytes),
    simHeavy: (job, opts, meshBytes) => call("sim_heavy", job, opts || {}, meshBytes),
    // built-in machine profiles and tool libraries
    profileList: () => call("profile_list"),
    // operation registry, and a 0.1/0.2 document upgraded + checked against it
    opTypes: () => call("op_types"),
    checkProject: (doc) => call("check_project", doc),
    faceRegions: (doc, model, meshBytes) => call("face_regions", doc, model, meshBytes),
    wizardKnurl: (doc, params, meshBytes) => call("wizard_knurl", doc, params || {}, meshBytes),
    // a new operation with every field filled: opts {type, setup, tool, geometry, machine}
    opDefaults: (doc, opts, meshBytes) => call("op_defaults", doc, opts || {}, meshBytes),
    // a setup's stock and part as its job has them (carried from the setup before: turned)
    setupFrame: (doc, opts) => call("setup_frame", doc, opts || {}),
    // feeds and speeds: {material, tool: {kind, diameter, angle?}, flutes?, flute_length?,
    // op?, adaptive?, stepover?, stepdown?, rpm?, machine?} -> {rpm, feed, plunge, ..., preset}
    feedsCalc: (opts) => call("feeds_calc", opts || {}),
    // a setup's stock material: {setup?, material, machine?, libraries?} -> {project, changed}
    setMaterial: (doc, opts) => call("set_material", doc, opts || {}),
    // features of a setup's part to point operations at: {setup?, model?, index_a?}
    pickables: (doc, opts, meshBytes) => call("pickables", doc, opts || {}, meshBytes),
    // the parameters picked features set on an operation: {type, ids, setup?, tool?, index_a?}
    applyPicks: (doc, opts, meshBytes) => call("apply_picks", doc, opts || {}, meshBytes),
    replaceModel: (doc, opts, meshBytes) => call("replace_model", doc, opts || {}, meshBytes),
    resolvePicks: (doc, opts, meshBytes) => call("resolve_picks", doc, opts || {}, meshBytes),
    // project files: the project as TOML text; the single-file bundle (.marcam, a zip of
    // project.toml and the files it names) written from [{ name, bytes }] and read back
    toToml: (doc) => call("to_toml", doc),
    bundleWrite: (doc, files) => {
      const list = (files || []).map((f) => [f.name, f.bytes.length]);
      const all = new Uint8Array(list.reduce((n, [, l]) => n + l, 0));
      let at = 0;
      for (const f of files || []) {
        all.set(f.bytes, at);
        at += f.bytes.length;
      }
      return call("bundle_write", doc, list, all);
    },
    bundleRead: (bytes) => call("bundle_read", null, null, bytes),
    // convert a profile file (marcam JSON, Fusion .tools/.json/.mch/.f3d) -> {kind, profile, notes}
    importProfile: (bytes, name) => call("import_profile", bytes, name || "profile.json"),
    meshTriangles: (job, opts, meshBytes) => call("mesh_triangles", job, opts || {}, meshBytes),
    // indexed 3D roughing's angle search, split across workers: the turns the coarse search
    // needs for {n}, their depths for {setup, tool, keys}, the fine search's turns from
    // {n, keys, depths}, then the n angles from {n, keys, depths} (all of them)
    gapKeys: (opts) => call("gap_keys", opts || {}),
    gapDepths: (job, opts, meshBytes) => call("gap_depths", job, opts || {}, meshBytes),
    gapFineKeys: (opts) => call("gap_fine_keys", opts || {}),
    // the toolpaths planned elsewhere (planAndPost's toolpaths_json), handed to this worker
    // under a key; calls whose opts.plan_key is that key use them instead of planning again
    seedPlan: (key, toolpathsJson) => call("seed_plan", key, toolpathsJson),
    readGcode: (text, frame) => call("read_gcode", text, frame || "three"),
    addCustom: (doc, opts) => call("add_custom", doc, opts),
    gapPick: (opts) => call("gap_pick", opts || {}),
    // the stock a setup starts with (what a setup before left): {triangles, bounds, stl?}
    carriedMesh: (job, opts, meshBytes) => call("carried_mesh", job, opts || {}, meshBytes),
    // material-removal playback: open a session, then seek to any segment
    simOpen: (job, opts, meshBytes) => call("sim_open", job, opts || {}, meshBytes),
    simSeek: (n) => call("sim_seek", String(n)),
    // the open 3D playback's snapshots built on, about `budget` of its replay's cost
    // (opts.min_spacing: the least cost between two)
    simWarm: (budget, opts) => call("sim_warm", String(budget), opts || {}),
    // drop the playback and GPU sessions (a new project)
    simClose: () => call("sim_close"),
    // polylines (mm, Y up) from SVG text, placed with its box at opts.at and opts.height tall
    svgPaths: (svgText, opts) => call("svg_paths", svgText, opts || {}),
    // machining time of any G-code text: {total_s, feed_s, rapid_s, other_s, ...}
    estimateGcode: (gcode, machine) => call("estimate_gcode", gcode, machine || ""),
    // simulation grid for {res: coarse|normal|fine|ultra, dx, na, cell, setup}: cells, memory
    simGrid: (job, opts, meshBytes) => call("sim_grid", job, opts || {}, meshBytes),
    // WebGPU stock simulation: inputs for web/gpu/stamp.wgsl, then the report for a stock
    gpuOpen: (job, opts, meshBytes) => call("gpu_open", job, opts || {}, meshBytes),
    gpuStats: (opts, keyBytes) => call("gpu_stats", opts || {}, "", keyBytes),
  };
}
