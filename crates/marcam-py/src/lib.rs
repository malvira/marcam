//! Python bindings: `import marcam`. Same JSON API as the WebAssembly build; results come
//! back as Python dicts and lists. Errors raise `ValueError`.

use marcam_wasm::api;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyString;

/// JSON text of a job given as `str` (JSON) or any JSON-serialisable object (e.g. a dict).
fn json_text(py: Python<'_>, v: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = v.cast::<PyString>() {
        // a project file's text: TOML (what marcam writes) or JSON
        let s = s.to_string();
        return Ok(match ::marcam::project_file::to_json_text(&s) {
            Ok(j) => j.into_owned(),
            Err(_) => s,
        });
    }
    let json = py.import("json")?;
    json.call_method1("dumps", (v,))?.extract()
}

/// Mesh bytes from `bytes` or a file path.
fn mesh_bytes(mesh: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<u8>> {
    let Some(m) = mesh else { return Ok(Vec::new()) };
    if m.is_none() {
        return Ok(Vec::new());
    }
    if let Ok(path) = m.extract::<std::path::PathBuf>() {
        if !m.is_instance_of::<pyo3::types::PyBytes>() {
            return std::fs::read(&path)
                .map_err(|e| PyValueError::new_err(format!("{}: {e}", path.display())));
        }
    }
    m.extract::<Vec<u8>>()
}

fn to_py(py: Python<'_>, v: serde_json::Value) -> PyResult<Py<PyAny>> {
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        return Err(PyValueError::new_err(e.to_owned()));
    }
    let json = py.import("json")?;
    Ok(json.call_method1("loads", (v.to_string(),))?.unbind())
}

/// Library version information.
#[pyfunction]
fn version(py: Python<'_>) -> PyResult<Py<PyAny>> {
    to_py(py, api::version())
}

/// The bundled knurled-knob example job (dict).
#[pyfunction]
fn example_job(py: Python<'_>) -> PyResult<Py<PyAny>> {
    to_py(
        py,
        serde_json::from_str(api::EXAMPLE_JOB).unwrap_or_default(),
    )
}

/// A built-in machine profile ("makera-z1" or "linuxcnc").
#[pyfunction]
#[pyo3(signature = (name = "makera-z1"))]
fn machine(py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
    to_py(py, api::machine(name))
}

