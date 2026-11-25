//! Replacing a model: the operations' picks found again on the new part.
//!
//! An operation keeps what was picked for it as signatures: the feature's kind and the
//! numbers that say what it is (a hole's centre, diameter and depth; a flat face's height,
//! area and outline box; ...), not the mesh's triangles or face numbers. When the model is
//! replaced (a new STL or STEP for the same model entry, its placement kept), each signature
//! is looked for among the new part's features, within tolerances. Found: the operation is
//! derived again from it (its points, outline, depth, faces). Not found, or more than one
//! match: the operation is put in error, naming what is missing; it stays in the project and
//! is left out of planning until it is picked again or changed.

use crate::pickables::{self, ApplyContext, Pickable};
use serde_json::{json, Map, Value};

/// A feature's signature: its kind, its label (for messages) and its data summarised (point
/// lists become their bounding box, nested lists their count).
pub fn signature(p: &Pickable) -> Value {
    json!({"kind": p.kind, "label": p.label, "data": summarize(&p.data)})
}

fn summarize(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter_map(|(k, v)| summary_of(v).map(|s| (k.clone(), s)))
                .collect(),
        ),
        _ => Value::Null,
    }
}

/// A value as it is compared: numbers, booleans, strings and short number lists as they are;
/// a list of points as its box; a list of point lists as its count; lists of flags dropped.
fn summary_of(v: &Value) -> Option<Value> {
    match v {
        Value::Number(_) | Value::Bool(_) | Value::String(_) => Some(v.clone()),
        Value::Array(a) if a.iter().all(Value::is_number) => Some(v.clone()),
        Value::Array(a)
            if a.iter().all(|p| {
                p.as_array()
                    .is_some_and(|q| q.iter().all(Value::is_number) && q.len() >= 2)
            }) =>
        {
            let dims = a
                .iter()
                .map(|p| p.as_array().map_or(0, Vec::len))
                .min()
                .unwrap_or(0)
                .min(3);
            let (mut lo, mut hi) = (vec![f64::INFINITY; dims], vec![f64::NEG_INFINITY; dims]);
            for p in a {
                for (k, c) in p.as_array().unwrap().iter().take(dims).enumerate() {
                    let c = c.as_f64().unwrap_or(0.0);
                    lo[k] = lo[k].min(c);
                    hi[k] = hi[k].max(c);
                }
            }
            Some(json!({"box": lo.into_iter().chain(hi).map(r4).collect::<Vec<_>>()}))
        }
        Value::Array(a) if a.iter().all(Value::is_array) => Some(json!({"count": a.len()})),
        _ => None,
    }
}

fn r4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// How far two numbers may differ and still be the same feature: 0.05 mm, or 1 % of the
/// size (an area, a length), whichever is larger.
fn tol(a: f64) -> f64 {
    (0.01 * a.abs()).max(0.05)
}

/// How far `got` is from `want` (0: the same; None: not the same feature).
fn distance(want: &Value, got: &Value) -> Option<f64> {
    match (want, got) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64()?, b.as_f64()?);
            let t = tol(a);
            ((a - b).abs() <= t).then(|| (a - b).abs() / t)
        }
        (Value::Bool(a), Value::Bool(b)) => (a == b).then_some(0.0),
        (Value::String(a), Value::String(b)) => (a == b).then_some(0.0),
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return None;
            }
            a.iter().zip(b).map(|(x, y)| distance(x, y)).sum()
        }
        (Value::Object(a), Value::Object(b)) => {
            let mut d = 0.0;
            for (k, x) in a {
                d += distance(x, b.get(k)?)?;
            }
            Some(d)
        }
        _ => None,
    }
}

/// The feature of `list` a signature names: the only one within the tolerances, or the
/// nearest when it is clearly nearer than any other. Err: a sentence saying why not.
pub fn find<'a>(sig: &Value, list: &'a [Pickable]) -> Result<&'a Pickable, String> {
    let kind = sig["kind"].as_str().unwrap_or("");
    let label = sig["label"].as_str().unwrap_or(kind);
    let mut hits: Vec<(f64, &Pickable)> = list
        .iter()
        .filter(|p| p.kind == kind)
        .filter_map(|p| distance(&sig["data"], &summarize(&p.data)).map(|d| (d, p)))
        .collect();
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    match hits.as_slice() {
        [] => Err(format!("{label}: not on the new model")),
        [(_, p)] => Ok(p),
        [(d0, p), (d1, _), ..] if *d1 > 4.0 * d0 + 0.5 => Ok(p),
        _ => Err(format!(
            "{label}: {} features on the new model match it: pick it again",
            hits.len()
        )),
    }
}

