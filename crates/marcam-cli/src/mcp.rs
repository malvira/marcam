//! `marcam mcp`: marcam as a Model Context Protocol server on stdio (JSON-RPC 2.0, one message
//! a line), for agents such as Claude Desktop and Claude Code.
//!
//! Tools: `describe` (operations, machines, tool libraries, materials), `open_project`,
//! `operation_defaults`, `plan` (a project, some of its operations, or one new operation:
//! G-code, warnings, times; files written where asked), `simulate` (material left, gouges,
//! collisions), `feeds`, `pickables`, `replace_model`, `add_custom_toolpath` (G-code written
//! elsewhere, by the client or a script: read, planned and checked like marcam's own, added
//! to a setup). Resources: the example projects and
//! the docs. The engine calls are the ones the web app makes (`marcam_wasm::api`), and
//! planning is `marcam run`'s, so the G-code is the same byte for byte.
//!
//! Files: only inside the client's roots (MCP `roots`) and the folders given with `--allow`;
//! with neither, none (a project may still be passed inline). Nothing is written outside them.
//! See `docs/mcp.md`.

use marcam::post;
use marcam_wasm::api;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

/// MCP revisions this server speaks (newest first).
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// G-code lines a `plan` result carries unless asked for all of it.
const HEAD_LINES: usize = 40;

/// Docs and examples served as resources (`marcam://docs/<name>`, `marcam://examples/<name>`).
const DOCS: [(&str, &str); 7] = [
    ("mcp.md", include_str!("../../../docs/mcp.md")),
    (
        "custom-toolpaths.md",
        include_str!("../../../docs/custom-toolpaths.md"),
    ),
    ("api.md", include_str!("../../../docs/api.md")),
    ("profiles.md", include_str!("../../../docs/profiles.md")),
    ("embedding.md", include_str!("../../../docs/embedding.md")),
    ("roadmap.md", include_str!("../../../docs/roadmap.md")),
    (
        "solidcraft.md",
        include_str!("../../../docs/adapters/solidcraft.md"),
    ),
];
const EXAMPLES: [(&str, &str); 6] = [
    (
        "knob.marcam.toml",
        include_str!("../../../examples/knob/knob.marcam.toml"),
    ),
    (
        "knob_indexed.marcam.toml",
        include_str!("../../../examples/knob/knob_indexed.marcam.toml"),
    ),
    (
        "knob_two_setups.marcam.toml",
        include_str!("../../../examples/knob/knob_two_setups.marcam.toml"),
    ),
    (
        "plate.marcam.toml",
        include_str!("../../../examples/plate/plate.marcam.toml"),
    ),
    (
        "plaque.marcam.toml",
        include_str!("../../../examples/plaque/plaque.marcam.toml"),
    ),
    (
        "ring.marcam.toml",
        include_str!("../../../examples/ring/ring.marcam.toml"),
    ),
];

/// A tool call's failure: shown to the agent as the tool's result (`isError`).
type ToolResult = Result<Value, String>;

struct Server {
    /// Folders given with `--allow` (canonical).
    allow: Vec<PathBuf>,
    /// The client's roots (canonical), when it has sent them.
    roots: Vec<PathBuf>,
    /// The client answers `roots/list`.
    client_roots: bool,
    /// A `roots/list` request is out: its id.
    roots_pending: Option<String>,
    next_id: u64,
    /// Messages read while waiting for the roots, handled next.
    queue: VecDeque<Value>,
}

