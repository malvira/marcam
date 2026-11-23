# Changelog

## Unreleased

### STEP import: no missing faces, and big parts drawn whole (2026-11-23)

A face SolidCraft's kernel cannot trim (one wound round a closed surface that no iso line can
seam, as an impeller's hub floor between twisted blade roots) was left out, so the part came in
with a hole. Such faces, and bodies the kernel cannot mesh, are now meshed by `marcam-step`
from the file itself, within the chord (warning: "face #N meshed from the file"). The web app's
3D view kept every n-th triangle of a part over its display budget (a STEP part at 0.01 mm has
about 440 000), which drew it striped with holes; it now merges nearby vertices instead, so
the part stays whole. Planning always used every triangle.

### Project files are TOML (2026-11-19)

Projects are written as TOML (`name.marcam.toml`), easier to read and edit by hand: the
model's key order, `[[setups.operations]]` tables, no unset fields, numbers as typed,
comments allowed. Part files stay beside it by relative path; the web app saves one
`name.marcam` file (a zip of `project.toml` and the part). JSON keeps reading: `.marcam.json`,
`.project.json` and 0.1 `.job.json` files open as before; `--json` writes JSON. The examples'
projects are now `examples/*/*.marcam.toml` (the 0.1 `knob.job.json` stays, for the goldens).
New: `marcam convert`, `marcam::project_file`, Python `marcam.to_toml`, WebAssembly `toToml`,
`bundleWrite`, `bundleRead`. See docs/api.md, "Project files".

### Renamed: 4cam is now marcam (2026-11-17)

The GitHub repository is now [malvira/marcam](https://github.com/malvira/marcam). Everything
that carried the old name was renamed:

| Was | Now |
| --- | --- |
| crates `fourcam-core`, `-cli`, `-wasm`, `-py`, `-gpu`, `-capi`, `-fixtures` | `marcam-core`, `marcam-cli`, ... |
| Rust library `fourcam` | `marcam` |
| command `4cam` | `marcam` (`4cam` is kept as an alias) |
| C library `fourcam_c`, header `fourcam.h`, functions `fourcam_*` | `marcam_c`, `marcam.h`, `marcam_*` |
| Python module `fourcam`, FreeCAD package `fourcam_freecad`, macro `4cam_plan.FCMacro` | `marcam`, `marcam_freecad`, `marcam_plan.FCMacro` |
| npm package and web files `fourcam.js` / `fourcam.wasm` | `marcam`, `marcam.js` / `marcam.wasm` |
| SolidCraft plugin `fourcam.zip` | `marcam.zip` |
| project files `.4cam.json` (`"format": "4cam-project"`) | `.marcam.json` (`"marcam-project"`) |
| profile formats `4cam-machine`, `4cam-tools` | `marcam-machine`, `marcam-tools` |
| `$FOURCAM_PROFILES`, `%APPDATA%cam\profiles` | `$MARCAM_PROFILES`, `%APPDATA%\marcam\profiles` |

Still read: `.4cam.json` projects, `4cam-machine` and `4cam-tools` profiles,
`$FOURCAM_PROFILES` and an existing `4cam` profile folder (when there is no `marcam` one),
and the web app's settings saved under `4cam.*`.

The G-code is unchanged: no program text carries the name.
