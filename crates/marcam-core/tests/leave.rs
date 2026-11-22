//! (Mar) Stock to leave on every operation: the walls (across) and floors (up) a rough leaves
//! for the finish after it. Each operation's path offset by it; a rough with it and a finish
//! at 0 leave the same shape as the finish alone.

use marcam::job::Job;
use marcam::project::Project;
use marcam::sim3::Heightmap3;
use marcam::{Move, Toolpath};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The plate's vise setup with `ops` (one file); the dome kept as its model.
fn plate_with(ops: Value) -> Job {
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
    v["setups"][0]["outputs"] = json!([{"name": "all", "title": "all", "operations": ids}]);
    v["setups"] = json!([v["setups"][0]]);
    let libs = marcam::profiles::builtin_libraries();
    let dir = root().join("examples/plate");
    let mut js =
        marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(&dir).ok())
            .unwrap();
    js.remove(0).1
}

fn plan(ops: Value) -> (Job, Vec<Toolpath>) {
    let j = plate_with(ops);
    let dir = root().join("examples/plate");
    let mesh = marcam::load_mesh(&j, &dir).unwrap();
    let tps = marcam::plan_with_mesh(&j, mesh.as_ref()).unwrap();
    (j, tps)
}

/// Each operation's feed moves (x, y, z), in planning order.
fn cuts(tps: &[Toolpath]) -> Vec<Vec<[f64; 3]>> {
    let mut out: Vec<Vec<[f64; 3]>> = Vec::new();
    for m in tps.iter().flat_map(|t| &t.moves) {
        match m {
            Move::Operation { .. } => out.push(Vec::new()),
            Move::Line3 { x, y, z, .. } => out.last_mut().unwrap().push([*x, *y, *z]),
            _ => {}
        }
    }
    out
}

fn lowest(c: &[[f64; 3]]) -> f64 {
    c.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min)
}

const TOOL_R: f64 = 3.175 / 2.0;

