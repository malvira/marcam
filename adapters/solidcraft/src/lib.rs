//! marcam as a SolidCraft plugin.
//!
//! Entries (SolidCraft caps a manifest at 16 KiB and 32 entries, so the operations are not
//! one entry each):
//! - `plan`: any marcam operation (`operation`, its fields in `params`) on a SolidCraft body:
//!   it meshes the body with SolidCraft's read-only `model.mesh` command, builds a one-setup
//!   marcam project round it (the body on the rotary axis, its front at X0, stock, tool and
//!   operation filled by marcam's defaults), plans and posts it, and writes the G-code, the
//!   project and the body's STL to the folder the user granted;
//! - `describe`: the operations and their fields, from marcam's registry at run time;
//! - `plan-project`: a whole marcam project (`.marcam.toml`) from a folder granted for
//!   reading, every program's G-code written to the output folder;
//! - `rotary-rough`, `rotary-finish`, `helical-knurl`, `flank-chamfer`: `plan` for those
//!   operations with their fields at the top level, as earlier versions had them.
//!
//! This file is the host-independent part (it builds and tests natively); `guest.rs` is the
//! WebAssembly component glue and `bin/pack.rs` makes the installable zip.
//! See `docs/adapters/solidcraft.md`.

use marcam::job::MeshSource;
use marcam::{gouge_check, limits_for, ops, plan_with_mesh, post, profiles, verify_gcode};
use serde_json::{json, Map, Value};

#[cfg(target_arch = "wasm32")]
mod guest;

/// Plugin id: the package folder name in SolidCraft.
pub const PLUGIN_ID: &str = "marcam";
/// The named folder the plugin asks to write into.
pub const GRANT: &str = "output";
/// The named folder `plan-project` reads a project (and its mesh files) from.
pub const PROJECT_GRANT: &str = "project";
/// The entries but the operations' own.
pub const PLAN: &str = "plan";
pub const DESCRIBE: &str = "describe";
pub const PLAN_PROJECT: &str = "plan-project";
/// Operations that keep an entry of their own (their fields at the top level).
pub const ALIASES: [&str; 4] = [
    "rotary_rough",
    "rotary_finish",
    "helical_knurl",
    "flank_chamfer",
];
/// The one SolidCraft command the plugin calls.
pub const MESH_COMMAND: &str = "model.mesh";
pub use marcam::body::{DEFAULT_MACHINE, DEFAULT_RPM, DEFAULT_TOOL_DIAMETER, DEFAULT_VBIT_ANGLE};
/// SolidCraft's `model.mesh` default chordal tolerance (mm).
pub const DEFAULT_TOLERANCE: f64 = 0.05;
pub use marcam::body::{STOCK_GRIP, STOCK_RADIAL_MARGIN};
/// Guest budget asked for in the manifest (fuel units, MiB, ms).
pub const LIMITS: (u64, u32, u32) = (100_000_000_000, 128, 30_000);
/// SolidCraft refuses a plugin file write over this many bytes; a program longer than this
/// is written gzipped (`<name>.nc.gz`).
pub const MAX_WRITE: usize = 1024 * 1024;

fn gzip(data: &[u8]) -> Result<Vec<u8>, Error> {
    use std::io::Write;
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(data)
        .and_then(|_| z.finish())
        .map_err(|e| Error::Engine(format!("gzip: {e}")))
}

/// A failure the guest reports as `invalid-input(field, reason)` or a plain error.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Input { field: String, reason: String },
    Engine(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Input { field, reason } => write!(f, "{field}: {reason}"),
            Error::Engine(s) => f.write_str(s),
        }
    }
}

fn input(field: &str, reason: impl Into<String>) -> Error {
    Error::Input {
        field: field.into(),
        reason: reason.into(),
    }
}

/// The operation types the plugin plans: all but those that only work from a revolved
/// profile (`rotary_turning`), since the plugin gets the part as a mesh, and a custom
/// toolpath (its G-code is its own program, not made from the part).
pub fn offered() -> Vec<ops::OperationType> {
    ops::registry()
        .into_iter()
        .filter(|t| t.geometry != ["profile"] && t.kind != "custom")
        .collect()
}

/// Entry name for an operation type (`rotary_rough` -> `rotary-rough`).
pub fn entry_name(kind: &str) -> String {
    kind.replace('_', "-")
}

