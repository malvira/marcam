//! WebAssembly build of marcam. No wasm-bindgen: the module exports `fc_alloc`, `fc_free` and
//! `fc_call`, and `js/marcam.js` wraps them. Every call takes up to three byte strings (UTF-8
//! JSON or raw mesh bytes) and returns a JSON document.
//!
//! The [`api`] functions are plain Rust so they are tested natively.

pub mod api {
    use marcam::job::{Job, StockBox};
    use marcam::pickables;
    use marcam::profiles::{self, MachineProfile, ToolLibrary};
    use marcam::{
        gouge_check, limits_for, plan_with_mesh, render, sim, sim3, simulate_job, verify_gcode,
        Machine, Mesh, Toolpath,
    };
    use serde_json::{json, Value};

    pub const EXAMPLE_JOB: &str = include_str!("../data/knob.job.json");

    fn err(msg: impl std::fmt::Display) -> Value {
        json!({ "error": msg.to_string() })
    }

    pub fn version() -> Value {
        json!({ "name": "marcam", "version": env!("CARGO_PKG_VERSION"), "schema_version": marcam::job::SCHEMA_VERSION })
    }

    /// Built-in machine by name, or the default (Makera Z1) for an empty name.
    pub fn machine(name: &str) -> Value {
        let name = if name.is_empty() { "makera-z1" } else { name };
        match Machine::builtin(name) {
            Some(m) => serde_json::to_value(m).unwrap_or(Value::Null),
            None => err(format!(
                "unknown machine {name:?} (built in: makera-z1, linuxcnc)"
            )),
        }
    }

    /// Machine from a built-in id/alias, a marcam machine profile, an older marcam machine file,
    /// or a Fusion 360 machine definition (JSON). Empty: the job's `machine`, else the Z1.
    fn parse_machine(text: &str, job: Option<&Job>) -> Result<MachineProfile, String> {
        let t = text.trim();
        let t = if t.is_empty() {
            job.and_then(|j| j.machine.as_deref())
                .unwrap_or("makera-z1-4axis")
        } else {
            t
        };
        if let Some(m) = profiles::builtin_machine(t) {
            return Ok(m);
        }
        if !t.starts_with('{') {
            return Err(format!("unknown machine {t:?}"));
        }
        match profiles::import(t.as_bytes(), "machine.json")?.item {
            profiles::Imported::Machine(m) => Ok(m),
            profiles::Imported::Tools(_) => Err("that is a tool library, not a machine".into()),
        }
    }

    /// The job to plan from a 0.1 job or a 0.2 project (its first setup).
    /// (Also given the mesh bytes, so face selections on mesh models resolve.)
    /// The setup's job (`opts.setup`, see [`SETUP`]) with the stock the setup before left
    /// when it takes that ([`marcam::carry::carry_stock`]: those setups planned and simulated).
    fn job_any_mesh(text: &str, libs: &[ToolLibrary], mesh: &[u8]) -> Result<Job, String> {
        job_of(text, libs, mesh, true)
    }

    /// The setup's job as the project gives it (no carried stock worked out: for drawing).
    fn job_plain(text: &str, libs: &[ToolLibrary], mesh: &[u8]) -> Result<Job, String> {
        job_of(text, libs, mesh, false)
    }

    fn job_of(text: &str, libs: &[ToolLibrary], mesh: &[u8], carry: bool) -> Result<Job, String> {
        let meshes = |src: &marcam::job::MeshSource| {
            (!mesh.is_empty()).then(|| src.mesh(mesh).ok()).flatten()
        };
        let mut jobs = marcam::project::jobs_from_any_with(text, libs, &meshes)?;
        if jobs.is_empty() {
            return Err("the project has no setups".into());
        }
        let k = match SETUP.with(|s| s.borrow().clone()) {
            Some(id) => jobs
                .iter()
                .position(|(sid, _)| *sid == id)
                .ok_or(format!("no setup {id:?}"))?,
            None => 0,
        };
        // a setup whose stock the one before left: that stock (the setups before planned and
        // simulated, the mesh the same file)
        if carry && jobs[k].1.carry.is_some() {
            let part = |j: &Job| {
                j.part
                    .mesh
                    .as_ref()
                    .filter(|_| !mesh.is_empty())
                    .and_then(|src| src.mesh(mesh).ok())
            };
            marcam::carry::carry_stock(&mut jobs, &part, Some(k))?;
        }
        Ok(jobs.swap_remove(k).1)
    }