/// Plan, post, verify and gouge-check a job.
///
/// `job`: dict or JSON text. `machine`: built-in name, dict or JSON text (default Makera Z1).
/// `mesh`: STL/3MF bytes or a file path, for jobs with `part.mesh`. Returns a dict with one
/// entry per program: `name`, `gcode`, `lines`, `issues`, `gouge`, `toolpath`.
#[pyfunction]
#[pyo3(signature = (job, machine = None, mesh = None, max_points = 200_000))]
fn plan_and_post(
    py: Python<'_>,
    job: &Bound<'_, PyAny>,
    machine: Option<&Bound<'_, PyAny>>,
    mesh: Option<&Bound<'_, PyAny>>,
    max_points: u64,
) -> PyResult<Py<PyAny>> {
    let job = json_text(py, job)?;
    let machine = match machine {
        Some(m) if !m.is_none() => json_text(py, m)?,
        _ => String::new(),
    };
    let mesh = mesh_bytes(mesh)?;
    let opts = format!(r#"{{"max_points": {max_points}}}"#);
    let v = py.detach(|| api::plan_and_post(&job, &machine, &mesh, &opts));
    to_py(py, v)
}

/// Simulate material removal of every program in order on a radial heightmap.
#[pyfunction]
#[pyo3(signature = (job, dx = 0.1, na = 360, mesh = None))]
fn simulate(
    py: Python<'_>,
    job: &Bound<'_, PyAny>,
    dx: f64,
    na: u32,
    mesh: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let job = json_text(py, job)?;
    let mesh = mesh_bytes(mesh)?;
    let opts = format!(r#"{{"dx": {dx}, "na": {na}}}"#);
    let v = py.detach(|| api::simulate(&job, &opts, &mesh));
    to_py(py, v)
}

/// Estimate the machining time of G-code text for a machine (built-in name, dict or JSON
/// text; default Makera Z1): dict with `total_s`, `feed_s`, `rapid_s`, `other_s`, distances
/// and notes. `plan_and_post` results carry the same per program and per operation.
#[pyfunction]
#[pyo3(signature = (gcode, machine = None))]
fn estimate(
    py: Python<'_>,
    gcode: &str,
    machine: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let machine = match machine {
        Some(m) if !m.is_none() => json_text(py, m)?,
        _ => String::new(),
    };
    let v = py.detach(|| api::estimate_gcode(gcode, &machine));
    to_py(py, v)
}

/// Any engine function by name, as the WebAssembly module and the C library expose them
/// (`docs/api.md`): up to three arguments, each JSON-able (dict, list, str) or bytes, whose
/// meaning depends on the function. Returns its result; raises `ValueError` on an error.
#[pyfunction]
#[pyo3(signature = (name, a = None, b = None, c = None))]
fn call(
    py: Python<'_>,
    name: &str,
    a: Option<&Bound<'_, PyAny>>,
    b: Option<&Bound<'_, PyAny>>,
    c: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let arg = |v: Option<&Bound<'_, PyAny>>| -> PyResult<Vec<u8>> {
        match v {
            None => Ok(Vec::new()),
            Some(v) if v.is_none() => Ok(Vec::new()),
            Some(v) if v.is_instance_of::<pyo3::types::PyBytes>() => v.extract::<Vec<u8>>(),
            Some(v) => Ok(json_text(py, v)?.into_bytes()),
        }
    };
    let (a, b, c) = (arg(a)?, arg(b)?, arg(c)?);
    let name = name.to_string();
    let v = py.detach(|| api::call(&name, &a, &b, &c));
    to_py(py, v)
}

/// The features of a setup's part an operation can be pointed at (holes, pockets, faces,
/// edge loops, surfaces, rotary bands), and which kinds each operation type takes.
#[pyfunction]
#[pyo3(signature = (project, mesh = None, setup = None, model = None, index_a = 0.0))]
fn pickables(
    py: Python<'_>,
    project: &Bound<'_, PyAny>,
    mesh: Option<&Bound<'_, PyAny>>,
    setup: Option<&str>,
    model: Option<&str>,
    index_a: f64,
) -> PyResult<Py<PyAny>> {
    let doc = json_text(py, project)?;
    let mesh = mesh_bytes(mesh)?;
    let req = serde_json::json!({"setup": setup, "model": model, "index_a": index_a}).to_string();
    let v = py.detach(|| api::pickables(&doc, &req, &mesh));
    to_py(py, v)
}

/// A project (dict, TOML or JSON text) as TOML text, the way marcam writes project files.
#[pyfunction]
fn to_toml(py: Python<'_>, project: &Bound<'_, PyAny>) -> PyResult<String> {
    let doc = json_text(py, project)?;
    ::marcam::project_file::convert_to_toml(&doc).map_err(pyo3::exceptions::PyValueError::new_err)
}

/// Replace a mesh model with a new part (`mesh`: its bytes or a path; `file`: the name the
/// project gives it): placement, setups, tools and operations kept, every operation's picks
/// found again. Returns `{"project": {...}, "operations": [{"setup", "op", "ok", "error"}]}`;
/// operations whose features are gone are in error in the project (left out of the G-code).
#[pyfunction]
#[pyo3(signature = (project, mesh, file, model = None))]
fn replace_model(
    py: Python<'_>,
    project: &Bound<'_, PyAny>,
    mesh: &Bound<'_, PyAny>,
    file: &str,
    model: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let doc = json_text(py, project)?;
    let mesh = mesh_bytes(Some(mesh))?;
    let req = serde_json::json!({"model": model, "file": file}).to_string();
    let v = py.detach(|| api::replace_model(&doc, &req, &mesh));
    to_py(py, v)
}

/// Add a custom toolpath (G-code written elsewhere: a script, another CAM) to a setup:
/// `{"project": ..., "operation": id, "read": {feed_moves, tools, warnings, errors}}`. Its
/// frame is the setup's: a vise X Y Z (the tool tip), the 4th axis X along the bar, Z the
/// radius, A degrees. `moves` (JSON: [{rapid?, x?, y?, z?, a?, f?}]) instead of `gcode`.
#[pyfunction]
#[pyo3(signature = (project, setup, gcode = None, tool = None, verbatim = false, name = None, moves = None))]
#[allow(clippy::too_many_arguments)]
fn add_custom_toolpath(
    py: Python<'_>,
    project: &Bound<'_, PyAny>,
    setup: &str,
    gcode: Option<&str>,
    tool: Option<u32>,
    verbatim: bool,
    name: Option<&str>,
    moves: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let doc = json_text(py, project)?;
    let moves: serde_json::Value = match moves {
        Some(m) => serde_json::from_str(&json_text(py, m)?)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
        None => serde_json::Value::Null,
    };
    let req = serde_json::json!({"setup": setup, "gcode": gcode, "tool": tool, "verbatim": verbatim, "name": name, "moves": moves}).to_string();
    let v = py.detach(|| api::add_custom(&doc, &req));
    to_py(py, v)
}

/// The parameters picked features (`ids` from [`pickables`]) set on an operation of type
/// `op_type`: `{"params": {...}, "notes": [...]}`.
#[pyfunction]
#[pyo3(signature = (project, op_type, ids, mesh = None, setup = None, tool = None, index_a = 0.0))]
#[allow(clippy::too_many_arguments)]
fn apply_picks(
    py: Python<'_>,
    project: &Bound<'_, PyAny>,
    op_type: &str,
    ids: Vec<String>,
    mesh: Option<&Bound<'_, PyAny>>,
    setup: Option<&str>,
    tool: Option<u32>,
    index_a: f64,
) -> PyResult<Py<PyAny>> {
    let doc = json_text(py, project)?;
    let mesh = mesh_bytes(mesh)?;
    let req = serde_json::json!({"type": op_type, "ids": ids, "setup": setup, "tool": tool, "index_a": index_a}).to_string();
    let v = py.detach(|| api::apply_picks(&doc, &req, &mesh));
    to_py(py, v)
}

/// A one-setup project around a part mesh, for CAD adapters: `operation` (e.g.
/// "rotary_finish"), the body's STL/3MF bytes (or a path), and inputs such as `axis`,
/// `machine`, `tool_kind`, `tool_diameter`, `stock_radius`, `name` or any of the operation's
/// fields (all defaulted). Returns `{"project": ..., "name": ...}`.
#[pyfunction]
#[pyo3(signature = (operation, mesh, **inputs))]
fn body_project(
    py: Python<'_>,
    operation: &str,
    mesh: &Bound<'_, PyAny>,
    inputs: Option<&Bound<'_, pyo3::types::PyDict>>,
) -> PyResult<Py<PyAny>> {
    let mut req: serde_json::Value = match inputs {
        Some(d) => serde_json::from_str(&json_text(py, d.as_any())?)
            .map_err(|e| PyValueError::new_err(e.to_string()))?,
        None => serde_json::json!({}),
    };
    req["operation"] = serde_json::json!(operation);
    let mesh = mesh_bytes(Some(mesh))?;
    let req = req.to_string();
    let v = py.detach(|| api::body_project(&req, &mesh));
    to_py(py, v)
}

#[pymodule]
fn marcam(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // STEP parts, when built with the step feature
    #[cfg(feature = "step")]
    marcam_step::register();
    m.add("step", cfg!(feature = "step"))?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(example_job, m)?)?;
    m.add_function(wrap_pyfunction!(machine, m)?)?;
    m.add_function(wrap_pyfunction!(plan_and_post, m)?)?;
    m.add_function(wrap_pyfunction!(simulate, m)?)?;
    m.add_function(wrap_pyfunction!(estimate, m)?)?;
    m.add_function(wrap_pyfunction!(call, m)?)?;
    m.add_function(wrap_pyfunction!(body_project, m)?)?;
    m.add_function(wrap_pyfunction!(pickables, m)?)?;
    m.add_function(wrap_pyfunction!(apply_picks, m)?)?;
    m.add_function(wrap_pyfunction!(add_custom_toolpath, m)?)?;
    m.add_function(wrap_pyfunction!(replace_model, m)?)?;
    m.add_function(wrap_pyfunction!(to_toml, m)?)?;
    Ok(())
}