/// Operation type of an operation's own entry ([`ALIASES`]), if it is one.
pub fn entry_kind(entry: &str) -> Option<&'static str> {
    ALIASES.into_iter().find(|k| entry_name(k) == entry)
}

/// Operation type for a name as `plan` takes it (`pocket`, or `rotary-rough` spelled as an
/// entry), if the plugin offers it.
pub fn operation_kind(name: &str) -> Option<&'static str> {
    offered()
        .into_iter()
        .map(|t| t.kind)
        .find(|k| *k == name || entry_name(k) == name)
}

/// The operation an entry runs and its inputs with the operation's fields at the top level:
/// an alias's as given; `plan`'s `operation` and `params` (whose fields go to the top level;
/// a field given in both is refused).
pub fn operation_call(entry: &str, params: &Value) -> Result<(&'static str, Value), Error> {
    if let Some(kind) = entry_kind(entry) {
        return Ok((kind, params.clone()));
    }
    if entry != PLAN {
        return Err(input("entry", format!("no marcam entry {entry:?}")));
    }
    let name = params
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| input("operation", "name the operation (see the describe entry)"))?;
    let kind = operation_kind(name).ok_or_else(|| {
        input(
            "operation",
            format!(
                "no marcam operation {name:?}: one of {}",
                offered()
                    .iter()
                    .map(|t| t.kind)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;
    let mut out = params.as_object().cloned().unwrap_or_default();
    out.remove("operation");
    match out.remove("params") {
        None | Some(Value::Null) => {}
        Some(Value::Object(fields)) => {
            for (k, v) in fields {
                if out.contains_key(&k) {
                    return Err(input(&k, "given twice: as an input and in params"));
                }
                out.insert(k, v);
            }
        }
        Some(_) => return Err(input("params", "the operation's fields, as an object")),
    }
    Ok((kind, Value::Object(out)))
}

fn number(min: Option<f64>) -> Value {
    let mut v = json!({"type": "number"});
    if let Some(m) = min {
        v["minimum"] = json!(m);
    }
    v
}

fn array(items: Value, len: Option<usize>) -> Value {
    let mut v = json!({"type": "array", "items": items});
    if let Some(n) = len {
        v["minItems"] = json!(n);
        v["maxItems"] = json!(n);
    }
    v
}

/// JSON schema of one operation field, from its registry kind. No defaults: marcam fills
/// every field from the tool, the machine and the part when it is left out.
fn param_schema(p: &ops::ParamSpec) -> Value {
    let num = json!({"type": "number"});
    let point = array(num.clone(), Some(2));
    match p.kind {
        "number" => number(p.min),
        "integer" => {
            let mut v = json!({"type": "integer"});
            if let Some(m) = p.min {
                v["minimum"] = json!(m);
            }
            v
        }
        "boolean" => json!({"type": "boolean"}),
        "vec2" => point.clone(),
        "rect" => array(num.clone(), Some(4)),
        "numbers" => array(num, None),
        "integers" => array(json!({"type": "integer"}), None),
        "points" => array(point, None),
        "paths" => array(array(point, None), None),
        "choice" => json!({"enum": p.choices}),
        _ => json!({"type": "string"}),
    }
}

/// Tool kinds an entry offers: the operation's, but the tapered ball (the plugin's inputs
/// have no tip or taper; use the marcam project or a tool library for one).
fn tool_kinds(t: &ops::OperationType) -> Vec<&'static str> {
    t.tool_shapes
        .iter()
        .copied()
        .filter(|k| *k != "tapered_ball")
        .collect()
}

/// The inputs every planning entry takes: the body, its placement, machine, tool, stock and
/// name (`tool_kinds` the tool kinds offered, `vbit` with a V-bit's angle).
fn common_inputs(tool_kinds: &[&str], vbit: bool) -> Map<String, Value> {
    let machines: Vec<String> = profiles::builtin_machines()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let mut props = Map::new();
    props.insert(
        "bodies".into(),
        json!({"type": "array", "items": {"type": "string"}, "maxItems": 1}),
    );
    props.insert(
        "tolerance".into(),
        json!({"type": "number", "minimum": 0, "default": DEFAULT_TOLERANCE}),
    );
    props.insert(
        "axis".into(),
        json!({"enum": ["X", "Y", "Z"], "default": "X"}),
    );
    props.insert(
        "machine".into(),
        json!({"enum": machines, "default": DEFAULT_MACHINE}),
    );
    props.insert(
        "tool_kind".into(),
        json!({"enum": tool_kinds, "default": tool_kinds.first()}),
    );
    props.insert(
        "tool_diameter".into(),
        json!({"type": "number", "minimum": 0, "default": DEFAULT_TOOL_DIAMETER}),
    );
    if vbit {
        props.insert(
            "vbit_angle".into(),
            json!({"type": "number", "minimum": 0, "maximum": 180, "default": DEFAULT_VBIT_ANGLE}),
        );
    }
    props.insert(
        "stock_radius".into(),
        json!({"type": "number", "minimum": 0}),
    );
    props.insert(
        "stock_length".into(),
        json!({"type": "number", "minimum": 0}),
    );
    props.insert(
        "spindle_rpm".into(),
        json!({"type": "integer", "minimum": 1, "default": DEFAULT_RPM}),
    );
    props.insert("name".into(), json!({"type": "string"}));
    props
}

/// Input schema of an operation's own entry ([`ALIASES`]): the common inputs, then the
/// operation's fields.
pub fn input_schema(t: &ops::OperationType) -> Value {
    let mut props = common_inputs(&tool_kinds(t), t.tool_shapes.contains(&"vbit"));
    for p in &t.params {
        // the ramp angle and the smoothing stay at marcam's defaults here (SolidCraft caps
        // manifests at 16 KiB; a marcam project sets them per operation)
        if p.name != "tool" && !p.name.is_empty() && !LEFT_AT_DEFAULT.contains(&p.name) {
            props.insert(p.name.into(), param_schema(p));
        }
    }
    json!({"type": "object", "required": ["bodies"], "properties": props})
}

/// Input schema of `plan`: the common inputs, the operation, and its fields as an object
/// (`describe` lists them; marcam names a wrong one).
pub fn plan_schema() -> Value {
    let mut props = common_inputs(&["flat", "ball", "vbit"], true);
    let kinds: Vec<&str> = offered().iter().map(|t| t.kind).collect();
    props.insert("operation".into(), json!({"enum": kinds}));
    props.insert("params".into(), json!({"type": "object"}));
    json!({"type": "object", "required": ["bodies", "operation"], "properties": props})
}

/// The operations the plugin plans and their fields (from marcam's registry), and the
/// inputs `plan` takes besides: `operation` limits it to one.
pub fn describe(params: &Value) -> Result<Value, Error> {
    let only = match params.get("operation") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let name = v.as_str().unwrap_or("");
            Some(
                operation_kind(name)
                    .ok_or_else(|| input("operation", format!("no marcam operation {name:?}")))?,
            )
        }
    };
    let ops: Vec<Value> = offered()
        .iter()
        .filter(|t| only.is_none_or(|k| k == t.kind))
        .map(|t| {
            let fields: Vec<Value> = t
                .params
                .iter()
                .filter(|p| p.name != "tool" && !p.name.is_empty())
                .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
                .collect();
            json!({
                "operation": t.kind,
                "title": t.title,
                "description": t.description,
                "group": t.group,
                "tool_kinds": tool_kinds(t),
                "own_entry": entry_kind(&entry_name(t.kind)).map(|_| entry_name(t.kind)),
                "fields": fields,
            })
        })
        .collect();
    let mut inputs = common_inputs(&["flat", "ball", "vbit"], true);
    inputs.insert(
        "operation".into(),
        json!({"enum": offered().iter().map(|t| t.kind).collect::<Vec<_>>()}),
    );
    inputs.insert(
        "params".into(),
        json!({"type": "object", "description": "the operation's fields"}),
    );
    let machines: Vec<Value> = profiles::builtin_machines()
        .into_iter()
        .map(|m| json!({"id": m.id, "name": m.name}))
        .collect();
    Ok(json!({
        "plugin": PLUGIN_ID,
        "version": env!("CARGO_PKG_VERSION"),
        "inputs": inputs,
        "operations": ops,
        "machines": machines,
    }))
}

