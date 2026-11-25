// Node smoke test of the WebAssembly build: node crates/marcam-wasm/js/test.mjs <marcam.wasm>
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import assert from "node:assert/strict";
import { load } from "./marcam.js";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const wasmPath = process.argv[2] ?? join(root, "web", "pkg", "marcam.wasm");
const cam = await load(readFileSync(wasmPath));

const v = cam.version();
assert.equal(v.name, "marcam");

const job = cam.exampleJob();
let t = Date.now();
const res = cam.planAndPost(job, "makera-z1", null, { max_points: 50000 });
assert.ok(!res.error, res.error);
assert.deepEqual(res.programs.map((p) => p.lines), [21787, 15146]);
for (const p of res.programs) assert.equal(p.issues.length, 0, p.issues.join("\n"));
// byte-identical to the G-code cut on the Z1
for (const p of res.programs) {
  const golden = readFileSync(join(root, "tests", "golden", `${p.name}.nc`), "utf8").replace(/\r\n/g, "\n");
  assert.equal(p.gcode, golden, `${p.name} differs from the golden file`);
}
console.log(`plan+post knob: ${Date.now() - t} ms, lines ${res.programs.map((p) => p.lines)}`);

const lc = cam.planAndPost(job, "linuxcnc");
assert.ok(lc.programs[0].gcode.includes("G93"));

const bad = cam.planAndPost("{", "");
assert.ok(bad.error);

// mesh job: needs the STL, then plans, finishes and simulates
const meshJob = JSON.parse(readFileSync(join(root, "examples", "knob", "knob_finish.job.json"), "utf8"));
const stl = readFileSync(join(root, "examples", "knob", "knob.stl"));
assert.ok(cam.planAndPost(meshJob, "").error);
t = Date.now();
const mres = cam.planAndPost(meshJob, "", new Uint8Array(stl), { max_points: 20000 });
assert.ok(!mres.error, mres.error);
for (const p of mres.programs) {
  assert.equal(p.issues.length, 0);
  assert.ok(p.gouge.max_depth < 0.01, `${p.name} gouges ${p.gouge.max_depth}`);
  assert.equal(p.collisions.count, 0, `${p.name}: holder/shank collisions`);
}

console.log(`plan+post mesh job: ${Date.now() - t} ms, programs ${mres.programs.map((p) => p.name)}`);

const tri = cam.meshTriangles(meshJob, { max_triangles: 5000 }, new Uint8Array(stl));
assert.equal(tri.total, 10080);

