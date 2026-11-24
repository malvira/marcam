//! `marcam` command-line interface.

use marcam::{
    gouge_check, limits_for, load_mesh, plan_with_mesh, post, render, sim, simulate_job,
    verify_gcode, FeedMode, Job, Limits, Machine, Mesh,
};
use std::io::Write;

mod mcp;
mod profiles_cmd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
marcam - 4th-axis rotary CAM engine

USAGE:
  marcam run <job.json> [--machine <id|file|url>] [--out <dir>]
      Plan the job, write one .nc file and one .png preview per program, verify
      each program and check it against the machine's limits. The machine is
      --machine, else the job's \"machine\", else makera-z1-4axis. Built in:
      makera-z1-4axis, makera-carvera-4axis, makera-carvera-air-4axis,
      linuxcnc-4axis (see `marcam profiles list`).
  marcam add-custom <project> <setup-id> <file.nc> [--tool <n>] [--verbatim] [--name <text>]
                    [--out <file>] [--json]
      Add a custom toolpath to a setup: G-code written elsewhere (a script, another CAM,
      an AI), in the setup's frame (a vise: X Y Z, the tool tip; the 4th axis: X along the
      bar, Z the radius, A degrees). Its tool: --tool, else the program's T word when the
      project has that tool, else the setup's first. What the program can't be read or
      planned with is printed with its line numbers; then it is planned (simulated, checked
      and estimated like the others). Writes the project (--out, else in place).
      --verbatim posts the program's own lines instead of re-posting them.
  marcam replace-model <project.json> <model-id> <new.stl|.3mf|.step> [--out <file>] [--json]
      Replace a mesh model with a new part (placement, setups, tools and operations
      kept) and find every operation's picks again on it. Operations whose picked
      features are gone (or match ambiguously) are put in error: kept, left out of
      the G-code until picked again. Writes the project (--out, else in place) and
      prints each operation's outcome; exits 1 when some are in error.
  marcam convert <file> [--out <file.marcam.toml>] [--json] [--bundle <file.marcam>]
      A project or job (TOML, JSON, or a .marcam bundle) as a TOML project (the format
      marcam writes); --json writes JSON instead; --bundle writes a single-file bundle with
      the part files it names.
  marcam upgrade <job.json> [--out <project.marcam.toml>] [--json]
      Convert a 0.1 job to a 0.2 project (models, tools, setups, operations).
      run/simulate/toolpath accept either version.
  marcam faces <job-or-project.json> [--model <id>] [--crease <deg>]
      Face regions of each model (split at sharp edges) with their stable ids,
      for an operation's geometry selection (kind faces, with model and ids).
  marcam pickables <project.json> [--setup <id>] [--model <id>] [--index <deg>]
                  [--type <operation>] [--json]
      The features of a setup's part an operation can be pointed at: holes,
      pockets, flat faces, edge loops, surfaces (vise, indexed at --index) or
      cylindrical and revolved bands (rotary). --type: only those it takes.
  marcam wizard knurl <job-or-project.json> [--params <p.json>] [--out <file>]
      Add turning, a diamond knurl on the body face and a V-bit front chamfer,
      derived from the model's faces (defaults: the Z1 knob).
  marcam ops list | show <type> | check <job-or-project.json>
  marcam ops defaults <type> <job-or-project.json> [--setup id] [--tool n] [--machine id]
      A new operation with every parameter filled from the setup's tools,
      spindle, machine limits and part faces.
      The operation registry: types, parameters (units, defaults, limits),
      accepted tools and geometry; check validates a file's operations.
  marcam profiles list|show|import|export|dir ...
      Machine profiles and tool libraries (`marcam profiles help`).
  marcam estimate <file.nc> [--machine <id|file|url>]
      Estimate the machining time of any G-code file: feeds as the machine's
      controller reads them (degrees-as-length, linear-only, inverse time), rapids,
      acceleration and corner slow-downs, dwells and tool changes. `run` prints the
      same per program, per operation and in total.
  marcam verify <file.nc> [--plunge <mm/min>] [--safe <radius>] [--ramp <deg>]
      Check G-code for over-fast Z descents and low X/A rapids.
      Defaults: --plunge 60, --safe = highest Z reached by a G0 move.
  marcam toolpath <job.json>
      Print the machine-neutral toolpath IR as JSON.
  marcam simulate <job.json> [--out <dir>] [--sim-res coarse|normal|fine|ultra]
                [--dx <mm>] [--na <cells>] [--gpu]
      Simulate material removal of all programs in order on a radial heightmap
      (default fine: 0.05 mm x 720 cells around; coarse 0.2 x 180, normal
      0.1 x 240, ultra 0.025 x 1440), compare with the part, and write a
      <program>_stock.png per program. Fails on rapids through stock or gouges.
  marcam mesh-info <file.stl|file.3mf>
      Print triangle count, bounding box and largest radius about X.
";

/// The `marcam` command: its arguments from the command line. (The `marcam-step` command of
/// crates/marcam-step is this with the STEP reader installed first.)
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

