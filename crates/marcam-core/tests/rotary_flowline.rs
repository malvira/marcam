//! Rotary flowline on a finned hub (an impeller's blades, simplified): four fins twisting
//! round the axis along X. Its channels found between them, each finished with passes that
//! follow its twist (A and X together), without leaving it; clean of the part.

use marcam::job::Job;
use marcam::{Mesh, Move};
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
            // (each face wound to face out: the +v side the other way round)
            for (r0, r1, v0, v1) in [(6.0, 12.0, -1.0, -1.0), (12.0, 12.0, -1.0, 1.0)] {
                quad(
                    &mut t,
                    [c(x0, r0, v0), c(x0, r1, v1), c(x1, r1, v1), c(x1, r0, v0)],
                );
            }
            quad(
                &mut t,
                [
                    c(x1, 6.0, 1.0),
                    c(x1, 12.0, 1.0),
                    c(x0, 12.0, 1.0),
                    c(x0, 6.0, 1.0),
                ],
            );
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

#[test]
fn the_fins_channels_are_found_and_followed_round() {
    let mesh = finned();
    let j = job(json!({}));
    let tps = marcam::plan_with_mesh(&j, Some(&mesh)).unwrap();
    let g = marcam::gouge_check(&j, &tps, Some(&mesh)).unwrap();
    let deepest = g.iter().map(|r| r.max_depth).fold(0.0, f64::max);
    assert!(deepest < 0.01, "{deepest} mm into the part");
    // its moves: one plunge per channel (four), A and X together along them
    let moves = &tps[0].moves;
    let plunges = moves
        .iter()
        .filter(|m| matches!(m, Move::Plunge { .. }))
        .count();
    assert_eq!(plunges, 4, "plunges");
    let cuts: Vec<[f64; 3]> = moves
        .iter()
        .filter_map(|m| match *m {
            Move::Cut { x, r, a, .. } => Some([x, r, a]),
            _ => None,
        })
        .collect();
    let along = cuts
        .windows(2)
        // (one move along a pass: not from one channel's last cut to the next one's first)
        .filter(|w| (w[1][0] - w[0][0]).abs() > 0.1 && (w[1][0] - w[0][0]).abs() < 1.0)
        .collect::<Vec<_>>();
    assert!(along.len() > 500);
    // following the twist: along X, A turns with it (the tool's A is 90 - the part's angle,
    // so against it), as the fins do
    for w in &along {
        let rate = (w[1][2] - w[0][2]) / (w[1][0] - w[0][0]);
        assert!(
            (rate + TWIST).abs() < 0.6,
            "A turns {rate:.2} a mm along X, not {}: {:?}",
            -TWIST,
            w
        );
    }
    // on the hub between the fins, never on a fin's top
    assert!(
        cuts.iter().all(|c| c[1] < 7.05),
        "a cut at radius {:.2}",
        cuts.iter().map(|c| c[1]).fold(0.0, f64::max)
    );
}

#[test]
fn no_channel_where_there_are_no_walls() {
    // a plain cylinder: no walls, nothing to follow
    let mesh = finned();
    let hub = Mesh {
        triangles: mesh.triangles[..144].to_vec(),
        faces: Vec::new(),
    };
    let tps = marcam::plan_with_mesh(&job(json!({})), Some(&hub)).unwrap();
    assert!(!tps[0].moves.iter().any(|m| matches!(m, Move::Cut { .. })));
    assert!(
        tps[0].notes.iter().any(|n| n.contains("no channel")),
        "{:?}",
        tps[0].notes
    );
}

#[test]
fn sector_wide_passes_climb_the_fins_along_their_twist() {
    let mesh = finned();
    let j = job(json!({"span": "sector", "stepover_mm": 1.0}));
    let tps = marcam::plan_with_mesh(&j, Some(&mesh)).unwrap();
    let g = marcam::gouge_check(&j, &tps, Some(&mesh)).unwrap();
    let deepest = g.iter().map(|r| r.max_depth).fold(0.0, f64::max);
    assert!(deepest < 0.01, "{deepest} mm into the part");
    let cuts: Vec<[f64; 3]> = tps[0]
        .moves
        .iter()
        .filter_map(|m| match *m {
            Move::Cut { x, r, a, .. } => Some([x, r, a]),
            _ => None,
        })
        .collect();
    // over the fins' tops (radius 12) as well as the hub between them
    let top = cuts.iter().map(|c| c[1]).fold(0.0, f64::max);
    let low = cuts.iter().map(|c| c[1]).fold(f64::INFINITY, f64::min);
    assert!(top > 11.9 && low < 6.1, "radius {low:.2}..{top:.2}");
    // still one plunge a sector, along the twist
    let plunges = tps[0]
        .moves
        .iter()
        .filter(|m| matches!(m, Move::Plunge { .. }))
        .count();
    assert_eq!(plunges, 4);
    let along: Vec<_> = cuts
        .windows(2)
        .filter(|w| (w[1][0] - w[0][0]).abs() > 0.1 && (w[1][0] - w[0][0]).abs() < 1.0)
        .collect();
    let mean = along
        .iter()
        .map(|w| (w[1][2] - w[0][2]) / (w[1][0] - w[0][0]))
        .sum::<f64>()
        / along.len() as f64;
    assert!((mean + TWIST).abs() < 0.2, "A turns {mean:.2} a mm along X");
}