/// Operation fields the plugin does not offer: marcam's defaults apply (tabs placed by hand,
/// their size, a contour's direction and leads, a pocket's ramp are set in the app or a
/// project; climb, no leads, plunging by default).
pub const LEFT_AT_DEFAULT: [&str; 11] = [
    "ramp_angle",
    "smoothing",
    "regions",
    "tab_at",
    "tab_width",
    "tab_height",
    "direction",
    "lead_in",
    "lead_out",
    "ramp",
    "rest_tool",
];

/// The package manifest (`plugin.json`), generated from the operation registry.
pub fn manifest() -> Value {
    let mut entries = vec![
        json!({"name": PLAN, "effect": "read", "input_schema": plan_schema()}),
        json!({"name": DESCRIBE, "effect": "read", "input_schema":
            {"type": "object", "properties": {"operation": {"type": "string"}}}}),
        json!({"name": PLAN_PROJECT, "effect": "read", "input_schema":
            {"type": "object", "required": ["project"], "properties": {"project": {"type": "string", "minLength": 1}}}}),
    ];
    for k in ALIASES {
        let t = ops::find(k).expect("an alias is a registry type");
        entries.push(json!({
            "name": entry_name(k),
            "effect": "read",
            "input_schema": input_schema(&t),
        }));
    }
    json!({
        "id": PLUGIN_ID,
        "name": "marcam",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "4th-axis rotary CAM: plan one marcam operation on a body and write its G-code to a granted folder.",
        "capabilities": ["commands@0.1"],
        "runtime": {
            "module": "plugin.wasm",
            "entries": entries,
            "commands": [MESH_COMMAND],
            "limits": {"fuel": LIMITS.0, "memory_mib": LIMITS.1, "deadline_ms": LIMITS.2},
            "permissions": {"files": [
                {"name": GRANT, "read": false, "write": true},
                {"name": PROJECT_GRANT, "read": true, "write": false},
            ]},
        },
    })
}

