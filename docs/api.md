# Embedding marcam

marcam is meant to be easy for other software to embed, the way FDM slicers embed a slicing
engine. Every host gets the same engine, defaults and checks. The surfaces below are the
supported API. Everything else is internal and may change.

## The document

The document is a project, `schema_version = "0.2"`, written as TOML (see "Project files"
below; JSON reads too). It holds:

- models: a revolved profile and/or a mesh file. A mesh model can instead be a grayscale
  image (PNG or PGM) with `"image": {"width", "height", "depth", "base", "invert", "wrap"}`:
  a relief, flat (white at `base + depth`) or wrapped round the rotary axis (`base` is then
  the radius of black). `MeshSource::mesh(bytes)` builds either kind;
- tools: inline, or references to a library;
- setups: machine, stock, holding, part, operations, and outputs (G-code files).

  Stock is a bar on the rotary axis (`{"shape": "cylinder", ...}`), the previous setup's
  result (`"from_previous"`), or a block in a vise (`{"shape": "box", "min": [x, y, z],
  "max": [x, y, z]}`, work coordinates, Z up, Z0 normally the top). Box setups take the 3-axis
  operation types (`face`, `pocket` (a rectangle, or any closed `paths` with islands),
  `drill`, `contour`, `raster3d`); bar setups take the
  rotary ones, and also the 3-axis types with `index_a` (deg): indexed 3+1, cut with the
  part turned to that A. A vise setup rejects a non-zero `index_a`. Example: `examples/plate/plate.marcam.toml`.

  Stock carried between setups: `{"shape": "from_previous", "turn": "y180", "offset": [x, y,
  z]}` starts a setup from what the setup before it left (planned and simulated), in a vise
  or on the rotary axis like that one. `turn` is `x180` or `y180` (over: front to back, left
  to right), `z180` (end for end) or absent; the stock is re-zeroed where it was (its X and
  Y low corner, and a block's top at Z0), then moved by `offset`. The part turns and moves
  with it (the setup's job takes the previous one's part placement, turned: `MeshSource`
  has a `turn`), so the other side of a block can be machined after the first. A block
  carries each cell's top and bottom, so cuts from either side are kept, through-cuts
  included (`marcam::carry`). From it, 3D roughing works only where material is left (a
  rest roughing of the carried block, also below: the other side's pocket is a hollow it
  skips), adaptive clearing too, and the simulation starts from it. A bar's `radius` and
  `length` are the previous setup's unless given. `carry::carry_stock` works it out for a
  project's jobs; `marcam run`, the web app, `marcam mcp` and the plugins do it.

  G-code options per setup: `"post": {"work_offset": "G55", "header": ["M8"], "footer":
  ["M9"], "line_numbers": true, "no_comments": true}`. The work offset (`G54`..`G59`,
  `G59.1`..`G59.3`, `G54.1 P<n>`) and the header lines come after the machine's preamble,
  the footer before its postamble (`{tool}`, `{rpm}`, `{safe}` filled in); line numbers are
  `N10`, `N20`, ...; without comments, `(...)` and `;` comments go. A drill operation with
  `"canned": true` (a vise setup) is written as one drilling cycle (`G98 G83 ... R Q F`, or
  `G81` when it does not peck, then `G80`) on a machine whose post has
  `"canned_cycles": true` (LinuxCNC's), switching from inverse-time feeds to G94 around it;
  elsewhere as its moves, with a comment saying so. The toolpath keeps the moves, so the
  simulation and the checks are the same; the verifier and the G-code time estimate read
  the cycles as the moves they make.

0.1 jobs upgrade automatically. Machine profiles and tool libraries are separate JSON files
(`docs/profiles.md`).

### Project files

marcam writes projects as TOML and reads TOML or JSON (JSON forever: `.marcam.json`,
`.project.json` and 0.1 `.job.json` files keep opening). The model is the same in both forms.

- **`name.marcam.toml`**: the project. Part files (STL, 3MF, STEP, a relief's image) stay
  beside it, named by relative path (`file = "knob.stl"`). Keys come in the model's order;
  operations are `[[setups.operations]]` tables; unset fields are not written; numbers are
  written as typed (3.175, not 3.1750000000000003). Comments (`# ...`) are allowed and
  ignored. Lengths are mm, angles degrees, feeds mm/min unless a key says otherwise.
- **`name.marcam`**: the single-file form the web app saves: a zip of `project.toml` and the
  part files it names (and `view.json`, the web view: which toolpaths are hidden). Any zip
  tool opens it.

A short one:

```toml
schema_version = "0.2"
name = "knob"

[[models]]
id = "part"
kind = "profile"
points = [[-20.0, 10.0], [-10.0, 8.0], [0.0, 6.0]]   # X (back to front), radius

[[tools]]
number = 1
description = "3.175 mm flat"
kind = "flat"
diameter = 3.175
flute_length = 15

[[setups]]
id = "s"
kind = "rotary"
stock = { shape = "cylinder", radius = 11.0, length = 30.0 }
clearance = 3.0
part = ["part"]
spindle_rpm = 12000

[[setups.operations]]
id = "op1"
type = "rotary_turning"
tool = 1
rough_depth = 0.4
rough_step = 1.0
rough_feed = 300.0
finish_allowance = 0.15
finish_step = 0.4
finish_feed = 250.0
plunge_feed = 60.0

[[setups.outputs]]
name = "knob"
title = "turned"
operations = ["op1"]
```

A setup may also list named NC exports (as Fusion's NC programs): some of its operations, from
any of its outputs, posted together to `<name>.nc` in planning order. Their toolpaths are the
planned ones (not planned again); where an operation does not follow straight on from the one
before it in the export, the post retracts, loads its tool and comes over to where it was
planned from. `posted` is the fingerprint of the G-code last written: the web app compares it
with what the operations post now and marks the export out of sync when they differ.

```toml
[[setups.exports]]
name = "turn_and_chamfer"
operations = ["op1", "op3"]
posted = "9f0c3a1b2d4e5f60"          # written by the app; optional
posted_at = "2026-11-28T05:10:00Z"
```

`plan_and_post` (WebAssembly) returns `exports: [{name, operations, missing, lines,
fingerprint, gcode}]` for the setup planned; Rust: `marcam::export::{splice, fingerprint}`.

Every function below that takes a document takes it as TOML text, JSON text or a parsed
object. Rust: `marcam::project_file` (`to_json_text`, `project_to_toml`, `write_bundle`,
`read_bundle`); WebAssembly: `toToml(doc)`, `bundleWrite(doc, files)`, `bundleRead(bytes)`;
Python: `marcam.to_toml(project)`; CLI: `marcam convert <file> [--json] [--bundle x.marcam]`
(and `marcam upgrade` writes TOML unless `--json`).

## Rust (`marcam` crate)

| Call | What it does |
| --- | --- |
| `project::Project::from_json_any(text)` | Parse a 0.1 job or 0.2 project. |
| `project::jobs_from_any_with(text, libraries, meshes)` | One plannable job per setup. |
| `plan_with_mesh(&job, mesh)` | Plan a setup into toolpaths (machine-neutral). |
| `post(&toolpath, &machine)` | Toolpath to G-code. |
| `verify_gcode(text, &limits)` | Unsafe-move check. |
| `kinematics::solve_a` / `to_machine` | General moves (`Move::Pose`: tip and tool direction in the part frame) solved for a machine. |
| `profiles::check_limits` | Check a job against a machine profile's limits. |
| `estimate::estimate_toolpath` / `estimate::estimate_gcode` | Machining time of a program (split by operation) or of any G-code text, for a machine profile. |
| `gouge_check` | Check toolpaths against the part. |
| `collision_check` | Tool shank and holder against the stock, per move with its G-code line (`collision::ToolBody` gives a tool's defaults). `simulate_job` reports the same per program. |
| `ops::registry()` | Operation types and their parameters. |
| `ops::check` | Validate an operation. |
| `project::Project::op_defaults` / `ops::defaults` | A new operation with every field filled from the tool, spindle, machine limits and part faces. |
| `geometry::face_regions` | Pickable faces with stable ids. A `faces` selection on `raster3d` or `rotary_finish` keeps the passes where the tool rests on those faces (`face_subset` gives their triangles). |
| `wizards::KnurlWizard` | The knurl wizard. |
| `sim::Playback` | Material-removal playback (rotary). |
| `sim3::Heightmap3` | Material removal for 3-axis programs (a Z heightmap of the block). |
| `project::simulate_project` | Simulate all setups in order. |

## WebAssembly (`web/pkg/marcam.js`)

All calls take and return JSON. No wasm-bindgen is needed.

- **Planning and posting:**
  - `planAndPost(doc, machine, mesh, {setup, libraries})` returns G-code, issues, the limit
    check, the gouge check and a toolpath polyline. Polyline points are `[x, r, a, kind]` for
    rotary programs (kind 0 rapid, 1 feed) and `[x, y, z, kind]` for 3-axis ones (kind 2
    rapid, 3 feed); box setups also return `stock_box`.
- **Operations:**
  - `opTypes()` lists the operation types;
  - `opDefaults(doc, {type, setup, tool, geometry, machine}, mesh)` returns a new operation
    with every field filled;
  - `checkProject(doc)` upgrades and validates a document.
- **Geometry and wizards:**
  - `faceRegions(doc, model, mesh)` returns the model's faces with stable ids;
  - `pickables(doc, {setup, model, index_a}, mesh)` returns the features of a setup's part
    an operation can be pointed at (`hole`, `pocket`, `flat_face`, `edge_loop`, `surface`
    on a vise or an indexed setup turned to `index_a`; `cylindrical_band`,
    `revolved_band`, `surface` on a rotary one), each with an `id`, a `label`, the face ids
    it covers, a `similar` key ("select all similar") and its numbers (`data`), plus
    `accepts`: the kinds each operation type takes;
  - `applyPicks(doc, {type, ids, setup, tool, index_a}, mesh)` returns `{params, notes}`:
    the fields picked features set on an operation of that type (drill: points, top, depth,
    peck; pocket: paths, top, depth; contour: path, side, top, depth; face: depth; finishing:
    faces; rotary roughing: x_range), and notes such as "use Bore instead" for a hole the
    tool cannot drill. Python: `marcam.pickables(project, mesh, setup=...)` and
    `marcam.apply_picks(project, type, ids, mesh, ...)`; C: `marcam_pickables`,
    `marcam_apply_picks`; Rust: `marcam::pickables`;
  - each pickable also has a `signature` (its kind, label and numbers, without the mesh's
    face ids). An operation keeps the signatures of what was picked for it in `picks`;
  - `replaceModel(doc, {model, file}, mesh)` replaces a mesh model with a new part (`mesh`:
    its bytes; `file`: the name the project gives it). Its placement, the setups, tools and
    operations stay. Every operation's `picks` are looked for on the new part (within
    0.05 mm, or 1 % of a size). Found: the operation is derived from them again. Not found,
    or more than one match: it gets an `error` naming the feature, stays in the project and
    is left out of planning until it is picked again or its type is changed. Returns
    `{project, operations: [{setup, op, ok, picks, error}]}`. Python:
    `marcam.replace_model(project, mesh, file, model=None)`; Rust:
    `marcam::replace::replace_model`; CLI: `marcam replace-model`;
  - `wizardKnurl(doc, params, mesh)` runs the knurl wizard.
- **Simulation:**
  - `gpuOpen(doc, {res, dx, na, cell, setup}, mesh)` / `gpuStats(opts, keyBytes)`: inputs
    for the WebGPU stock simulation (`web/gpu/stamp.wgsl`, run by `web/gpu/gpusim.js`) and
    the report for a stock it produced; an error means "simulate on the CPU";
  - `simGrid(doc, {res, dx, na, cell, setup}, mesh)` gives the grid a resolution makes
    (cells, memory) before running; `res` is coarse, normal, fine or ultra;
  - `simulate(doc, {res, dx, na, cell}, mesh)` simulates material removal for the whole job.
    Box setups return `"kind": "xyz"`, an X/Y `grid` (`cell` mm, default about 240 cells
    along the longer side), the part top per cell and the block tops `z` after each program;
  - `simOpen(doc, {setup, dx, na, cell}, mesh)` and `simSeek(n)` play it back move by move
    (for box setups the changed rows are Y rows, with `z` and the operation tags).
- **Profiles:**
  - `estimateGcode(gcode, machine)` estimates the machining time of any G-code text;
    `planAndPost` results carry `time` per program (with `ops`) and `time_total_s`;
  - `svgPaths(svgText, {at, height, tolerance})` turns an SVG into polylines (mm, Y up) for
    `engrave`/`vcarve` paths;
  - `profileList()` lists the built-in machines and tool libraries;
  - `importProfile(bytes, name)` reads a marcam or Fusion 360 profile file.

## Python (`import marcam`)

The same JSON API is available from Python as `plan_and_post`, `simulate`, `example_job`,
`machine` and `version`.

## CLI (`marcam`)

The CLI covers the same features:

- `run`, `simulate` and `verify`;
- `upgrade`;
- `ops list|show|check|defaults`;
- `faces`;
- `pickables` (`--type drill` lists only what drilling takes, `--json` for programs);
- `replace-model <project> <model> <new mesh>` (exits 1 when an operation is put in error);
- `wizard knurl`;
- `profiles list|show|import|export`;
- `mesh-info`.

## Adapters

- MCP: `marcam mcp` serves the engine to agents over the Model Context Protocol (stdio).
  See `docs/mcp.md`.
- SolidCraft: `adapters/solidcraft` is a SolidCraft command plugin: `plan` runs any marcam
  operation on a body and writes G-code to a granted folder, `describe` lists the operations
  and their fields, `plan-project` plans a whole project from a folder. See
  `docs/adapters/solidcraft.md`.
