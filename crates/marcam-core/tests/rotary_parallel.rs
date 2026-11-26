//! Rotary parallel (simultaneous 4-axis finishing) on the knob's profile: its three styles,
//! line (passes along X, stepping round), circular (rings round the axis, stepping along X)
//! and spiral (X and A together, a full turn at each end); clean of the part, and what they
//! cut turning seen by the 3D stock's replay.

use marcam::dexel::{replay, Dexel, ToolSolid};
use marcam::job::Job;
use marcam::{Move, Toolpath};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The knob with one program: a rotary parallel in `style` over X -20..-5 (its body), with
/// `extra` set on it.
fn knob(style: &str, extra: Value) -> Job {
    let text = std::fs::read_to_string(root().join("examples/knob/knob.job.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    let mut op = json!({"type": "rotary_parallel", "tool": 1, "pattern": style,
        "x_range": [-20.0, -5.0], "feed": 600.0, "plunge_feed": 100.0, "smoothing": 0.0});
    for (k, x) in extra.as_object().unwrap() {
        op[k] = x.clone();
    }
    v["programs"] = json!([{"name": "p", "title": "p", "operations": [op]}]);
    Job::from_json(&v.to_string()).unwrap()
}

/// Where the tool feeds through (x, r, a), in order: where each plunge lands and each cut.
fn cuts(tps: &[Toolpath]) -> Vec<[f64; 3]> {
    let (mut x, mut a) = (0.0, 0.0);
    let mut out = Vec::new();
    for m in tps.iter().flat_map(|t| &t.moves) {
        match *m {
            Move::Rapid { x: nx, a: na, .. } => {
                x = nx.unwrap_or(x);
                a = na.unwrap_or(a);
            }
            Move::Plunge { r, .. } => out.push([x, r, a]),
            Move::Cut {
                x: nx, r, a: na, ..
            } => {
                (x, a) = (nx, na);
                out.push([x, r, a]);
            }
            _ => {}
        }
    }
    out
}

fn clean(job: &Job, tps: &[Toolpath]) {
    let g = marcam::gouge_check(job, tps, None).unwrap();
    let deepest = g.iter().map(|r| r.max_depth).fold(0.0, f64::max);
    assert!(g.iter().any(|r| r.moves_checked > 0), "nothing checked");
    assert!(deepest < 0.01, "{deepest} mm into the part");
}

#[test]
fn circular_rings_round_the_axis_a_stepover_apart_along_x() {
    let job = knob("circular", json!({"stepover_mm": 1.0}));
    let tps = marcam::plan(&job).unwrap();
    clean(&job, &tps);
    let c = cuts(&tps);
    // each ring: a full turn at one X
    let mut rings: Vec<(f64, f64, f64)> = Vec::new(); // (x, a from, a to)
    for w in c.windows(2) {
        let ([x0, _, a0], [x1, _, a1]) = (w[0], w[1]);
        if (x1 - x0).abs() < 1e-9 && (a1 - a0).abs() > 1e-9 {
            match rings.last_mut() {
                Some(r) if (r.0 - x1).abs() < 1e-9 => r.2 = a1,
                _ => rings.push((x1, a0, a1)),
            }
        }
    }
    assert_eq!(rings.len(), 16, "{rings:?}");
    for (i, r) in rings.iter().enumerate() {
        assert!(
            (r.0 - (-20.0 + i as f64)).abs() < 1e-6,
            "ring {i} at X {}",
            r.0
        );
        assert!(
            (r.2 - r.1).abs() >= 360.0 - 1e-6,
            "ring {i}: {}..{}",
            r.1,
            r.2
        );
    }
    // a limited range: back and forth across it
    let job = knob(
        "circular",
        json!({"stepover_mm": 1.0, "a_range": [30.0, 120.0]}),
    );
    let tps = marcam::plan(&job).unwrap();
    clean(&job, &tps);
    let c = cuts(&tps);
    let (lo, hi) = c
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p[2]), hi.max(p[2]))
        });
    assert!(lo >= 30.0 - 1e-6 && hi <= 120.0 + 1e-6, "A {lo}..{hi}");
}

#[test]
fn spiral_moves_x_and_a_together_with_a_full_turn_at_each_end() {
    let job = knob("spiral", json!({"stepover_mm": 0.5, "clean_walls": false}));
    let tps = marcam::plan(&job).unwrap();
    clean(&job, &tps);
    let c = cuts(&tps);
    let both = c
        .windows(2)
        .filter(|w| (w[1][0] - w[0][0]).abs() > 1e-9 && (w[1][2] - w[0][2]).abs() > 1e-9)
        .count();
    assert!(both > 100, "{both} moves with X and A together");
    // 15 mm at 0.5 a turn: 30 turns, and one at each end
    let turned = c.last().unwrap()[2] - c.first().unwrap()[2];
    assert!((turned - 32.0 * 360.0).abs() < 10.0, "{turned} degrees");
    let at_end = |x: f64| {
        let a: Vec<f64> = c
            .iter()
            .filter(|p| (p[0] - x).abs() < 1e-9)
            .map(|p| p[2])
            .collect();
        a.iter().fold(f64::NEG_INFINITY, |m, &v| m.max(v))
            - a.iter().fold(f64::INFINITY, |m, &v| m.min(v))
    };
    assert!(at_end(-5.0) >= 359.0 && at_end(-20.0) >= 359.0);
}

#[test]
fn line_passes_along_x_at_fixed_angles() {
    let job = knob("line", json!({"stepover_deg": 30.0}));
    let tps = marcam::plan(&job).unwrap();
    clean(&job, &tps);
    let c = cuts(&tps);
    let mut angles: Vec<f64> = c.iter().map(|p| (p[2] * 1e6).round() / 1e6).collect();
    angles.sort_by(|a, b| a.total_cmp(b));
    angles.dedup();
    for k in 0..12 {
        let a = 30.0 * k as f64;
        assert!(
            angles.iter().any(|&v| (v - a).abs() < 1e-6),
            "no pass at {a}"
        );
    }
}

#[test]
fn what_the_rings_cut_turning_is_taken_from_the_3d_stock() {
    // the knob's bar, rings 1 mm inside it: the stock is cut by the turning moves alone
    let job = knob(
        "circular",
        json!({"stepover_mm": 1.0, "leave_radial": 0.0, "min_radius": 11.5}),
    );
    let tps = marcam::plan(&job).unwrap();
    let mut dx = Dexel::bar(job.stock.radius, job.stock.length, 0.2);
    let v0 = dx.volume();
    let tool = ToolSolid {
        radius: 1.5875,
        ball: false,
        height: 12.0,
        cone: 0.0,
    };
    let turning: Vec<_> = marcam::sim::segments(&tps)
        .into_iter()
        .filter(|s| !s.rapid && s.from.is_some_and(|f| (f[0] - s.to[0]).abs() < 1e-9))
        .collect();
    assert!(!turning.is_empty());
    let r = replay(&mut dx, &turning, &|_| Some(tool), 0.25, 0.0);
    let taken = v0 - dx.volume();
    // 16 rings of a 3.175 wide, 1 mm deep groove at radius 12.5: about 16 x 3.175 x 75 mm2
    assert!(taken > 1000.0, "{taken:.0} mm3 taken turning");
    assert!(r.rapid_hits.is_empty());
}