/// Serve on stdin/stdout until stdin closes. `--allow DIR` (repeatable) adds a folder.
pub fn serve(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut allow = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow" => {
                let d = args.get(i + 1).ok_or("--allow needs a folder")?;
                let c = std::fs::canonicalize(d).map_err(|e| format!("--allow {d}: {e}"))?;
                allow.push(c);
                i += 2;
            }
            a => return Err(format!("marcam mcp: unknown argument {a:?} (--allow DIR)")),
        }
    }
    let mut s = Server {
        allow,
        roots: Vec::new(),
        client_roots: false,
        roots_pending: None,
        next_id: 1,
        queue: VecDeque::new(),
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        let msg = match s.queue.pop_front() {
            Some(m) => m,
            None => match lines.next() {
                Some(Ok(line)) if line.trim().is_empty() => continue,
                Some(Ok(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        send(
                            &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
                        );
                        continue;
                    }
                },
                _ => break,
            },
        };
        let batch = match msg {
            Value::Array(v) => v,
            m => vec![m],
        };
        for m in batch {
            s.handle(m, &mut lines);
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

impl Server {
    fn handle(&mut self, m: Value, lines: &mut impl Iterator<Item = std::io::Result<String>>) {
        let id = m.get("id").cloned();
        let Some(method) = m.get("method").and_then(Value::as_str).map(str::to_string) else {
            // a response: the roots asked for
            self.take_roots(&m);
            return;
        };
        let params = m.get("params").cloned().unwrap_or(json!({}));
        let reply = |r: Result<Value, (i64, String)>| {
            if let Some(id) = &id {
                send(&match r {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, message)) => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                    }
                });
            }
        };
        match method.as_str() {
            "initialize" => {
                self.client_roots = params["capabilities"]["roots"].is_object();
                let asked = params["protocolVersion"].as_str().unwrap_or(PROTOCOLS[0]);
                let version = PROTOCOLS
                    .iter()
                    .find(|v| **v == asked)
                    .copied()
                    .unwrap_or(PROTOCOLS[0]);
                reply(Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "marcam", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "marcam plans 4th-axis rotary and 3-axis CNC toolpaths and posts G-code. Call describe first for the operation types and their fields; open_project reads a .marcam.toml; plan makes the G-code. Files must be inside the roots (or --allow folders)."
                })));
            }
            "notifications/initialized" | "notifications/roots/list_changed" => {
                if self.client_roots {
                    self.ask_roots();
                }
            }
            "ping" => reply(Ok(json!({}))),
            "tools/list" => reply(Ok(json!({"tools": tools()}))),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if self.roots_pending.is_some() {
                    self.wait_roots(lines);
                }
                let out = self.call(&name, &args);
                reply(Ok(match out {
                    Ok(v) => json!({
                        "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
                        "structuredContent": v,
                        "isError": false,
                    }),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                }));
            }
            "resources/list" => reply(Ok(json!({"resources": resources()}))),
            "resources/templates/list" => reply(Ok(json!({"resourceTemplates": []}))),
            "resources/read" => {
                let uri = params["uri"].as_str().unwrap_or("");
                reply(match read_resource(uri) {
                    Some(text) => Ok(
                        json!({"contents": [{"uri": uri, "mimeType": if uri.ends_with(".md") { "text/markdown" } else { "application/toml" }, "text": text}]}),
                    ),
                    None => Err((-32002, format!("no resource {uri:?}"))),
                });
            }
            m if m.starts_with("notifications/") => {}
            _ => reply(Err((-32601, format!("no method {method:?}")))),
        }
    }

    /// Ask the client for its roots.
    fn ask_roots(&mut self) {
        let id = format!("marcam-roots-{}", self.next_id);
        self.next_id += 1;
        send(&json!({"jsonrpc": "2.0", "id": id, "method": "roots/list"}));
        self.roots_pending = Some(id);
    }

    /// The roots, if `m` answers the request for them.
    fn take_roots(&mut self, m: &Value) {
        let id = m.get("id").and_then(Value::as_str);
        if id.is_none() || id != self.roots_pending.as_deref() {
            return;
        }
        self.roots_pending = None;
        self.roots = m["result"]["roots"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["uri"].as_str())
            .filter_map(file_uri_path)
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
    }

    /// Read messages until the roots come (the others are handled after).
    fn wait_roots(&mut self, lines: &mut impl Iterator<Item = std::io::Result<String>>) {
        while self.roots_pending.is_some() {
            let Some(Ok(line)) = lines.next() else {
                self.roots_pending = None;
                return;
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("method").is_none() && v.get("id").is_some() {
                self.take_roots(&v);
            } else {
                self.queue.push_back(v);
            }
        }
    }

    /// The folders files may be in.
    fn bases(&self) -> Vec<&PathBuf> {
        self.roots.iter().chain(&self.allow).collect()
    }

    /// `p` as a path inside the roots or allowed folders (relative: to the first of them).
    /// `write`: it may not exist yet (its nearest existing folder must be inside).
    fn path(&self, p: &str, write: bool) -> Result<PathBuf, String> {
        let bases = self.bases();
        if bases.is_empty() {
            return Err("no folder is open to marcam: the client gave no roots and the server was started without --allow".into());
        }
        let raw = Path::new(p);
        if raw.components().any(|c| c == Component::ParentDir) {
            return Err(format!("{p}: no '..' in paths"));
        }
        let full = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            bases[0].join(raw)
        };
        // the nearest folder that exists, resolved (links and all)
        let mut probe = full.clone();
        let mut rest = Vec::new();
        let real = loop {
            match std::fs::canonicalize(&probe) {
                Ok(c) => break c,
                Err(_) if write => {
                    let Some(name) = probe.file_name().map(|n| n.to_os_string()) else {
                        return Err(format!("{p}: not found"));
                    };
                    rest.push(name);
                    if !probe.pop() {
                        return Err(format!("{p}: not found"));
                    }
                }
                Err(e) => return Err(format!("{p}: {e}")),
            }
        };
        if !bases.iter().any(|b| real.starts_with(b)) {
            return Err(format!(
                "{p} is outside the folders open to marcam ({})",
                bases
                    .iter()
                    .map(|b| b.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut out = real;
        for n in rest.into_iter().rev() {
            out.push(n);
        }
        Ok(out)
    }

    fn call(&mut self, name: &str, a: &Value) -> ToolResult {
        match name {
            "describe" => describe(a),
            "open_project" => {
                let p = self.project(a)?;
                open_project(&p)
            }
            "operation_defaults" => {
                let p = self.project(a)?;
                let ty = a["type"].as_str().ok_or("type: the operation type")?;
                let mut opts = json!({"type": ty});
                for k in ["setup", "tool", "machine"] {
                    if !a[k].is_null() {
                        opts[k] = a[k].clone();
                    }
                }
                engine("op_defaults", &p.json, &opts, &p.mesh)
            }
            "plan" => {
                let p = self.project(a)?;
                let out = match a["out"].as_str() {
                    Some(d) => Some(self.path(d, true)?),
                    None => None,
                };
                plan(&p, a, out.as_deref())
            }
            "simulate" => {
                let p = self.project(a)?;
                simulate(&p, a)
            }
            // (its options are the first argument)
            "feeds" => engine("feeds_calc", a, &json!({}), &[]),
            "pickables" => {
                let p = self.project(a)?;
                let mut opts = json!({});
                for k in ["setup", "model", "index_a"] {
                    if !a[k].is_null() {
                        opts[k] = a[k].clone();
                    }
                }
                engine("pickables", &p.json, &opts, &p.mesh)
            }
            "replace_model" => {
                let p = self.project(a)?;
                let file = a["file"].as_str().ok_or("file: the new model's path")?;
                let fpath = self.path(file, false)?;
                let bytes = std::fs::read(&fpath).map_err(|e| format!("{file}: {e}"))?;
                // the project names the file relative to itself
                let rel = match &p.dir {
                    Some(d) => fpath
                        .strip_prefix(std::fs::canonicalize(d).unwrap_or(d.clone()))
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| fpath.to_string_lossy().replace('\\', "/")),
                    None => file.to_string(),
                };
                let mut opts = json!({"file": rel});
                if !a["model"].is_null() {
                    opts["model"] = a["model"].clone();
                }
                let r = engine("replace_model", &p.json, &opts, &bytes)?;
                if let Some(out) = a["out"].as_str() {
                    let o = self.path(out, true)?;
                    let text = marcam::project_file::convert_to_toml(&r["project"].to_string())?;
                    std::fs::write(&o, text).map_err(|e| format!("{out}: {e}"))?;
                    let mut r = r;
                    r["written"] = json!(o.display().to_string());
                    return Ok(r);
                }
                Ok(r)
            }
            "add_custom_toolpath" => {
                let p = self.project(a)?;
                let gcode =
                    match (a["gcode"].as_str(), a["file"].as_str()) {
                        (Some(g), _) => g.to_string(),
                        (None, Some(f)) => {
                            let fp = self.path(f, false)?;
                            std::fs::read_to_string(&fp).map_err(|e| format!("{f}: {e}"))?
                        }
                        _ => return Err(
                            "gcode (the program's text) or file (a .nc inside the open folders)"
                                .into(),
                        ),
                    };
                let setup = match a["setup"].as_str() {
                    Some(s) => s.to_string(),
                    None => p.json["setups"][0]["id"]
                        .as_str()
                        .ok_or("the project has no setups")?
                        .to_string(),
                };
                let tool = a["tool"].as_u64().map(|t| t as u32);
                let verbatim = a["verbatim"].as_bool().unwrap_or(false);
                let (doc, id, prog) =
                    crate::add_custom(&p.json, &setup, &gcode, tool, verbatim, a["name"].as_str())?;
                let read = json!({
                    "feed_moves": prog.steps.iter().filter(|s| !s.rapid).count(),
                    "tools": prog.tools,
                    "warnings": prog.warnings,
                    "errors": prog.errors,
                });
                if !prog.errors.is_empty() {
                    return Err(format!(
                        "the program can't be planned as it is: {}",
                        prog.errors.join("; ")
                    ));
                }
                // planned as it will be: its own program only
                let with = Project {
                    text: doc.to_string(),
                    json: doc.clone(),
                    dir: p.dir.clone(),
                    mesh: p.mesh.clone(),
                };
                let planned = plan(&with, &json!({"setup": setup, "operations": [id]}), None)?;
                let mut r = json!({"operation": id, "setup": setup, "read": read, "plan": planned});
                match a["out"].as_str() {
                    Some(out) => {
                        let o = self.path(out, true)?;
                        let text = if out.ends_with(".json") {
                            serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n"
                        } else {
                            marcam::project_file::convert_to_toml(&doc.to_string())?
                        };
                        std::fs::write(&o, text).map_err(|e| format!("{out}: {e}"))?;
                        r["written"] = json!(o.display().to_string());
                    }
                    None => r["project"] = doc,
                }
                Ok(r)
            }
            _ => Err(format!("no tool {name:?}")),
        }
    }

    /// The project a call names: `path` (a file inside the open folders) or `project` (the
    /// project itself, TOML text or JSON; `dir` the folder its files are in).
    fn project(&self, a: &Value) -> Result<Project, String> {
        let (text, dir) = match (&a["path"], &a["project"]) {
            (Value::String(p), _) => {
                let full = self.path(p, false)?;
                let s = full.to_string_lossy().to_string();
                (crate::read(&s)?, Some(crate::project_dir(&s)))
            }
            (_, Value::String(t)) => (t.clone(), self.dir_arg(a)?),
            (_, v @ Value::Object(_)) => (v.to_string(), self.dir_arg(a)?),
            _ => return Err("path (a project file) or project (the project itself)".into()),
        };
        let json_text = marcam::project_file::to_json_text(&text)
            .map(|c| c.into_owned())
            .unwrap_or(text.clone());
        let json: Value =
            serde_json::from_str(&json_text).map_err(|e| format!("not a marcam project: {e}"))?;
        // the setup's (or first) mesh model's file, for the engine calls that take one
        let mesh = match &dir {
            Some(d) => mesh_bytes(&json, a["setup"].as_str(), d),
            None => Vec::new(),
        };
        Ok(Project {
            text,
            json,
            dir,
            mesh,
        })
    }

    fn dir_arg(&self, a: &Value) -> Result<Option<PathBuf>, String> {
        match a["dir"].as_str() {
            Some(d) => Ok(Some(self.path(d, false)?)),
            None => Ok(None),
        }
    }
}

struct Project {
    /// As given (TOML or JSON).
    text: String,
    json: Value,
    /// The folder its files are in (None: inline, no files).
    dir: Option<PathBuf>,
    /// Its mesh model's bytes (empty: none).
    mesh: Vec<u8>,
}

/// The bytes of the mesh model a setup uses (or the project's first mesh model).
fn mesh_bytes(project: &Value, setup: Option<&str>, dir: &Path) -> Vec<u8> {
    let models = project["models"].as_array().cloned().unwrap_or_default();
    let wanted: Option<String> = project["setups"].as_array().and_then(|ss| {
        ss.iter()
            .find(|s| setup.is_none_or(|id| s["id"] == id))
            .and_then(|s| s["part"].as_array())
            .and_then(|p| p.first())
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let model = models
        .iter()
        .filter(|m| m["kind"] == "mesh" || m["file"].is_string())
        .find(|m| wanted.as_deref().is_none_or(|w| m["id"] == w))
        .or_else(|| models.iter().find(|m| m["file"].is_string()));
    model
        .and_then(|m| m["file"].as_str())
        .and_then(|f| std::fs::read(dir.join(f)).ok())
        .unwrap_or_default()
}

/// A call into the engine's JSON API (as the web app makes it).
fn engine(name: &str, doc: &Value, opts: &Value, mesh: &[u8]) -> ToolResult {
    let r = api::call(
        name,
        doc.to_string().as_bytes(),
        opts.to_string().as_bytes(),
        mesh,
    );
    match r.get("error") {
        Some(Value::String(e)) => Err(e.clone()),
        Some(e) if !e.is_null() => Err(e.to_string()),
        _ => Ok(r),
    }
}

fn describe(a: &Value) -> ToolResult {
    let topic = a["topic"].as_str().unwrap_or("all");
    let mut out = Map::new();
    if matches!(topic, "all" | "operations") {
        let mut ops = engine("op_types", &json!({}), &json!({}), &[])?;
        if let (Some(only), Some(list)) = (a["operation"].as_str(), ops.as_array_mut()) {
            list.retain(|t| t["kind"] == only);
            if list.is_empty() {
                return Err(format!("no operation type {only:?}"));
            }
        }
        out.insert("operations".into(), ops);
    }
    let reg = crate::profiles_cmd::Registry::load();
    if matches!(topic, "all" | "machines") {
        let machines: Vec<Value> = reg
            .machines
            .iter()
            .map(|(m, _)| json!({"id": m.id, "name": m.name, "vendor": m.vendor, "model": m.model, "spindle": m.spindle, "max_feed": m.max_feed, "tool_changer": m.tool_changer, "cut_direction": m.cut_direction}))
            .collect();
        out.insert("machines".into(), json!(machines));
    }
    if matches!(topic, "all" | "tools") {
        let libs: Vec<Value> = reg
            .libraries()
            .iter()
            .map(|l| {
                json!({"id": l.id, "name": l.name, "tools": l.tools.iter().map(|t| json!({"id": t.id, "name": t.name, "shape": t.shape, "flutes": t.flutes, "flute_length": t.flute_length, "presets": t.presets.len()})).collect::<Vec<_>>()})
            })
            .collect();
        out.insert("tool_libraries".into(), json!(libs));
    }
    if matches!(topic, "all" | "materials") {
        out.insert("materials".into(), json!(marcam::profiles::MATERIALS));
    }
    if out.is_empty() {
        return Err(format!(
            "topic {topic:?}: operations, machines, tools, materials or all"
        ));
    }
    Ok(Value::Object(out))
}

fn open_project(p: &Project) -> ToolResult {
    let r = engine("check_project", &p.json, &json!({}), &[])?;
    let pj = &r["project"];
    let setups: Vec<Value> = pj["setups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            json!({
                "id": s["id"], "name": s["name"], "kind": s["kind"], "machine": s["machine"],
                "material": s["material"], "stock": s["stock"],
                "operations": s["operations"].as_array().into_iter().flatten().map(|o| json!({"id": o["id"], "type": o["type"], "name": o["name"], "tool": o["tool"], "enabled": o["enabled"]})).collect::<Vec<_>>(),
                "outputs": s["outputs"],
            })
        })
        .collect();
    Ok(json!({
        "name": pj["name"],
        "upgraded": r["upgraded"],
        "problems": r["problems"],
        "models": pj["models"],
        "tools": pj["tools"],
        "setups": setups,
        "project": pj,
    }))
}

/// `plan`: the project's setups (or one), only some operations, or one operation added with
/// its defaults; each program's G-code (its head, or all of it with `gcode: "full"`),
/// verifier issues, warnings, time, gouges, collisions and machine limits; with `out`, the
/// `.nc` files written there.
fn plan(p: &Project, a: &Value, out: Option<&Path>) -> ToolResult {
    let mut doc = p.json.clone();
    // a project of 0.1 (a job): planned as it is
    let is_project = doc["setups"].is_array();
    if let Some(op) = a.get("operation").filter(|v| !v.is_null()) {
        if !is_project {
            return Err("operation: needs a project (0.2), not a job".into());
        }
        let ty = op["type"]
            .as_str()
            .ok_or("operation.type: the operation type")?;
        let setup = a["setup"]
            .as_str()
            .map(str::to_string)
            .or_else(|| doc["setups"][0]["id"].as_str().map(str::to_string))
            .ok_or("the project has no setups")?;
        let mut opts = json!({"type": ty, "setup": setup});
        if !op["tool"].is_null() {
            opts["tool"] = op["tool"].clone();
        }
        let mut filled = engine("op_defaults", &doc, &opts, &p.mesh)?;
        for (k, v) in op.as_object().into_iter().flatten() {
            filled[k] = v.clone();
        }
        filled["id"] = json!("mcp_op");
        let s = doc["setups"]
            .as_array_mut()
            .and_then(|ss| ss.iter_mut().find(|s| s["id"] == setup.as_str()))
            .ok_or(format!("no setup {setup:?}"))?;
        let name = a["name"].as_str().unwrap_or(ty).to_string();
        s["operations"]
            .as_array_mut()
            .ok_or("setup without operations")?
            .push(filled);
        s["outputs"] = json!([{"name": name, "title": name, "operations": ["mcp_op"]}]);
    }
    if let Some(only) = a["operations"].as_array() {
        let keep: Vec<&str> = only.iter().filter_map(Value::as_str).collect();
        for s in doc["setups"].as_array_mut().into_iter().flatten() {
            for o in s["operations"].as_array_mut().into_iter().flatten() {
                if !keep.contains(&o["id"].as_str().unwrap_or("")) {
                    o["enabled"] = json!(false);
                }
            }
        }
    }
    let text = if a.get("operation").is_some_and(|v| !v.is_null()) || a["operations"].is_array() {
        doc.to_string()
    } else {
        p.text.clone()
    };
    let reg = crate::profiles_cmd::Registry::load();
    let empty = PathBuf::new();
    let dir = p.dir.as_ref().unwrap_or(&empty);
    let meshes = |src: &marcam::job::MeshSource| match &p.dir {
        Some(d) => src.load(d).ok(),
        None => None,
    };
    let mut jobs = marcam::project::jobs_from_any_with(&text, &reg.libraries(), &meshes)?;
    // setups whose stock the one before left: that stock
    let part = |j: &marcam::Job| marcam::load_mesh(j, dir).ok().flatten();
    marcam::carry::carry_stock(&mut jobs, &part, None)?;
    let full = a["gcode"].as_str() == Some("full");
    let mut setups = Vec::new();
    let mut written = Vec::new();
    for (sid, job) in &jobs {
        if a["setup"].as_str().is_some_and(|w| w != sid) {
            continue;
        }
        let wanted = a["machine"]
            .as_str()
            .map(str::to_string)
            .or_else(|| job.machine.clone())
            .unwrap_or_else(|| "makera-z1-4axis".into());
        let profile = crate::profiles_cmd::resolve_machine(&reg, &wanted)?;
        let mesh = marcam::load_mesh(job, dir).map_err(|e| e.to_string())?;
        let tps = marcam::plan_with_mesh(job, mesh.as_ref()).map_err(|e| e.to_string())?;
        let gouges = marcam::gouge_check(job, &tps, mesh.as_ref()).map_err(|e| e.to_string())?;
        let collisions =
            marcam::collision_check(job, &tps, &profile.post).map_err(|e| e.to_string())?;
        let limits: Vec<Value> = marcam::profiles::check_limits(job, &tps, &profile)
            .into_iter()
            .map(|i| json!({"severity": i.severity, "program": i.program, "message": i.message}))
            .collect();
        let mut programs = Vec::new();
        for tp in &tps {
            let g = post(tp, &profile.post);
            let issues: Vec<String> =
                marcam::verify_gcode(&g, &marcam::limits_for(tp, &profile.post))
                    .iter()
                    .map(|i| i.to_string())
                    .collect();
            let time = marcam::estimate::estimate_toolpath(tp, &profile);
            let warnings: Vec<&str> = tp
                .notes
                .iter()
                .filter(|n| n.starts_with("(Warning"))
                .map(|n| n.trim_start_matches("(Warning: ").trim_end_matches(')'))
                .collect();
            let gouge = gouges
                .iter()
                .find(|x| x.program == tp.name)
                .filter(|x| x.moves_checked > 0)
                .map(|x| x.max_depth);
            let coll = collisions.iter().find(|c| c.program == tp.name);
            let mut file = Value::Null;
            if let Some(o) = out {
                std::fs::create_dir_all(o).map_err(|e| format!("{}: {e}", o.display()))?;
                let f = o.join(format!("{}.nc", tp.name));
                std::fs::write(&f, &g).map_err(|e| format!("{}: {e}", f.display()))?;
                written.push(f.display().to_string());
                file = json!(f.display().to_string());
            }
            let gcode = if full {
                g.clone()
            } else {
                let head: Vec<&str> = g.lines().take(HEAD_LINES).collect();
                head.join("\n")
            };
            programs.push(json!({
                "name": tp.name,
                "title": tp.title,
                "lines": g.lines().count(),
                "issues": issues,
                "warnings": warnings,
                "time_s": time.estimate.total_s,
                "time": marcam::estimate::format_duration(time.estimate.total_s),
                "operations": time.ops.iter().map(|(id, kind, s)| json!({"op": id + 1, "type": kind, "time_s": s})).collect::<Vec<_>>(),
                "gouge_mm": gouge,
                "collisions": coll.map(|c| json!({"count": c.count, "max_depth": c.max_depth})),
                "file": file,
                "gcode": gcode,
                "gcode_complete": full || g.lines().count() <= HEAD_LINES,
            }));
        }
        setups.push(
            json!({"setup": sid, "machine": profile.id, "programs": programs, "limits": limits}),
        );
    }
    if setups.is_empty() {
        return Err(match a["setup"].as_str() {
            Some(w) => format!("no setup {w:?}"),
            None => "the project has no setups".into(),
        });
    }
    Ok(json!({"setups": setups, "written": written}))
}

/// `simulate`: the stock cut by each program (a setup's), compared with the part: material
/// left, gouges, collisions, rapids through the stock.
fn simulate(p: &Project, a: &Value) -> ToolResult {
    let mut opts = json!({"resolution": a["resolution"].as_str().unwrap_or("normal")});
    if let Some(s) = a["setup"].as_str() {
        opts["setup"] = json!(s);
    }
    let mut r = engine("simulate", &p.json, &opts, &p.mesh)?;
    let mut stocks = Vec::new();
    for s in r["stocks"].as_array_mut().into_iter().flatten() {
        let o = s.as_object_mut().map(std::mem::take).unwrap_or_default();
        let mut keep = Map::new();
        for (k, v) in o {
            if !matches!(k.as_str(), "r" | "z" | "colors" | "ops") {
                keep.insert(k, v);
            }
        }
        stocks.push(Value::Object(keep));
    }
    Ok(json!({"kind": r["kind"], "stock_radius": r["stock_radius"], "programs": stocks}))
}

fn tools() -> Value {
    let project_args = json!({
        "path": {"type": "string", "description": "A project file (.marcam.toml, .marcam, .json) inside the open folders"},
        "project": {"description": "Or the project itself: TOML text or a JSON object"},
        "dir": {"type": "string", "description": "With project: the folder its mesh files are in"},
        "setup": {"type": "string", "description": "A setup's id (default: all, or the first)"},
    });
    let with = |extra: Value| {
        let mut p = project_args.as_object().cloned().unwrap_or_default();
        for (k, v) in extra.as_object().into_iter().flatten() {
            p.insert(k.clone(), v.clone());
        }
        json!({"type": "object", "properties": p})
    };
    json!([
        {"name": "describe", "title": "Describe marcam",
         "description": "The operation types with their fields (units, ranges, choices, help), the machines, the tool libraries and the stock materials.",
         "inputSchema": {"type": "object", "properties": {
            "topic": {"type": "string", "enum": ["all", "operations", "machines", "tools", "materials"]},
            "operation": {"type": "string", "description": "Only this operation type"}}},
         "annotations": {"readOnlyHint": true}},
        {"name": "open_project", "title": "Open a project",
         "description": "Read and check a marcam project: its setups, operations, tools, models and any problems (an older file is upgraded in the answer, not on disk).",
         "inputSchema": with(json!({})), "annotations": {"readOnlyHint": true}},
        {"name": "operation_defaults", "title": "An operation's defaults",
         "description": "A new operation of a type with every field filled for the project's setup, tool and part (the values plan would use).",
         "inputSchema": {"type": "object", "required": ["type"], "properties": with(json!({"type": {"type": "string"}, "tool": {"type": "integer"}, "machine": {"type": "string"}}))["properties"]},
         "annotations": {"readOnlyHint": true}},
        {"name": "plan", "title": "Plan and post",
         "description": "Plan a project (or one setup, some operations, or one new operation with its defaults) and post the G-code: per program the lines, the first lines of G-code (all with gcode: full), verifier issues, warnings, machining time, gouge depth, holder collisions and machine limits. With out, the .nc files are written there.",
         "inputSchema": with(json!({
            "operations": {"type": "array", "items": {"type": "string"}, "description": "Only these operation ids"},
            "operation": {"type": "object", "description": "One new operation: {type, ...fields}; the rest from the defaults"},
            "name": {"type": "string", "description": "With operation: the program's name"},
            "machine": {"type": "string", "description": "A machine id (default: the setup's)"},
            "gcode": {"type": "string", "enum": ["head", "full"]},
            "out": {"type": "string", "description": "A folder inside the open folders for the .nc files"}}))},
        {"name": "simulate", "title": "Simulate the stock",
         "description": "Cut the stock with each program and compare it with the part: material left (max, mean), gouges, holder collisions, rapids through the stock.",
         "inputSchema": with(json!({"resolution": {"type": "string", "enum": ["coarse", "normal", "fine", "ultra"]}})),
         "annotations": {"readOnlyHint": true}},
        {"name": "feeds", "title": "Feeds and speeds",
         "description": "Spindle speed, feed, plunge, depth per pass and stepover for a tool in a material: chip load x flutes x rpm, the material's surface speed, radial chip thinning (adaptive), within a machine's limits.",
         "inputSchema": {"type": "object", "required": ["material", "tool"], "properties": {
            "material": {"type": "string"},
            "tool": {"type": "object", "description": "{kind: flat|ball|vbit|drill, diameter, angle?}"},
            "flutes": {"type": "integer"}, "flute_length": {"type": "number"},
            "op": {"type": "string"}, "adaptive": {"type": "boolean"},
            "stepover": {"type": "number", "description": "mm"}, "stepdown": {"type": "number"},
            "rpm": {"type": "number"}, "machine": {"type": "string"}}},
         "annotations": {"readOnlyHint": true}},
        {"name": "pickables", "title": "Features to pick",
         "description": "The features found on a setup's part (holes, pockets, faces, surfaces) that operations can be pointed at, and the operation types each suits.",
         "inputSchema": with(json!({"model": {"type": "string"}, "index_a": {"type": "number"}})),
         "annotations": {"readOnlyHint": true}},
        {"name": "add_custom_toolpath", "title": "Add a custom toolpath",
         "description": "Add G-code written elsewhere (by you, a script, another CAM) to a setup as a custom toolpath: read (what it can't read or plan is listed with its line numbers), planned, checked (gouges, holder collisions, machine limits, feed caps) and estimated like marcam's own operations, then posted through the machine's post (or verbatim). Frames: a vise setup takes X Y Z (the tool tip, the setup's work coordinates, Z0 the top of the stock); a 4th-axis setup takes X along the bar, Z the radius from the axis and A in degrees, with Y 0. Returns the program as read, the plan (G-code head, issues, warnings, time), and the project (written to out if given).",
         "inputSchema": {"type": "object", "properties": with(json!({
            "gcode": {"type": "string", "description": "The program's text"},
            "file": {"type": "string", "description": "Or a .nc file inside the open folders"},
            "tool": {"type": "integer", "description": "The tool number (default: the program's T word when the project has it, else the setup's first)"},
            "verbatim": {"type": "boolean", "description": "Post the program's own lines as given, not re-posted"},
            "name": {"type": "string"},
            "out": {"type": "string", "description": "Write the project here (inside the open folders; .json or TOML)"}}))["properties"]}},
        {"name": "replace_model", "title": "Replace the model",
         "description": "Put a new version of the part (an STL inside the open folders) in the project: each operation's picks are found again on it, or reported gone. With out, the new project is written there (TOML).",
         "inputSchema": {"type": "object", "required": ["file"], "properties": with(json!({"file": {"type": "string"}, "model": {"type": "string"}, "out": {"type": "string"}}))["properties"]}},
    ])
}

fn resources() -> Value {
    let docs = DOCS.iter().map(|(n, _)| json!({"uri": format!("marcam://docs/{n}"), "name": n, "mimeType": "text/markdown"}));
    let ex = EXAMPLES.iter().map(|(n, _)| json!({"uri": format!("marcam://examples/{n}"), "name": n, "description": "Example project (its mesh files are in the repository's examples folder)", "mimeType": "application/toml"}));
    Value::Array(docs.chain(ex).collect())
}

fn read_resource(uri: &str) -> Option<&'static str> {
    if let Some(n) = uri.strip_prefix("marcam://docs/") {
        return DOCS.iter().find(|(d, _)| *d == n).map(|(_, t)| *t);
    }
    if let Some(n) = uri.strip_prefix("marcam://examples/") {
        return EXAMPLES.iter().find(|(d, _)| *d == n).map(|(_, t)| *t);
    }
    None
}

/// The path of a `file://` URI (percent-escapes decoded; `/C:/...` on Windows).
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    let s = String::from_utf8(bytes).ok()?;
    // file:///C:/x -> C:/x
    let s = if s.len() > 2 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s[1..].to_string()
    } else {
        s
    };
    Some(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris() {
        assert_eq!(
            file_uri_path("file:///home/a%20b"),
            Some(PathBuf::from("/home/a b"))
        );
        assert_eq!(
            file_uri_path("file:///C:/cam/x"),
            Some(PathBuf::from("C:/cam/x"))
        );
        assert_eq!(file_uri_path("http://x"), None);
    }
}
