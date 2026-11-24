# marcam as an MCP server

`marcam mcp` runs marcam as a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdio, so an agent (Claude Desktop, Claude Code, any MCP client) can read marcam
projects, plan and post G-code, simulate the stock and work out feeds and speeds. It is part
of the `marcam` command-line program; nothing else to install.

The engine calls are the ones the web app makes, and planning is `marcam run`'s, so the
G-code an agent gets is byte-identical to what `marcam run` writes (a test checks the knob's
programs against the goldens through the server).

## Set it up

Build or install the CLI (`cargo install --path crates/marcam-cli`, or use
`target/release/marcam` after `cargo build --release -p marcam-cli`). Then give your client
the command and the folder(s) marcam may use.

**Claude Code** (from the folder with your projects):

```sh
claude mcp add marcam -- marcam mcp --allow .
```

or in the project's `.mcp.json`:

```json
{
  "mcpServers": {
    "marcam": { "command": "marcam", "args": ["mcp", "--allow", "."] }
  }
}
```

**Claude Desktop** (`claude_desktop_config.json`: Settings > Developer > Edit Config):

```json
{
  "mcpServers": {
    "marcam": {
      "command": "C:\\tools\\marcam.exe",
      "args": ["mcp", "--allow", "C:\\cam\\projects"]
    }
  }
}
```

On macOS or Linux use the paths there, e.g. `"/usr/local/bin/marcam"` and
`"/Users/me/cam"`.

## Files: only where you allow

marcam reads and writes files only inside:

- the client's **roots** (MCP `roots/list`; Claude Code gives the folder it runs in), and
- the folders given with `--allow DIR` (repeatable).

With neither, it opens no files at all; a project can still be passed inline (`project`).
Paths may be absolute or relative to the first of those folders; `..` is refused, and a path
that resolves (links included) outside them is refused with a message saying so. Written
files (`plan`'s `out`, `replace_model`'s `out`) go only inside them too.

## Tools

| Tool | What it does |
| --- | --- |
| `describe` | The operation types with their fields (unit, range, choices, help text), the machines, the tool libraries (built in and imported) and the stock materials. `topic` narrows it; `operation` gives one type |
| `open_project` | Reads and checks a project (`.marcam.toml`, `.marcam`, or JSON): setups, operations, tools, models, problems. An older file is upgraded in the answer, not on disk |
| `operation_defaults` | A new operation of `type` with every field filled for the project's setup, tool and part: the values `plan` would use |
| `plan` | Plans and posts the project (or one `setup`, only some `operations`, or one new `operation` with its defaults). Per program: line count, the first 40 lines of G-code (all with `gcode: "full"`), verifier issues, warnings, machining time (per operation too), gouge depth, holder collisions, machine limits. With `out`, writes the `.nc` files there |
| `simulate` | Cuts the stock with each program (`resolution`: coarse to ultra) and compares it with the part: material left (max, mean), gouges, holder collisions, rapids through the stock |
| `feeds` | Feeds and speeds for a tool in a material: rpm, feed, plunge, depth per pass, stepover, chip load and thinning, within a machine's limits (see the calculator in the app) |
| `pickables` | The features found on a setup's part (holes, pockets, faces, surfaces) and the operation types each suits |
| `replace_model` | Puts a new version of the part (an STL) in the project: each operation's picks are found again on it or reported gone; with `out`, writes the new project |
| `add_custom_toolpath` | Adds G-code written elsewhere (`gcode`, or a `file` inside the open folders) to a `setup` as a custom toolpath: what it can't read or plan is listed with its line numbers; it is planned and checked like the other operations (the answer has the plan); with `out`, writes the project. See [custom-toolpaths.md](custom-toolpaths.md) for the frames and an example to follow |

The project for a tool is `path` (a file) or `project` (TOML text or a JSON object, with
`dir` for the folder its mesh files are in). Every tool answers with JSON (as text and as
structured content); a refusal (a bad field, a path outside, an operation too fine to plan)
comes back as the tool's error with the reason.

An example exchange, asking for an adaptive pocket on the plate:

```json
{"name": "plan", "arguments": {"path": "plate/plate.marcam.toml",
  "operation": {"type": "pocket", "rect": [5, 5, 25, 35], "depth": 3, "adaptive": true, "stepover": 0.15},
  "name": "pocket", "gcode": "full", "out": "out"}}
```

## Resources

- `marcam://docs/<name>`: this page, `api.md`, `profiles.md`, `embedding.md`, `roadmap.md`,
  `solidcraft.md`;
- `marcam://examples/<name>`: the example projects (knob, indexed knob, two setups, plate,
  plaque, ring). Their mesh and image files are in the repository's `examples` folder; copy
  the folder into an allowed one to plan them.

## Protocol

JSON-RPC 2.0 on stdin/stdout, one message a line; MCP revisions 2025-06-18, 2025-03-26 and
2024-11-05. The server asks for the client's roots after `initialized` (and again on
`roots/list_changed`). Nothing goes to stdout but protocol messages.

## Proof

`cargo test -p marcam-cli --test mcp` runs the server as a client does:

- the handshake, `tools/list`, `ping`, an unknown method, the resources;
- `describe`, `feeds`;
- the knob: `open_project`; `plan` byte-identical to the goldens, inline and from the file,
  and written to `out`; only some operations; one new operation; `operation_defaults`;
  `simulate`;
- the plate: `plan`, `pickables`, `simulate` (3-axis), `replace_model` writing the project;
- custom toolpaths: `add_custom_toolpath` on the plate (its warnings, its plan, the project
  written and planned) and the knob's 4th axis, refused off the axis, and through `plan`;
- files: allowed inside the client's roots, refused outside them (reading and writing), and
  none without roots or `--allow`.