/// The operation types whose picks are faces they are bounded by (not fields filled from
/// the features).
const FACE_TYPES: [&str; 5] = [
    "raster3d",
    "rough3d",
    "waterline",
    "rotary_finish",
    "rotary_parallel",
];

/// What happened to one operation.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct OpReport {
    pub setup: String,
    pub op: String,
    /// Its picks were all found again (or it had none).
    pub ok: bool,
    /// How many picks it had.
    pub picks: usize,
    /// Why not, when not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Find every operation's picks again on the project's models as they are now (after a model
/// was replaced): found, the operation is derived again from the features and any error is
/// cleared; not found, it is put in error. `mesh_of` reads a mesh model's file. Returns the
/// project (JSON, every other field as it was) and a report per operation with picks.
pub fn resolve(
    project: &Value,
    mesh_of: &dyn Fn(&crate::job::MeshSource) -> Result<crate::Mesh, String>,
    tool_diameter: &dyn Fn(&crate::project::Project, u32) -> Option<f64>,
) -> Result<(Value, Vec<OpReport>), String> {
    let (p, _) = crate::project::Project::from_json_any(&project.to_string())?;
    let mut out = project.clone();
    let mut report = Vec::new();
    let setups = out["setups"]
        .as_array_mut()
        .ok_or("the project has no setups")?;
    for s in setups.iter_mut() {
        let sid = s["id"].as_str().unwrap_or("").to_string();
        let Some(ops) = s["operations"].as_array_mut() else {
            continue;
        };
        // the part's features per index angle (indexed setups turn the part)
        let mut cache: Vec<(f64, String, Vec<Pickable>)> = Vec::new();
        for op in ops.iter_mut() {
            let sigs: Vec<Value> = op["picks"].as_array().cloned().unwrap_or_default();
            if sigs.is_empty() {
                continue;
            }
            let id = op["id"].as_str().unwrap_or("").to_string();
            let ty = op["type"].as_str().unwrap_or("").to_string();
            let index_a = op["index_a"].as_f64().unwrap_or(0.0);
            if !cache.iter().any(|c| c.0 == index_a) {
                let (_, part, frame) =
                    pickables::setup_part(&p, Some(&sid), None, index_a, mesh_of)?;
                let model = setup_model(&p, &sid).unwrap_or_default();
                cache.push((index_a, model, pickables::pickables(&part, frame)));
            }
            let (_, model, list) = cache.iter().find(|c| c.0 == index_a).unwrap();
            let found: Vec<Result<&Pickable, String>> =
                sigs.iter().map(|g| find(g, list)).collect();
            let missing: Vec<String> = found
                .iter()
                .filter_map(|r| r.as_ref().err().cloned())
                .collect();
            let o = op.as_object_mut().unwrap();
            if !missing.is_empty() {
                let e = missing.join("; ");
                o.insert("error".into(), json!(e));
                report.push(OpReport {
                    setup: sid.clone(),
                    op: id,
                    ok: false,
                    picks: sigs.len(),
                    error: Some(e),
                });
                continue;
            }
            let picks: Vec<&Pickable> = found.into_iter().map(Result::unwrap).collect();
            let applied = if FACE_TYPES.contains(&ty.as_str()) {
                // bounded by the features' faces (their ids on the new model)
                let mut ids: Vec<String> = Vec::new();
                for x in &picks {
                    for f in &x.faces {
                        if !ids.contains(f) {
                            ids.push(f.clone());
                        }
                    }
                }
                let mut geo: Vec<Value> = o
                    .get("geometry")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|g| g["kind"] != "faces")
                    .collect();
                geo.push(json!({"kind": "faces", "model": model, "ids": ids}));
                o.insert("geometry".into(), Value::Array(geo));
                if let Some(side) = picks.iter().find(|x| x.kind == "side_face") {
                    o.insert("index_a".into(), side.data["index_a"].clone());
                }
                Ok(())
            } else {
                let cx = ApplyContext {
                    stock_top: pickables::stock_top(&p, &sid),
                    tool_diameter: o
                        .get("tool")
                        .and_then(Value::as_u64)
                        .and_then(|n| tool_diameter(&p, n as u32)),
                    model: model.clone(),
                    op: Some(Value::Object(o.clone())),
                };
                pickables::apply(&ty, &picks, &cx).map(|(params, _)| merge(o, params))
            };
            match applied {
                Ok(()) => {
                    o.remove("error");
                    // the signatures of what was found: the new model's numbers
                    o.insert(
                        "picks".into(),
                        Value::Array(picks.iter().map(|x| x.signature.clone()).collect()),
                    );
                    report.push(OpReport {
                        setup: sid.clone(),
                        op: id,
                        ok: true,
                        picks: sigs.len(),
                        error: None,
                    });
                }
                Err(e) => {
                    o.insert("error".into(), json!(e));
                    report.push(OpReport {
                        setup: sid.clone(),
                        op: id,
                        ok: false,
                        picks: sigs.len(),
                        error: Some(e),
                    });
                }
            }
        }
    }
    Ok((out, report))
}