/// The `model.mesh` call for an entry's parameters: `(command, args)`.
pub fn mesh_request(params: &Value) -> Result<Value, Error> {
    let body = params
        .get("bodies")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 1)
        .and_then(|a| a[0].as_str())
        .ok_or_else(|| input("bodies", "select one body"))?;
    let tolerance = match params.get("tolerance") {
        None | Some(Value::Null) => DEFAULT_TOLERANCE,
        Some(v) => v
            .as_f64()
            .filter(|t| t.is_finite() && *t > 0.0)
            .ok_or_else(|| input("tolerance", "a positive number (mm)"))?,
    };
    Ok(json!({"body": body, "tolerance": tolerance}))
}

/// Binary STL (f32, as any STL) of a `model.mesh` result.
pub fn stl_from_mesh_result(r: &Value) -> Result<Vec<u8>, Error> {
    let bad = |what: &str| Error::Engine(format!("{MESH_COMMAND} returned no valid {what}"));
    let pos = r
        .get("positions")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("positions"))?;
    let tris = r
        .get("triangles")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("triangles"))?;
    let mut verts = Vec::with_capacity(pos.len());
    for p in pos {
        let c: Vec<f64> = p
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if c.len() != 3 || c.iter().any(|x| !x.is_finite()) {
            return Err(bad("positions"));
        }
        verts.push([c[0] as f32, c[1] as f32, c[2] as f32]);
    }
    if tris.is_empty() {
        return Err(Error::Engine("the body has no triangles".into()));
    }
    let mut out = Vec::with_capacity(84 + 50 * tris.len());
    let mut header = [0u8; 80];
    let tag = b"marcam: SolidCraft body (model.mesh, component-local mm)";
    header[..tag.len()].copy_from_slice(tag);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(tris.len() as u32).to_le_bytes());
    for t in tris {
        let idx: Vec<usize> = t
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_u64)
                    .map(|i| i as usize)
                    .collect()
            })
            .unwrap_or_default();
        if idx.len() != 3 || idx.iter().any(|&i| i >= verts.len()) {
            return Err(bad("triangles"));
        }
        let [a, b, c] = [verts[idx[0]], verts[idx[1]], verts[idx[2]]];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let n = if len > 0.0 {
            [n[0] / len, n[1] / len, n[2] / len]
        } else {
            [0.0; 3]
        };
        for f in n.iter().chain(&a).chain(&b).chain(&c) {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    Ok(out)
}

/// One file the entry writes into the granted folder.
#[derive(Debug, Clone, PartialEq)]
pub struct OutFile {
    pub path: String,
    pub data: Vec<u8>,
}

