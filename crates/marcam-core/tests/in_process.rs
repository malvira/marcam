//! (Mar) In-process stock: each 2D operation starts at the material the operations before it
//! left in its way (and a stock carried in), not at the stock's top: "face the stock to the
//! first height, then a 2D contour around the first step starts at the faced height".

use marcam::carry::carry_stock;
use marcam::job::Job;
use marcam::project::Project;
use marcam::{Move, Toolpath};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The plate's vise setup with `ops` only (one output file), no model.
fn plate_with(ops: Value) -> Value {
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    let ids: Vec<Value> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].clone())
        .collect();
    v["setups"][0]["operations"] = ops;
    v["setups"][0]["part"] = json!([]);
    v["setups"][0]["outputs"] = json!([{"name": "all", "title": "all", "operations": ids}]);
    v["setups"] = json!([v["setups"][0]]);
    v
}

fn jobs(v: &Value) -> Vec<(String, Job)> {
    let libs = marcam::profiles::builtin_libraries();
    let dir = root().join("examples/plate");
    marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(&dir).ok()).unwrap()
}

/// The heights each operation's feed moves end at, by operation (in planning order).
fn zs(tps: &[Toolpath]) -> Vec<Vec<f64>> {
    let mut out: Vec<Vec<f64>> = Vec::new();
    for m in tps.iter().flat_map(|t| &t.moves) {
        match m {
            Move::Operation { .. } => out.push(Vec::new()),
            Move::Line3 { z, .. } => out.last_mut().unwrap().push(*z),
            _ => {}
        }
    }
    out
}

fn top(z: &[f64]) -> f64 {
    z.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn circle(cx: f64, cy: f64, r: f64) -> Vec<[f64; 2]> {
    (0..72)
        .map(|k| {
            let a = k as f64 * std::f64::consts::TAU / 72.0;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

fn face(id: &str, depth: f64) -> Value {
    json!({"id": id, "type": "face", "tool": 1, "depth": depth, "step_down": 0.5, "stepover": 0.6, "feed": 300.0, "plunge_feed": 60.0})
}

fn contour(id: &str, r: f64, depth: f64) -> Value {
    json!({"id": id, "type": "contour", "tool": 1, "path": circle(30.0, 20.0, r), "side": "outside", "depth": depth, "step_down": 1.0, "feed": 300.0, "plunge_feed": 60.0})
}

#[test]
fn a_stepped_boss_each_cut_starts_at_the_material_left() {
    // face 1 down; a contour round the first step (r 8) to -4: from the faced height, no pass
    // above it; a pocket clears round it to -4; a contour round a smaller step inside the
    // cleared area (r 6, to -7) starts at -4
    let ops = json!([
        face("f", 1.0),
        contour("c1", 8.0, 4.0),
        {"id": "p", "type": "pocket", "tool": 1, "rect": [12.0, 4.0, 48.0, 36.0], "depth": 4.0, "step_down": 1.0, "stepover": 0.5, "feed": 300.0, "plunge_feed": 60.0},
        contour("c2", 6.0, 7.0),
    ]);
    let v = plate_with(ops.clone());
    let js = jobs(&v);
    let tps = marcam::plan(&js[0].1).unwrap();
    let z = zs(&tps);
    assert_eq!(z.len(), 4);
    // the facing: from the stock's top as before, to -1
    assert!((top(&z[0]) + 0.5).abs() < 1e-9, "{}", top(&z[0]));
    // the first contour: its first pass cuts from -1 (the faced height) to -2; none above
    assert!((top(&z[1]) + 2.0).abs() < 1e-9, "{:?}", z[1]);
    assert!((z[1].iter().copied().fold(0.0, f64::min) + 4.0).abs() < 1e-9);
    // the pocket: from -1 too (the faced top round the boss)
    assert!(top(&z[2]) < -1.0 + 1e-9, "{}", top(&z[2]));
    // the second contour: inside the cleared area, from -4 down: -5, -6, -7
    assert!((top(&z[3]) + 5.0).abs() < 1e-9, "{:?}", top(&z[3]));
    assert!((z[3].iter().copied().fold(0.0, f64::min) + 7.0).abs() < 1e-9);
    // off: from the stock's top, as before
    let mut off = ops.clone();
    for o in off.as_array_mut().unwrap() {
        o["in_process"] = json!(false);
    }
    let z0 = zs(&marcam::plan(&jobs(&plate_with(off))[0].1).unwrap());
    assert!((top(&z0[1]) + 1.0).abs() < 1e-9 && (top(&z0[3]) + 1.0).abs() < 1e-9);
    // (and on, the same final depths with fewer passes)
    assert!(z[3].len() < z0[3].len());
}

#[test]
fn an_operation_alone_plans_as_before() {
    // the first operation of a setup (nothing cut before it): the same moves either way
    let on = marcam::plan(&jobs(&plate_with(json!([contour("c", 8.0, 3.0)])))[0].1).unwrap();
    let mut c = contour("c", 8.0, 3.0);
    c["in_process"] = json!(false);
    let off = marcam::plan(&jobs(&plate_with(json!([c])))[0].1).unwrap();
    assert_eq!(
        serde_json::to_string(&on[0].moves).unwrap(),
        serde_json::to_string(&off[0].moves).unwrap()
    );
}

#[test]
fn a_carried_block_is_faced_from_where_it_was_left() {
    // setup 1 faces the plate 0.5 down; setup 2 takes it (not turned) and faces it to -1: one
    // pass at -1, not one at -0.5 in the air first
    let mut v = plate_with(json!([face("f1", 0.5)]));
    let mut s2 = v["setups"][0].clone();
    s2["id"] = json!("vise2");
    s2["stock"] = json!({"shape": "from_previous"});
    s2["operations"] = json!([face("f2", 1.0)]);
    s2["outputs"] = json!([{"name": "two", "title": "two", "operations": ["f2"]}]);
    v["setups"] = json!([v["setups"][0], s2]);
    let mut js = jobs(&v);
    carry_stock(&mut js, &|_| None, None).unwrap();
    let z = zs(&marcam::plan(&js[1].1).unwrap());
    assert!((top(&z[0]) + 1.0).abs() < 1e-9, "{:?}", top(&z[0]));
}