fn circle(cx: f64, cy: f64, r: f64) -> Vec<[f64; 2]> {
    (0..72)
        .map(|k| {
            let a = k as f64 * std::f64::consts::TAU / 72.0;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

fn op(id: &str, o: Value) -> Value {
    let mut v = json!({"id": id, "tool": 1, "feed": 300.0, "plunge_feed": 60.0});
    for (k, x) in o.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn facing_leaves_its_floor() {
    let face = |lz: f64| {
        op(
            "f",
            json!({"type": "face", "depth": 1.0, "step_down": 0.5, "stepover": 0.6, "leave_z": lz}),
        )
    };
    let (_, a) = plan(json!([face(0.0)]));
    let (_, b) = plan(json!([face(0.3)]));
    assert!((lowest(&cuts(&a)[0]) + 1.0).abs() < 1e-9);
    assert!(
        (lowest(&cuts(&b)[0]) + 0.7).abs() < 1e-9,
        "{}",
        lowest(&cuts(&b)[0])
    );
}

#[test]
fn a_pocket_leaves_its_walls_and_floor() {
    let pocket = |lxy: f64, lz: f64| {
        op(
            "p",
            json!({"type": "pocket", "rect": [10.0, 10.0, 30.0, 25.0], "depth": 3.0, "step_down": 1.0, "stepover": 0.5, "leave_xy": lxy, "leave_z": lz}),
        )
    };
    let ext = |c: &[[f64; 3]]| {
        let xs = c.iter().map(|p| p[0]);
        (
            xs.clone().fold(f64::INFINITY, f64::min),
            xs.fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let (_, a) = plan(json!([pocket(0.0, 0.0)]));
    let (_, b) = plan(json!([pocket(0.4, 0.25)]));
    let (ca, cb) = (&cuts(&a)[0], &cuts(&b)[0]);
    let ((a0, a1), (b0, b1)) = (ext(ca), ext(cb));
    assert!(
        (a0 - (10.0 + TOOL_R)).abs() < 1e-6 && (a1 - (30.0 - TOOL_R)).abs() < 1e-6,
        "{a0} {a1}"
    );
    assert!(
        (b0 - a0 - 0.4).abs() < 1e-6 && (a1 - b1 - 0.4).abs() < 1e-6,
        "{b0} {b1}"
    );
    assert!((lowest(ca) + 3.0).abs() < 1e-9 && (lowest(cb) + 2.75).abs() < 1e-9);
}

/// The farthest and nearest of the moves from (cx, cy).
fn radii(c: &[[f64; 3]], cx: f64, cy: f64, z: f64) -> (f64, f64) {
    let rs: Vec<f64> = c
        .iter()
        .filter(|p| (p[2] - z).abs() < 1e-9)
        .map(|p| (p[0] - cx).hypot(p[1] - cy))
        .collect();
    (
        rs.iter().copied().fold(f64::INFINITY, f64::min),
        rs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    )
}

#[test]
fn a_contour_leaves_its_wall_on_the_waste_side_and_its_floor() {
    let contour = |side: &str, lxy: f64, lz: f64| {
        op(
            "c",
            json!({"type": "contour", "path": circle(30.0, 20.0, 8.0), "side": side, "depth": 3.0, "step_down": 1.0, "leave_xy": lxy, "leave_z": lz}),
        )
    };
    // outside (a boss): the wall left outside it
    let (_, a) = plan(json!([contour("outside", 0.0, 0.0)]));
    let (_, b) = plan(json!([contour("outside", 0.5, 0.2)]));
    let (ra, _) = radii(&cuts(&a)[0], 30.0, 20.0, -3.0);
    let (rb, _) = radii(&cuts(&b)[0], 30.0, 20.0, -2.8);
    assert!((rb - ra - 0.5).abs() < 0.02, "{ra} {rb}");
    assert!((lowest(&cuts(&b)[0]) + 2.8).abs() < 1e-9);
    // inside (a hole): left inside it, a smaller hole
    let (_, c) = plan(json!([contour("inside", 0.0, 0.0)]));
    let (_, d) = plan(json!([contour("inside", 0.5, 0.0)]));
    let (_, rc) = radii(&cuts(&c)[0], 30.0, 20.0, -3.0);
    let (_, rd) = radii(&cuts(&d)[0], 30.0, 20.0, -3.0);
    assert!((rc - rd - 0.5).abs() < 0.02, "{rc} {rd}");
    // negative: past the wall (a cut 0.3 into the boss)
    let (_, e) = plan(json!([contour("outside", -0.3, 0.0)]));
    let (re, _) = radii(&cuts(&e)[0], 30.0, 20.0, -3.0);
    assert!((ra - re - 0.3).abs() < 0.02, "{ra} {re}");
}

#[test]
fn drilling_and_boring_leave_their_bottom_and_wall() {
    let drill = |lz: f64| {
        op(
            "d",
            json!({"type": "drill", "points": [[20.0, 20.0]], "depth": 5.0, "peck": 2.0, "retract": 1.0, "leave_z": lz}),
        )
    };
    let (_, a) = plan(json!([drill(0.5)]));
    assert!((lowest(&cuts(&a)[0]) + 4.5).abs() < 1e-9);
    let bore = |lxy: f64, lz: f64| {
        op(
            "b",
            json!({"type": "bore", "points": [[20.0, 20.0]], "diameter": 8.0, "depth": 4.0, "helix_pitch": 0.5, "stepover": 0.4, "finish_allowance": 0.1, "spring_passes": 0, "leave_xy": lxy, "leave_z": lz}),
        )
    };
    let (_, b0) = plan(json!([bore(0.0, 0.0)]));
    let (_, b1) = plan(json!([bore(0.3, 0.4)]));
    let far = |c: &[[f64; 3]]| {
        c.iter()
            .map(|p| (p[0] - 20.0).hypot(p[1] - 20.0))
            .fold(0.0, f64::max)
    };
    let (f0, f1) = (far(&cuts(&b0)[0]), far(&cuts(&b1)[0]));
    assert!((f0 - f1 - 0.3).abs() < 0.02, "{f0} {f1}");
    assert!(
        (lowest(&cuts(&b1)[0]) + 3.6).abs() < 1e-6,
        "{}",
        lowest(&cuts(&b1)[0])
    );
}

#[test]
fn a_3d_finish_leaves_its_surface_stock_up() {
    let raster = |lz: f64| {
        op(
            "r",
            json!({"type": "raster3d", "tool": 3, "x_range": [15.0, 45.0], "y_range": [10.0, 30.0], "stepover": 1.0, "sample": 0.5, "step_down": 10.0, "floor": -6.0, "leave_z": lz}),
        )
    };
    let (_, a) = plan(json!([raster(0.0)]));
    let (_, b) = plan(json!([raster(0.3)]));
    let (ca, cb) = (&cuts(&a)[0], &cuts(&b)[0]);
    // the same passes, 0.3 higher (above the floor)
    assert_eq!(ca.len(), cb.len());
    let n = ca
        .iter()
        .zip(cb)
        .filter(|(p, q)| {
            p[0] == q[0] && p[1] == q[1] && p[2] > -5.9 && (q[2] - p[2] - 0.3).abs() < 1e-6
        })
        .count();
    assert!(n * 2 > ca.len(), "{n} of {}", ca.len());
    // (an old project's `leave` read as the floors')
    let old: marcam::job::Raster3d = serde_json::from_value(json!({"tool": 3, "x_range": [0.0, 1.0], "y_range": [0.0, 1.0], "sample": 0.5, "step_down": 1.0, "floor": 0.0, "leave": 0.25, "feed": 1.0, "plunge_feed": 1.0})).unwrap();
    assert_eq!(old.leave_z, 0.25);
}

fn sim(j: &Job, tps: &[Toolpath]) -> Heightmap3 {
    let sb = j.stock_box.unwrap();
    let mut hm = Heightmap3::of_job(j, &sb, 0.1);
    let cutters = |t: u32| j.tool(t).and_then(|d| marcam::cutter_for(t, d.shape).ok());
    for tp in tps {
        marcam::sim3::simulate3(&mut hm, tp, &cutters, 0.05);
    }
    hm
}

#[test]
fn a_rough_with_stock_left_and_a_finish_leave_the_finished_shape() {
    // a boss r 8: roughed leaving 0.5 on its wall and 0.3 on the floor, then finished at 0:
    // the same shape as the finish alone, and the finish only takes the stock left
    let contour = |id: &str, lxy: f64, lz: f64| {
        op(
            id,
            json!({"type": "contour", "path": circle(30.0, 20.0, 8.0), "side": "outside", "depth": 3.0, "step_down": 1.0, "leave_xy": lxy, "leave_z": lz}),
        )
    };
    let (j2, both) = plan(json!([contour("r", 0.5, 0.3), contour("f", 0.0, 0.0)]));
    let (j1, alone) = plan(json!([contour("f", 0.0, 0.0)]));
    let (h2, h1) = (sim(&j2, &both), sim(&j1, &alone));
    // the part (the boss, and the floor round it the finish cuts) the same; the waste the
    // rough took further out is not the part
    let mut worst: f64 = 0.0;
    for j in 0..h1.ny {
        for i in 0..h1.nx {
            let (x, y) = h1.centre(i, j);
            let r = (x - 30.0).hypot(y - 20.0);
            if r < 7.9 || (8.15..10.5).contains(&r) {
                worst = worst.max((h2.z[j * h2.nx + i] - h1.z[j * h1.nx + i]).abs());
            }
        }
    }
    assert!(worst < 0.02, "{worst}");
    // (and that is the boss r 8 at the top, the floor at -3 round it)
    let at = |x: f64, y: f64| {
        h2.z[((y - h2.y0) / h2.cell) as usize * h2.nx + ((x - h2.x0) / h2.cell) as usize]
    };
    assert!(
        at(30.0, 20.0).abs() < 1e-9 && (at(39.5, 20.0) + 3.0).abs() < 0.02,
        "{} {}",
        at(30.0, 20.0),
        at(39.5, 20.0)
    );
    // the finish: from the rough's floor (in-process stock), not the stock's top
    let c = cuts(&both);
    let top = c[1].iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max);
    assert!(top < 0.0, "{top}");
}