/// What an entry produced: files to write, and the JSON it returns to SolidCraft.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub files: Vec<OutFile>,
    pub result: Value,
}

/// Build the project for one entry (`plan` or an operation's own): the body (an STL in
/// component-local mm) placed on the rotary axis, default stock and tool, and the operation
/// filled by marcam's defaults with the given fields on top
/// ([`marcam::body::body_project`], which every CAD adapter shares). Returns the project JSON
/// and the program name.
pub fn build_project(entry: &str, params: &Value, stl: &[u8]) -> Result<(Value, String), Error> {
    let (kind, params) = operation_call(entry, params)?;
    let params = &params;
    let entry = &entry_name(kind);
    let t = ops::find(kind).ok_or_else(|| input("operation", "unknown operation"))?;
    if params.is_object() {
        // the entry's inputs: no tapered ball here (no tip or taper inputs)
        if let Some(k) = params.get("tool_kind").and_then(Value::as_str) {
            if !tool_kinds(&t).contains(&k) {
                return Err(input(
                    "tool_kind",
                    format!("{kind} takes {}", tool_kinds(&t).join(", ")),
                ));
            }
        }
        for k in params
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| k.as_str())
        {
            if matches!(k, "body" | "tip_diameter" | "taper_angle") {
                return Err(input(k, format!("not an input of {entry}")));
            }
        }
    }
    marcam::body::body_project(kind, params, stl, &["bodies", "tolerance"]).map_err(|e| match e {
        marcam::body::BodyError::Input { field, reason } => Error::Input {
            field,
            reason: reason.replace(
                &format!("not an input of {kind}"),
                &format!("not an input of {entry}"),
            ),
        },
        marcam::body::BodyError::Engine(s) => Error::Engine(s),
    })
}

/// Plan and post a project built by [`build_project`] the way the `marcam run` command does,
/// with the body STL standing in for the project's mesh file.
pub fn plan_and_post(project: &Value, stl: &[u8]) -> Result<(Vec<(String, String)>, Value), Error> {
    plan_and_post_with(&project.to_string(), &|src: &MeshSource| src.mesh(stl).ok())
}

/// Plan and post a project (JSON or TOML text) the way `marcam run` does, its mesh files from
/// `meshes`: each program's name and G-code, and the report.
pub fn plan_and_post_with(
    text: &str,
    meshes: &dyn Fn(&MeshSource) -> Option<marcam::Mesh>,
) -> Result<(Vec<(String, String)>, Value), Error> {
    let libraries = profiles::builtin_libraries();
    let jobs =
        marcam::project::jobs_from_any_with(text, &libraries, meshes).map_err(Error::Engine)?;
    let mut programs = Vec::new();
    let mut report = Vec::new();
    for (_, job) in &jobs {
        let wanted = job
            .machine
            .clone()
            .unwrap_or_else(|| DEFAULT_MACHINE.into());
        let profile = profiles::builtin_machine(&wanted)
            .ok_or_else(|| Error::Engine(format!("no built-in machine {wanted:?}")))?;
        let mesh = match &job.part.mesh {
            Some(m) => Some(meshes(m).ok_or_else(|| {
                Error::Engine(format!("the mesh {:?} could not be read", m.file))
            })?),
            None => None,
        };
        let tps = plan_with_mesh(job, mesh.as_ref()).map_err(|e| Error::Engine(e.to_string()))?;
        let limits: Vec<Value> = profiles::check_limits(job, &tps, &profile)
            .into_iter()
            .map(|i| json!({"severity": i.severity, "program": i.program, "message": i.message}))
            .collect();
        let gouges =
            gouge_check(job, &tps, mesh.as_ref()).map_err(|e| Error::Engine(e.to_string()))?;
        let collisions = marcam::collision_check(job, &tps, &profile.post)
            .map_err(|e| Error::Engine(e.to_string()))?;
        for tp in &tps {
            let gcode = post(tp, &profile.post);
            let issues: Vec<String> = verify_gcode(&gcode, &limits_for(tp, &profile.post))
                .iter()
                .map(|i| i.to_string())
                .collect();
            let time = marcam::estimate::estimate_toolpath(tp, &profile);
            let gouge = gouges.iter().find(|g| g.program == tp.name);
            let coll = collisions.iter().find(|c| c.program == tp.name);
            report.push(json!({
                "program": tp.name,
                "file": format!("{}.nc", tp.name),
                "lines": gcode.lines().count(),
                "time_s": time.estimate.total_s,
                "issues": issues,
                "gouge_mm": gouge.filter(|g| g.moves_checked > 0).map(|g| g.max_depth),
                "collisions": coll.map(|c| json!({"count": c.count, "max_depth": c.max_depth, "first": c.collisions.first()})),
            }));
            programs.push((tp.name.clone(), gcode));
        }
        if !limits.is_empty() {
            report.push(json!({"limits": limits}));
        }
    }
    Ok((programs, json!(report)))
}