    thread_local! {
        /// Setup chosen by the current call's `opts.setup` (None = the first).
        static SETUP: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    thread_local! {
        /// Toolpaths planned elsewhere (the page's planning worker) for a project, by the key
        /// the page gives them: [`plan_for_sim`] takes them when a call names that key.
        static SEED: std::cell::RefCell<Option<(String, Vec<Toolpath>)>> = const { std::cell::RefCell::new(None) };
    }

    /// Hand this worker the toolpaths planned elsewhere (`toolpaths`: JSON, as planAndPost's
    /// `toolpaths_json`) under `key`: `{ok, toolpaths}`.
    /// A custom toolpath's program read as the planner reads it (`frame`: "three" or
    /// "rotary"): `{steps, feeds, tools, speeds, warnings, errors}` (each `line N: ...`), for
    /// its form to show its lines with what is wrong with them.
    pub fn read_gcode(text: &str, frame: &str) -> Value {
        let f = if frame == "rotary" {
            marcam::gcode_in::Frame::Rotary
        } else {
            marcam::gcode_in::Frame::Three
        };
        let p = marcam::gcode_in::read(text, f);
        json!({
            "steps": p.steps.len(),
            "feeds": p.steps.iter().filter(|s| !s.rapid).count(),
            "tools": p.tools,
            "speeds": p.speeds,
            "warnings": p.warnings,
            "errors": p.errors,
        })
    }

    /// Add a custom toolpath to a setup: `opts` = {setup, gcode | moves, tool?, verbatim?,
    /// name?}. Its defaults for the setup, the program, its tool (`tool`, else the program's T
    /// word when the project has that tool, else the setup's first operation's, else the
    /// project's first tool), in the setup's last output file (a new one when it has none).
    /// `{project, operation, frame, read: {feed_moves, tools, warnings, errors}}`; an error
    /// when the project or setup can't take it (the program's own errors are in `read`).
    pub fn add_custom(doc: &str, opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let mut d: Value = match serde_json::from_str(doc) {
            Ok(v) => v,
            Err(e) => return err(format!("not a project: {e}")),
        };
        if d["schema_version"] != "0.2" {
            return err("a 0.2 project (an older job is upgraded first)");
        }
        let Some(setups) = d["setups"].as_array() else {
            return err("the project has no setups");
        };
        let setup = o["setup"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                setups
                    .first()
                    .and_then(|s| s["id"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let Some(si) = setups.iter().position(|s| s["id"] == setup.as_str()) else {
            return err(format!(
                "no setup {setup:?} (the project's: {})",
                setups
                    .iter()
                    .filter_map(|s| s["id"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let s = &setups[si];
        let rotary = s["kind"] == "rotary"
            || (s["kind"].is_null()
                && s["stock"]["shape"] != "box"
                && s["holding"]["kind"] == "rotary_chuck");
        let frame = if rotary {
            marcam::gcode_in::Frame::Rotary
        } else {
            marcam::gcode_in::Frame::Three
        };
        let gcode = o["gcode"].as_str().unwrap_or("").to_string();
        let moves = o["moves"].clone();
        let read = if gcode.trim().is_empty() && moves.is_array() {
            match serde_json::from_value::<Vec<marcam::gcode_in::JsonMove>>(moves.clone()) {
                Ok(m) => marcam::gcode_in::from_json(&m, frame),
                Err(e) => return err(format!("moves: {e}")),
            }
        } else {
            marcam::gcode_in::read(&gcode, frame)
        };
        if gcode.trim().is_empty() && !moves.as_array().is_some_and(|m| !m.is_empty()) {
            return err("gcode (the program's text) or moves");
        }
        let has = |n: u32| {
            d["tools"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["number"] == n))
        };
        let tool = o["tool"]
            .as_u64()
            .map(|t| t as u32)
            .or_else(|| read.tools.iter().copied().find(|t| has(*t)))
            .or_else(|| {
                s["operations"]
                    .as_array()
                    .and_then(|o| o.first())
                    .and_then(|o| o["tool"].as_u64())
                    .map(|n| n as u32)
            })
            .or_else(|| {
                d["tools"]
                    .as_array()
                    .and_then(|t| t.first())
                    .and_then(|t| t["number"].as_u64())
                    .map(|n| n as u32)
            });
        let Some(tool) = tool else {
            return err("no tool: the project has none (add one, or give its number)");
        };
        if !has(tool) {
            return err(format!("the project has no tool T{tool}"));
        }
        let mut op = op_defaults(
            &d.to_string(),
            &json!({"type": "custom", "setup": setup, "tool": tool}).to_string(),
            &[],
        );
        if op.get("error").is_some_and(|e| !e.is_null()) {
            return op;
        }
        let ids: Vec<String> = setups
            .iter()
            .flat_map(|s| s["operations"].as_array().into_iter().flatten())
            .filter_map(|o| o["id"].as_str().map(str::to_string))
            .collect();
        let id = (1..)
            .map(|k| format!("op{k}"))
            .find(|i| !ids.contains(i))
            .expect("an id");
        op["id"] = json!(id);
        op["type"] = json!("custom");
        op["tool"] = json!(tool);
        op["gcode"] = json!(gcode);
        if gcode.trim().is_empty() {
            op["moves"] = moves;
        }
        op["verbatim"] = json!(o["verbatim"].as_bool().unwrap_or(false));
        if let Some(n) = o["name"].as_str() {
            op["name"] = json!(n);
        }
        let s = &mut d["setups"][si];
        if let Some(ops) = s["operations"].as_array_mut() {
            ops.push(op);
        } else {
            s["operations"] = json!([op]);
        }
        let title = o["name"].as_str().unwrap_or("Custom toolpath").to_string();
        match s["outputs"].as_array_mut() {
            Some(outs) if !outs.is_empty() => {
                if let Some(list) = outs.last_mut().and_then(|o| o["operations"].as_array_mut()) {
                    list.push(json!(id));
                }
            }
            _ => {
                s["outputs"] =
                    json!([{"name": format!("{setup}_{id}"), "title": title, "operations": [id]}])
            }
        }
        json!({
            "project": d,
            "operation": id,
            "frame": if rotary { "rotary" } else { "three" },
            "read": {
                "feed_moves": read.steps.iter().filter(|s| !s.rapid).count(),
                "tools": read.tools,
                "warnings": read.warnings,
                "errors": read.errors,
            },
        })
    }

    pub fn seed_plan(key: &str, toolpaths: &str) -> Value {
        match serde_json::from_str::<Vec<Toolpath>>(toolpaths) {
            Ok(t) => {
                let n = t.len();
                SEED.with(|s| *s.borrow_mut() = Some((key.to_string(), t)));
                json!({ "ok": true, "toolpaths": n })
            }
            Err(e) => err(format!("seed_plan: {e}")),
        }
    }

    thread_local! {
        /// Operations planned before (see marcam::PlanCache).
        static PLAN_CACHE: std::cell::RefCell<marcam::PlanCache> = std::cell::RefCell::new(marcam::PlanCache::new());
    }

    /// The project's operations (ids, colours) the simulation plays: those not hidden
    /// (`opts.hide`, planning numbers), in order, so they line up with its operation list.
    fn visible_ops(ids: Vec<String>, colors: Vec<Option<String>>, opts: &Value) -> Value {
        let hide: Vec<usize> = opts["hide"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_u64().map(|x| x as usize))
                    .collect()
            })
            .unwrap_or_default();
        let keep = |k: &usize| !hide.contains(k);
        json!({
            "ids": ids.into_iter().enumerate().filter(|(k, _)| keep(k)).map(|(_, v)| v).collect::<Vec<_>>(),
            "colors": colors.into_iter().enumerate().filter(|(k, _)| keep(k)).map(|(_, v)| v).collect::<Vec<_>>(),
        })
    }

    /// The job planned for a simulation (through the plan cache), without the operations
    /// `opts.hide` lists (planning numbers): hidden in the view, so not simulated either.
    fn plan_for_sim(
        job: &Job,
        mesh: Option<&Mesh>,
        opts: &Value,
    ) -> Result<Vec<Toolpath>, marcam::Error> {
        // (the toolpaths the page's planner made, handed over for this very project: not
        // planned again here, the impeller's took 12 s more before its playback could open)
        let seeded = opts["plan_key"].as_str().and_then(|k| {
            SEED.with(|s| {
                s.borrow()
                    .as_ref()
                    .filter(|(key, _)| key == k)
                    .map(|(_, t)| t.clone())
            })
        });
        let mut tps = match seeded {
            Some(t) => t,
            None => PLAN_CACHE.with(|c| {
                let mut c = c.borrow_mut();
                let r = marcam::plan_with_mesh_cached(job, mesh, &mut c);
                c.take_counts();
                r
            })?,
        };
        let hide: Vec<u32> = opts["hide"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_u64().map(|x| x as u32))
                    .collect()
            })
            .unwrap_or_default();
        marcam::sim::without_ops(&mut tps, &hide);
        Ok(tps)
    }

    fn with_setup<T>(opts: &Value, f: impl FnOnce() -> T) -> T {
        let id = opts["setup"].as_str().map(str::to_string);
        SETUP.with(|s| *s.borrow_mut() = id);
        let out = f();
        SETUP.with(|s| *s.borrow_mut() = None);
        out
    }

    /// For a 0.2 project: (project operation ids in planning order, their colours).
    fn op_ids_and_colors(doc: &str) -> Option<(Vec<String>, Vec<Option<String>>)> {
        let v: Value = serde_json::from_str(doc).ok()?;
        if v["schema_version"] != "0.2" {
            return None;
        }
        let (p, _) = marcam::project::Project::from_json_any(doc).ok()?;
        let want = SETUP.with(|s| s.borrow().clone());
        let setup = match want {
            Some(id) => p.setup(&id)?,
            None => p.setups.first()?,
        };
        let ids = p.planned_op_ids(setup);
        let colors = ids
            .iter()
            .map(|id| {
                setup
                    .operations
                    .iter()
                    .find(|o| &o.id == id)
                    .and_then(|o| o.color.clone())
            })
            .collect();
        Some((ids, colors))
    }

    /// Built-in libraries plus `opts.libraries` (marcam tool library objects).
    fn libraries(opts: &Value) -> Result<Vec<ToolLibrary>, String> {
        let mut libs = profiles::builtin_libraries();
        for l in opts["libraries"].as_array().into_iter().flatten() {
            let lib = ToolLibrary::from_json(&l.to_string())?;
            libs.retain(|x| x.id != lib.id);
            libs.push(lib);
        }
        Ok(libs)
    }

    /// Built-in machine profiles and tool libraries.
    pub fn profile_list() -> Value {
        json!({
            "machines": profiles::builtin_machines(),
            "libraries": profiles::builtin_libraries(),
            "materials": profiles::MATERIALS,
        })
    }

    /// Convert a profile file (marcam JSON, Fusion .tools/.json/.mch/.f3d, older machine file)
    /// into marcam JSON: `{kind: "machine"|"tools", profile, notes}`.
    pub fn import_profile(bytes: &[u8], name: &str) -> Value {
        match profiles::import(bytes, name) {
            Ok(imp) => match imp.item {
                profiles::Imported::Machine(m) => {
                    json!({"kind": "machine", "profile": m, "notes": imp.notes})
                }
                profiles::Imported::Tools(l) => {
                    json!({"kind": "tools", "profile": l, "notes": imp.notes})
                }
            },
            Err(e) => err(e),
        }
    }

    /// Mesh bytes (STL or 3MF), placed on the axis as the job's `part.mesh` says.
    fn parse_mesh(job: &Job, bytes: &[u8]) -> Result<Option<Mesh>, String> {
        if bytes.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            match &job.part.mesh {
                Some(src) => src.mesh(bytes),
                None => Mesh::from_bytes(bytes),
            }
            .map_err(|e| e.to_string())?,
        ))
    }

    /// Tool-tip polyline, flattened `[x, r, a, kind, ...]` (kind 0 rapid, 1 feed), thinned
    /// to about `max_points` points (rapids and direction changes between rapid and feed are
    /// always kept). 3-axis programs give `[x, y, z, kind, ...]` with kind 2 rapid, 3 feed.
    /// The preview polyline (thinned to about `max_points`), and where each operation starts
    /// in it: (operation index, first point).
    fn polyline(
        tp: &Toolpath,
        max_points: usize,
        xyz: bool,
    ) -> (Vec<f32>, usize, bool, Vec<(u32, usize)>) {
        let (pts, marks) = if xyz {
            render::polyline3_marked(tp)
        } else {
            render::polyline_marked(tp)
        };
        let mut starts = Vec::new();
        let mut next_mark = 0;
        let kind0 = if xyz { 2u8 } else { 0 };
        let total = pts.len();
        let stride = if max_points > 0 && total > max_points {
            total.div_ceil(max_points)
        } else {
            1
        };
        let mut out = Vec::with_capacity(4 * (total / stride + 16));
        for (i, p) in pts.iter().enumerate() {
            while next_mark < marks.len() && marks[next_mark].1 <= i {
                starts.push((marks[next_mark].0, out.len() / 4));
                next_mark += 1;
            }
            let keep = stride == 1
                || i % stride == 0
                || i + 1 == total
                || !p.3
                || pts.get(i + 1).is_some_and(|q| !q.3);
            if keep {
                out.extend_from_slice(&[
                    p.0 as f32,
                    p.1 as f32,
                    p.2 as f32,
                    f32::from(kind0 + u8::from(p.3)),
                ]);
            }
        }
        for m in &marks[next_mark..] {
            starts.push((m.0, out.len() / 4));
        }
        (out, total, stride > 1, starts)
    }

    /// Plan, post, verify and gouge-check a job. `opts`: `{"max_points": n}` (default 200000).
    pub fn plan_and_post(job_json: &str, machine_text: &str, mesh: &[u8], opts: &str) -> Value {
        let opts_v: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts_v) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts_v, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let profile = match parse_machine(machine_text, Some(&job)) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let machine = profile.post.clone();
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        if job.part.mesh.is_some() && mesh.is_none() {
            return err(format!(
                "this job uses the mesh file {:?}: load it too",
                job.part
                    .mesh
                    .as_ref()
                    .map(|m| m.file.as_str())
                    .unwrap_or("")
            ));
        }
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let max_points = opts["max_points"].as_u64().unwrap_or(200_000) as usize;
        // operations planned before and unchanged are reused (the page's edits plan only what
        // they touch)
        // (`cache: false`: planned from scratch, the cache left alone: tests compare with it)
        let planned_now = if opts["cache"] == false {
            plan_with_mesh(&job, mesh.as_ref())
        } else {
            PLAN_CACHE
                .with(|c| marcam::plan_with_mesh_cached(&job, mesh.as_ref(), &mut c.borrow_mut()))
        };
        let tps = match planned_now {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        let (reused, planned) = if opts["cache"] == false {
            (
                0,
                tps.iter()
                    .map(|t| {
                        t.moves
                            .iter()
                            .filter(|m| matches!(m, marcam::Move::Operation { .. }))
                            .count()
                    })
                    .sum(),
            )
        } else {
            PLAN_CACHE.with(|c| c.borrow_mut().take_counts())
        };
        let gouges = gouge_check(&job, &tps, mesh.as_ref()).unwrap_or_default();
        let collisions = marcam::collision_check(&job, &tps, &machine).unwrap_or_default();
        marcam::phase("machine limits");
        let limits = profiles::check_limits(&job, &tps, &profile);
        let jaws = marcam::jaw_check(&job, &tps);
        // project operation ids by planning order (0.2 documents), for per-operation times
        let op_ids = with_setup(&opts_v, || op_ids_and_colors(job_json)).map(|(ids, _)| ids);
        let mut total_s = 0.0;
        let programs: Vec<Value> = tps
            .iter()
            .map(|tp| {
                marcam::phase(&format!("post {}", tp.name));
                // (and each move's line: the results' entries go to their place in the
                // playback, a line to its move)
                let (gcode, _, move_lines) = marcam::post::post_lines(tp, &machine);
                let time = marcam::estimate::estimate_toolpath(tp, &profile);
                total_s += time.estimate.total_s;
                let ops: Vec<Value> = time
                    .ops
                    .iter()
                    .map(|(id, kind, s)| {
                        let pid = op_ids.as_ref().and_then(|v| v.get(*id as usize)).cloned();
                        json!({"index": id, "id": pid, "kind": kind, "seconds": s})
                    })
                    .collect();
                let issues: Vec<String> = verify_gcode(&gcode, &limits_for(tp, &machine))
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let g = gouges.iter().find(|g| g.program == tp.name);
                let c = collisions.iter().find(|c| c.program == tp.name);
                let (points, total, thinned, starts) = polyline(tp, max_points, job.stock_box.is_some());
                // each operation's first point in `points` (by planning index and project id)
                let op_starts: Vec<Value> = starts
                    .iter()
                    .map(|(i, k)| json!({"index": i, "id": op_ids.as_ref().and_then(|v| v.get(*i as usize)).cloned(), "start": k}))
                    .collect();
                json!({
                    "name": tp.name,
                    "title": tp.title,
                    "tool": tp.tool,
                    "lines": gcode.lines().count(),
                    // what its G-code warns about (tools without a number, one tool twice)
                    // (and cuts that reach the vise's jaws)
                    "warnings": tp.notes.iter().filter(|n| n.starts_with("(Warning")).map(|n| n.trim_start_matches("(Warning: ").trim_end_matches(')').to_string())
                        // (an operation's number in the plan: its id in the project)
                        .map(|w| match (w.find("[op#"), op_ids.as_ref()) {
                            (Some(i), Some(ids)) => {
                                let rest = &w[i + 4..];
                                let end = rest.find(']').unwrap_or(0);
                                match rest[..end].parse::<usize>().ok().and_then(|k| ids.get(k)) {
                                    Some(id) => format!("{}{id}{}", &w[..i], &rest[end + 1..]),
                                    None => w,
                                }
                            }
                            _ => w,
                        })
                        .chain(jaws.iter().filter(|(p, _)| p == &tp.name).map(|(_, w)| w.clone())).collect::<Vec<_>>(),
                    "gcode": gcode,
                    "move_lines": f32s(&move_lines.iter().map(|&l| l as f32).collect::<Vec<f32>>()),
                    "issues": issues,
                    "gouge": g.map(|g| json!({"moves_checked": g.moves_checked, "max_depth": g.max_depth, "at": g.at})),
                    "collisions": c,
                    "safe_r": tp.safe_r,
                    "time": {
                        "total_s": time.estimate.total_s,
                        "feed_s": time.estimate.feed_s,
                        "rapid_s": time.estimate.rapid_s,
                        "other_s": time.estimate.other_s,
                        "feed_mm": time.estimate.feed_mm,
                        "rapid_mm": time.estimate.rapid_mm,
                        "ops": ops,
                    },
                    "toolpath": { "points": points, "total": total, "thinned": thinned, "ops": op_starts },
                })
            })
            .collect();
        json!({
            "machine": machine.name,
            "machine_id": profile.id,
            "limits": limits,
            "tools": job.tools,
            "stock": job.stock,
            "stock_box": job.stock_box,
            "jaws": job.jaws,
            "profile": job.part.profile,
            "mesh_bounds": mesh.as_ref().and_then(|m| m.bounds()),
            "programs": programs,
            "time_total_s": total_s,
            "plan_cache": { "reused": reused, "planned": planned },
            // (`export_toolpaths`: the toolpaths too, for the page to hand to its simulating
            // worker instead of it planning them again)
            "toolpaths_json": (opts["export_toolpaths"] == true).then(|| serde_json::to_string(&tps).ok()).flatten(),
        })
    }

    /// Machining time of any G-code text for `machine` (id/alias or a profile object; default
    /// Makera Z1): {total_s, feed_s, rapid_s, other_s, feed_mm, rapid_mm, notes}.
    pub fn estimate_gcode(text: &str, machine_text: &str) -> Value {
        let profile = match parse_machine(machine_text, None) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let e = marcam::estimate::estimate_gcode(
            text,
            &marcam::estimate::Dynamics::from_profile(&profile),
        );
        json!({
            "machine": profile.id,
            "total_s": e.total_s,
            "feed_s": e.feed_s,
            "rapid_s": e.rapid_s,
            "other_s": e.other_s,
            "feed_mm": e.feed_mm,
            "rapid_mm": e.rapid_mm,
            "notes": e.notes,
        })
    }

    /// Simulate all programs in order. `opts`: `{"dx": mm, "na": cells}` (default 0.1, 360).
    /// Returns the grid, the part map and the stock map after each program (radii, f32).
    pub fn simulate(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        // (the person's libraries too: a project with one of their tools)
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        // (the setup asked for: `opts.setup`)
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let (dx, na) = rotary_res(&opts);
        let tps = match plan_for_sim(&job, mesh.as_ref(), &opts) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        if let Some(why) = too_slow(&job, &tps, mesh.as_ref(), &opts, BATCH_FACTOR) {
            return json!({"error": why, "too_slow": true});
        }
        if let Some(sb) = job.stock_box {
            return simulate_xyz(&job, &tps, mesh.as_ref(), &sb, &opts);
        }
        let res = match simulate_job(&job, &tps, mesh.as_ref(), dx, na, 0.02) {
            Ok(r) => r,
            Err(e) => return err(e),
        };
        // the stock after each program, as the simulation left it (not simulated again: the
        // impeller's 126k moves took twice as long)
        let mut stocks = Vec::new();
        for (((name, st, dev), coll), r) in res.programs.iter().zip(&res.collisions).zip(&res.after)
        {
            stocks.push(json!({
                "name": name,
                "collisions": coll,
                "samples": st.samples,
                "cells_cut": st.cells_cut,
                "rapid_hits": st.rapid_hits.len(),
                "rapid_grazes": st.rapid_grazes.len(),
                "heavy": st.heavy,
                // (which moves: the results' entries go to them in the playback)
                "rapid_moves": st.rapid_hits.iter().take(200).collect::<Vec<_>>(),
                "graze_moves": st.rapid_grazes.iter().take(200).collect::<Vec<_>>(),
                "max_left": dev.max_left,
                "mean_left": dev.mean_left,
                "max_gouge": dev.max_gouge,
                "gouge_cells": dev.gouge_cells,
                "feature_cells": dev.feature_cells,
                "r": r,
            }));
        }
        let g = res.stock.grid;
        json!({
            "grid": { "x0": g.x0, "dx": g.dx, "nx": g.nx, "na": g.na },
            "stock_radius": job.stock.radius,
            "part": res.part.center.r,
            "stocks": stocks,
        })
    }

    /// A setup's stock and part as its job has them (`opts.setup`): a setup whose stock the
    /// one before left has them turned and moved from that one's. For drawing; nothing planned.
    pub fn setup_frame(job_json: &str, opts: &str) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = libraries(&opts).unwrap_or_default();
        match with_setup(&opts, || job_plain(job_json, &libs, &[])) {
            Ok(job) => json!({
                "stock": job.stock,
                "stock_box": job.stock_box,
                "stock_axis": job.stock_axis,
                "profile": job.part.profile,
                "mesh": job.part.mesh,
                "carry": job.carry,
            }),
            Err(e) => err(e),
        }
    }

    /// The stock a setup starts with when it takes what a setup before left (`opts.setup`):
    /// that cut result, turned and moved into this setup, as triangles (flattened xyz, f32)
    /// for drawing, and (`opts.stl`) as a binary STL (base64).
    pub fn carried_mesh(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let Some(c) = job.carried.as_deref() else {
            return err("this setup's stock is not what a setup before left");
        };
        // (Mar: "the carried over cut stock is still really bad") the solid it was sampled
        // from, when it went on as a mesh: drawn as it is (the samples are for simulating)
        let solid = marcam::carry::solid_of(c);
        let sampled_volume = marcam::carry::sampled_volume(c);
        let m = if let Some(s) = &solid {
            (**s).clone()
        } else {
            match c {
                marcam::carry::Carried::Block {
                    x0,
                    y0,
                    cell,
                    nx,
                    ny,
                    top,
                    bot,
                } => marcam::carry_mesh::block_mesh(*x0, *y0, *cell, *nx, *ny, top, bot),
                marcam::carry::Carried::Bar(map) => marcam::carry_mesh::bar_mesh(map),
            }
        };
        let tri: Vec<f32> = m
            .triangles
            .iter()
            .flat_map(|t| t.iter().flat_map(|p| p.iter().map(|&c| c as f32)))
            .collect();
        let mut out = json!({ "triangles": tri, "total": m.triangles.len(), "bounds": m.bounds(),
            "solid": solid.is_some(),
            "volume": {"drawn": marcam::carry::mesh_volume(&m), "simulated": sampled_volume} });
        if opts["stl"].as_bool() == Some(true) {
            out["stl"] = json!(b64(&marcam::carry_mesh::to_stl(
                &m,
                "marcam: the stock a setup starts with"
            )));
        }
        out
    }

    /// (Indexed 3D roughing's angle search, across the page's workers) the turns whose depths
    /// it needs for `opts.n` angles: `{keys}` (tenths of a degree).
    pub fn gap_keys(opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        json!({ "keys": marcam::gap_keys(o["n"].as_u64().unwrap_or(0) as u32) })
    }

    /// The depths at `opts.keys` for tool `opts.tool` in setup `opts.setup`: `{depths}`.
    pub fn gap_depths(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let keys: Vec<i64> = opts["keys"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        let tool = opts["tool"].as_u64().unwrap_or(0) as u32;
        match marcam::indexed_gap_depths(&job, mesh.as_ref(), tool, &keys) {
            Ok(d) => json!({ "depths": d }),
            Err(e) => err(e),
        }
    }

    /// The turns the fine search needs, given the coarse ones' `opts.depths` at `opts.keys`:
    /// `{keys}` (those not given).
    pub fn gap_fine_keys(opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let at = depths_at(&o);
        let n = o["n"].as_u64().unwrap_or(0) as u32;
        if marcam::gap_keys(n).iter().any(|k| !at.contains_key(k)) {
            return err("gap_fine_keys: depths missing");
        }
        let keys: Vec<i64> = marcam::gap_fine_keys(n, |k| at[&k])
            .into_iter()
            .filter(|k| !at.contains_key(k))
            .collect();
        json!({ "keys": keys })
    }

    fn depths_at(o: &Value) -> std::collections::HashMap<i64, f64> {
        let nums = |k: &str| -> Vec<f64> {
            o[k].as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default()
        };
        nums("keys")
            .into_iter()
            .map(|k| k as i64)
            .zip(nums("depths"))
            .collect()
    }

    /// The `opts.n` angles from the depths `opts.depths` at `opts.keys`: `{angles}`.
    pub fn gap_pick(opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let at = depths_at(&o);
        let n = o["n"].as_u64().unwrap_or(0) as u32;
        // (a depth missing: the planner finds the angles itself)
        let missing = |k: &i64| !at.contains_key(k);
        if marcam::gap_keys(n).iter().any(missing)
            || marcam::gap_fine_keys(n, |k| at.get(&k).copied().unwrap_or(f64::INFINITY))
                .iter()
                .any(missing)
        {
            return err("gap_pick: depths missing");
        }
        json!({ "angles": marcam::gap_pick(n, |k| at[&k]) })
    }

    /// Oriented mesh triangles (flattened xyz, f32) for display, thinned to about
    /// `max_triangles` (`opts`, default 200000).
    pub fn mesh_triangles(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        // (a project with one of the person's tools reads only with their libraries)
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        // (Mar's "they just snap back": without `opts.setup` this drew the first setup's part
        // in every setup, so a part moved in setup 2 never moved on screen)
        let job = match with_setup(&opts, || job_plain(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let m = match parse_mesh(&job, mesh) {
            Ok(Some(m)) => m,
            Ok(None) => return err("no mesh given"),
            Err(e) => return err(e),
        };
        let max = opts["max_triangles"].as_u64().unwrap_or(200_000).max(1) as usize;
        let thinned = thin_for_display(&m.triangles, max);
        let tri: Vec<f32> = thinned
            .as_deref()
            .unwrap_or(&m.triangles)
            .iter()
            .flat_map(|t| t.iter().flat_map(|p| p.iter().map(|&c| c as f32)))
            .collect();
        json!({ "triangles": tri, "total": m.triangles.len(), "thinned": thinned.is_some(), "bounds": m.bounds() })
    }

    /// A mesh of more than `max` triangles made coarser for display: its vertices merged
    /// within the cells of a grid (each cell's vertices at their mean), the cell grown until at
    /// most `max` triangles are left. The surface stays whole: keeping every n-th triangle
    /// instead striped a fine mesh with holes (a STEP part at its 0.01 mm chord: 440 000
    /// slivers, drawn a third of them). `None` when it has no more than `max` already.
    fn thin_for_display(tris: &[[[f64; 3]; 3]], max: usize) -> Option<Vec<[[f64; 3]; 3]>> {
        use std::collections::{HashMap, HashSet};
        if tris.len() <= max {
            return None;
        }
        let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let area: f64 = tris
            .iter()
            .map(|[a, b, c]| {
                let (u, v) = (sub(*b, *a), sub(*c, *a));
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
            })
            .sum();
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in tris.iter().flatten() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let diag = (0..3)
            .map(|k| (hi[k] - lo[k]).powi(2))
            .sum::<f64>()
            .sqrt()
            .max(1e-9);
        // about two triangles a cell of the surface to start with
        let mut cell = (2.0 * area / max as f64).sqrt().max(diag * 1e-6);
        for _ in 0..64 {
            let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
            let mut sums: Vec<([f64; 3], f64)> = Vec::new();
            let mut kept: Vec<[usize; 3]> = Vec::new();
            let mut seen = HashSet::new();
            for t in tris {
                let c = t.map(|p| {
                    let key = [0, 1, 2].map(|k| ((p[k] - lo[k]) / cell).floor() as i64);
                    let id = *ids.entry(key).or_insert_with(|| {
                        sums.push(([0.0; 3], 0.0));
                        sums.len() - 1
                    });
                    let s = &mut sums[id];
                    for (a, b) in s.0.iter_mut().zip(p) {
                        *a += b;
                    }
                    s.1 += 1.0;
                    id
                });
                if c[0] == c[1] || c[1] == c[2] || c[0] == c[2] {
                    continue;
                }
                let mut key = c;
                key.sort_unstable();
                if seen.insert(key) {
                    kept.push(c);
                }
            }
            if kept.len() <= max {
                let at: Vec<[f64; 3]> = sums.iter().map(|(s, n)| s.map(|x| x / n)).collect();
                return Some(kept.iter().map(|c| c.map(|i| at[i])).collect());
            }
            cell *= 1.25;
        }
        None
    }

    // ------------------------------------------------------------ playback sessions

    enum Session {
        Rotary(sim::Playback),
        Xyz(sim3::Playback3),
    }

    /// Cell size for a block: `opts.cell`, else about 240 cells along its longer side.
    /// The resolution asked for: a preset (`res`: coarse, normal, fine, ultra; default normal)
    /// with `dx` / `na` / `cell` overriding it.
    fn preset_of(opts: &Value) -> sim::Resolution {
        opts["res"]
            .as_str()
            .and_then(sim::Resolution::preset)
            .unwrap_or_else(|| sim::Resolution::preset("normal").expect("built in"))
    }

    /// Rotary grid: (dx mm, cells around).
    fn rotary_res(opts: &Value) -> (f64, usize) {
        let r = preset_of(opts);
        (
            opts["dx"].as_f64().unwrap_or(r.dx).max(0.005),
            (opts["na"].as_u64().map_or(r.na, |v| v as usize)).clamp(8, 8192),
        )
    }

    /// 3-axis cell size (mm) for a block.
    fn cell_for(sb: &StockBox, opts: &Value) -> f64 {
        opts["cell"]
            .as_f64()
            .unwrap_or_else(|| preset_of(opts).cell_for(sb))
            .max(0.005)
    }

    /// Cell visits a second the simulation makes in a browser (measured on the knob: a little
    /// under the native rate on one thread): rotary grids, block heightmaps. These estimate a
    /// playback; the batch simulation (`simulate`) takes about twice as long (the job twice,
    /// and the collision check).
    const BROWSER_RATE_ROTARY: f64 = 34e6;
    const BROWSER_RATE_XYZ: f64 = 170e6;
    const BATCH_FACTOR: f64 = 2.0;
    const PLAYBACK: f64 = 1.0;
    /// Longest simulation started (seconds in a browser, estimated): past it the grid is
    /// refused with the finest preset that fits (`opts.max_seconds` lowers it).
    pub const SIM_MAX_SECONDS: f64 = 600.0;

    /// About how long simulating `tps` takes in a browser at `opts`' resolution (seconds).
    fn sim_seconds(
        job: &Job,
        tps: &[Toolpath],
        mesh: Option<&Mesh>,
        opts: &Value,
    ) -> Result<f64, String> {
        let cutters = cutters_of(job);
        if let Some(sb) = job.stock_box {
            let cell = cell_for(&sb, opts);
            let step = (cell * 0.5).min(0.1);
            let w = sim3::work_estimate3(
                cell,
                sb.min[2],
                sb.max[2],
                &sim3::segments3(tps),
                &cutters,
                step,
            );
            return Ok(w / BROWSER_RATE_XYZ);
        }
        let (dx, na) = rotary_res(opts);
        let g = marcam::sim_grid(job, mesh, dx, na).map_err(|e| e.to_string())?;
        let w = sim::work_estimate(
            g,
            job.stock.radius,
            &sim::segments(tps),
            &cutters,
            (dx * 2.0).min(0.1),
        );
        Ok(w / BROWSER_RATE_ROTARY)
    }

    fn fmt_seconds(s: f64) -> String {
        if s < 90.0 {
            format!("{} s", s.ceil().max(1.0))
        } else if s < 5400.0 {
            format!("{} min", (s / 60.0).round())
        } else {
            format!("{:.1} h", s / 3600.0)
        }
    }

    /// Why the simulation is not started at `opts`' resolution (too long), with the finest
    /// preset that would do: None to go on.
    fn too_slow(
        job: &Job,
        tps: &[Toolpath],
        mesh: Option<&Mesh>,
        opts: &Value,
        factor: f64,
    ) -> Option<String> {
        let max = opts["max_seconds"].as_f64().unwrap_or(SIM_MAX_SECONDS);
        let s = factor * sim_seconds(job, tps, mesh, opts).ok()?;
        if s <= max {
            return None;
        }
        let at = |res: &str| {
            let mut o = opts.clone();
            if let Some(m) = o.as_object_mut() {
                m.remove("dx");
                m.remove("na");
                m.remove("cell");
                m.insert("res".into(), json!(res));
            }
            sim_seconds(job, tps, mesh, &o).ok().map(|t| factor * t)
        };
        let fits = ["ultra", "fine", "normal", "coarse"]
            .into_iter()
            .find_map(|r| at(r).filter(|&t| t <= max).map(|t| (r, t)));
        let what = opts["res"]
            .as_str()
            .filter(|_| opts["dx"].is_null() && opts["cell"].is_null())
            .unwrap_or("this resolution");
        Some(match fits {
            Some((r, t)) => format!(
                "simulating this job at {what} would take about {} (longer than {}): choose {r} (about {})",
                fmt_seconds(s), fmt_seconds(max), fmt_seconds(t)
            ),
            None => format!(
                "simulating this job would take about {} even at coarse (longer than {}): it has too many moves to simulate",
                fmt_seconds(at("coarse").unwrap_or(s)), fmt_seconds(max)
            ),
        })
    }

    /// The simulation grid a document gets at a resolution, without running it: cells and
    /// rough memory (engine and 3D display), so hosts can warn before very fine grids.
    pub fn sim_grid(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let mb = |b: f64| (b / 1048576.0 * 10.0).round() / 10.0;
        // `estimate`: also how long it would take (it plans the job, from the plan cache when
        // planned already), and at each preset
        let timing = |job: &Job, mesh: Option<&Mesh>| -> Value {
            if opts["estimate"] != true {
                return Value::Null;
            }
            let Ok(tps) = plan_for_sim(job, mesh, &opts) else {
                return Value::Null;
            };
            let at = |res: &str| {
                let mut o = opts.clone();
                if let Some(m) = o.as_object_mut() {
                    m.remove("dx");
                    m.remove("na");
                    m.remove("cell");
                    m.insert("res".into(), json!(res));
                }
                sim_seconds(job, &tps, mesh, &o).ok()
            };
            json!({
                "seconds": sim_seconds(job, &tps, mesh, &opts).ok(),
                "max_seconds": SIM_MAX_SECONDS,
                "batch_factor": BATCH_FACTOR,
                "presets": {"coarse": at("coarse"), "normal": at("normal"), "fine": at("fine"), "ultra": at("ultra")},
            })
        };
        if let Some(sb) = job.stock_box {
            let cell = cell_for(&sb, &opts);
            let nx = (((sb.max[0] - sb.min[0]) / cell).ceil() as usize).max(2);
            let ny = (((sb.max[1] - sb.min[1]) / cell).ceil() as usize).max(2);
            let cells = nx * ny;
            let mesh = parse_mesh(&job, mesh).ok().flatten();
            return json!({
                "kind": "xyz", "cell": cell, "nx": nx, "ny": ny, "cells": cells,
                "engine_mb": mb(cells as f64 * 18.0), "display_mb": mb(cells as f64 * 48.0),
                "too_large": cells > sim::MAX_CELLS, "max_cells": sim::MAX_CELLS,
                "time": timing(&job, mesh.as_ref()),
            });
        }
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let (dx, na) = rotary_res(&opts);
        match marcam::sim_grid(&job, mesh.as_ref(), dx, na) {
            Ok(g) => json!({
                "kind": "rotary", "dx": g.dx, "na": g.na, "nx": g.nx, "cells": g.len(),
                "engine_mb": mb(g.len() as f64 * 18.0), "display_mb": mb(g.len() as f64 * 48.0),
                "too_large": g.len() > sim::MAX_CELLS, "max_cells": sim::MAX_CELLS,
                "time": timing(&job, mesh.as_ref()),
            }),
            Err(e) => err(e),
        }
    }

    fn grid_xyz(hm: &sim3::Heightmap3) -> Value {
        json!({"x0": hm.x0, "y0": hm.y0, "cell": hm.cell, "nx": hm.nx, "ny": hm.ny})
    }

    /// The operation markers of planned programs, as the playback legend lists them.
    fn ops_of(tps: &[Toolpath]) -> Vec<Value> {
        let mut ops = Vec::new();
        for (p, tp) in tps.iter().enumerate() {
            for m in &tp.moves {
                if let marcam::Move::Operation { id, kind, .. } = m {
                    ops.push(json!({"id": id, "tag": id + 1, "kind": kind, "program": tp.name, "program_index": p}));
                }
            }
        }
        ops
    }

    fn cutters_of(job: &Job) -> impl Fn(u32) -> Option<marcam::dropcutter::Cutter> + '_ {
        move |t| {
            job.tool(t)
                .and_then(|d| marcam::cutter_for(t, d.shape).ok())
        }
    }

    /// Batch simulation of a 3-axis (box stock) job: the block after each program.
    fn simulate_xyz(
        job: &Job,
        tps: &[Toolpath],
        mesh: Option<&Mesh>,
        sb: &StockBox,
        opts: &Value,
    ) -> Value {
        let mut hm = sim3::Heightmap3::of_job(job, sb, cell_for(sb, opts));
        if hm.z.len() > sim::MAX_CELLS {
            return err(format!(
                "simulation grid too fine: {} cells (limit {})",
                hm.z.len(),
                sim::MAX_CELLS
            ));
        }
        let part = mesh
            .map(|m| sim3::part_top(&hm, m))
            .unwrap_or_else(|| vec![None; hm.z.len()]);
        let cutters = cutters_of(job);
        let step = (hm.cell * 0.5).min(0.1);
        let intended = marcam::intended_tags(job, tps);
        let bodies = |t: u32| job.tool(t).map(marcam::collision::ToolBody::of);
        let mut stocks = Vec::new();
        for tp in tps {
            let (st, coll) = marcam::collision::simulate_block(
                &mut hm,
                tp,
                &cutters,
                &bodies,
                step,
                sb.max[2],
                job.jaws.map(|g| (*sb, sb.min[2] + g)),
                job.fixture.as_ref(),
            );
            let dev = sim3::deviation3_with(&hm, &part, 0.02, &|t| intended.contains(&t));
            stocks.push(json!({
                "name": tp.name,
                "collisions": coll,
                "samples": st.samples,
                "cells_cut": st.cells_cut,
                "rapid_hits": st.rapid_hits.len(),
                "rapid_grazes": st.rapid_grazes.len(),
                "max_left": dev.max_left,
                "mean_left": dev.mean_left,
                "max_gouge": dev.max_gouge,
                "gouge_cells": dev.gouge_cells,
                "feature_cells": dev.feature_cells,
                "z": hm.z.iter().map(|&v| v as f32).collect::<Vec<f32>>(),
            }));
        }
        json!({
            "kind": "xyz",
            "grid": grid_xyz(&hm),
            "stock_box": sb,
            "part": part.iter().map(|p| p.map(|v| v as f32)).collect::<Vec<_>>(),
            "stocks": stocks,
        })
    }

    /// A playback path point: machine tip [x, y, z], A, rapid, tool, operation tag. Rotary
    /// moves have the tool on the centreline (y 0, z = radius).
    fn path_point(sg: &sim::Segment) -> [f32; 7] {
        let (y, z, a) = match sg.index_a {
            Some(a) => (sg.to[1], sg.to[2], a),
            None => (0.0, sg.to[1], sg.to[2]),
        };
        [
            sg.to[0] as f32,
            y as f32,
            z as f32,
            a as f32,
            f32::from(u8::from(sg.rapid)),
            sg.tool.unwrap_or(0) as f32,
            sg.op as f32,
        ]
    }

    /// Machine time (seconds, from the start of the job) at the end of each segment: each
    /// program posted for `machine` and estimated line by line (the post's line of each move),
    /// the programs one after the other. Tool changes, dwells and the like go with the move
    /// after them.
    fn segment_times(
        tps: &[Toolpath],
        segs: &[sim::Segment],
        machine: &MachineProfile,
    ) -> Vec<f32> {
        let dy = marcam::estimate::Dynamics::from_profile(machine);
        let mut offset = 0.0;
        // per program: the time up to and including each move, and its total
        let mut upto: Vec<Vec<f64>> = Vec::with_capacity(tps.len());
        let mut start: Vec<f64> = Vec::with_capacity(tps.len());
        for tp in tps {
            let (text, _, lines) = marcam::post::post_lines(tp, &machine.post);
            let est = marcam::estimate::estimate_gcode(&text, &dy);
            let mut cum = vec![0.0; est.per_line.len() + 1];
            for (i, t) in est.per_line.iter().enumerate() {
                cum[i + 1] = cum[i] + t;
            }
            upto.push(lines.iter().map(|&l| cum[l.min(cum.len() - 1)]).collect());
            start.push(offset);
            offset += est.total_s;
        }
        segs.iter()
            .map(|sg| {
                let p = sg.program as usize;
                let t = upto
                    .get(p)
                    .and_then(|u| u.get(sg.move_index))
                    .copied()
                    .unwrap_or(0.0);
                (start.get(p).copied().unwrap_or(0.0) + t) as f32
            })
            .collect()
    }

    /// The machine a playback is timed for: `opts.machine` (an id or a profile), else the
    /// job's, else the Z1.
    fn playback_machine(opts: &Value, job: &Job) -> MachineProfile {
        let text = match &opts["machine"] {
            Value::String(s) => s.clone(),
            Value::Object(_) => opts["machine"].to_string(),
            _ => String::new(),
        };
        parse_machine(&text, Some(job))
            .or_else(|_| parse_machine("", Some(job)))
            .unwrap_or_else(|_| profiles::builtin_machine("makera-z1-4axis").expect("built in"))
    }

    /// Open a 3-axis playback: like [`sim_open`], on the block.
    fn sim_open_xyz(
        job: &Job,
        tps: &[Toolpath],
        sb: &StockBox,
        opts: &Value,
        job_json: &str,
        machine: &MachineProfile,
    ) -> Value {
        let hm = sim3::Heightmap3::of_job(job, sb, cell_for(sb, opts));
        if hm.z.len() > sim::MAX_CELLS {
            return err(format!(
                "simulation grid too fine: {} cells (limit {})",
                hm.z.len(),
                sim::MAX_CELLS
            ));
        }
        let grid = grid_xyz(&hm);
        // a bar lying in the vise: each row's top to start from (the page's mirror too)
        let rows: Option<Vec<f64>> = job
            .stock_axis
            .map(|_| (0..hm.ny).map(|j| hm.z[j * hm.nx]).collect());
        // (a bar standing up, or a cut piece carried in: rows are not enough) every cell's top
        // to start from
        let stock_start = (job.carried.is_some() || job.stock_axis.is_some())
            .then(|| f32s(&hm.z.iter().map(|&v| v as f32).collect::<Vec<f32>>()));
        let step = (hm.cell * 0.5).min(0.1);
        let pb = sim3::Playback3::new(hm, tps, &cutters_of(job), step);
        let mut path = Vec::with_capacity(pb.len() * 7);
        for sg in &pb.segments {
            path.extend_from_slice(&path_point(sg));
        }
        let first_seg_of_program: Vec<usize> = (0..tps.len())
            .map(|p| {
                pb.segments
                    .iter()
                    .position(|s| s.program as usize == p)
                    .unwrap_or(pb.len())
            })
            .collect();
        let out = json!({
            "kind": "xyz",
            "grid": grid,
            "stock_box": sb,
            "stock_rows": rows,
            "stock_start": stock_start,
            "segments": pb.len(),
            "program_starts": first_seg_of_program,
            "path": f32s(&path),
            "moves": f32s(&pb.segments.iter().map(|s| s.move_index as f32).collect::<Vec<f32>>()),
            "times": f32s(&segment_times(tps, &pb.segments, machine)),
            "ops": ops_of(tps),
            "project_ops": with_setup(opts, || op_ids_and_colors(job_json)).map(|(ids, colors)| visible_ops(ids, colors, opts)),
            "tools": job.tools,
        });
        SESSION.with(|s| *s.borrow_mut() = Some(Session::Xyz(pb)));
        out
    }

    thread_local! {
        static SESSION: std::cell::RefCell<Option<Session>> = const { std::cell::RefCell::new(None) };
        static GPU_SESSION: std::cell::RefCell<Option<GpuSession>> = const { std::cell::RefCell::new(None) };
    }

    /// What `gpu_stats` needs to turn the browser's GPU keys into a report.
    #[allow(clippy::large_enum_variant)]
    enum GpuSession {
        Rotary {
            start: sim::RadialMap,
            part: sim::Part,
            x_range: (f64, f64),
            intended: std::collections::HashSet<u16>,
        },
        Block {
            start: sim3::Heightmap3,
            zbase: f64,
            part: Vec<Option<f64>>,
            intended: std::collections::HashSet<u16>,
        },
    }

    fn u32s(v: &[u32]) -> String {
        b64(&v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
    }

    /// Inputs for the browser's WebGPU stock simulation (web/gpu/stamp.wgsl) at the asked-for
    /// resolution: the tool positions (`words`, 16 u32 each), where each segment's and each
    /// program's positions start, which segments are rapids, the starting keys, the grid and
    /// the part map. An error means "use the CPU" (indexed 3+1 moves, more than 255
    /// operations, too large a grid).
    pub fn gpu_open(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let tps = match plan_for_sim(&job, mesh.as_ref(), &opts) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        let cutters = cutters_of(&job);
        let intended = marcam::intended_tags(&job, &tps);
        let names: Vec<&str> = tps.iter().map(|t| t.name.as_str()).collect();
        let pack = |input: &marcam::gpu_input::GpuInput| {
            json!({
                "words": u32s(&input.words),
                "positions": input.len(),
                "seg_start": u32s(&input.seg_start),
                "seg_rapid": b64(&input.seg_rapid.iter().map(|&r| u8::from(r)).collect::<Vec<u8>>()),
                "prog_seg": input.prog_seg,
            })
        };
        if let Some(sb) = job.stock_box {
            let hm = sim3::Heightmap3::of_job(&job, &sb, cell_for(&sb, &opts));
            if hm.z.len() > sim::MAX_CELLS {
                return err("simulation grid too fine");
            }
            let step = (hm.cell * 0.5).min(0.1);
            let input = match marcam::gpu_input::block(&hm, &tps, &cutters, step) {
                Ok(i) => i,
                Err(e) => return err(e),
            };
            let part = mesh
                .as_ref()
                .map(|m| sim3::part_top(&hm, m))
                .unwrap_or_else(|| vec![None; hm.z.len()]);
            let keys = marcam::gpu_input::block_keys(&hm, input.zbase);
            let out = json!({
                "kind": "xyz",
                "grid": grid_xyz(&hm),
                "zbase": input.zbase,
                "stock_box": sb,
                "keys": u32s(&keys),
                "input": pack(&input),
                "programs": names,
                "part": part.iter().map(|p| p.map(|v| v as f32)).collect::<Vec<_>>(),
            });
            GPU_SESSION.with(|g| {
                *g.borrow_mut() = Some(GpuSession::Block {
                    start: hm,
                    zbase: input.zbase,
                    part,
                    intended,
                })
            });
            return out;
        }
        let (dx, na) = rotary_res(&opts);
        let res = match simulate_job(&job, &[], mesh.as_ref(), dx, na, 0.02) {
            Ok(r) => r,
            Err(e) => return err(e),
        };
        let grid = res.stock.grid;
        let input = match marcam::gpu_input::rotary(grid, &tps, &cutters, (dx * 2.0).min(0.1)) {
            Ok(i) => i,
            Err(e) => return err(e),
        };
        let start = sim::RadialMap::of_job(grid, &job);
        let keys = marcam::gpu_input::rotary_keys(&start);
        let x_range = match (mesh.as_ref().and_then(|m| m.bounds()), &job.part.profile) {
            (Some((lo, hi)), _) => (lo[0], hi[0]),
            (None, Some(p)) => {
                let g = marcam::geom::RevolvedProfile::new(p, job.stock.radius);
                (g.x_min(), g.x_max())
            }
            _ => (grid.x0, grid.x0 + grid.dx * grid.nx as f64),
        };
        let out = json!({
            "kind": "rotary",
            "grid": {"x0": grid.x0, "dx": grid.dx, "nx": grid.nx, "na": grid.na},
            "stock_radius": job.stock.radius,
            "keys": u32s(&keys),
            "input": pack(&input),
            "programs": names,
            "part": res.part.center.r,
        });
        GPU_SESSION.with(|g| {
            *g.borrow_mut() = Some(GpuSession::Rotary {
                start,
                part: res.part,
                x_range,
                intended,
            })
        });
        out
    }

    /// The report for one program's stock as the GPU left it: `keys` (u32 little-endian, as
    /// read back) after the program; `opts` {rapid_hits}.
    pub fn gpu_stats(opts: &str, keys: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let keys: Vec<u32> = keys
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        GPU_SESSION.with(|g| {
            let g = g.borrow();
            let Some(sess) = g.as_ref() else {
                return err("no GPU session (call gpu_open)");
            };
            let dev = match sess {
                GpuSession::Rotary {
                    start,
                    part,
                    x_range,
                    intended,
                } => {
                    if keys.len() != start.r.len() {
                        return err("keys do not match the grid");
                    }
                    let mut st = start.clone();
                    marcam::gpu_input::rotary_from_keys(
                        &mut st,
                        &marcam::gpu_input::rotary_keys(start),
                        &keys,
                    );
                    sim::deviation_with(&st, part, x_range.0, x_range.1, 0.02, &|t| {
                        intended.contains(&t)
                    })
                }
                GpuSession::Block {
                    start,
                    zbase,
                    part,
                    intended,
                } => {
                    if keys.len() != start.z.len() {
                        return err("keys do not match the grid");
                    }
                    let mut hm = start.clone();
                    marcam::gpu_input::block_from_keys(
                        &mut hm,
                        *zbase,
                        &marcam::gpu_input::block_keys(start, *zbase),
                        &keys,
                    );
                    sim3::deviation3_with(&hm, part, 0.02, &|t| intended.contains(&t))
                }
            };
            json!({
                "max_left": dev.max_left,
                "mean_left": dev.mean_left,
                "max_gouge": dev.max_gouge,
                "gouge_cells": dev.gouge_cells,
                "feature_cells": dev.feature_cells,
                "rapid_hits": opts["rapid_hits"].as_u64().unwrap_or(0),
                "rapid_grazes": opts["rapid_grazes"].as_u64().unwrap_or(0),
            })
        })
    }

    /// Standard base64 of raw bytes (typed arrays travel this way: 4/3 the size of the data).
    pub fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                if k <= c.len() {
                    out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn f32s(v: &[f32]) -> String {
        b64(&v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
    }

    fn u16s(v: &[u16]) -> String {
        b64(&v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
    }

    /// Open a material-removal playback of the whole job. `opts`: `{"dx", "na", "libraries"}`.
    /// Returns the grid, the part map, the operations, the tools and every segment's end point
    /// (`path`: f32 [x, y, z, a, rapid, tool, op] per segment, base64: machine tool tip and A).
    pub fn sim_open(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let (dx, na) = rotary_res(&opts);
        let tps = match plan_for_sim(&job, mesh.as_ref(), &opts) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        let machine = playback_machine(&opts, &job);
        // `path_only`: the moves and their times, no stock (a playback of the toolpath alone)
        if opts["path_only"] == true {
            // (a stock playback open before is not needed any more)
            SESSION.with(|s| *s.borrow_mut() = None);
            let segs = if job.stock_box.is_some() {
                sim3::segments3(&tps)
            } else {
                sim::segments(&tps)
            };
            let mut path = Vec::with_capacity(segs.len() * 7);
            for sg in &segs {
                path.extend_from_slice(&path_point(sg));
            }
            let starts: Vec<usize> = (0..tps.len())
                .map(|p| {
                    segs.iter()
                        .position(|s| s.program as usize == p)
                        .unwrap_or(segs.len())
                })
                .collect();
            return json!({
                "kind": if job.stock_box.is_some() { "xyz" } else { "rotary" },
                "path_only": true,
                "stock_radius": job.stock.radius,
                "stock_box": job.stock_box,
                "segments": segs.len(),
                "program_starts": starts,
                "path": f32s(&path),
                "moves": f32s(&segs.iter().map(|s| s.move_index as f32).collect::<Vec<f32>>()),
                "times": f32s(&segment_times(&tps, &segs, &machine)),
                "ops": ops_of(&tps),
                "project_ops": with_setup(&opts, || op_ids_and_colors(job_json)).map(|(ids, colors)| visible_ops(ids, colors, &opts)),
                "tools": job.tools,
            });
        }
        if let Some(why) = too_slow(&job, &tps, mesh.as_ref(), &opts, PLAYBACK) {
            return json!({"error": why, "too_slow": true});
        }
        if let Some(sb) = job.stock_box {
            return sim_open_xyz(&job, &tps, &sb, &opts, job_json, &machine);
        }
        // grid and part map exactly as the batch simulation builds them
        let res = match simulate_job(&job, &[], mesh.as_ref(), dx, na, 0.02) {
            Ok(r) => r,
            Err(e) => return err(e),
        };
        let grid = res.stock.grid;
        let cutters = |t: u32| {
            job.tool(t)
                .and_then(|d| marcam::cutter_for(t, d.shape).ok())
        };
        let start = sim::RadialMap::of_job(grid, &job);
        // (Mar's carry-through: the playback of a setup taking the cut piece of the one before
        // started from a fresh bar on screen) what it starts from, when not the bar as it
        // comes: the page's mirror starts from it
        let stock_start = job.carried.is_some().then(|| f32s(&start.r));
        let pb = sim::Playback::new(start, &tps, &cutters, (dx * 2.0).min(0.1));
        let mut path = Vec::with_capacity(pb.len() * 7);
        for sg in &pb.segments {
            path.extend_from_slice(&path_point(sg));
        }
        let mut ops = Vec::new();
        for (p, tp) in tps.iter().enumerate() {
            for m in &tp.moves {
                if let marcam::Move::Operation { id, kind, .. } = m {
                    ops.push(json!({"id": id, "tag": id + 1, "kind": kind, "program": tp.name, "program_index": p}));
                }
            }
        }
        let first_seg_of_program: Vec<usize> = (0..tps.len())
            .map(|p| {
                pb.segments
                    .iter()
                    .position(|s| s.program as usize == p)
                    .unwrap_or(pb.len())
            })
            .collect();
        // a bar that is not round: its radius at each cell's angle (the page's mirror starts
        // from it)
        let ring = job.stock.section.map(|_| {
            (0..grid.na)
                .map(|j| {
                    job.stock
                        .radius_at((j as f64 + 0.5) * std::f64::consts::TAU / grid.na as f64)
                })
                .collect::<Vec<f64>>()
        });
        let out = json!({
            "grid": {"x0": grid.x0, "dx": grid.dx, "nx": grid.nx, "na": grid.na},
            "stock_radius": job.stock.radius,
            "stock_ring": ring,
            "stock_start": stock_start,
            "part": f32s(&res.part.center.r),
            "segments": pb.len(),
            "program_starts": first_seg_of_program,
            "path": f32s(&path),
            "moves": f32s(&pb.segments.iter().map(|s| s.move_index as f32).collect::<Vec<f32>>()),
            "times": f32s(&segment_times(&tps, &pb.segments, &machine)),
            "ops": ops,
            "project_ops": with_setup(&opts, || op_ids_and_colors(job_json)).map(|(ids, colors)| visible_ops(ids, colors, &opts)),
            "tools": job.tools,
        });
        SESSION.with(|s| *s.borrow_mut() = Some(Session::Rotary(pb)));
        out
    }

    /// The simulated stock's surface in 3D, for the view (rotary: see
    /// [`marcam::left3d::stock_surface`]): its vertices, triangles, and the material left at
    /// each vertex (NaN past the part's ends). `opts`: as [`simulate`]'s, and `h` (mm, the
    /// rays' spacing; by default the finest that keeps the bar under 6 million cells, at
    /// least 0.25).
    pub fn sim_surface(job_json: &str, opts: &str, mesh: &[u8]) -> Value {
        let opts: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let libs = match libraries(&opts) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let job = match with_setup(&opts, || job_any_mesh(job_json, &libs, mesh)) {
            Ok(j) => j,
            Err(e) => return err(e),
        };
        if job.stock_box.is_some() || job.carried.is_some() {
            return json!({"error": "the 3D stock is for a bar on the rotary axis", "unsupported": true});
        }
        let mesh = match parse_mesh(&job, mesh) {
            Ok(m) => m,
            Err(e) => return err(e),
        };
        let tps = match plan_for_sim(&job, mesh.as_ref(), &opts) {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        let r = job.stock.radius;
        let cells = (2.0 * r) * (2.0 * r) * job.stock.length.max(1.0);
        let h = opts["h"]
            .as_f64()
            .unwrap_or_else(|| (cells / 6.0e6).cbrt().max(0.25));
        let s = marcam::left3d::stock_surface(&job, &tps, mesh.as_ref(), h);
        let idx: Vec<u8> = s
            .mesh
            .indices
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        json!({
            "h": s.h,
            "positions": f32s(&s.mesh.positions),
            "indices": b64(&idx),
            "left": f32s(&s.left),
            "vertices": s.mesh.positions.len() / 3,
            "triangles": s.mesh.indices.len() / 3,
        })
    }

    /// Drop the open playback and GPU sessions (their stock grids): a new project starts clean.
    pub fn sim_close() -> Value {
        let had = SESSION.with(|s| s.borrow_mut().take().is_some());
        let had_gpu = GPU_SESSION.with(|s| s.borrow_mut().take().is_some());
        json!({"closed": had, "gpu_closed": had_gpu})
    }

    /// Move the open playback to segment `n`; returns the rows that changed (radii and
    /// cut-by tags, base64) so the display can update in place.
    pub fn sim_seek(n: usize) -> Value {
        SESSION.with(|s| {
            let mut s = s.borrow_mut();
            let Some(sess) = s.as_mut() else {
                return err("no playback open (call sim_open)");
            };
            match sess {
                Session::Rotary(pb) => {
                    let rows = pb.seek(n);
                    let pos = pb.position();
                    match rows {
                        None => json!({"pos": pos, "rows": null}),
                        Some((i0, i1)) => {
                            let na = pb.stock.grid.na;
                            let (a, b) = (i0 * na, (i1 + 1) * na);
                            json!({"pos": pos, "rows": [i0, i1],
                                   "r": f32s(&pb.stock.r[a..b]), "op": u16s(&pb.stock.op[a..b])})
                        }
                    }
                }
                // 3-axis: rows are Y indices of nx cells; "z" holds the tops
                Session::Xyz(pb) => {
                    let rows = pb.seek(n);
                    let pos = pb.position();
                    match rows {
                        None => json!({"pos": pos, "rows": null}),
                        Some((j0, j1)) => {
                            let nx = pb.stock.nx;
                            let (a, b) = (j0 * nx, (j1 + 1) * nx);
                            let z: Vec<f32> = pb.stock.z[a..b].iter().map(|&v| v as f32).collect();
                            json!({"pos": pos, "rows": [j0, j1],
                                   "z": f32s(&z), "op": u16s(&pb.stock.op[a..b])})
                        }
                    }
                }
            }
        })
    }

    /// Polylines from an SVG document: `opts` {at: [x, y], height (mm, 0 keeps the size),
    /// tolerance}; returns {paths, count}.
    pub fn svg_paths(svg: &str, opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let tol = o["tolerance"].as_f64().unwrap_or(0.02).max(0.001);
        let paths = match marcam::engrave::svg_paths(svg, tol) {
            Ok(p) => p,
            Err(e) => return err(e),
        };
        let at = [
            o["at"][0].as_f64().unwrap_or(0.0),
            o["at"][1].as_f64().unwrap_or(0.0),
        ];
        let placed = marcam::engrave::place(&paths, at, o["height"].as_f64().unwrap_or(0.0));
        // 3 decimals keep documents readable
        let round: Vec<Vec<[f64; 2]>> = placed
            .iter()
            .map(|p| {
                p.iter()
                    .map(|q| {
                        [
                            (q[0] * 1000.0).round() / 1000.0,
                            (q[1] * 1000.0).round() / 1000.0,
                        ]
                    })
                    .collect()
            })
            .collect();
        json!({"count": round.len(), "paths": round})
    }

    /// Face regions of a model (`model` id; the mesh bytes for mesh models), plus each
    /// triangle's region index and the triangles themselves (f32 base64) for picking.
    /// (`model` may be `{"model": id, "setup": id}`: the part where that setup holds it, a
    /// carried one turned and moved with its stock.)
    pub fn face_regions(doc: &str, model: &str, mesh: &[u8]) -> Value {
        let (p, _) = match marcam::project::Project::from_json_any(doc) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let (model, setup) = match serde_json::from_str::<Value>(model.trim()) {
            Ok(Value::Object(o)) => (
                o.get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                o.get("setup").and_then(Value::as_str).map(str::to_string),
            ),
            _ => (model.trim().to_string(), None),
        };
        let Some(m) = p.models.iter().find(|m| m.id == model) else {
            // a carried piece with no model: its faces
            if let Some(s) = model
                .is_empty()
                .then(|| carried_only(&p, setup.as_deref()))
                .flatten()
            {
                let piece = match carried_piece(doc, &s.id, mesh) {
                    Ok(m) => m,
                    Err(e) => return err(e),
                };
                let (regions, of) = marcam::geometry::face_regions(&piece, 30.0);
                let tri: Vec<f32> = piece
                    .triangles
                    .iter()
                    .flat_map(|t| t.iter().flat_map(|p| p.iter().map(|&c| c as f32)))
                    .collect();
                let idx: Vec<u16> = of.iter().map(|&k| k as u16).collect();
                return json!({"regions": regions, "triangles": f32s(&tri), "region_of": u16s(&idx), "carried": true});
            }
            return err(format!("no model {model:?}"));
        };
        let held = setup
            .and_then(|id| p.setups.iter().find(|x| x.id == id))
            .and_then(|x| p.model_source_in(x, &model));
        let mesh = match &m.kind {
            marcam::project::ModelKind::Profile(prof) => Mesh::revolve(prof, 360),
            marcam::project::ModelKind::Mesh(src) => {
                match held.as_ref().unwrap_or(src).mesh(mesh) {
                    Ok(x) => x,
                    Err(e) => return err(e),
                }
            }
        };
        let (regions, of) = marcam::geometry::face_regions(&mesh, 30.0);
        let tri: Vec<f32> = mesh
            .triangles
            .iter()
            .flat_map(|t| t.iter().flat_map(|p| p.iter().map(|&c| c as f32)))
            .collect();
        let idx: Vec<u16> = of.iter().map(|&k| k as u16).collect();
        json!({"regions": regions, "triangles": f32s(&tri), "region_of": u16s(&idx)})
    }

    /// (a setup that takes what a setup before left, with no model of its own) the cut piece
    /// it takes, as a mesh in its frame: what is picked on there.
    fn carried_piece(doc: &str, setup: &str, mesh: &[u8]) -> Result<Mesh, String> {
        let libs = libraries(&json!({}))?;
        let job = with_setup(&json!({ "setup": setup }), || {
            job_any_mesh(doc, &libs, mesh)
        })?;
        let c = job
            .carried
            .as_deref()
            .ok_or("the setup has no part model")?;
        Ok(match c {
            marcam::carry::Carried::Block {
                x0,
                y0,
                cell,
                nx,
                ny,
                top,
                bot,
            } => marcam::carry_mesh::block_mesh(*x0, *y0, *cell, *nx, *ny, top, bot),
            marcam::carry::Carried::Bar(map) => marcam::carry_mesh::bar_mesh(map),
        })
    }

    /// The setup's carried piece when it has no model of its own (None: it has one, or takes
    /// no piece).
    fn carried_only<'a>(
        p: &'a marcam::project::Project,
        setup: Option<&str>,
    ) -> Option<&'a marcam::project::Setup> {
        let s = match setup {
            Some(id) => p.setups.iter().find(|s| s.id == id)?,
            None => p.setups.first()?,
        };
        (s.part.is_empty() && matches!(s.stock, marcam::project::StockDef::FromPrevious { .. }))
            .then_some(s)
    }

    /// The setup, its part model as a mesh in the setup's frame, and the recognition frame:
    /// `opts` = {setup?, model?, index_a?}.
    fn pick_context(
        doc: &str,
        o: &Value,
        mesh: &[u8],
    ) -> Result<(marcam::project::Project, String, Mesh, pickables::Frame), String> {
        let (p, _) = marcam::project::Project::from_json_any(doc)?;
        // (Mar: "picking ... says no model") a carried piece with no model: picked on itself
        if let Some(s) = carried_only(&p, o["setup"].as_str())
            .filter(|_| o["model"].as_str().is_none_or(str::is_empty))
        {
            let part = carried_piece(doc, &s.id, mesh)?;
            let kind = s.kind.clone().unwrap_or_else(|| "rotary".into());
            let frame = if kind == "rotary" {
                pickables::Frame::Rotary
            } else {
                pickables::Frame::Xyz {
                    index_a: o["index_a"].as_f64().unwrap_or(0.0),
                    sides: kind == "indexed",
                }
            };
            let id = s.id.clone();
            return Ok((p, id, part, frame));
        }
        let (setup, part, frame) = pickables::setup_part(
            &p,
            o["setup"].as_str(),
            o["model"].as_str(),
            o["index_a"].as_f64().unwrap_or(0.0),
            &|src| src.mesh(mesh).map_err(|e| e.to_string()),
        )?;
        Ok((p, setup, part, frame))
    }

    /// The features of the setup's part an operation can be pointed at, and which kinds each
    /// operation type takes: `opts` = {setup?, model?, index_a?}.
    pub fn pickables(doc: &str, opts: &str, mesh: &[u8]) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let (_, setup, part, frame) = match pick_context(doc, &o, mesh) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let list = pickables::pickables(&part, frame);
        let accepts: serde_json::Map<String, Value> = marcam::ops::registry()
            .iter()
            .map(|t| (t.kind.to_string(), json!(pickables::accepts(t.kind))))
            .collect();
        json!({
            "setup": setup,
            "frame": match frame { pickables::Frame::Rotary => "rotary", _ => "xyz" },
            "pickables": list,
            "accepts": accepts,
        })
    }

    /// A project tool's diameter: inline, or from its library.
    fn tool_diameter_in(p: &marcam::project::Project, n: u32, libs: &[ToolLibrary]) -> Option<f64> {
        p.tools
            .iter()
            .find(|t| t.number() == n)
            .and_then(|t| match t {
                marcam::project::ToolEntry::Inline(_) => {
                    marcam::replace::inline_tool_diameter(p, n)
                }
                marcam::project::ToolEntry::Library { library, tool, .. } => libs
                    .iter()
                    .find(|l| &l.id == library)
                    .and_then(|l| l.tool(tool))
                    .map(|t| t.shape.diameter()),
            })
    }

    /// Replace a mesh model with a new part (`mesh`: its bytes; `opts` = {model?, file,
    /// libraries?}; the model defaults to the project's first mesh model): its placement,
    /// setups, tools and operations kept, every operation's picks found again on it. Returns
    /// {project, operations: [{setup, op, ok, picks, error?}]}.
    pub fn replace_model(doc: &str, opts: &str, mesh: &[u8]) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let project: Value = match serde_json::from_str(doc) {
            Ok(v) => v,
            Err(e) => return err(format!("not JSON: {e}")),
        };
        let model = match o["model"].as_str() {
            Some(m) => m.to_string(),
            None => match project["models"]
                .as_array()
                .and_then(|ms| ms.iter().find(|m| m["kind"] == "mesh"))
            {
                Some(m) => m["id"].as_str().unwrap_or("").to_string(),
                None => return err("the project has no mesh model to replace"),
            },
        };
        let Some(file) = o["file"].as_str() else {
            return err("replace_model needs the new file's name");
        };
        let libs = libraries(&o).unwrap_or_default();
        match marcam::replace::replace_model(
            &project,
            &model,
            file,
            &|src| src.mesh(mesh).map_err(|e| e.to_string()),
            &|p, n| tool_diameter_in(p, n, &libs),
        ) {
            Ok((project, report)) => {
                json!({"model": model, "project": project, "operations": report})
            }
            Err(e) => err(e),
        }
    }

    /// The features an operation's picks are on the part as it is: `opts` = {setup?, model?,
    /// index_a?, signatures: [...]}. Returns {picks: [{id, faces, label} | {error}]}.
    pub fn resolve_picks(doc: &str, opts: &str, mesh: &[u8]) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let (_, _, part, frame) = match pick_context(doc, &o, mesh) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let list = pickables::pickables(&part, frame);
        let picks: Vec<Value> = o["signatures"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| match marcam::replace::find(s, &list) {
                Ok(p) => json!({"id": p.id, "faces": p.faces, "label": p.label}),
                Err(e) => json!({"error": e, "label": s["label"]}),
            })
            .collect();
        json!({ "picks": picks })
    }

    /// The parameters picked features set on an operation: `opts` = {type, ids, setup?,
    /// model?, index_a?, tool?}. Returns {params, notes}.
    pub fn apply_picks(doc: &str, opts: &str, mesh: &[u8]) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let Some(kind) = o["type"].as_str() else {
            return err("apply_picks needs a type");
        };
        let (p, setup, part, frame) = match pick_context(doc, &o, mesh) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let list = pickables::pickables(&part, frame);
        let ids: Vec<&str> = o["ids"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut picks = Vec::new();
        for id in &ids {
            match list.iter().find(|x| x.id == *id) {
                Some(x) => picks.push(x),
                None => return err(format!("the part has no pickable {id:?}")),
            }
        }
        let stock_top = pickables::stock_top(&p, &setup);
        let libs = libraries(&o).unwrap_or_default();
        let tool_diameter = o["tool"]
            .as_u64()
            .and_then(|n| tool_diameter_in(&p, n as u32, &libs));
        let cx = pickables::ApplyContext {
            stock_top,
            tool_diameter,
            model: o["model"].as_str().unwrap_or("").to_string(),
            op: (!o["op"].is_null()).then(|| o["op"].clone()),
        };
        match pickables::apply(kind, &picks, &cx) {
            Ok((mut params, mut notes)) => {
                // (Mar's carried part: the OD picked to knurl ran into the chuck's jaws) a
                // knurl's body kept clear of them, as its defaults are
                if kind == "helical_knurl" {
                    let job = p
                        .setups
                        .iter()
                        .find(|s| s.id == setup)
                        .and_then(|s| p.setup_job(s, &libs).ok());
                    if let (Some(job), Some(back), Some(front)) = (
                        job,
                        params["body_back"].as_array().cloned(),
                        params["body_front"].as_array().cloned(),
                    ) {
                        let clear = marcam::ops::chuck_clear(&job);
                        let (x0, r0) = (
                            back[0].as_f64().unwrap_or(0.0),
                            back[1].as_f64().unwrap_or(0.0),
                        );
                        let (x1, r1) = (
                            front[0].as_f64().unwrap_or(0.0),
                            front[1].as_f64().unwrap_or(0.0),
                        );
                        if x0 < clear && x1 - clear >= 1.0 {
                            let r = r0 + (r1 - r0) * (clear - x0) / (x1 - x0);
                            let um = |v: f64| (v * 1000.0).round() / 1000.0;
                            params["body_back"] = json!([um(clear), um(r)]);
                            notes.push(format!(
                                "the knurl starts at X{clear:.2}, 2 mm clear of the chuck's jaws"
                            ));
                        } else if x1 - clear < 1.0 {
                            notes.push(
                                "the band picked is in the chuck's jaws: nothing of it to knurl"
                                    .into(),
                            );
                        }
                    }
                }
                json!({"params": params, "notes": notes})
            }
            Err(e) => err(e),
        }
    }

    /// Defaults for a new operation: `opts` = {type, setup?, tool?, geometry?, machine?,
    /// libraries?}. Every parameter comes back filled.
    /// Feeds and speeds ([`marcam::feeds`]): opts are a [`marcam::feeds::FeedsInput`] and
    /// `machine` (an id or a profile) for its spindle range, power and feed cap. Returns
    /// the values with `preset`, the row they make for the material and operation.
    pub fn feeds_calc(opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let machine = match &o["machine"] {
            Value::String(id) => profiles::builtin_machine(id),
            Value::Object(_) => serde_json::from_value(o["machine"].clone()).ok(),
            _ => None,
        };
        let mut input: marcam::feeds::FeedsInput = match serde_json::from_value(o.clone()) {
            Ok(i) => i,
            Err(e) => return err(e),
        };
        if let Some(m) = &machine {
            if let Some(sp) = m.spindle {
                input.min_rpm = sp.min_rpm;
                input.max_rpm = sp.max_rpm;
                input.power_w = sp.power_w.unwrap_or(0.0);
            }
            if let Some(f) = m.max_feed {
                // (new operations' feeds stay within three quarters of the cap)
                input.feed_cap = 0.75 * f;
            }
        }
        match marcam::feeds::calculate(&input) {
            Ok(f) => {
                let preset = marcam::feeds::preset(&f, &input.material, input.op.as_deref());
                let mut v = serde_json::to_value(&f).unwrap_or(Value::Null);
                v["preset"] = serde_json::to_value(preset).unwrap_or(Value::Null);
                v
            }
            Err(e) => err(e),
        }
    }

    pub fn op_defaults(doc: &str, opts: &str, mesh: &[u8]) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let (p, _) = match marcam::project::Project::from_json_any(doc) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let libs = match libraries(&o) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let Some(kind) = o["type"].as_str() else {
            return err("op_defaults needs a type");
        };
        let setup = o["setup"]
            .as_str()
            .map(str::to_string)
            .or_else(|| p.setups.first().map(|s| s.id.clone()));
        let Some(setup) = setup else {
            return err("the project has no setups");
        };
        let geometry: Vec<marcam::geometry::Selection> =
            serde_json::from_value(o["geometry"].clone()).unwrap_or_default();
        let machine = match &o["machine"] {
            Value::String(id) => profiles::builtin_machine(id),
            Value::Object(_) => serde_json::from_value(o["machine"].clone()).ok(),
            _ => None,
        };
        // (a carried part where the setup holds it)
        let held = p.setups.iter().find(|x| x.id == setup).cloned();
        let meshes = |id: &str| {
            let src = match &held {
                Some(x) => p.model_source_in(x, id)?,
                None => match &p.models.iter().find(|m| m.id == id)?.kind {
                    marcam::project::ModelKind::Mesh(src) => src.clone(),
                    _ => return None,
                },
            };
            (!mesh.is_empty()).then(|| src.mesh(mesh).ok()).flatten()
        };
        let tool = o["tool"].as_u64().map(|t| t as u32);
        // (Mar's carry-through) a setup taking the cut piece of the one before, with no model:
        // that piece (the setups before planned and simulated; memoized) for its defaults
        let carried = held
            .as_ref()
            .filter(|x| {
                x.part.is_empty()
                    && matches!(x.stock, marcam::project::StockDef::FromPrevious { .. })
            })
            .and_then(|_| {
                let opts = json!({ "setup": setup });
                with_setup(&opts, || job_any_mesh(doc, &libs, mesh)).ok()
            })
            .and_then(|j| j.carried);
        match p.op_defaults_with(
            kind,
            &setup,
            tool,
            &geometry,
            &libs,
            machine.as_ref(),
            &meshes,
            carried,
        ) {
            Ok(v) => v,
            Err(e) => err(e),
        }
    }

    /// Change a setup's stock material: `opts` = {setup?, material (null or "": not set),
    /// machine?, libraries?}. Cutting parameters still at the old material's values take the
    /// new one's; typed ones are kept. Returns {project, changed: ["<op id>: <param>"]}.
    pub fn set_material(doc: &str, opts: &str) -> Value {
        let o: Value = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .unwrap_or(Value::Null);
        let (mut p, _) = match marcam::project::Project::from_json_any(doc) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let libs = match libraries(&o) {
            Ok(l) => l,
            Err(e) => return err(e),
        };
        let Some(setup) = o["setup"]
            .as_str()
            .map(str::to_string)
            .or_else(|| p.setups.first().map(|s| s.id.clone()))
        else {
            return err("the project has no setups");
        };
        let machine = match &o["machine"] {
            Value::String(id) => profiles::builtin_machine(id),
            Value::Object(_) => serde_json::from_value(o["machine"].clone()).ok(),
            _ => None,
        };
        let material = o["material"].as_str().map(str::to_string);
        match p.set_material(&setup, material, &libs, machine.as_ref()) {
            Ok(changed) => json!({"project": p, "changed": changed}),
            Err(e) => err(e),
        }
    }

    /// Run the knurl wizard on a document (params JSON, mesh bytes for mesh models); returns
    /// the updated 0.2 project and what was added.
    pub fn wizard_knurl(doc: &str, params: &str, mesh: &[u8]) -> Value {
        let (mut p, _) = match marcam::project::Project::from_json_any(doc) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        let w: marcam::wizards::KnurlWizard =
            match serde_json::from_str(if params.trim().is_empty() {
                "{}"
            } else {
                params
            }) {
                Ok(w) => w,
                Err(e) => return err(format!("wizard params: {e}")),
            };
        let models = p.models.clone();
        let meshes = |id: &str| match &models.iter().find(|m| m.id == id)?.kind {
            marcam::project::ModelKind::Mesh(src) => {
                (!mesh.is_empty()).then(|| src.mesh(mesh).ok()).flatten()
            }
            _ => None,
        };
        match w.run(&mut p, &meshes) {
            Ok(res) => json!({"project": p, "added": res}),
            Err(e) => err(e),
        }
    }

    /// Dispatch by name (what `fc_call` uses).
    /// A one-setup project around a part mesh, for CAD adapters (`marcam::body`):
    /// `request` is `{"operation": "rotary_finish", ...inputs and the operation's fields}`,
    /// `mesh` the body's STL/3MF bytes. Returns `{"project", "name"}`; the project's mesh
    /// model names `<name>.stl` (save the same bytes under that name to plan it again).
    pub fn body_project(request: &str, mesh: &[u8]) -> Value {
        let mut req: Value = match serde_json::from_str(if request.trim().is_empty() {
            "{}"
        } else {
            request
        }) {
            Ok(v) => v,
            Err(e) => return err(format!("request: {e}")),
        };
        let Some(kind) = req
            .as_object_mut()
            .and_then(|o| o.remove("operation"))
            .and_then(|v| v.as_str().map(str::to_string))
        else {
            return err("request needs \"operation\": an operation type");
        };
        match marcam::body::body_project(&kind, &req, mesh, &[]) {
            Ok((project, name)) => json!({"project": project, "name": name}),
            Err(marcam::body::BodyError::Input { field, reason }) => {
                json!({"error": format!("{field}: {reason}"), "field": field})
            }
            Err(e) => err(e),
        }
    }

    pub fn call(name: &str, a: &[u8], b: &[u8], c: &[u8]) -> Value {
        let s = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
        // a project or job: TOML or JSON (the functions read JSON)
        let d = |v: &[u8]| {
            let t = String::from_utf8_lossy(v).into_owned();
            match marcam::project_file::to_json_text(&t) {
                Ok(j) => j.into_owned(),
                Err(_) => t,
            }
        };
        match name {
            "version" => version(),
            "example_job" => serde_json::from_str(EXAMPLE_JOB).unwrap_or(Value::Null),
            "machine" => machine(&s(a)),
            "plan_and_post" => plan_and_post(&d(a), &s(b), c, ""),
            "plan_and_post_opts" => {
                // a = job, b = {"machine": ..., "max_points": ...}, c = mesh
                let o: Value = serde_json::from_slice(b).unwrap_or(Value::Null);
                let m = match &o["machine"] {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
                plan_and_post(&d(a), &m, c, &s(b))
            }
            "simulate" => simulate(&d(a), &s(b), c),
            "sim_surface" => sim_surface(&d(a), &s(b), c),
            "profile_list" => profile_list(),
            "face_regions" => face_regions(&d(a), &s(b), c),
            "wizard_knurl" => wizard_knurl(&d(a), &s(b), c),
            "body_project" => body_project(&s(a), b),
            "op_defaults" => op_defaults(&d(a), &s(b), c),
            "setup_frame" => setup_frame(&d(a), &s(b)),
            "feeds_calc" => feeds_calc(&s(a)),
            "set_material" => set_material(&d(a), &s(b)),
            "pickables" => pickables(&d(a), &s(b), c),
            "apply_picks" => apply_picks(&d(a), &s(b), c),
            "replace_model" => replace_model(&d(a), &s(b), c),
            "resolve_picks" => resolve_picks(&d(a), &s(b), c),
            "op_types" => {
                // each type with the pickable kinds it takes (marcam::pickables::accepts)
                let mut v = serde_json::to_value(marcam::ops::registry()).unwrap_or(Value::Null);
                for t in v.as_array_mut().into_iter().flatten() {
                    let kind = t["kind"].as_str().unwrap_or("").to_string();
                    t["picks"] = json!(pickables::accepts(&kind));
                }
                v
            }
            "check_project" => match marcam::project::Project::from_json_any(&d(a)) {
                Ok((p, upgraded)) => {
                    json!({"project": p, "upgraded": upgraded, "problems": p.check_operations()})
                }
                Err(e) => err(e),
            },
            "sim_open" => sim_open(&d(a), &s(b), c),
            "sim_seek" => sim_seek(s(a).trim().parse().unwrap_or(0)),
            "sim_close" => sim_close(),
            "import_profile" => import_profile(a, &s(b)),
            "mesh_triangles" => mesh_triangles(&d(a), &s(b), c),
            "gap_keys" => gap_keys(&s(a)),
            "gap_depths" => gap_depths(&d(a), &s(b), c),
            "gap_pick" => gap_pick(&s(a)),
            "seed_plan" => seed_plan(&s(a), &s(b)),
            "read_gcode" => read_gcode(&s(a), &s(b)),
            "add_custom" => add_custom(&s(a), &s(b)),
            "gap_fine_keys" => gap_fine_keys(&s(a)),
            "carried_mesh" => carried_mesh(&d(a), &s(b), c),
            "svg_paths" => svg_paths(&s(a), &s(b)),
            "estimate_gcode" => estimate_gcode(&s(a), &s(b)),
            "sim_grid" => sim_grid(&d(a), &s(b), c),
            "gpu_open" => gpu_open(&d(a), &s(b), c),
            "gpu_stats" => gpu_stats(&s(a), c),
            // project files: TOML text, and the single-file bundle (.marcam)
            "to_toml" => match marcam::project::Project::from_json_any(&d(a)) {
                Ok((p, _)) => match marcam::project_file::project_to_toml(&p) {
                    Ok(t) => json!({ "text": t }),
                    Err(e) => err(e),
                },
                Err(e) => err(e),
            },
            "bundle_write" => {
                // a = the project, b = [[name, length], ...] of the files concatenated in c
                let list: Vec<(String, usize)> = serde_json::from_slice(b).unwrap_or_default();
                let toml = match marcam::project::Project::from_json_any(&d(a)) {
                    Ok((p, _)) => match marcam::project_file::project_to_toml(&p) {
                        Ok(t) => t,
                        Err(e) => return err(e),
                    },
                    Err(e) => return err(e),
                };
                let mut files = Vec::new();
                let mut at = 0;
                for (name, n) in list {
                    let Some(bytes) = c.get(at..at + n) else {
                        return err("bundle_write: the files are shorter than their lengths");
                    };
                    files.push((name, bytes.to_vec()));
                    at += n;
                }
                json!({ "bytes": b64(&marcam::project_file::write_bundle(&toml, &files)) })
            }
            "bundle_read" => match marcam::project_file::read_bundle(c) {
                Ok((text, files)) => match marcam::project_file::to_json_text(&text)
                    .map(|t| serde_json::from_str::<Value>(&t))
                {
                    Ok(Ok(project)) => json!({
                        "project": project,
                        "files": files.iter().map(|(n, b)| json!({"name": n, "bytes": b64(b)})).collect::<Vec<_>>(),
                    }),
                    Ok(Err(e)) => err(format!("project.toml: {e}")),
                    Err(e) => err(e),
                },
                Err(e) => err(e),
            },
            other => err(format!("unknown function {other:?}")),
        }
    }
}

// the page told where a long call is (see marcam::phase): an import the JS wrapper gives
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn marcam_phase(ptr: *const u8, len: usize);
}

fn phase_to_js(what: &str) {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: the pointer and length are of a live &str for the call
    unsafe {
        marcam_phase(what.as_ptr(), what.len())
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = what;
}

/// Allocate `len` bytes for the host to write into.
#[no_mangle]
pub extern "C" fn fc_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Free memory from [`fc_alloc`] or a result of [`fc_call`] (`len` = its full length).
///
/// # Safety
/// `ptr` and `len` must come from `fc_alloc(len)` or be a result pointer with its length.
#[no_mangle]
pub unsafe extern "C" fn fc_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
    }
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

/// Call `name` with three byte-string arguments. Returns a pointer to a 4-byte little-endian
/// length followed by that many bytes of JSON; free it with `fc_free(ptr, 4 + length)`.
///
/// # Safety
/// Each pointer/length pair must describe readable memory (or be null/0).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fc_call(
    name_ptr: *const u8,
    name_len: usize,
    a_ptr: *const u8,
    a_len: usize,
    b_ptr: *const u8,
    b_len: usize,
    c_ptr: *const u8,
    c_len: usize,
) -> *mut u8 {
    // (where a long call is, told to the page as it goes: set once)
    marcam::set_phase_hook(phase_to_js);
    let name = String::from_utf8_lossy(slice(name_ptr, name_len)).into_owned();
    let out = api::call(
        &name,
        slice(a_ptr, a_len),
        slice(b_ptr, b_len),
        slice(c_ptr, c_len),
    )
    .to_string()
    .into_bytes();
    let mut buf = Vec::with_capacity(4 + out.len());
    buf.extend_from_slice(&(out.len() as u32).to_le_bytes());
    buf.extend_from_slice(&out);
    let mut buf = buf.into_boxed_slice();
    let p = buf.as_mut_ptr();
    std::mem::forget(buf);
    p
}

#[cfg(test)]
mod tests {
    use super::api;