fn merge(o: &mut Map<String, Value>, params: Map<String, Value>) {
    // a pocket's picked outline replaces its rectangle
    if params.contains_key("paths") && o.get("type").and_then(Value::as_str) == Some("pocket") {
        o.remove("rect");
    }
    for (k, v) in params {
        o.insert(k, v);
    }
}

/// The mesh model a setup's features come from (as `setup_part` chooses it).
fn setup_model(p: &crate::project::Project, setup: &str) -> Option<String> {
    use crate::project::ModelKind;
    let s = p.setups.iter().find(|s| s.id == setup)?;
    s.part
        .iter()
        .find(|id| {
            p.models
                .iter()
                .any(|m| &m.id == *id && matches!(m.kind, ModelKind::Mesh(_)))
        })
        .or(s.part.first())
        .cloned()
}

/// Replace a model's file (its placement, setups, tools and operations kept) and find every
/// operation's picks again on it: the project with the model's `file` set to `file`, and
/// the report. `mesh_of` reads mesh files (the new one among them).
pub fn replace_model(
    project: &Value,
    model: &str,
    file: &str,
    mesh_of: &dyn Fn(&crate::job::MeshSource) -> Result<crate::Mesh, String>,
    tool_diameter: &dyn Fn(&crate::project::Project, u32) -> Option<f64>,
) -> Result<(Value, Vec<OpReport>), String> {
    let mut p = project.clone();
    let m = p["models"]
        .as_array_mut()
        .and_then(|ms| ms.iter_mut().find(|m| m["id"] == model))
        .ok_or(format!("the project has no model {model:?}"))?;
    if m["kind"] != "mesh" {
        return Err(format!(
            "model {model:?} is not a mesh: only a mesh model can be replaced"
        ));
    }
    m["file"] = json!(file);
    // a relief built from an image, replaced by a mesh: no image settings any more (another
    // image keeps them)
    let image = [".png", ".pgm"]
        .iter()
        .any(|e| file.to_ascii_lowercase().ends_with(e));
    if !image {
        if let Some(o) = m.as_object_mut() {
            o.remove("image");
        }
    }
    resolve(&p, mesh_of, tool_diameter)
}

/// A project's inline tool's diameter (library tools: unknown here).
pub fn inline_tool_diameter(p: &crate::project::Project, number: u32) -> Option<f64> {
    use crate::job::ToolShape;
    p.tools
        .iter()
        .find(|t| t.number() == number)
        .and_then(|t| match t {
            crate::project::ToolEntry::Inline(t) => Some(match t.shape {
                ToolShape::Flat { diameter }
                | ToolShape::Ball { diameter }
                | ToolShape::TaperedBall { diameter, .. } => diameter,
                ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0),
            }),
            _ => None,
        })
}