struct Opts {
    positional: Vec<String>,
    flags: Vec<(String, String)>,
}

fn parse(args: &[String], known: &[&str]) -> Result<Opts, String> {
    let mut o = Opts {
        positional: Vec::new(),
        flags: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            // `name?` in `known`: a switch without a value
            if known.contains(&format!("{name}?").as_str()) {
                o.flags.push((name.to_string(), "on".into()));
                continue;
            }
            if !known.contains(&name) {
                return Err(format!("unknown option --{name}\n\n{USAGE}"));
            }
            let v = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
            o.flags.push((name.to_string(), v.clone()));
        } else {
            o.positional.push(a.clone());
        }
    }
    Ok(o)
}

impl Opts {
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.flag(name).is_some()
    }

    fn one_input(&self) -> Result<&str, String> {
        match self.positional.as_slice() {
            [p] => Ok(p),
            _ => Err(format!("expected exactly one input file\n\n{USAGE}")),
        }
    }
}

/// A project or job file's text: TOML or JSON as it is, a bundle's (.marcam) project.toml.
fn read(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if marcam::project_file::is_bundle(&bytes) {
        let (text, _) =
            marcam::project_file::read_bundle(&bytes).map_err(|e| format!("{path}: {e}"))?;
        return Ok(text);
    }
    String::from_utf8(bytes).map_err(|_| format!("{path}: not a text file"))
}

