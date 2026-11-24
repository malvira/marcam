# Operation generators and plugins: design note

Status: proposal, for review before the plugin code is written. The custom toolpath (static
G-code, [custom-toolpaths.md](custom-toolpaths.md)) is done; script generators come next,
then the plugin API. Both share one **context** in and one **result** out, so a script can
become a plugin, and a plugin a built-in operation, without its interface changing.

## The three kinds of source

| Kind | Runs where | Language | Stored in the project |
| --- | --- | --- | --- |
| Static | nowhere (data) | G-code or JSON moves | the program text |
| Script generator | the user's machine only: CLI, MCP, Python; in the browser, JS in a worker | Python (CLI/MCP/Python), JS (browser) | the script's text and its parameters' values |
| Plugin | anywhere: native (wasmtime) and the browser (WebAssembly in a worker) | any language that builds `wasm32-unknown-unknown` (Rust SDK provided) | a reference: id, version, SHA-256 of the module; the module bundled in `.marcam` files |

Every kind ends the same way: its moves are planned as a custom toolpath's are
(`marcam::strategy::custom`), so they are simulated, checked (gouges, holder and fixture
collisions, machine limits, feed caps), estimated, carried on and posted like built-in
operations. A generator can never skip the checks.

Safety: Python runs only where the user runs marcam (CLI, MCP, the Python package), never
fetched from the web; the browser runs JS generators and plugins in a worker with no network
and no DOM; plugins get no filesystem, network, clock or randomness (pure functions of their
input, so a project replans the same way).

## The context (in)

JSON, version 1. Units mm, degrees, mm/min; the setup's frame (vise: X Y Z, the tool tip,
the setup's WCS; 4th axis: X along the bar, Z the radius, A degrees about +X).

```json
{
  "context_version": 1,
  "setup": {"id": "vise1", "kind": "vise|rotary", "machine": "makera-carvera-4axis",
            "spindle_rpm": 12000, "clearance": 3.0, "safe": 5.0},
  "stock": {"shape": "box", "min": [0, 0, -10], "max": [60, 40, 0]},
  "stock_now": {"kind": "heightmap|radial", "grid": {...}, "values": "base64 f32"},
  "part": {"mesh": "base64 binary STL | null", "bounds": [[...], [...]]},
  "picks": [{"kind": "edge_loop", "data": {...}}],
  "tool": {"number": 1, "kind": "flat", "diameter": 3.175, "flute_length": 12, "angle": null},
  "feeds": {"feed": 400, "plunge_feed": 60, "ramp_angle": 5},
  "params": {"depth": 1.0, "pitch": 2.5}
}
```

- `stock_now` is the in-process stock as the operations before this one left it (a
  heightmap on a vise, the radial map on the 4th axis), coarse by default; a generator asks
  for it in its manifest (`needs: ["stock_now"]`) since it can be large.
- `part.mesh` likewise only when asked for (`needs: ["part_mesh"]`); `picks` are the
  features picked for the operation, as the built-in operations get them.

## The result (out)

```json
{
  "result_version": 1,
  "moves": [{"rapid": true, "x": 5, "y": 5, "z": 5}, {"z": -1, "f": 60}, {"x": 25, "f": 400}],
  "gcode": null,
  "warnings": ["the pitch was rounded to 2.5 mm"],
  "notes": ["4 passes"]
}
```

`moves` (the JSON moves the custom toolpath reads) or `gcode` (text, read the same way). An
`error` string instead stops the operation with that message. Warnings appear as for any
operation.

## The manifest (both scripts and plugins)

```json
{
  "id": "acme.helical-engrave", "name": "Helical engraving", "version": "1.2.0",
  "license": "MIT", "authors": ["..."],
  "setup_kinds": ["rotary"], "tool_shapes": ["vbit"], "picks": ["cylindrical_band"],
  "needs": ["stock_now"],
  "params": [
    {"name": "depth", "kind": "number", "unit": "mm", "default": 0.3, "min": 0.01, "help": "..."},
    {"name": "turns", "kind": "integer", "default": 3, "min": 1, "help": "..."}
  ]
}
```

