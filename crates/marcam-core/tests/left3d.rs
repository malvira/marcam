//! (copied from rotary_flowline.rs) Rotary flowline on a finned hub (an impeller's blades, simplified): four fins twisting
//! round the axis along X. Its channels found between them, each finished with passes that
//! follow its twist (A and X together), without leaving it; clean of the part.

use marcam::job::Job;
use marcam::Mesh;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The fins' twist (degrees per mm along X).
const TWIST: f64 = 2.0;

fn quad(t: &mut Vec<[[f64; 3]; 3]>, p: [[f64; 3]; 4]) {
    t.push([p[0], p[1], p[2]]);
    t.push([p[0], p[2], p[3]]);
}

/// A hub of radius 6 from X -30 to 0, and four fins out to radius 12, 2 mm thick, twisting.
fn finned() -> Mesh {
    let mut t = Vec::new();
    let n = 72;
    let at = |x: f64, r: f64, th: f64| [x, r * th.cos(), r * th.sin()];
    for i in 0..n {
        let (a0, a1) = (
            i as f64 * std::f64::consts::TAU / n as f64,
            (i + 1) as f64 * std::f64::consts::TAU / n as f64,
        );
        quad(
            &mut t,
            [
                at(-30.0, 6.0, a0),
                at(-30.0, 6.0, a1),
                at(0.0, 6.0, a1),
                at(0.0, 6.0, a0),
            ],
        );
    }
    for k in 0..4 {
        for j in 0..60 {
            let (x0, x1) = (-30.0 + j as f64 * 0.5, -30.0 + (j + 1) as f64 * 0.5);
            let th = |x: f64| (90.0 * k as f64 + TWIST * (x + 30.0)).to_radians();
            // a fin's corner: radius r, v across its thickness, at X x
            let c = |x: f64, r: f64, v: f64| {
                let a = th(x);
                [x, r * a.cos() - v * a.sin(), r * a.sin() + v * a.cos()]
            };
            for (r0, r1, v0, v1) in [
                (6.0, 12.0, 1.0, 1.0),
                (6.0, 12.0, -1.0, -1.0),
                (12.0, 12.0, -1.0, 1.0),
            ] {
                quad(
                    &mut t,
                    [c(x0, r0, v0), c(x0, r1, v1), c(x1, r1, v1), c(x1, r0, v0)],
                );
            }
        }
    }
    Mesh {
        triangles: t,
        faces: Vec::new(),
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[allow(dead_code)]
fn job(extra: Value) -> Job {
    let text = std::fs::read_to_string(root().join("examples/knob/knob.job.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["stock"] = json!({"radius": 13.0, "length": 40.0});
    v["tools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"number": 3, "description": "3.175 ball",
        "kind": "ball", "diameter": 3.175, "flute_length": 12.0}));
    let mut op = json!({"type": "rotary_flowline", "tool": 3, "x_range": [-28.0, -2.0],
        "stepover_mm": 0.5, "feed": 600.0, "plunge_feed": 100.0, "smoothing": 0.0});
    for (k, x) in extra.as_object().unwrap() {
        op[k] = x.clone();
    }
    v["programs"] = json!([{"name": "p", "title": "p", "operations": [op]}]);
    Job::from_json(&v.to_string()).unwrap()
}

/// The finned hub with the program `ops` (each with its type, the ball T3, feeds).
fn job_ops(ops: Value) -> Job {
    let text = std::fs::read_to_string(root().join("examples/knob/knob.job.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["stock"] = json!({"radius": 13.0, "length": 40.0});
    v["tools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"number": 3, "description": "3.175 ball",
        "kind": "ball", "diameter": 3.175, "flute_length": 12.0}));
    v["programs"] = json!([{"name": "p", "title": "p", "operations": ops}]);
    Job::from_json(&v.to_string()).unwrap()
}

/// Plan `ops` on `mesh` (clean of it) and measure what is left in 3D.
fn measure(mesh: &Mesh, ops: Value) -> marcam::left3d::Left3d {
    let j = job_ops(ops);
    let tps = marcam::plan_with_mesh(&j, Some(mesh)).unwrap();
    let g = marcam::gouge_check(&j, &tps, Some(mesh)).unwrap();
    assert!(g.iter().all(|r| r.max_depth < 0.01), "gouged");
    marcam::left3d::left_in_3d(&j, &tps, mesh, 0.1, 3.175 / 2.0)
}

fn finish(leave: f64) -> Value {
    json!({"type": "rotary_finish", "tool": 3, "pattern": "along",
        "x_range": [-30.5, 0.5], "stepover_deg": 3.0, "sample_mm": 0.25, "leave_radial": leave,
        "feed": 600.0, "plunge_feed": 100.0, "smoothing": 0.0})
}

#[test]
fn the_measure_reads_the_stock_a_finish_leaves_on_a_plain_hub() {
    // the hub alone (a cylinder of radius 6): finished leaving 0.3, it reads about 0.3; leaving
    // nothing, about nothing (within the 0.1 mm grid)
    let mesh = finned();
    let hub = Mesh {
        triangles: mesh.triangles[..144].to_vec(),
        faces: Vec::new(),
    };
    let left = measure(&hub, json!([finish(0.3)]));
    eprintln!("leave 0.3: {:?}", (left.max_left, left.over, left.inside));
    assert!((0.25..=0.4).contains(&left.max_left), "{left:?}");
    assert!(
        left.over[2] as f64 > 0.5 * left.points as f64,
        "most of it over 0.2: {left:?}"
    );
    let none = measure(&hub, json!([finish(0.0)]));
    eprintln!("leave 0: {:?}", (none.max_left, none.over, none.inside));
    assert!(none.max_left <= 0.1, "{none:?}");
}

#[test]
fn a_radial_finish_leaves_the_fins_faces_and_the_wall_finish_takes_it() {
    let mesh = finned();
    let wall = json!({"type": "rotary_wall", "tool": 3, "x_range": [-30.5, 0.5],
        "step_down": 0.3, "feed": 600.0, "plunge_feed": 100.0, "smoothing": 0.0});
    let radial = measure(&mesh, json!([finish(0.0)]));
    eprintln!(
        "radial only: {:?}",
        (
            radial.max_left,
            radial.max_at,
            radial.over,
            radial.unreachable,
            radial.inside
        )
    );
    let both = measure(&mesh, json!([finish(0.0), wall]));
    eprintln!(
        "with the walls: {:?}",
        (
            both.max_left,
            both.max_at,
            both.over,
            both.unreachable,
            both.inside
        )
    );
    assert!(radial.max_left > 0.1, "{radial:?}");
    assert!(both.max_left <= 0.05 + 0.03, "{both:?}");
}