/// Everything one entry does after the body is meshed: the files to write and the result.
pub fn run_entry(entry: &str, params: &Value, mesh_result: &Value) -> Result<Outcome, Error> {
    let stl = stl_from_mesh_result(mesh_result)?;
    let (project, name) = build_project(entry, params, &stl)?;
    let (programs, mut report) = plan_and_post(&project, &stl)?;
    let mut files = vec![
        OutFile {
            path: format!("{name}.stl"),
            data: stl,
        },
        // the project as marcam writes it (TOML): `marcam run` reads it back
        OutFile {
            path: format!("{name}.marcam.toml"),
            data: marcam::project_file::convert_to_toml(&project.to_string())
                .map_err(Error::Engine)?
                .into_bytes(),
        },
    ];
    files.extend(program_files(programs)?);
    name_written(&mut report, &files);
    let written: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let result = json!({
        "operation": operation_call(entry, params)?.0,
        "folder": GRANT,
        "files": written,
        "programs": report,
    });
    Ok(Outcome { files, result })
}

/// Each program's G-code as a file (gzipped past SolidCraft's write limit).
fn program_files(programs: Vec<(String, String)>) -> Result<Vec<OutFile>, Error> {
    let mut files = Vec::new();
    for (prog, gcode) in programs {
        files.push(if gcode.len() > MAX_WRITE {
            OutFile {
                path: format!("{prog}.nc.gz"),
                data: gzip(gcode.as_bytes())?,
            }
        } else {
            OutFile {
                path: format!("{prog}.nc"),
                data: gcode.into_bytes(),
            }
        });
    }
    if let Some(f) = files.iter().find(|f| f.data.len() > MAX_WRITE) {
        return Err(Error::Engine(format!(
            "{} is {} bytes, over SolidCraft's {MAX_WRITE}-byte file write limit",
            f.path,
            f.data.len()
        )));
    }
    Ok(files)
}

/// The report names the file actually written (`.nc.gz` for a long program).
fn name_written(report: &mut Value, files: &[OutFile]) {
    for r in report.as_array_mut().into_iter().flatten() {
        if let Some(file) = r.get("file").and_then(Value::as_str).map(str::to_string) {
            if files.iter().any(|f| f.path == format!("{file}.gz")) {
                r["file"] = json!(format!("{file}.gz"));
            }
        }
    }
}

/// `plan-project`: the marcam project at `params.project` (a `.marcam.toml` or `.json`, a
/// path in the project folder), its mesh files beside it, read with `read`; every program's
/// G-code to write.
pub fn plan_project(
    params: &Value,
    read: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
) -> Result<Outcome, Error> {
    let path = params
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| input("project", "the project file's path in the project folder"))?;
    if path.starts_with('/') || path.contains('\\') || path.split('/').any(|c| c == "..") {
        return Err(input(
            "project",
            "a path inside the project folder, with / between folders",
        ));
    }
    let bytes = read(path)?;
    let text = String::from_utf8(bytes).map_err(|_| input("project", "not a text file"))?;
    // mesh files are relative to the project's folder
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let meshes = |src: &MeshSource| {
        let file = if dir.is_empty() {
            src.file.clone()
        } else {
            format!("{dir}/{}", src.file)
        };
        src.mesh(&read(&file).ok()?).ok()
    };
    let (programs, mut report) = plan_and_post_with(&text, &meshes)?;
    if programs.is_empty() {
        return Err(input("project", "no programs to plan"));
    }
    let files = program_files(programs)?;
    name_written(&mut report, &files);
    let written: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let result = json!({
        "project": path,
        "folder": GRANT,
        "files": written,
        "programs": report,
    });
    Ok(Outcome { files, result })
}