`params` use the built-in operations' schema (`marcam ops show <type>`): kind, unit, default,
min/max, choices, help, advanced. The app draws a generator's form from it like a built-in
operation's, and plans again when a field changes. A script declares its manifest as a
`MANIFEST` dictionary (Python) or an exported `manifest` object (JS); a plugin exports it
through the ABI.

## The plugin ABI (WebAssembly)

A plugin is one `wasm32-unknown-unknown` module with no imports (or only the SDK's allocator
shims, which are inside the module). Exports:

| Export | Signature | Meaning |
| --- | --- | --- |
| `memory` | memory | its linear memory |
| `marcam_alloc` | `(len: u32) -> u32` | a buffer of `len` bytes for the host to write into |
| `marcam_free` | `(ptr: u32, len: u32)` | free it |
| `marcam_abi_version` | `() -> u32` | 1 |
| `marcam_manifest` | `() -> u64` | the manifest JSON: `ptr << 32 \| len` |
| `marcam_generate` | `(ptr: u32, len: u32) -> u64` | the context JSON in; the result JSON out (`ptr << 32 \| len`) |

The host checks `marcam_abi_version`, runs the module with a fuel/time limit and a memory
cap (default 256 MB), and treats a trap as the operation's error. JSON both ways keeps the
ABI small and the same as the scripts'; a later version may add a binary stock map.

The Rust SDK (`marcam-plugin-sdk`) gives the types (`Context`, `Result`, `Move`,
`Manifest`, `Param`) and a macro:

```rust
marcam_plugin_sdk::plugin! {
    manifest: Manifest { id: "acme.helical-engrave", /* ... */ },
    generate: |cx: &Context| -> Result<Output, String> { /* moves */ },
}
```

## The registry and the project

- Built-in operations, installed plugins and the project's script generators appear in the
  operation picker together; plugins and scripts are marked as such.
- An operation from a plugin is `{"type": "plugin", "plugin": {"id", "version", "sha256"},
  "params": {...}, "tool": n}`; a script one `{"type": "script", "language": "python|js",
  "source": "...", "params": {...}, "tool": n}`. A project names the plugin by id, version
  and hash, so it plans the same way later; `.marcam` bundles carry the module.
- Installing: `marcam plugins install <file.wasm>` (local files only), the app's
  *Tools > Plugins* (a file chooser), and MCP's `install_plugin` (a file inside the open
  folders). Installed plugins live in the user's marcam folder next to the machine profiles.
- MCP tools: `run_generator(script | plugin, params, setup)` (returns the moves' plan, its
  checks and warnings), `generator_context(setup)` (what a generator receives),
  `save_generator` (into the project), `list_plugins`, `install_plugin`, `run_plugin`.

## Built-in operations through the same trait

In Rust, a generator is

```rust
pub trait Generator {
    fn manifest(&self) -> Manifest;
    fn generate(&self, cx: &Context) -> Result<Output, String>;
}
```

The built-in operations can be wrapped in it (their parameters already have this schema),
which keeps the boundary honest: anything a built-in operation gets, a plugin can get. A
proven script or plugin is promoted to a built-in by moving its body into a Rust module,
registering it with the same id and parameters; its tests (context in, moves out, the checks)
carry over unchanged.

## What is not in version 1

Toolpaths with tool changes inside one operation (one tool per operation, as now); 5-axis
poses (the moves are 3-axis or the 4th axis's); plugins that read other files or keep state
between runs.

## Questions for review

1. JSON across the ABI (simple, the same as scripts) or a binary layout for the stock map
   from the start?
2. Should a project be allowed to embed a plugin's module (always plannable) or only
   reference it (smaller projects; installs needed)? Proposed: reference, embedded in
   `.marcam` bundles.
3. Python generators in the browser (Pyodide) later, or JS only there?
