//! Rotary deburring (4-axis simultaneous): the finned hub's fins' top edges chamfered with a
//! 90° V-bit along them, X and A together, the tip the depth below each edge. (Its bar just
//! over the fins: a deburr goes where the shank and holder clear the stock, and the whole bar
//! of a fresh setup stands over everything.)

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn job(op: &str) -> (marcam::Job, Option<marcam::Mesh>) {
    let text = format!(
        r#"
schema_version = "0.2"
name = "hub"

[[models]]
id = "part"
name = "finned_hub.stl"
kind = "mesh"
file = "finned_hub.stl"
axis = "x"
translate = [-30.0, 0.0, 0.0]

[[tools]]
number = 2
library = "makera-starter"
tool = "vbit-90-3.175"

[[setups]]
id = "setup1"
name = "Setup 1"
machine = "makera-z1-4axis"
kind = "rotary"
clearance = 3.0
part = ["part"]
spindle_rpm = 12000
tools = [2]

[setups.stock]
shape = "cylinder"
radius = 12.1
length = 40.0

[setups.holding]
kind = "rotary_chuck"
tailstock = false
chuck = 8.0

[[setups.operations]]
id = "op1"
type = "rotary_deburr"
tool = 2
{op}
feed = 300.0
plunge_feed = 100.0

[[setups.outputs]]
name = "hub"
title = "hub"
reset_a = false
operations = ["op1"]
"#
    );
    let dir = root().join("tests/fixtures");
    let libs = marcam::profiles::builtin_libraries();
    let job = marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&dir).ok())
        .unwrap()
        .remove(0)
        .1;
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    (job, mesh)
}

#[test]
fn the_fins_top_edges_are_followed_x_and_a_together_the_depth_below_them() {
    let (job, mesh) = job("x_range = [-30.0, 0.0]\ndepth = 0.3");
    let mesh_ref = mesh.as_ref().unwrap();
    let edges = marcam::strategy::deburr_edges(mesh_ref, -30.0, 0.0, 45.0);
    assert!(edges.len() >= 4, "{} edges", edges.len());
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    // runs along the edges: X and A together, each point the depth below an edge point
    let mut both = 0;
    let mut cuts = 0;
    let mut prev: Option<(f64, f64)> = None;
    let near_edge = |x: f64, r: f64, a: f64| {
        // the part-frame point under the tool at A, radius r + depth
        let t = (90.0 - a).to_radians();
        let p = [x, (r + 0.3) * t.cos(), (r + 0.3) * t.sin()];
        // (within 0.1 of an edge's segment)
        edges.iter().any(|ch| {
            ch.windows(2).any(|w| {
                let u = [w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]];
                let v = [p[0] - w[0][0], p[1] - w[0][1], p[2] - w[0][2]];
                let uu = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
                let t = if uu > 0.0 {
                    ((u[0] * v[0] + u[1] * v[1] + u[2] * v[2]) / uu).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let d = [v[0] - t * u[0], v[1] - t * u[1], v[2] - t * u[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 0.1
            })
        })
    };
    for m in &tps[0].moves {
        if let marcam::Move::Cut { x, r, a, .. } = *m {
            cuts += 1;
            assert!(
                near_edge(x, r, a),
                "a cut at x {x} r {r} A {a} not under an edge"
            );
            if let Some((x0, a0)) = prev {
                if (x - x0).abs() > 1e-4 && (a - a0).abs() > 1e-4 {
                    both += 1;
                }
            }
            prev = Some((x, a));
        }
    }
    assert!(
        cuts > 100 && both > cuts / 4,
        "{both} of {cuts} cuts move X and A together"
    );
    // the program verifies clean
    let m = marcam::Machine::makera_z1();
    let g = marcam::post(&tps[0], &m);
    let issues = marcam::verify::verify(&g, &marcam::limits_for(&tps[0], &m));
    assert!(
        issues.is_empty(),
        "{:?}",
        issues.iter().map(|i| i.to_string()).collect::<Vec<_>>()
    );
}

#[test]
fn a_range_without_edges_says_so() {
    // past the hub's front: nothing there
    let (job, mesh) = job("x_range = [5.0, 9.0]");
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    assert!(
        tps[0].notes.iter().any(|n| n.contains("no sharp edge")),
        "{:?}",
        tps[0].notes
    );
}

#[test]
fn on_the_whole_bar_it_goes_nowhere_and_says_so() {
    // the bar 14 round fins 12 high: the holder over any edge would meet it
    let (mut job, mesh) = job("x_range = [-30.0, 0.0]");
    job.stock.radius = 14.0;
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    assert!(
        tps[0].notes.iter().any(|n| n.contains("shank or holder")),
        "{:?}",
        tps[0].notes
    );
}