/// The folder a project's files are found in: its own, or, for a bundle (.marcam), a
/// temporary folder its files are taken out to.
fn project_dir(path: &str) -> PathBuf {
    let own = Path::new(path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let Ok(bytes) = std::fs::read(path) else {
        return own;
    };
    if !marcam::project_file::is_bundle(&bytes) {
        return own;
    }
    let Ok((_, files)) = marcam::project_file::read_bundle(&bytes) else {
        return own;
    };
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = std::env::temp_dir().join(format!("marcam-bundle-{:016x}", h.finish()));
    for (name, data) in files {
        // (paths inside the bundle only: no "..")
        if name.split(['/', '\\']).any(|c| c == "..") {
            continue;
        }
        let f = dir.join(&name);
        if let Some(parent) = f.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(f, data);
    }
    dir
}

/// Read a job (0.1) or project (0.2; its setups are planned one after another as one job
/// each), resolving tool references against the built-in and imported libraries.
fn load_job(path: &str) -> Result<Job, String> {
    let reg = profiles_cmd::Registry::load();
    let mut jobs = marcam::project::jobs_from_any(&read(path)?, &reg.libraries())
        .map_err(|e| format!("{path}: {e}"))?;
    if jobs.len() > 1 {
        eprintln!(
            "note: {path} has {} setups; planning the first ({})",
            jobs.len(),
            jobs[0].0
        );
    }
    if jobs.is_empty() {
        return Err(format!("{path}: no setups"));
    }
    Ok(jobs.swap_remove(0).1)
}

fn cmd_wizard_knurl(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let (mut p, _) = marcam::project::Project::from_json_any(&read(path)?)
        .map_err(|e| format!("{path}: {e}"))?;
    let w: marcam::wizards::KnurlWizard = match o.flag("params") {
        Some(f) => serde_json::from_str(&read(f)?).map_err(|e| format!("{f}: {e}"))?,
        None => Default::default(),
    };
    let dir = project_dir(path);
    let models = p.models.clone();
    let meshes = |id: &str| match &models.iter().find(|m| m.id == id)?.kind {
        marcam::project::ModelKind::Mesh(src) => src.load(&dir).ok(),
        _ => None,
    };
    let res = w.run(&mut p, &meshes)?;
    for n in &res.notes {
        eprintln!("note: {n}");
    }
    eprintln!(
        "added operations {} and outputs {}",
        res.operations.join(", "),
        res.outputs.join(", ")
    );
    let text = serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?
        + "
";
    match o.flag("out") {
        Some(out) => std::fs::write(out, text).map_err(|e| format!("{out}: {e}"))?,
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_faces(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let (p, _) = marcam::project::Project::from_json_any(&read(path)?)
        .map_err(|e| format!("{path}: {e}"))?;
    let dir = &*project_dir(path);
    let crease: f64 = o.flag("crease").map_or(Ok(30.0), |v| {
        v.parse().map_err(|_| format!("--crease: {v}"))
    })?;
    for m in &p.models {
        if o.flag("model").is_some_and(|want| want != m.id) {
            continue;
        }
        let mesh = match &m.kind {
            marcam::project::ModelKind::Profile(prof) => Mesh::revolve(prof, 360),
            marcam::project::ModelKind::Mesh(src) => src.load(dir).map_err(|e| e.to_string())?,
        };
        let (regions, _) = marcam::geometry::face_regions(&mesh, crease);
        println!("model {} ({} faces):", m.id, regions.len());
        for r in &regions {
            let a = r.a.map_or("all round".to_string(), |a| {
                format!("A {:.1}..{:.1}", a[0], a[1])
            });
            println!(
                "  {}  {:<9} X {:>8.3}..{:<8.3} R {:.3}..{:.3}  {a}  area {:.1} mm2",
                r.id, r.shape, r.x[0], r.x[1], r.r[0], r.r[1], r.area
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_ops(args: &[String]) -> Result<ExitCode, String> {
    match args.first().map(String::as_str) {
        None | Some("list") => {
            for t in marcam::ops::registry() {
                println!(
                    "{:<16} {:<10} {} (tools: {}; geometry: {})",
                    t.kind,
                    t.group,
                    t.title,
                    t.tool_shapes.join("/"),
                    t.geometry.join("/")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("show") => {
            let kind = args.get(1).ok_or("ops show <type>")?;
            let t = marcam::ops::find(kind).ok_or(format!("unknown operation type {kind:?}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("defaults") => {
            let kind = args.get(1).ok_or(
                "ops defaults <type> <job-or-project.json> [--setup id] [--tool n] [--machine id]",
            )?;
            let o = parse(&args[2..], &["setup", "tool", "machine"])?;
            let path = o.one_input()?;
            let (p, _) = marcam::project::Project::from_json_any(&read(path)?)
                .map_err(|e| format!("{path}: {e}"))?;
            let reg = profiles_cmd::Registry::load();
            let machine = match o.flag("machine") {
                Some(m) => Some(profiles_cmd::resolve_machine(&reg, m)?),
                None => None,
            };
            let tool = o
                .flag("tool")
                .map(|t| t.parse::<u32>().map_err(|_| format!("--tool: {t}")))
                .transpose()?;
            let setup = o
                .flag("setup")
                .map(str::to_string)
                .or_else(|| p.setups.first().map(|s| s.id.clone()))
                .ok_or("no setups")?;
            let dir = project_dir(path);
            let models = p.models.clone();
            let meshes = |id: &str| match &models.iter().find(|m| m.id == id)?.kind {
                marcam::project::ModelKind::Mesh(src) => src.load(&dir).ok(),
                _ => None,
            };
            let v = p.op_defaults(
                kind,
                &setup,
                tool,
                &[],
                &reg.libraries(),
                machine.as_ref(),
                &meshes,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("check") => {
            let path = args.get(1).ok_or("ops check <job-or-project.json>")?;
            let (p, _) = marcam::project::Project::from_json_any(&read(path)?)
                .map_err(|e| format!("{path}: {e}"))?;
            let errs = p.check_operations();
            for e in &errs {
                println!("{e}");
            }
            println!(
                "{path}: {} operation(s) checked, {} problem(s)",
                p.setups.iter().map(|s| s.operations.len()).sum::<usize>(),
                errs.len()
            );
            Ok(if errs.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Some(other) => Err(format!(
            "unknown ops command {other:?} (list, show <type>, check <file>)"
        )),
    }
}

/// Add a custom toolpath (G-code) to `setup` of `project` (a 0.2 project's JSON): the
/// engine's `add_custom` (see marcam_wasm), so the CLI, MCP, Python and the web app add it
/// alike. Returns the project, the new operation's id and the program as read.
pub fn add_custom(
    project: &serde_json::Value,
    setup: &str,
    gcode: &str,
    tool: Option<u32>,
    verbatim: bool,
    name: Option<&str>,
) -> Result<(serde_json::Value, String, marcam::gcode_in::Program), String> {
    let opts = serde_json::json!({"setup": setup, "gcode": gcode, "tool": tool, "verbatim": verbatim, "name": name});
    let r = marcam_wasm::api::call(
        "add_custom",
        project.to_string().as_bytes(),
        opts.to_string().as_bytes(),
        &[],
    );
    if let Some(e) = r.get("error").filter(|e| !e.is_null()) {
        return Err(e.as_str().map(str::to_string).unwrap_or(e.to_string()));
    }
    let rotary = r["frame"] == "rotary";
    let frame = if rotary {
        marcam::gcode_in::Frame::Rotary
    } else {
        marcam::gcode_in::Frame::Three
    };
    Ok((
        r["project"].clone(),
        r["operation"].as_str().unwrap_or("").to_string(),
        marcam::gcode_in::read(gcode, frame),
    ))
}

fn cmd_add_custom(o: &Opts) -> Result<ExitCode, String> {
    let [path, setup, nc] = o.positional.as_slice() else {
        return Err(format!(
            "expected <project> <setup-id> <file.nc>\n\n{USAGE}"
        ));
    };
    let text = read(path)?;
    let json_text = marcam::project_file::to_json_text(&text)
        .map(|c| c.into_owned())
        .unwrap_or(text.clone());
    let (p, _) =
        marcam::project::Project::from_json_any(&json_text).map_err(|e| format!("{path}: {e}"))?;
    let project = serde_json::to_value(&p).map_err(|e| e.to_string())?;
    let gcode = std::fs::read_to_string(nc).map_err(|e| format!("{nc}: {e}"))?;
    let tool = match o.flag("tool") {
        Some(t) => Some(
            t.trim_start_matches(['T', 't'])
                .parse::<u32>()
                .map_err(|_| format!("--tool {t}: a tool number"))?,
        ),
        None => None,
    };
    let (doc, id, prog) = add_custom(
        &project,
        setup,
        &gcode,
        tool,
        o.has("verbatim"),
        o.flag("name"),
    )?;
    // planned as it will be: its errors stop it here
    let dir = project_dir(path);
    let (jobs_err, issues) = match marcam::project::jobs_from_any_with(
        &doc.to_string(),
        &marcam::profiles::builtin_libraries(),
        &|src| src.load(&dir).ok(),
    ) {
        Ok(jobs) => {
            let mut notes = Vec::new();
            let mut err = None;
            for (sid, job) in &jobs {
                if sid != setup {
                    continue;
                }
                match plan_job(job, path) {
                    Ok(tps) => {
                        for tp in &tps {
                            notes.extend(tp.notes.iter().filter(|n| n.contains("line ")).cloned());
                        }
                    }
                    Err(e) => err = Some(e),
                }
            }
            (err, notes)
        }
        Err(e) => (Some(e.to_string()), Vec::new()),
    };
    let target = o.flag("out").unwrap_or(path);
    let out_text = if target.ends_with(".json") {
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n"
    } else {
        marcam::project_file::convert_to_toml(&doc.to_string())?
    };
    if o.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "operation": id, "file": target, "feeds": prog.steps.iter().filter(|s| !s.rapid).count(),
                "tools": prog.tools, "warnings": prog.warnings, "errors": prog.errors, "plan_error": jobs_err,
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "{id}: {} feed moves read from {nc}",
            prog.steps.iter().filter(|s| !s.rapid).count()
        );
        for w in &prog.warnings {
            println!("  warning: {w}");
        }
        for e in &prog.errors {
            println!("  error: {e}");
        }
        let _ = &issues;
        if let Some(e) = &jobs_err {
            println!("  not planned: {e}");
        }
    }
    if jobs_err.is_some() || !prog.errors.is_empty() {
        // (not written: the project as it was)
        eprintln!("{target} not written: the program can't be planned as it is");
        return Ok(ExitCode::from(1));
    }
    std::fs::write(target, out_text).map_err(|e| format!("{target}: {e}"))?;
    if !o.has("json") {
        println!("wrote {target}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_replace_model(o: &Opts) -> Result<ExitCode, String> {
    let [path, model, mesh] = o.positional.as_slice() else {
        return Err(format!(
            "expected <project.json> <model-id> <new mesh file>\n\n{USAGE}"
        ));
    };
    let dir = project_dir(path);
    let (p, _) = marcam::project::Project::from_json_any(&read(path)?)
        .map_err(|e| format!("{path}: {e}"))?;
    let project = serde_json::to_value(&p).map_err(|e| e.to_string())?;
    // the model names its file relative to the project's folder when it is in it
    let mesh_path = Path::new(mesh);
    let file = std::fs::canonicalize(mesh_path)
        .ok()
        .and_then(|m| {
            std::fs::canonicalize(&dir)
                .ok()
                .and_then(|d| m.strip_prefix(d).ok().map(Path::to_path_buf))
        })
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| {
            std::path::absolute(mesh_path)
                .unwrap_or(mesh_path.to_path_buf())
                .to_string_lossy()
                .into_owned()
        });
    let (out, report) = marcam::replace::replace_model(
        &project,
        model,
        &file,
        &|src| src.load(&dir).map_err(|e| e.to_string()),
        &marcam::replace::inline_tool_diameter,
    )?;
    let target = o.flag("out").unwrap_or(path);
    let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())? + "\n";
    std::fs::write(target, text).map_err(|e| format!("{target}: {e}"))?;
    let bad = report.iter().filter(|r| !r.ok).count();
    if o.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"file": file, "operations": report}))
                .map_err(|e| e.to_string())?
        );
    } else {
        println!("model {model} is now {file}; wrote {target}");
        for r in &report {
            match &r.error {
                None => println!("  {}/{}: {} pick(s) found again", r.setup, r.op, r.picks),
                Some(e) => println!("  {}/{}: ERROR {e}", r.setup, r.op),
            }
        }
        println!("{} operation(s) with picks, {bad} in error (left out of the G-code until picked again)", report.len());
    }
    Ok(if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn cmd_convert(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let (p, upgraded) = marcam::project::Project::from_json_any(&read(path)?)
        .map_err(|e| format!("{path}: {e}"))?;
    let v = serde_json::to_value(&p).map_err(|e| e.to_string())?;
    if let Some(out) = o.flag("bundle") {
        // the project and the part files it names, from beside it (or from its bundle)
        let dir = project_dir(path);
        let mut files = Vec::new();
        for m in &p.models {
            if let marcam::project::ModelKind::Mesh(src) = &m.kind {
                let bytes =
                    std::fs::read(dir.join(&src.file)).map_err(|e| format!("{}: {e}", src.file))?;
                files.push((src.file.replace('\\', "/"), bytes));
            }
        }
        let toml = marcam::project_file::project_to_toml(&p)?;
        std::fs::write(out, marcam::project_file::write_bundle(&toml, &files))
            .map_err(|e| format!("{out}: {e}"))?;
        println!("wrote {out} (project.toml and {} file(s))", files.len());
        return Ok(ExitCode::SUCCESS);
    }
    let text = if o.has("json") {
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n"
    } else {
        marcam::project_file::project_to_toml(&p)?
    };
    match o.flag("out") {
        Some(out) => {
            std::fs::write(out, &text).map_err(|e| format!("{out}: {e}"))?;
            println!(
                "wrote {out}{}",
                if upgraded {
                    " (upgraded from a 0.1 job)"
                } else {
                    ""
                }
            );
        }
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_upgrade(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let (p, upgraded) = marcam::project::Project::from_json_any(&read(path)?)
        .map_err(|e| format!("{path}: {e}"))?;
    let name = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .trim_end_matches(".job");
    let mut p = p;
    if upgraded {
        p.name = name.to_string();
    }
    // TOML (what marcam writes), or JSON with --json
    let text = if o.has("json") {
        serde_json::to_string_pretty(&p).map_err(|e| e.to_string())? + "\n"
    } else {
        marcam::project_file::project_to_toml(&p)?
    };
    match o.flag("out") {
        Some(out) => {
            std::fs::write(out, text).map_err(|e| format!("{out}: {e}"))?;
            println!(
                "wrote {out} (schema 0.2{})",
                if upgraded { ", upgraded from 0.1" } else { "" }
            );
        }
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        print!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    match cmd.as_str() {
        "run" => cmd_run(&parse(rest, &["machine", "out"])?),
        "profiles" => {
            if rest.first().is_some_and(|a| a == "help" || a == "--help") {
                print!("{}", profiles_cmd::USAGE);
                return Ok(ExitCode::SUCCESS);
            }
            profiles_cmd::cmd(rest)?;
            Ok(ExitCode::SUCCESS)
        }
        "verify" => cmd_verify(&parse(rest, &["plunge", "safe", "ramp"])?),
        "estimate" => cmd_estimate(&parse(rest, &["machine"])?),
        "pickables" => cmd_pickables(&parse(rest, &["setup", "model", "index", "type", "json?"])?),
        "toolpath" => cmd_toolpath(&parse(rest, &[])?),
        "upgrade" => cmd_upgrade(&parse(rest, &["out", "json?"])?),
        "convert" => cmd_convert(&parse(rest, &["out", "json?", "bundle"])?),
        "replace-model" => cmd_replace_model(&parse(rest, &["out", "json?"])?),
        "add-custom" => cmd_add_custom(&parse(
            rest,
            &["tool", "verbatim?", "name", "out", "json?"],
        )?),
        "ops" => cmd_ops(rest),
        "mcp" => mcp::serve(rest),
        "faces" => cmd_faces(&parse(rest, &["model", "crease"])?),
        "wizard" => {
            let (name, more) = rest
                .split_first()
                .ok_or("wizard knurl <file> [--params p.json] [--out file]")?;
            if name != "knurl" {
                return Err(format!("unknown wizard {name:?} (available: knurl)"));
            }
            cmd_wizard_knurl(&parse(more, &["params", "out"])?)
        }
        "mesh-info" => cmd_mesh_info(&parse(rest, &[])?),
        "simulate" => cmd_simulate(&parse(rest, &["out", "dx", "na", "sim-res", "gpu?"])?),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn cmd_run(o: &Opts) -> Result<ExitCode, String> {
    let job_path = o.one_input()?;
    let reg = profiles_cmd::Registry::load();
    let dir = project_dir(job_path);
    let meshes = |src: &marcam::job::MeshSource| src.load(&dir).ok();
    let mut jobs = marcam::project::jobs_from_any_with(&read(job_path)?, &reg.libraries(), &meshes)
        .map_err(|e| format!("{job_path}: {e}"))?;
    // setups whose stock the one before left: that stock
    let part = |j: &Job| load_mesh(j, &dir).ok().flatten();
    marcam::carry::carry_stock(&mut jobs, &part, None).map_err(|e| format!("{job_path}: {e}"))?;
    let mut bad = 0;
    let mut total = 0.0;
    for (setup, job) in &jobs {
        if jobs.len() > 1 {
            println!("== setup {setup}");
        }
        if job.programs.iter().all(|p| p.operations.is_empty()) {
            println!("warning: setup {setup} has no enabled operation: nothing to plan");
        }
        let (b, t) = run_setup(o, job_path, job, &reg)?;
        bad += b;
        total += t;
        if jobs.len() > 1 {
            println!("setup {setup}: {}", marcam::estimate::format_duration(t));
        }
    }
    println!(
        "estimated machining time: {}",
        marcam::estimate::format_duration(total)
    );
    Ok(if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Plan, post, verify and check one setup's job; returns the number of problems.
fn run_setup(
    o: &Opts,
    job_path: &str,
    job: &Job,
    reg: &profiles_cmd::Registry,
) -> Result<(usize, f64), String> {
    let job = job.clone();
    let mut seconds = 0.0;
    let wanted = o
        .flag("machine")
        .map(str::to_string)
        .or_else(|| job.machine.clone())
        .unwrap_or_else(|| "makera-z1-4axis".into());
    let profile = profiles_cmd::resolve_machine(reg, &wanted)?;
    println!("machine: {} ({})", profile.id, profile.name);
    let machine: Machine = profile.post.clone();
    let out = PathBuf::from(o.flag("out").unwrap_or("."));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let (tps, mesh) = plan_job_with_mesh(&job, job_path)?;
    let gouges = gouge_check(&job, &tps, mesh.as_ref()).map_err(|e| e.to_string())?;
    let collisions = marcam::collision_check(&job, &tps, &machine).map_err(|e| e.to_string())?;
    let mut bad = 0;
    for i in marcam::profiles::check_limits(&job, &tps, &profile) {
        let at = i.program.map(|p| format!("{p}: ")).unwrap_or_default();
        println!("{}: {at}{}", i.severity, i.message);
        if i.severity == "error" {
            bad += 1;
        }
    }
    for tp in &tps {
        let text = post(tp, &machine);
        let path = out.join(format!("{}.nc", tp.name));
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        let part_profile = job.part.profile.as_ref().map(|p| p.points.as_slice());
        let png = out.join(format!("{}.png", tp.name));
        let image = match &job.stock_box {
            Some(sb) if render::is_xyz(tp) => render::preview_png3(tp, sb),
            _ => render::preview_png(tp, job.stock.radius, part_profile),
        };
        std::fs::write(&png, image).map_err(|e| format!("{}: {e}", png.display()))?;
        let issues = verify_gcode(&text, &limits_for(tp, &machine));
        println!(
            "{}: {} lines, {} issue(s)",
            path.display(),
            text.lines().count(),
            issues.len()
        );
        for i in &issues {
            println!("  {i}");
        }
        // what the G-code warns about (tools without a T number, one tool under two)
        for n in tp.notes.iter().filter(|n| n.starts_with("(Warning")) {
            println!(
                "  warning: {}",
                n.trim_start_matches("(Warning: ").trim_end_matches(')')
            );
        }
        bad += issues.len();
        let time = marcam::estimate::estimate_toolpath(tp, &profile);
        let e = &time.estimate;
        let fmt = marcam::estimate::format_duration;
        println!(
            "  time: {} (cutting {}, rapids {}, tool changes/dwells {})",
            fmt(e.total_s),
            fmt(e.feed_s),
            fmt(e.rapid_s),
            fmt(e.other_s)
        );
        for (id, kind, s) in &time.ops {
            println!("    op {} {kind}: {}", id + 1, fmt(*s));
        }
        seconds += e.total_s;
        if let Some(c) = collisions.iter().find(|c| c.program == tp.name) {
            if c.count > 0 {
                println!(
                    "  HOLDER/SHANK COLLISION: {} move(s), deepest {:.3} mm",
                    c.count_text(),
                    c.max_depth
                );
                for k in c.collisions.iter().take(5) {
                    println!(
                        "    line {}: T{} {} {:.3} mm into the stock{} (tip X{:.3} Y{:.3} Z{:.3} A{:.3})",
                        k.line.unwrap_or(0),
                        k.tool,
                        k.contact,
                        k.depth,
                        if k.rapid { " on a rapid" } else { "" },
                        k.tip[0],
                        k.tip[1],
                        k.tip[2],
                        k.a
                    );
                }
                bad += 1;
            } else {
                println!("  holder/shank check: clear");
            }
        }
        if let Some(g) = gouges.iter().find(|g| g.program == tp.name) {
            if g.moves_checked > 0 {
                println!(
                    "  gouge check: {} cut moves, deepest {:.4} mm into the part",
                    g.moves_checked, g.max_depth
                );
                if g.max_depth > GOUGE_TOL {
                    let (x, a) = g.at.unwrap_or_default();
                    println!("  GOUGE at X{x:.3} A{a:.3}");
                    bad += 1;
                }
            }
        }
    }
    Ok((bad, seconds))
}

fn cmd_pickables(o: &Opts) -> Result<ExitCode, String> {
    use marcam::pickables;
    let path = o.one_input()?;
    let (p, _) = marcam::project::Project::from_json_any(&read(path)?)?;
    let dir = project_dir(path);
    let index = match o.flag("index") {
        Some(v) => v
            .parse::<f64>()
            .map_err(|_| format!("--index: not a number: {v}"))?,
        None => 0.0,
    };
    let (setup, part, frame) =
        pickables::setup_part(&p, o.flag("setup"), o.flag("model"), index, &|src| {
            src.load(&dir).map_err(|e| e.to_string())
        })?;
    let mut list = pickables::pickables(&part, frame);
    if let Some(t) = o.flag("type") {
        let ok = pickables::accepts(t);
        if ok.is_empty() {
            return Err(format!(
                "{t}: picking does not apply (pick nothing, set its fields)"
            ));
        }
        list.retain(|x| ok.contains(&x.kind.as_str()));
    }
    if o.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"setup": setup, "pickables": list}))
                .map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!("setup {setup}: {} pickable(s)", list.len());
    for x in &list {
        println!("  {:<40} {}", x.id, x.label);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_estimate(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let text = read(path)?;
    let reg = profiles_cmd::Registry::load();
    let profile =
        profiles_cmd::resolve_machine(&reg, o.flag("machine").unwrap_or("makera-z1-4axis"))?;
    let e = marcam::estimate::estimate_gcode(
        &text,
        &marcam::estimate::Dynamics::from_profile(&profile),
    );
    let fmt = marcam::estimate::format_duration;
    println!("machine: {} ({})", profile.id, profile.name);
    println!(
        "{path}: {} (cutting {} over {:.0} mm, rapids {} over {:.0} mm, tool changes/dwells {})",
        fmt(e.total_s),
        fmt(e.feed_s),
        e.feed_mm,
        fmt(e.rapid_s),
        e.rapid_mm,
        fmt(e.other_s)
    );
    for n in &e.notes {
        println!("  note: {n}");
    }
    Ok(ExitCode::SUCCESS)
}

fn highest_rapid_z(text: &str) -> Option<f64> {
    text.lines()
        .filter(|l| l.trim_start().starts_with("G0 ") || l.trim_start().starts_with("G00 "))
        .flat_map(|l| l.split_whitespace())
        .filter_map(|w| w.strip_prefix('Z').and_then(|v| v.parse::<f64>().ok()))
        .reduce(f64::max)
}

fn cmd_verify(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let text = read(path)?;
    let num = |name: &str| -> Result<Option<f64>, String> {
        o.flag(name)
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| format!("--{name}: not a number: {v}"))
            })
            .transpose()
    };
    let plunge_feed = num("plunge")?.unwrap_or(60.0);
    let safe_r = match num("safe")? {
        Some(s) => s,
        None => highest_rapid_z(&text).ok_or("no G0 Z move found; pass --safe")?,
    };
    let inverse = text.lines().any(|l| {
        let code = l.split('(').next().unwrap_or("");
        code.split_whitespace().any(|w| w == "G93")
    });
    let limits = Limits {
        plunge_feed,
        safe_r,
        feed_mode: if inverse {
            FeedMode::InverseTime
        } else {
            FeedMode::DegreesAsLength
        },
        surface_radius_pad: 0.0,
        // the planner's rule: descents gentler than the ramp angle (default the engine's 30
        // degrees; --ramp 0 holds every descent) may run faster than the plunge limit
        ramp_angle: num("ramp")?.unwrap_or(marcam::job::default_ramp_angle()),
    };
    let issues = verify_gcode(&text, &limits);
    println!(
        "{}: {} issue(s) (plunge limit {plunge_feed} mm/min, safe Z {safe_r}, ramp {} deg)",
        Path::new(path).display(),
        issues.len(),
        limits.ramp_angle
    );
    for i in &issues {
        println!("  {i}");
    }
    Ok(if issues.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Deepest cut into the part that `run` and `simulate` accept (mm).
const GOUGE_TOL: f64 = 0.02;

/// Plan a job, loading its mesh (if any) relative to the job file.
fn plan_job(job: &Job, job_path: &str) -> Result<Vec<marcam::Toolpath>, String> {
    plan_job_with_mesh(job, job_path).map(|(t, _)| t)
}

fn plan_job_with_mesh(
    job: &Job,
    job_path: &str,
) -> Result<(Vec<marcam::Toolpath>, Option<Mesh>), String> {
    let dir = &*project_dir(job_path);
    let mesh = load_mesh(job, dir).map_err(|e| e.to_string())?;
    let tps = plan_with_mesh(job, mesh.as_ref()).map_err(|e| e.to_string())?;
    Ok((tps, mesh))
}

fn cmd_simulate(o: &Opts) -> Result<ExitCode, String> {
    let job_path = o.one_input()?;
    let job = load_job(job_path)?;
    let num = |name: &str, default: f64| -> Result<f64, String> {
        o.flag(name).map_or(Ok(default), |v| {
            v.parse()
                .map_err(|_| format!("--{name}: not a number: {v}"))
        })
    };
    // --sim-res coarse|normal|fine|ultra sets both; --dx / --na override (default: fine)
    let preset = match o.flag("sim-res") {
        Some(name) => sim::Resolution::preset(name).ok_or(format!(
            "--sim-res: {name:?} is not coarse, normal, fine or ultra"
        ))?,
        None => sim::Resolution::preset("fine").expect("built in"),
    };
    let dx = num("dx", preset.dx)?;
    let na = num("na", preset.na as f64)? as usize;
    let out = PathBuf::from(o.flag("out").unwrap_or("."));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let (tps, mesh) = plan_job_with_mesh(&job, job_path)?;
    let res =
        simulate_job(&job, &tps, mesh.as_ref(), dx, na, GOUGE_TOL).map_err(|e| e.to_string())?;
    // replay to render the stock after each program
    let mut stock = sim::RadialMap::of_stock(res.stock.grid, &job.stock);
    let cutters = |t: u32| {
        job.tool(t)
            .and_then(|d| marcam::cutter_for(t, d.shape).ok())
    };
    let mut bad = false;
    // --gpu (built with the `gpu` feature): the stock for the previews on the GPU
    #[cfg(feature = "gpu")]
    let gpu = o.has("gpu").then(marcam_gpu::Gpu::new).flatten();
    #[cfg(not(feature = "gpu"))]
    if o.has("gpu") {
        return Err("this marcam was built without GPU support (cargo feature `gpu`)".into());
    }
    for (((tp, prog), (name, st, dev)), coll) in tps
        .iter()
        .zip(&job.programs)
        .zip(&res.programs)
        .zip(&res.collisions)
    {
        #[cfg(feature = "gpu")]
        {
            let used = marcam_gpu::simulate_or_cpu(
                gpu.as_ref(),
                &mut stock,
                std::slice::from_ref(tp),
                &cutters,
                (dx * 2.0).min(0.1),
            );
            if o.has("gpu") {
                println!("{name}: stock on {used:?}");
            }
        }
        #[cfg(not(feature = "gpu"))]
        sim::simulate(&mut stock, tp, &cutters, (dx * 2.0).min(0.1));
        // knurls and chamfers cut below the turned profile by design
        let shapes_part = prog.operations.iter().any(|o| {
            matches!(
                o,
                marcam::job::Operation::HelicalKnurl(_)
                    | marcam::job::Operation::FlankChamfer(_)
                    | marcam::job::Operation::Parting(_)
            )
        });
        let png = out.join(format!("{name}_stock.png"));
        let title = format!("{name} - simulated stock (grey: part)");
        std::fs::write(
            &png,
            sim::stock_png(&title, &stock, &res.part.center, job.stock.radius),
        )
        .map_err(|e| format!("{}: {e}", png.display()))?;
        println!(
            "{name}: {} tool positions, {} cells cut, {} rapid(s) through stock",
            st.samples,
            st.cells_cut,
            st.rapid_hits.len()
        );
        println!(
            "  material left on the part: max {:.3} mm, mean {:.3} mm; deepest gouge {:.3} mm ({} cells over {GOUGE_TOL} mm)",
            dev.max_left, dev.mean_left, dev.max_gouge, dev.gouge_cells
        );
        if st.samples == 0
            && tp
                .moves
                .iter()
                .any(|m| matches!(m, marcam::Move::Cut { .. }))
        {
            println!(
                "  (not simulated: its tools have no full shape, e.g. a V-bit without a diameter)"
            );
        }
        if shapes_part && dev.gouge_cells > 0 {
            println!(
                "  (knurl/chamfer: cuts below the turned profile on purpose, so these are not counted as gouges)"
            );
        }
        if coll.count > 0 {
            println!(
                "  HOLDER/SHANK COLLISION: {} move(s), deepest {:.3} mm (first at move {})",
                coll.count_text(),
                coll.max_depth,
                coll.collisions[0].move_index
            );
        }
        println!("  preview: {}", png.display());
        bad |= !st.rapid_hits.is_empty() || (dev.gouge_cells > 0 && !shapes_part) || coll.count > 0;
    }
    Ok(if bad {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_mesh_info(o: &Opts) -> Result<ExitCode, String> {
    let path = o.one_input()?;
    let mesh = Mesh::load(Path::new(path)).map_err(|e| e.to_string())?;
    println!("{path}: {} triangles", mesh.triangles.len());
    if let Some((lo, hi)) = mesh.bounds() {
        println!(
            "  bounds X {:.3}..{:.3}  Y {:.3}..{:.3}  Z {:.3}..{:.3}",
            lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
        );
        println!("  largest radius about X: {:.3}", mesh.max_radius());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_toolpath(o: &Opts) -> Result<ExitCode, String> {
    let job_path = o.one_input()?;
    let job = load_job(job_path)?;
    let tps = plan_job(&job, job_path)?;
    let json = serde_json_string(&tps)?;
    // ignore a closed pipe (e.g. `| head`)
    let _ = writeln!(std::io::stdout(), "{json}");
    Ok(ExitCode::SUCCESS)
}

fn serde_json_string(tps: &[marcam::Toolpath]) -> Result<String, String> {
    marcam::toolpath::to_json(tps).map_err(|e| e.to_string())
}