    #[test]
    fn example_runs_through_the_json_api() {
        let v = api::plan_and_post(api::EXAMPLE_JOB, "", &[], r#"{"max_points": 5000}"#);
        assert!(v.get("error").is_none(), "{v}");
        let progs = v["programs"].as_array().unwrap();
        assert_eq!(progs.len(), 2);
        assert_eq!(progs[0]["lines"], 21787);
        assert_eq!(progs[1]["lines"], 15146);
        for p in progs {
            assert_eq!(p["issues"].as_array().unwrap().len(), 0);
            let pts = p["toolpath"]["points"].as_array().unwrap();
            assert!(pts.len() % 4 == 0 && pts.len() / 4 < 9000, "{}", pts.len());
            assert_eq!(p["toolpath"]["thinned"], true);
        }
        assert!(progs[0]["gouge"]["max_depth"].as_f64().unwrap() < 0.01);
        assert!(progs[0]["gcode"]
            .as_str()
            .unwrap()
            .starts_with("(Knob radial profile"));
    }

    #[test]
    fn machines_errors_and_dispatch() {
        assert_eq!(api::machine("linuxcnc")["feed_mode"], "inverse_time");
        assert!(api::machine("nope").get("error").is_some());
        let v = api::plan_and_post(api::EXAMPLE_JOB, "linuxcnc", &[], "");
        assert!(v["programs"][0]["gcode"].as_str().unwrap().contains("G93"));
        assert!(api::plan_and_post("{", "", &[], "").get("error").is_some());
        let bad = api::EXAMPLE_JOB.replace("\"tool\": 1", "\"tool\": 7");
        let v = api::plan_and_post(&bad, "", &[], "");
        assert!(v["error"].as_str().unwrap().contains("T7"));
        assert!(api::call("nope", &[], &[], &[]).get("error").is_some());
        assert_eq!(api::call("version", &[], &[], &[])["name"], "marcam");
    }

    #[test]
    fn profiles_import_limits_and_library_tools() {
        let l = api::profile_list();
        assert_eq!(l["machines"].as_array().unwrap().len(), 4);
        assert_eq!(l["libraries"][0]["id"], "makera-starter");
        let fixture = include_bytes!("../../../tests/fixtures/fusion-tool-library.json");
        let imp = api::import_profile(fixture, "My Tools.json");
        assert_eq!(imp["kind"], "tools", "{imp}");
        assert_eq!(imp["profile"]["tools"].as_array().unwrap().len(), 3);
        assert!(api::import_profile(b"nope", "x.json")
            .get("error")
            .is_some());
        // the knob on the Z1: limit warning for the 4000 mm/min cap
        let v = api::plan_and_post(api::EXAMPLE_JOB, "makera-z1-4axis", &[], "");
        assert_eq!(v["machine_id"], "makera-z1-4axis");
        let lim = v["limits"].as_array().unwrap();
        assert!(
            lim.iter()
                .any(|i| i["message"].as_str().unwrap().contains("F5209")),
            "{lim:?}"
        );
        // a job using an imported library tool, on the Carvera (alias)
        let mut job: serde_json::Value = serde_json::from_str(api::EXAMPLE_JOB).unwrap();
        job["tools"][0] = serde_json::json!({"number": 1, "library": "my-tools", "tool": "fixture-flat-3.175-12mm"});
        let opts = serde_json::json!({"libraries": [imp["profile"]]}).to_string();
        let v = api::plan_and_post(&job.to_string(), "carvera", &[], &opts);
        assert!(v.get("error").is_none(), "{v}");
        assert_eq!(v["machine_id"], "makera-carvera-4axis");
        assert!(v["programs"][0]["gcode"]
            .as_str()
            .unwrap()
            .contains("(T1 Fixture Flat 3.175*12mm)"));
        // machine given as a profile object
        let z1 = serde_json::to_string(&api::profile_list()["machines"][0]).unwrap();
        assert_eq!(
            api::plan_and_post(api::EXAMPLE_JOB, &z1, &[], "")["machine_id"],
            "makera-z1-4axis"
        );
    }

    #[test]
    fn playback_session_seeks_and_reports_rows() {
        assert_eq!(api::b64(b"Man"), "TWFu");
        assert_eq!(api::b64(b"Ma"), "TWE=");
        assert_eq!(api::b64(b"M"), "TQ==");
        assert!(api::sim_seek(5).get("error").is_some() || api::sim_seek(5)["pos"].is_number());
        let o = api::sim_open(api::EXAMPLE_JOB, r#"{"dx": 0.2, "na": 90}"#, &[]);
        assert!(o.get("error").is_none(), "{o}");
        let n = o["segments"].as_u64().unwrap() as usize;
        assert!(n > 30_000);
        assert_eq!(o["ops"].as_array().unwrap().len(), 3);
        assert_eq!(o["ops"][1]["kind"], "helical_knurl");
        assert_eq!(o["program_starts"][0], 0);
        let s = api::sim_seek(2000);
        assert_eq!(s["pos"], 2000);
        assert!(s["rows"].is_array(), "{s}");
        let s = api::sim_seek(10);
        assert_eq!(s["rows"][0], 0, "jumping back redraws all rows");
        let s = api::sim_seek(n + 100);
        assert_eq!(api::sim_close()["closed"], true);
        assert!(
            api::sim_seek(5).get("error").is_some(),
            "the playback is gone after sim_close"
        );
        assert_eq!(api::sim_close()["closed"], false);
        assert_eq!(s["pos"].as_u64().unwrap() as usize, n);
    }

    #[test]
    fn mesh_job_needs_mesh_and_simulates() {
        let job = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/knob/knob_finish.job.json"
        ))
        .unwrap();
        let stl = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/knob/knob.stl"
        ))
        .unwrap();
        assert!(api::plan_and_post(&job, "", &[], "").get("error").is_some());
        let tri = api::mesh_triangles(&job, r#"{"max_triangles": 1000}"#, &stl);
        assert_eq!(tri["total"], 10080);
        assert!(tri["triangles"].as_array().unwrap().len() <= 1000 * 9);
        assert_eq!(tri["thinned"], true);
        // profile-only job simulates fine at a coarse grid
        let s = api::simulate(api::EXAMPLE_JOB, r#"{"dx": 0.2, "na": 90}"#, &[]);
        assert!(s.get("error").is_none(), "{}", s["error"]);
        let st = s["stocks"].as_array().unwrap();
        assert_eq!(st.len(), 2);
        assert_eq!(st[0]["rapid_hits"], 0);
        assert_eq!(st[0]["gouge_cells"], 0);
        let n = s["grid"]["nx"].as_u64().unwrap() * s["grid"]["na"].as_u64().unwrap();
        assert_eq!(st[0]["r"].as_array().unwrap().len() as u64, n);
    }
}
