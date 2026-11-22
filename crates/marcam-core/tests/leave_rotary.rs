//! (Mar) Stock to leave on the rotary operations: radially (the bar's radius) and axially
//! (the faces across it, along X). The knob's turning, knurl and flank chamfer, rough and
//! finish on its mesh, and parting.

use marcam::job::Job;
use marcam::{Move, Toolpath};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The knob's job with its program `prog`'s operation `k` changed by `f`.
fn knob(f: impl Fn(&mut Value)) -> Job {
    let text = std::fs::read_to_string(root().join("examples/knob/knob.job.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    f(&mut v);
    Job::from_json(&v.to_string()).unwrap()
}

/// Each operation's coordinated cuts (x, r), in planning order.
fn cuts(tps: &[Toolpath]) -> Vec<Vec<[f64; 2]>> {
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    for m in tps.iter().flat_map(|t| &t.moves) {
        match m {
            Move::Operation { .. } => out.push(Vec::new()),
            Move::Cut { x, r, .. } => out.last_mut().unwrap().push([*x, *r]),
            _ => {}
        }
    }
    out
}

fn min_r(c: &[[f64; 2]]) -> f64 {
    c.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)
}

/// The smallest radius cut at about `x` (within 0.05).
fn r_at(c: &[[f64; 2]], x: f64) -> f64 {
    c.iter()
        .filter(|p| (p[0] - x).abs() < 0.05)
        .map(|p| p[1])
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn turning_leaves_its_radius_and_its_faces() {
    let base = cuts(&marcam::plan(&knob(|_| {})).unwrap());
    let with = |k: &str, l: f64| {
        cuts(
            &marcam::plan(&knob(|v| {
                v["programs"][0]["operations"][0][k] = json!(l);
            }))
            .unwrap(),
        )
    };
    // radially: 0.3 more everywhere on the profile (the body at X -8)
    let radial = with("leave_radial", 0.3);
    let (a, b) = (r_at(&base[0], -8.0), r_at(&radial[0], -8.0));
    assert!((b - a - 0.3).abs() < 0.02, "{a} {b}");
    // axially: the shoulder at X -15 (r 5 -> 10) further from it: just past it, the tool does
    // not go down as far as without
    let axial = with("leave_axial", 0.4);
    // (the tool's edge 0.2 short of the face without; past where the face is left with)
    let x = -15.0 - 3.175 / 2.0 - 0.2;
    let near = |c: &[[f64; 2]]| {
        c.iter()
            .filter(|p| (p[0] - x).abs() < 0.1)
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min)
    };
    let (a, b) = (near(&base[0]), near(&axial[0]));
    assert!(a < 5.5 && b > 9.5, "{a} {b}");
}

#[test]
fn the_knurl_and_the_flank_chamfer_leave_their_radius() {
    let base = cuts(&marcam::plan(&knob(|_| {})).unwrap());
    let left = cuts(
        &marcam::plan(&knob(|v| {
            v["programs"][1]["operations"][0]["leave_radial"] = json!(0.2);
            v["programs"][1]["operations"][1]["leave_radial"] = json!(0.2);
        }))
        .unwrap(),
    );
    for k in [1, 2] {
        let (a, b) = (min_r(&base[k]), min_r(&left[k]));
        assert!((b - a - 0.2).abs() < 0.02, "op {k}: {a} {b}");
    }
}

#[test]
fn old_rotary_leaves_read_as_radial() {
    let f: marcam::job::RotaryFinish = serde_json::from_value(json!({"tool": 1, "pattern": "around", "x_range": [-10.0, 0.0], "leave": 0.2, "feed": 1.0, "plunge_feed": 1.0})).unwrap();
    let r: marcam::job::RotaryRough = serde_json::from_value(json!({"tool": 1, "x_range": [-10.0, 0.0], "step_down": 1.0, "step_x": 1.0, "allowance": 0.3, "feed": 1.0, "plunge_feed": 1.0})).unwrap();
    assert_eq!((f.leave_radial, r.leave_radial), (0.2, 0.3));
}

#[test]
fn parting_leaves_the_part_s_face() {
    let part = |la: f64| {
        knob(|v| {
            v["programs"] = json!([{"name": "part", "title": "part", "operations": [{"type": "parting", "tool": 1, "x": -25.0, "stop_radius": 3.0, "depth_per_rev": 0.2, "feed": 200.0, "plunge_feed": 60.0, "leave_axial": la}]}]);
        })
    };
    let xs = |j: &Job| {
        let c = cuts(&marcam::plan(j).unwrap());
        c[0].iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max)
    };
    let (a, b) = (xs(&part(0.0)), xs(&part(0.5)));
    assert!((a - b - 0.5).abs() < 1e-6, "{a} {b}");
}

#[test]
fn a_turning_rough_with_stock_left_and_a_finish_leave_the_profile() {
    // the knob turned leaving 0.3 radially and 0.4 axially, then turned at 0: the same bar as
    // the finish alone (each X ring's largest radius over the part)
    let two = knob(|v| {
        let mut rough = v["programs"][0]["operations"][0].clone();
        rough["leave_radial"] = json!(0.3);
        rough["leave_axial"] = json!(0.4);
        let fin = v["programs"][0]["operations"][0].clone();
        v["programs"] = json!([{"name": "t", "title": "t", "operations": [rough, fin]}]);
    });
    let one = knob(|v| {
        let fin = v["programs"][0]["operations"][0].clone();
        v["programs"] = json!([{"name": "t", "title": "t", "operations": [fin]}]);
    });
    let bar = |j: &Job| {
        let grid = marcam::carry::bar_grid(&j.stock);
        let mut map = marcam::sim::RadialMap::of_job(grid, j);
        let cutters = |t: u32| j.tool(t).and_then(|d| marcam::cutter_for(t, d.shape).ok());
        for tp in &marcam::plan(j).unwrap() {
            marcam::sim::simulate(&mut map, tp, &cutters, 0.2);
        }
        map
    };
    let (a, b) = (bar(&two), bar(&one));
    let g = a.grid;
    let mut worst: f64 = 0.0;
    for i in 0..g.nx {
        let x = g.x0 + (i as f64 + 0.5) * g.dx;
        if !(-21.0..-0.2).contains(&x) {
            continue;
        }
        let top = |m: &marcam::sim::RadialMap| {
            m.r[i * g.na..(i + 1) * g.na]
                .iter()
                .fold(0.0f32, |s, &r| s.max(r)) as f64
        };
        worst = worst.max((top(&a) - top(&b)).abs());
    }
    assert!(worst < 0.02, "{worst}");
}
