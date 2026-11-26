# marcam

An open-source CAM engine for 4th-axis (rotary) and 3-axis machining, written in Rust, with a
web app that runs it entirely in your browser.

![The knurled knob in the app: its toolpaths planned, played back on the stock in 3D, each operation in its colour](docs/media/app-knob.png)

You load a part (STL, 3MF or STEP), set up the stock and how it is held (a bar on the 4th
axis, indexed 3+1, or a block or bar in a vise), pick operations on the part, and marcam
plans the toolpaths, posts G-code for your machine, checks it for unsafe moves and simulates
the cut. Everything updates as you edit: there is no Generate button.

> **Renamed:** this project was called 4cam until 2026-11-17. The crates are now `marcam-*`
> (library `marcam`), the command is `marcam` (`4cam` still works), and projects save as
> `.marcam`. Older files (`.4cam.json`, `.marcam.json`, `4cam-machine` / `4cam-tools`
> profiles) still open. See [CHANGELOG.md](CHANGELOG.md).

## What it looks like

The knob's diamond knurl, cut by a V-bit while the part turns, played back on the stock in 3D:

![The knob's knurl played back on the stock in 3D](docs/media/knob-knurl.gif)

| The knob at the end of its programs | An impeller at rest after its five operations |
| --- | --- |
| ![The knob's stock in 3D at the end: the turned profile in blue, the knurl in yellow, the chamfer in green](docs/media/knob-3d-stock.png) | ![The impeller's simulated stock in 3D, coloured by the material left on the part](docs/media/impeller-3d-stock.png) |

An impeller cut from a bar (indexed 3+1 roughing, rest roughing, a rotary finish, its
blades' faces finished 4-axis simultaneous, then parted off), 449,000 moves, played back at
×1000:

![The impeller's playback from the bar to the finished part, each operation in its colour](docs/media/impeller-playback.gif)

## What it does

- **Setups:** a bar on the 4th axis (simultaneous), indexed 3+1 (the bar at fixed angles),
  or a block or bar in a vise (3-axis). A later setup can take the piece the one before
  left (carried over, turned any way), so a part can be cut in a vise and finished on the
  4th axis.
- **Rotary operations:** turning a profile, roughing and finishing a mesh, rotary parallel,
  flowline and wall finishing (4-axis simultaneous, for an impeller's channels and blades),
  diamond or straight knurls, V-bit flank chamfers, parting off.
- **3-axis operations:** facing, pockets, 2D contours with tabs, drilling and pecking,
  boring, chamfers and deburring, thread milling, 3D roughing (adaptive), 3D parallel and
  waterline finishing, engraving, V-carving, and custom toolpaths from your own G-code.
- **Picked on the part:** faces, edges, pockets and holes are found on the model and picked
  with a click; each pick plans the operation again as you go.
- **Checks:** every program is verified (no fast plunges, no low rapids), gouge-checked
  against the part, and checked for the tool's shank and holder against the stock and the
  fixture (a collision in red, a near miss within 0.5 mm in yellow). The simulation finds
  rapids through the stock (confirmed on a 3D model of the stock), operations that take more
  than twice their tool's normal bite, and the material left on the part. A warning with a
  place jumps the playback there when clicked.
- **Simulation and playback:** the stock is simulated in the background as soon as it is
  shown, then drawn in 3D (a tri-dexel of the bar), each surface in the colour of the
  operation that last cut it. The playback simulates the stock as it plays, from ×1 to
  ×1000 of machine time, with a scrubber; the background simulation runs on the GPU where
  there is one.
- **Machines and tools:** profiles for the Makera Z1, Carvera and Carvera Air (each with
  4th axis) and a generic LinuxCNC machine (G93 inverse-time feeds), a starter tool library
  with cutting data per material, and importers for Fusion 360 tool libraries and machine
  definitions. See [docs/profiles.md](docs/profiles.md).
- **Also:** a command line, a Python package, a WebAssembly package and an MCP server for
  agents, all on the same JSON API.

### Milestones

- **0:** reproduces, byte for byte, the knurled-knob programs that were cut on a Makera Z1
  (`tests/golden/`).
- **1:** rotary finishing from a mesh (STL, 3MF), a radial drop-cutter, a LinuxCNC post.
- **2:** roughing and material-removal simulation; gouge checks; Python and WebAssembly
  packages.
- **3:** the web app (the engine in a Web Worker), tested end to end in headless Chrome.
- Since then: 3-axis setups, carried stock, 4-axis simultaneous finishing, a 3D in-process
  stock, holder and shank checks, live planning. See [docs/roadmap.md](docs/roadmap.md).

## Operations (job reference)

- `rotary_turning`: roughs the stock down to the profile in radial levels with a flat
  endmill moving helically along X, then finishes it in one helical pass.
- `helical_knurl`: a knurl, `pattern` diamond (two families of helical V grooves over a
  conical body) or straight (grooves along X). It comes in from the stock as the operations
  before left it, and says so when the stock stands above the knurl's body.
- `flank_chamfer`: chamfers an edge with the flank of a V-bit while the part turns.
- `parting`: cuts the part off the bar (or grooves it down to a radius) with a flat end mill
  or a V-bit. The tool feeds in helically as the part turns, its depth per turn and its feed
  falling towards the axis (`taper`). It can leave a `sliver` core to break by hand, cut a
  groove wider than the tool in steps along X, and chamfer the part's edge first. Its `x` is
  the groove's part-side edge, picked by clicking the part (snapped to a band's end within
  1 mm), typed, or the part's back by default. Operations after a full cut-off are warned
  about. `examples/knob/knob_parting.marcam.toml` cuts the knob off (golden
  `tests/golden/knob_3_parting.nc`).
- `rotary_rough`: roughs bar stock down to the mesh plus an `allowance`.
  - It cuts in radial levels `step_down` apart; each level is a helix advancing `step_x`
    per turn.
  - At every point the tool sits at the lower of the level and the drop-cutter surface, so a
    level never cuts more than one step and never into the allowance.
  - Levels alternate direction so A stays bounded.
  - `rest`: only where the operations before left stock (on the 3D stock), its bite limited.
- `rotary_finish`: finishes the part mesh with a radial drop-cutter. The tool can be flat,
  ball or V-bit. There are two patterns:
  - `along`: rasters along X at a constant A. Set the stepover with `stepover_deg`, or with
    `stepover_mm` (measured at the largest radius).
  - `around`: one helix around the axis, advancing `stepover_mm` per turn.

  Straight moves are split wherever the surface rises more than `tolerance` (default
  0.005 mm) above them. At a vertical wall the tool climbs before it crosses. `around` adds
  one full turn at the foot of each wall (`clean_walls`, on by default), because a helix only
  reaches a wall's foot at one angle per turn. If the job has
  no `part.mesh`, `rotary_finish` uses `part.profile`, revolved into a 720-facet mesh.
- `rotary_parallel`, `rotary_flowline`, `rotary_wall`: 4-axis simultaneous finishing (X and
  A together): passes along X, rings or a spiral; passes that follow the channels between an
  impeller's blades; waterlines on the blades' faces along their twist.
- 3-axis (vise, or indexed on the bar): `face`, `pocket`, `drill`, `bore`, `contour`,
  `raster3d`, `rough3d`, `waterline`, `engrave`, `vcarve`, `chamfer` (chamfer or deburr
  outlines and holes with a V-bit) and `thread_mill` (threads in holes or on bosses on a
  helix, right or left hand, as G2/G3 on a vise). A `contour` is cut `climb` (the default, or the
  machine profile's `cut_direction`) or `conventional`, and can leave tabs holding the part:
  `tabs` evenly round the outline, or placed by hand (`tab_at`, a click on the outline in the
  app: a fraction of the way round the outline as written, so it stays put when the operation
  is planned again, its direction changes or the model is replaced), each `tab_width` long
  and `tab_height` high. A rapid that would come straight down into the stock (as the moves
  before left it) stops above it and feeds the rest of the way, with a warning.
- `custom`: a toolpath from your own G-code (or JSON moves), checked and posted like
  marcam's own. See [docs/custom-toolpaths.md](docs/custom-toolpaths.md).

Each library tool can carry cutting data per stock material and operation type (the tool
library's form); a setup's `material` picks it for new operations, and a project can give a
library tool rows of its own (Tools > Tool numbers > Cutting data), picked before the
library's. See [docs/profiles.md](docs/profiles.md).

## Usage

Build once:

```bash
cargo build --release
```

Plan a job. This writes one `.nc` and one `.png` preview per program and verifies each
program:

```bash
target/release/marcam run examples/knob/knob.job.json --out out
```

```bash
target/release/marcam run examples/knob/knob_finish.job.json --machine linuxcnc --out out
```

The other commands:

- Toolpath smoothing: operations that make dense paths take a `smoothing` tolerance (engine
  default 0.005 mm for finishing, 0.01 for roughing, 0 off). Points are dropped while the
  path stays within it (measured square to the move and along the tool, so a steep wall is
  not cut into), rotary moves are reduced in X, radius and arc length (at most 45 degrees of
  A per move), corners at one Z are rounded where the blend is worth it, and on a vise the
  post writes circular runs as G2/G3 (each replaced move within the tolerance of the arc).
  The knob example keeps `smoothing: 0`, so its programs stay the ones that were cut.
- `marcam verify file.nc [--plunge 60] [--safe 15.5] [--ramp 30]` flags feed moves where Z
  falls faster than the plunge feed, and X/A rapids below the safe radius. It reads `G93`
  (inverse-time) files too. Like the planner, it lets descents gentler than the ramp angle
  (default 30 degrees, each operation's `ramp_angle`) run at any feed; steeper descents and
  plunges keep to the plunge feed. `--ramp 0` holds every descent to it.
- `marcam simulate job.json [--out dir] [--dx 0.05] [--na 720]` runs every program in order
  on one simulated bar (a radial heightmap) and compares the result with the part.
  - It reports rapids through stock, the material left and any gouges.
  - It writes `<program>_stock.png` for each program: a side view, and an unrolled map of
    the material left.
  - It skips tools without a full shape (a V-bit with no `diameter`).
- `marcam run` also gouge-checks each turning, roughing and finishing program against the part.
- `marcam toolpath job.json` prints the machine-neutral toolpath as JSON.
- `marcam mesh-info part.3mf` prints the triangle count, the bounding box and the largest
  radius about X.

Built-in machines are `makera-z1-4axis` (the default), `makera-carvera-4axis`,
`makera-carvera-air-4axis` and `linuxcnc-4axis`. The short names `makera-z1`, `carvera`,
`carvera-air` and `linuxcnc` work too.

The machine comes from `--machine`, else the job's `"machine"`, else the Z1. `--machine`
also takes a profile file or a URL. `run` checks the job against the machine's limits.

Profiles and tool libraries:

- `marcam profiles list` shows the built-in and imported profiles and libraries.
- `marcam profiles show <id>` prints a profile.
- `marcam profiles import <file-or-url>` reads marcam JSON, Fusion 360 `.tools`/`.json` tool
  libraries, or `.mch`/`.f3d` machines. It saves the result to the user profile folder.
- `marcam profiles export <id> --out file.json` writes a profile out.

Jobs can use library tools:

```json
{"number": 1, "library": "makera-starter", "tool": "spiral-o-metal-3.175x12"}
```

See [docs/profiles.md](docs/profiles.md).

## For agents (MCP)

`marcam mcp --allow <folder>` serves marcam to agents over the Model Context Protocol
(Claude Desktop, Claude Code): describe the operations, open and plan projects (the G-code
`marcam run` writes), simulate, feeds and speeds, pickable features, replacing the model.
Files only inside the folders allowed. See [docs/mcp.md](docs/mcp.md).

## Web app

Build the engine and serve the repository. Either command does both:

```bash
sh web/serve.sh
```

```powershell
powershell -File web/serve.ps1
```

Then open <http://localhost:8000/web/>. Nothing is uploaded: the engine runs in your
browser.

- **File:** New, Open (a `.marcam` project, a `.marcam.toml`, or an older `.marcam.json` /
  `.project.json` / `.job.json`; or drop the file on the page), Save / Save as (one
  `.marcam` file: a zip of the project as TOML and its part, so it opens whole anywhere),
  and Import model (STL, 3MF or STEP, mm or inch). The project's name shows with an
  "unsaved" mark; New, Open and the examples ask before discarding changes.
- **The project tree** (left): the tool library, then each setup with its Stock, Part,
  Fixture, tools and operations. A click anywhere on a row opens its details below the tree,
  where every setting is edited in place and applies as it is changed:
  - a setup's type (Rotary 4th axis, Indexed 3+1, 3-axis vise) as cards at the top; a new
    type converts the setup (the part placed again, the stock re-sized, operations that do
    not fit removed, with an Undo);
  - the stock (a round, square, hex or rectangular bar, or a block; fitted to the part with
    a margin, or typed), and the fixture (chuck, tailstock, vise jaws), drawn and checked;
  - the part, moved with handles in the 3D view (arrows and rings, rotary: along X, round
    and across the axis) or with Center on axis, Flush to front and the like;
  - each operation with a badge (✓ planned, • changed, ◐ being planned, ⚠ failed), its
    colour, an eye to hide it, duplicate and delete; dragged (or Alt+Up / Alt+Down) to
    reorder. Add operation offers only the types that fit the setup. Operations that work on
    a feature are picked on the part with a click.
  - **+ Add setup** adds one that continues from the piece the setup before left, or starts
    from its own stock.
- **Live updating** (on by default; Tools > Update automatically): there is no Generate
  button. Half a second after an edit (never while a mouse button is held) the operations it
  touched are planned again in the background, the others kept (the engine's plan cache).
  The results stay on screen until the new ones come; Download .nc waits for them. Tools >
  Replan all plans everything now.
- **Results** (one tab per program): the line count, the verifier, machine limits, the
  estimated time, the holder/shank check, the gouge check, the simulated stock's numbers and
  warnings (each foldable, each entry with a place jumping the playback there), the G-code
  with **Download .nc**, and a side view.
- **The 3D view** (three.js): its toggles, also as keys with the pointer over the view: Part
  (P), Simulated stock (S), the stock's outline (B), Toolpaths (T: all, up to the playhead,
  or none), Rapids (R), the Tool (C), Only the selected operation (O), the Chuck (K) and the
  Tailstock (L), and Play (Space). The simulated stock appears with nothing clicked: the
  uncut bar at once, the simulation's stock when it lands (progress in the ring in the
  view's corner), then its 3D surface.
- **Playback:** the tool at its place, the toolpath drawn as it is played (each operation in
  its colour), the stock simulated along in 3D. Speed in machine time, ×1 (real time) to
  ×1000 on a log scale; Step; ⇥ to the end; a scrubber; the label shows the machine time
  reached of the total and the operation playing. Hidden operations are left out of
  everything. Speed and stock are remembered.
- **Simulation grid** (Coarse, Normal, Fine, Ultra, or custom cell sizes): the background
  simulation runs at Coarse first, then at the chosen grid when it is quick; one estimated
  over 30 seconds asks first.
- **Tools > Tool library:** view, add, edit and remove tools and their cutting data, import a
  file or URL (Fusion 360 `.tools`/`.json`/`.mch`/`.f3d` or marcam JSON), export JSON.
- **View > Colours:** every colour the 3D view draws with, kept in this browser, with Reset.

`node web/tests/e2e.mjs` drives the page in an installed Chrome or Edge in headless mode,
over the DevTools protocol, with no npm packages. It checks:

- the knob programs match the golden G-code byte for byte, with 0 issues;
- the 3D view renders and the scrubber works;
- **Download .nc** saves the right file;
- the simulation (in 3D) shows no gouges, and the playback's stock matches it;
- the LinuxCNC post uses G93;
- live planning, the setups, carried stock, the handles and error messages;
- the mesh rough/finish job;
- the phone layout.

The test saves screenshots to `web/tests/screenshots/`.

## Python and WebAssembly

Both packages expose the same JSON API:

- `plan_and_post(job, machine, mesh)` returns each program's G-code, verifier issues, gouge
  check and a tool-tip polyline.
- `simulate(job, ...)` returns the stock heightmap after each program.
- There are also `example_job`, `machine` and `version`.

Python: build the wheel with the standard library only, then install it:

```bash
python scripts/build_wheel.py
```

```bash
pip install target/wheels/marcam-0.1.0-cp39-abi3-win_amd64.whl
```

The wheel name depends on your platform. To use it:

```python
import marcam
res = marcam.plan_and_post(marcam.example_job(), machine="linuxcnc")
print(res["programs"][0]["gcode"][:200])
```

WebAssembly: `scripts/build-wasm.sh` (or `.ps1`) writes `web/pkg/marcam.wasm` and
`marcam.js`. The module has plain exports, so wasm-bindgen is not needed. `marcam.js`
wraps them for browsers and Node:

```js
import { load } from "./pkg/marcam.js";
const cam = await load(new URL("./pkg/marcam.wasm", import.meta.url));
const res = cam.planAndPost(cam.exampleJob(), "makera-z1");
```

`scripts/check.sh` runs everything:

- `cargo fmt` and `cargo clippy`;
- the Rust tests;
- the WebAssembly package under Node, checked against the golden G-code;
- the Python wheel, installed in a fresh venv and tested.

## Examples

`examples/knob/` has the jobs, the revolved `knob.stl` and, in `output/`, the G-code and
PNG previews they produce, and in `output/sim/` the simulated stock after each program. You
can look at all of these without running anything. Each preview
has two panels: a side view (X against radius, with the finished part filled in grey) and
an unrolled view (X against A, coloured by radius).

## Coordinates

- A rotates the part about X, in degrees. Positive A turns it right-handed about +X.
- X0 is the front face of the part, which extends toward -X.
- Y0/Z0 is the rotation axis, so Z is the radius at the tool tip.
- Units are mm.

A mesh goes on the axis with `part.mesh.axis` (`"x"`, `"y"` or `"z"`: the mesh axis that
becomes X) and `part.mesh.translate`.

## Tests

```bash
cargo test
```

The tests cover:

- the golden G-code;
- the drop-cutter against a brute-force search, and against the analytic knob profile
  (worst error about 1e-4 mm);
- finishing paths, which must never dip more than 0.01 mm below the part;
- the simulated stock after mesh roughing and finishing: no gouges, the allowance kept, and
  the body within 0.06 mm of the part;
- G93 feed arithmetic;
- the mesh import formats;
- the CLI end to end;
- seeded fuzzing of planning and the simulation (`crates/marcam-wasm/tests/fuzz.rs`):
  random parts and operations pushed to extremes, each planned, posted and played back,
  failing on a panic, a NaN, too much time or too much memory. A fixed set runs with
  `cargo test`; `MARCAM_FUZZ_CASES=500` runs more, `MARCAM_FUZZ_SEED=<n> MARCAM_FUZZ_CASES=1`
  replays one (`web/tests/simfuzz.mjs` is a smaller run in the browser).

### New features: checked as a person sees them

Every new feature gets a check in `web/tests/usereye.mjs` before it is merged or deployed.
The check takes the path a person would, with real mouse and keyboard input in a fresh page:

1. Find the feature by its name on screen.
2. See its fields.
3. Use it.
4. See its result on screen.

After deploying, run usereye again against the deployed app
(`node web/tests/usereye.mjs --url <the app's address>`). A feature is not done, or live,
until that passes. Engine tests and `e2e.mjs` are not enough on their own: they set the
project directly and can pass while the feature is hidden from a person.

The merge checklist:

1. `scripts/check.sh --fast` green.
2. The feature's usereye check added and passing.
3. Merged and deployed.
4. usereye green against the deployed app.

Planning and simulating are bounded: an operation set too fine to plan (more than 2 million
moves, or 300 million units of surface checking) fails in seconds with the reason, and a
simulation estimated to take longer than 10 minutes in a browser is refused, naming the
finest resolution that fits (the app asks first about anything over 30 seconds, and shows a
progress ring in the 3D view that cancels on a click).

## Layout

- `crates/marcam-core`: the library (`marcam`). It holds the job schema, geometry, mesh
  I/O, the drop-cutter, the strategies, the toolpath IR, the post-processor, the verifier
  and the PNG previews.
- `crates/marcam-cli`: the `marcam` binary.
- `crates/marcam-wasm`: the WebAssembly build and its JSON API (`js/marcam.js` wraps it).
- `crates/marcam-py`: the Python bindings (pyo3, abi3, Python 3.9 or later).
- `web/`: the browser app (`index.html`, `app.js`, `worker.js`) and its end-to-end test.
- `scripts/`: the build and check scripts.
- `crates/marcam-core/profiles/`: built-in machine profiles and tool libraries
  (`profiles/_downloads/` at the root is a git-ignored cache of vendor files).
- `docs/`: the profile formats, Makera sources and the roadmap;
  [docs/embedding.md](docs/embedding.md) for embedding marcam (Rust, C/C++, C#/.NET, Python,
  JavaScript; runnable examples in `examples/embed/`) and
  [docs/packaging.md](docs/packaging.md) for the crates, wheel and npm package (prepared,
  not published).
- `examples/`: example jobs and their outputs.
- `tests/fixtures/`: test fixtures. These are the knob's original Python part and G-code
  generators, and a synthetic Fusion-format tool library. `crates/marcam-fixtures`
  regenerates `examples/knob/knob.stl`. Part generation is a fixture concern, not an engine
  feature.

## License

Apache-2.0. See `LICENSE`.