t = Date.now();
const sim = cam.simulate(job, { dx: 0.1, na: 180 });
assert.ok(!sim.error, sim.error);
assert.equal(sim.stocks[0].rapid_hits, 0);
assert.equal(sim.stocks[0].gouge_cells, 0);
assert.equal(sim.stocks[0].collisions.count, 0);
assert.equal(sim.stocks[0].r.length, sim.grid.nx * sim.grid.na);
console.log(`simulate knob: ${Date.now() - t} ms, max left ${sim.stocks[0].max_left.toFixed(3)} mm`);
// profiles: built-in list, Fusion tool library import, machine limits
const reg = cam.profileList();
assert.deepEqual(reg.machines.map((m) => m.id), ["makera-z1-4axis", "makera-carvera-4axis", "makera-carvera-air-4axis", "linuxcnc-4axis"]);
const imp = cam.importProfile(readFileSync(join(root, "tests", "fixtures", "fusion-tool-library.json")), "Fixture Tools.json");
assert.equal(imp.kind, "tools");
assert.equal(imp.profile.tools.length, 3);
const lim = cam.planAndPost(job, "carvera-air");
assert.equal(lim.machine_id, "makera-carvera-air-4axis");
// the Carvera reads F on A moves as surface mm/min (no feed above the cap now), but its stock
// firmware turns A at most 5 RPM: the knob's 5209 deg/min is flagged
assert.ok(lim.limits.some((i) => i.message.includes("5209 deg/min") && i.message.includes("1800 deg/min")), JSON.stringify(lim.limits));
console.log(`profiles: ${reg.machines.length} machines, ${reg.libraries[0].tools.length} starter tools, limits ${lim.limits.length}`);
// operation registry and 0.2 projects
const types = cam.opTypes();
assert.deepEqual(types.map((t) => t.kind).sort(), ["bore", "chamfer", "contour", "custom", "drill", "engrave", "face", "flank_chamfer", "helical_knurl", "parting", "pocket", "raster3d", "rotary_finish", "rotary_flowline", "rotary_parallel", "rotary_rough", "rotary_turning", "rotary_wall", "rough3d", "thread_mill", "vcarve", "waterline"]);
const chk = cam.checkProject(JSON.parse(readFileSync(join(root, "examples", "knob", "knob.job.json"), "utf8")));
assert.equal(chk.upgraded, true);
assert.deepEqual(chk.problems, []);
const proj = cam.checkProject(readFileSync(join(root, "examples", "knob", "knob.marcam.toml"), "utf8")).project;
const pr = cam.planAndPost(proj, "makera-z1");
assert.deepEqual(pr.programs.map((p) => p.lines), [21787, 15146]);
console.log(`registry: ${types.length} operation types; 0.2 project plans like the 0.1 job`);
// knurl wizard on an empty setup gives the knob program back
const empty = JSON.parse(JSON.stringify(proj));
empty.setups[0].operations = [];
empty.setups[0].outputs = [];
const wz = cam.wizardKnurl(empty, {});
assert.ok(!wz.error, wz.error);
assert.deepEqual(wz.added.operations, ["op1", "op2", "op3"]);
const wr = cam.planAndPost(wz.project, "makera-z1");
for (const p of wr.programs) {
  const golden = readFileSync(join(root, "tests", "golden", `${p.name}.nc`), "utf8").replace(/\r\n/g, "\n");
  assert.equal(p.gcode, golden, `${p.name} from the wizard differs`);
}
const fr = cam.faceRegions(proj, "profile");
assert.equal(fr.regions.length, 8);
console.log("knurl wizard reproduces the cut G-code; 8 face regions");
// 3-axis plate on the Carvera: XYZ polylines (kinds 2/3), box stock, clean programs
const plate = cam.checkProject(readFileSync(join(root, "examples", "plate", "plate.marcam.toml"), "utf8")).project;
const dome = new Uint8Array(readFileSync(join(root, "examples", "plate", "dome.stl")));
const pl = cam.planAndPost(plate, "", dome, { max_points: 20000 });
assert.ok(!pl.error, pl.error);
assert.equal(pl.machine_id, "makera-carvera-4axis");
assert.deepEqual(pl.stock_box, { min: [0, 0, -10], max: [60, 40, 0] });
assert.deepEqual(pl.programs.map((p) => p.issues.length), [0, 0]);
for (const p of pl.programs) {
  const kinds = new Set();
  for (let i = 3; i < p.toolpath.points.length; i += 4) kinds.add(p.toolpath.points[i]);
  assert.deepEqual([...kinds].sort(), [2, 3]);
}
// 3-axis simulation: the block after each program, compared with the dome
const psim = cam.simulate(plate, {}, dome);
assert.ok(!psim.error, psim.error);
assert.equal(psim.kind, "xyz");
assert.deepEqual([psim.grid.nx, psim.grid.ny, psim.grid.cell], [240, 160, 0.25]);
assert.deepEqual(psim.stocks.map((s) => s.rapid_hits), [0, 0]);
assert.equal(psim.stocks[1].gouge_cells, 0);
const top = (s, x, y) => s.z[Math.floor(y / 0.25) * 240 + Math.floor(x / 0.25)];
assert.ok(Math.abs(top(psim.stocks[0], 15, 20) + 3.5) < 1e-5, "pocket floor");
assert.ok(Math.abs(top(psim.stocks[0], 40, 6) + 4.5) < 1e-5, "hole bottom");
// playback reaches the batch result, with every cut cell tagged by its operation
const po = cam.simOpen(plate, {}, dome);
assert.ok(!po.error, po.error);
assert.equal(po.kind, "xyz");
assert.deepEqual(po.ops.map((o) => o.kind), ["face", "pocket", "drill", "bore", "contour", "raster3d"]);
const end = cam.simSeek(po.segments);
assert.deepEqual(end.rows, [0, 159]);
const zEnd = new Float32Array(Uint8Array.from(atob(end.z), (c) => c.charCodeAt(0)).buffer);
assert.deepEqual([...zEnd], psim.stocks[1].z);
const tags = new Uint16Array(Uint8Array.from(atob(end.op), (c) => c.charCodeAt(0)).buffer);
assert.equal(tags[Math.floor(20 / 0.25) * 240 + Math.floor(15 / 0.25)], 2, "pocket floor cut by op 2");
console.log(`3-axis simulation: ${po.segments} moves, ${po.ops.length} operations, dome max left ${psim.stocks[1].max_left.toFixed(3)} mm`);
console.log(`3-axis plate: ${pl.programs.map((p) => `${p.name} ${p.lines} lines`).join(", ")}`);
// the dome raster with a tapered ball (1 mm tip, 5 deg a side) instead of the ball
{
  const tb = JSON.parse(JSON.stringify(plate));
  tb.tools[1] = { number: 3, description: "tapered ball", kind: "tapered_ball", tip_diameter: 1, taper_angle: 5, diameter: 3.175, flute_length: 12 };
  const r = cam.planAndPost(tb, "", dome, { max_points: 1000 });
  assert.ok(!r.error, r.error);
  assert.deepEqual(r.programs.map((p) => p.issues.length), [0, 0]);
  const ps = cam.simulate(tb, {}, dome);
  assert.ok(!ps.error, ps.error);
  assert.ok(ps.stocks[1].max_gouge < 0.03 && ps.stocks[1].rapid_hits === 0, JSON.stringify({ g: ps.stocks[1].max_gouge, r: ps.stocks[1].rapid_hits }));
  console.log(`tapered ball dome: ${r.programs[1].lines} lines, deepest gouge ${ps.stocks[1].max_gouge.toFixed(3)} mm`);
}
// the dome's raster limited to its cap (a face selection on the mesh): every call that
// lowers the project gets the mesh, so the selection resolves
{
  const capped = JSON.parse(JSON.stringify(plate));
  capped.setups[0].operations.find((o) => o.id === "op5").geometry = [{ kind: "faces", model: "dome", ids: ["f049d19c2"] }];
  const r = cam.planAndPost(capped, "", dome, { max_points: 200000 });
  assert.ok(!r.error, r.error);
  const pts = r.programs[1].toolpath.points;
  let far = 0;
  for (let i = 0; i < pts.length; i += 4) if (pts[i + 3] === 3) far = Math.max(far, Math.hypot(pts[i] - 46, pts[i + 1] - 20));
  assert.ok(far <= 6 + 1.5875 + 0.05, `raster reaches ${far} mm from the dome's axis`);
  const tri = cam.meshTriangles(capped, { max_triangles: 5000 }, dome);
  assert.ok(!tri.error, tri.error);
  const ps = cam.simulate(capped, {}, dome);
  assert.ok(!ps.error && ps.stocks[1].rapid_hits === 0, ps.error);
  console.log(`dome cap only: raster within ${far.toFixed(2)} mm of the axis; meshTriangles and simulate resolve the selection`);
}
// V-carved outline letters on the plate (marcam's own font, inked with a round pen)
{
  const lettered = JSON.parse(JSON.stringify(plate));
  lettered.setups[0].operations.push({ id: "op9", type: "vcarve", tool: 4, text: "4CAM", text_height: 8, text_at: [5, 2],
    max_depth: 1.0, stepover: 0.1, sample: 0.1, feed: 300, plunge_feed: 60 });
  lettered.setups[0].outputs[0].operations.push("op9");
  const r = cam.planAndPost(lettered, "", dome, { max_points: 1000 });
  assert.ok(!r.error, r.error);
  assert.deepEqual(r.programs.map((p) => p.issues.length), [0, 0]);
  console.log(`outline text: plate_1_flat now ${r.programs[0].lines} lines`);
}
// a pocket of any shape: an L with a round island, from paths instead of the rectangle
{
  const shaped = JSON.parse(JSON.stringify(plate));
  const island = Array.from({ length: 49 }, (_, k) => [15 + 4 * Math.cos(k * Math.PI / 24), 15 + 4 * Math.sin(k * Math.PI / 24)]);
  const pocket = shaped.setups[0].operations.find((o) => o.type === "pocket");
  delete pocket.rect;
  pocket.paths = [[[5, 5], [55, 5], [55, 20], [30, 20], [30, 35], [5, 35], [5, 5]], island];
  const r = cam.planAndPost(shaped, "", dome, { max_points: 1000 });
  assert.ok(!r.error, r.error);
  assert.deepEqual(r.programs.map((p) => p.issues.length), [0, 0]);
  assert.equal(r.programs[0].collisions.count, 0);
  const ps = cam.simulate(shaped, {}, dome);
  assert.ok(!ps.error && ps.stocks[0].rapid_hits === 0, ps.error);
  console.log(`shaped pocket: ${r.programs[0].lines} lines, simulated without rapids through stock`);
}
// the plate with T1 held 2 mm out (1 mm of flute): its holder meets the block beside the pocket
{
  const short = JSON.parse(JSON.stringify(plate));
  short.tools[0] = { number: 1, description: "stubby 3.175", kind: "flat", diameter: 3.175, flute_length: 1, stickout: 2 };
  const r = cam.planAndPost(short, "", dome, { max_points: 1000 });
  assert.ok(!r.error, r.error);
  const hit = r.programs.map((p) => p.collisions).filter((c) => c.count > 0);
  assert.ok(hit.length > 0 && hit[0].collisions[0].contact === "holder" && hit[0].collisions[0].line > 0,
    JSON.stringify(r.programs.map((p) => p.collisions.count)));
  const ps = cam.simulate(short, {}, dome);
  assert.ok(ps.stocks.some((st) => st.collisions.count > 0), "the simulation reports them too");
  console.log(`stubby tool: ${hit.map((c) => `${c.count} collisions, deepest ${c.max_depth.toFixed(2)} mm`).join("; ")}`);
}
// indexed 3+1 on the knob: plans clean, playback path carries Y and A for every segment
const kidx = cam.checkProject(readFileSync(join(root, "examples", "knob", "knob_indexed.marcam.toml"), "utf8")).project;
const ki = cam.planAndPost(kidx, "makera-z1");
assert.ok(!ki.error, ki.error);
assert.deepEqual(ki.programs.map((p) => p.issues.length), [0, 0, 0]);
assert.ok(ki.programs[2].gcode.includes("G0 A90.000"));
const kio = cam.simOpen(kidx, { na: 120 });
assert.ok(!kio.error, kio.error);
const kpath = new Float32Array(Uint8Array.from(atob(kio.path), (c) => c.charCodeAt(0)).buffer);
assert.equal(kpath.length, 7 * kio.segments);
const ys = new Set();
for (let i = 0; i < kio.segments; i++) if (kpath[7 * i + 1] !== 0) ys.add(kpath[7 * i + 3]);
assert.deepEqual([...ys].sort((a, b) => a - b), [0], "Y moves only in the A0 D-flat (the hole is on the centreline)");
console.log(`indexed 3+1: ${ki.programs[2].lines} lines, playback ${kio.segments} moves`);
// SVG import: a 40 x 20 user-unit rectangle placed at (5, 5) and scaled to 10 mm tall (20 x 10 mm)
const sv = cam.svgPaths('<svg xmlns="http://www.w3.org/2000/svg"><rect x="0" y="0" width="40" height="20"/></svg>', { at: [5, 5], height: 10 });
assert.ok(!sv.error, sv.error);
assert.equal(sv.count, 1);
assert.deepEqual(sv.paths[0].map((q) => q.join(",")).sort(), ["5,15", "25,15", "25,5", "5,5", "5,15"].sort());
assert.ok(cam.svgPaths("not svg", {}).error);
// wrapped engraving on the Z1 (ring example)
const ring = cam.checkProject(readFileSync(join(root, "examples", "ring", "ring.marcam.toml"), "utf8")).project;
const rg = cam.planAndPost(ring, "makera-z1");
assert.ok(!rg.error, rg.error);
assert.deepEqual(rg.programs.map((p) => p.issues.length), [0]);
console.log(`engraving: ring ${rg.programs[0].lines} lines; SVG rectangle -> ${sv.paths[0].length} points`);
// machining time: per program (split by operation) and in total; any G-code text too
const kt = cam.planAndPost(proj, "makera-z1");
const kfiles = kt.programs.map((p) => p.time.total_s);
assert.ok(Math.abs(kt.time_total_s - kfiles[0] - kfiles[1]) < 1e-6 && kfiles.every((s) => s > 60));
assert.deepEqual(kt.programs.map((p) => p.time.ops.map((o) => o.id)), [["op1"], ["op2", "op3"]]);
const eg = cam.estimateGcode("G0 X0 Y0 Z5\nG1 X100 F600\n", "makera-z1");
assert.ok(eg.feed_s > 10 && eg.feed_s < 10.1, JSON.stringify(eg));
console.log(`time estimates: knob ${kfiles.map((s) => (s / 60).toFixed(1)).join(" + ")} min`);
// simulation resolution: presets and custom sizes give the grid before running
const gN = cam.simGrid(job, { res: "normal" });
const gF = cam.simGrid(job, { res: "fine" });
assert.equal(gN.na, 240);
assert.equal(gF.na, 720);
assert.ok(gF.cells > 5 * gN.cells && !gF.too_large);
assert.equal(cam.simGrid(plate, { cell: 0.5 }).nx, 120);
assert.ok(cam.simGrid(job, { dx: 0.001, na: 8192 }).too_large);
console.log(`resolution: knob normal ${gN.cells} cells, fine ${gF.cells}`);
// WebGPU inputs: positions for the shader, one segment list per program, keys on the grid;
// indexed jobs are refused (they stay on the CPU); the stats of the untouched stock
const go = cam.gpuOpen(job, { res: "normal" });
assert.ok(!go.error, go.error);
assert.equal(go.kind, "rotary");
assert.equal(go.input.prog_seg.length, 3);
assert.ok(go.input.positions > 100000);
const keyBytes = Uint8Array.from(atob(go.keys), (c) => c.charCodeAt(0));
assert.equal(keyBytes.length, 4 * go.grid.nx * go.grid.na);
const st0 = cam.gpuStats({ rapid_hits: 0 }, keyBytes);
assert.ok(!st0.error && st0.gouge_cells === 0 && st0.max_left > 3, JSON.stringify(st0));
assert.ok(cam.gpuOpen(kidx, {}).error.includes("indexed"));
const gp = cam.gpuOpen(plate, { res: "normal" }, dome);
assert.equal(gp.kind, "xyz");
console.log(`WebGPU inputs: knob ${go.input.positions} positions, plate ${gp.input.positions}`);
// picking: the bracket's features, and a drill operation filled from its through holes
const bracket = readFileSync(join(root, "tests", "fixtures", "picking", "bracket.project.json"), "utf8");
const bstl = readFileSync(join(root, "tests", "fixtures", "picking", "bracket.stl"));
const pk = cam.pickables(bracket, {}, bstl);
assert.ok(!pk.error, pk.error);
const holes = pk.pickables.filter((x) => x.kind === "hole");
assert.equal(holes.length, 5);
assert.deepEqual(pk.accepts.drill, ["hole"]);
const through = holes.filter((h) => h.data.through).map((h) => h.id);
const filled = cam.applyPicks(bracket, { type: "drill", ids: through, tool: 1 }, bstl);
assert.ok(!filled.error, filled.error);
assert.equal(filled.params.points.length, 4);
assert.equal(filled.params.depth, 12.5);
assert.ok(cam.applyPicks(bracket, { type: "drill", ids: [pk.pickables.find((x) => x.kind === "pocket").id] }, bstl).error.includes("does not take a pocket"));
console.log(`picking: ${pk.pickables.length} pickables on the bracket, drill points ${filled.params.points.length}`);
console.log("wasm OK");
