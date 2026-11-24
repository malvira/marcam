# Custom toolpaths

A **custom toolpath** is an operation whose moves come from elsewhere: G-code an AI writes
through MCP, a person's Python script, another CAM. marcam reads it into its own moves, so it
is simulated, checked (gouges into the part, holder and fixture collisions, machine limits,
feed caps), estimated and carried on to the next setup like any operation, and posted
through the machine's post (or, with `verbatim`, as its own lines).

It works in any setup: 3-axis on a vise, and on the 4th axis.

## The frame

| Setup | Axes | Meaning |
| --- | --- | --- |
| Vise (3-axis) | X Y Z | The tool tip in the setup's work coordinates (its WCS): Z0 is the top of the stock unless the setup moves its zero |
| 4th axis | X Z A | X along the bar (0 at its front, negative towards the chuck), Z the radius from the axis, A degrees about +X. Y stays 0: moves off the axis are refused |

Units are mm (G21) unless the program says G20 (inches, read and turned into mm).

## What it reads

- Motion: `G0 G1 G2 G3`; arcs in `G17` (XY), `G18` (ZX) and `G19` (YZ), by `I J K`
  (incremental from the start) or `R` (negative: the larger arc); helical arcs (the third
  axis moves linearly). Arcs become lines within 0.002 mm of the circle.
- Modes: `G90 G91`, `G20 G21`; `F` (mm/min), `S`, `A`.
- Tools and spindle: `T`, `M6`, `M3 M4 M5` (M4 is read as M3, with a warning); coolant
  `M7 M8 M9`; `M0 M1` (no stop when planning).
- Work offsets `G54..G59`: read as the setup's WCS (noted once).
- Harmless modal codes: `G40 G43 G49 G61 G64 G80 G94`.
- `G4` (a dwell), comments `( )` and `;`, block numbers `N`, program names `O`, `%`;
  `M2`/`M30` ends it (lines after are not read, with a warning).

Anything else is a **warning with its line number** and that line's motion is not read:
canned cycles (`G81..G89`), `G28 G53`, cutter compensation `G41 G42`, inverse time `G93`,
subprograms `M98`, block delete `/`, unknown words. Nothing is dropped silently.

**Errors** (the program is not planned): a move off the 4th axis (Y not 0), `A` on a vise, an
arc whose ends are not on one circle, an `R` too small for its ends, `A` changing along an
arc, a feed move before the position is known (start with a `G0` to a point), a program with
no feed moves.

Instead of G-code, `moves` takes JSON: `[{rapid?, x?, y?, z?, a?, f?}]`, every axis absolute,
in the same frame and units.

## Its tool, feeds and post

- The tool is the operation's `tool`. Adding one, it is the program's `T` word when the
  project has that tool, else the setup's first. A program naming another tool gets a
  warning; it is planned with the operation's.
- `feed` is used by feed moves before the program's first `F`; `plunge_feed` is the fastest
  descent: a program plunging faster is reported by the verifier, as for any operation.
- By default the moves are posted through the machine's post (its axis words, its decimals,
  its feed mode). With `verbatim: true` the program's own lines are written instead (less
  `%` and `M2`/`M30`, and with `G90 G21 G17 G94` after them if it changed those modes);
  they are still simulated and checked as read.

## How to add one

**In the app**: Add operation → *Custom toolpath (G-code)*, then paste the program or choose
a file (`.nc`, `.gcode`, `.ngc`, `.tap`). Below it, what can't be read or planned is listed
with each line.

**From the command line**:

```sh
marcam add-custom part.marcam.toml vise1 slot.nc --name "Slot from my script"
marcam run part.marcam.toml --out out
```

It prints the program's warnings and errors with their lines, plans the setup with it, and
writes the project (`--out` for another file). `--tool 3` picks the tool, `--verbatim` posts
it as given, `--json` answers in JSON.

**From Python**:

```python
import marcam, json
project = json.load(open("part.project.json"))
lines = ["G21 G90", "G0 X5 Y5 Z5", "G1 Z-0.5 F60"]
lines += [f"G1 X{5 + k} Y5 F300" for k in range(1, 21)]
res = marcam.add_custom_toolpath(project, "vise1", "\n".join(lines), name="my slot")
print(res["read"]["warnings"], res["read"]["errors"])
plan = marcam.plan_and_post(res["project"])  # every setup planned and posted
```

**Through MCP** (`marcam mcp`, see [mcp.md](mcp.md)): the `add_custom_toolpath` tool. An AI
client can follow this:

1. `open_project` on the project file, to see its setups (their kind: vise or rotary), stock
   and tools.
2. Write the program in that setup's frame (the table above). On a vise, start above the
   stock (`G0 Z5`), go down with `G1` at a plunge feed, cut with `G1`/`G2`/`G3`.
3. Call `add_custom_toolpath` with `path`, `setup`, `gcode` (and `tool`, `name`, and `out`
   to write the project):

   ```json
   {"name": "add_custom_toolpath", "arguments": {
     "path": "plate/plate.marcam.toml", "setup": "vise1", "name": "Slot",
     "gcode": "G21 G90\nG0 X10 Y10 Z5\nG1 Z-1 F50\nG1 X40 F400\nG2 X40 Y30 I0 J10\nG0 Z5\n",
     "out": "plate/with_slot.marcam.toml"}}
   ```

4. Read the answer: `read.warnings` and `read.errors` (each `line N: ...`), and `plan`: the
   program's first lines of G-code, its verifier `issues`, warnings, time, gouge depth and
   holder collisions. Fix the program and call again until they are clean.
5. `simulate` the written project to see material left and gouges; `plan` with `out` writes
   the `.nc` files.

The same through `plan`: one new operation `{"type": "custom", "gcode": "..."}` plans it
without adding it to the project.

## Proof

- `cargo test -p marcam-core --lib gcode_in`: the reader (modes, units, arcs by IJK and R,
  helices, the 4th axis, warnings with lines, JSON moves);
- `cargo test -p marcam-core --test custom`: a vise program planned, posted clean and cutting
  where it says (simulated), read back from the post as the same path, verbatim, the 4th
  axis's helix, errors and warnings, a fast plunge reported, JSON moves;
- `cargo test -p marcam-cli --test mcp`: `add_custom_toolpath` on the plate and the knob,
  refused off the axis, and through `plan`;
- `scripts/test-python.sh`: `add_custom_toolpath` from Python;
- `web/tests/usereye.mjs`: Custom toolpath picked in the app, a `.nc` made by a Python script
  chosen, its path and the simulated cut on the screen.
