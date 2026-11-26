//! Rotary pocket roughing (4-axis simultaneous): the finned hub's channels between its fins
//! cleared level by level in radius, each level adaptively (a steady load), X and A together.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn job(op: &str) -> (marcam::Job, Option<marcam::Mesh>) {
    job_with(op, "", r#"["op1"]"#)
}

fn job_with(op: &str, extra: &str, ops: &str) -> (marcam::Job, Option<marcam::Mesh>) {
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
number = 1
library = "makera-starter"
tool = "spiral-o-metal-3.175x12"

[[setups]]
id = "setup1"
name = "Setup 1"
machine = "makera-z1-4axis"
kind = "rotary"
clearance = 3.0
part = ["part"]
spindle_rpm = 12000
tools = [1]

[setups.stock]
shape = "cylinder"
radius = 14.0
length = 40.0

[setups.holding]
kind = "rotary_chuck"
tailstock = false
chuck = 8.0

[[setups.operations]]
id = "op1"
type = "rotary_pocket"
tool = 1
{op}
feed = 500.0
plunge_feed = 150.0

{extra}

[[setups.outputs]]
name = "hub"
title = "hub"
reset_a = false
operations = {ops}
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
fn the_hubs_channels_are_cleared_level_by_level_x_and_a_together() {
    let (job, mesh) = job(r#"x_range = [-28.0, -2.0]
a_range = [0.0, 360.0]
step_down = 1.5
stepover = 0.15
leave_radial = 0.3"#);
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    // cuts that move X and A together (simultaneous), at a constant radius on each level
    let mut both = 0;
    let mut cuts = 0;
    let mut prev: Option<(f64, f64, f64)> = None;
    for m in &tps[0].moves {
        if let marcam::Move::Cut { x, r, a, .. } = *m {
            if let Some((x0, r0, a0)) = prev {
                cuts += 1;
                if (x - x0).abs() > 1e-3 && (a - a0).abs() > 1e-3 && (r - r0).abs() < 1e-9 {
                    both += 1;
                }
            }
            prev = Some((x, r, a));
        }
    }
    assert!(
        cuts > 500 && both > cuts / 4,
        "{both} of {cuts} cuts move X and A together"
    );
    // simulated: clean (no gouge, no rapid through the stock), the channels taken down to the
    // part plus what is left, at a steady load (nothing over twice the tool's normal bite)
    let sim = marcam::simulate_job(&job, &tps, mesh.as_ref(), 0.2, 180, 0.05).unwrap();
    let (st, dev) = (&sim.programs[0].1, &sim.programs[0].2);
    assert!(dev.max_gouge < 0.05, "gouge {}", dev.max_gouge);
    assert!(
        st.rapid_hits.is_empty(),
        "rapids through the stock {:?}",
        st.rapid_hits
    );
    assert!(st.heavy.is_empty(), "overloaded: {:?}", st.heavy);
    assert!(
        dev.mean_left < 1.5,
        "left: mean {} max {}",
        dev.mean_left,
        dev.max_left
    );
}

#[test]
fn a_part_of_the_way_round_only_there() {
    let (job, mesh) = job(r#"x_range = [-28.0, -2.0]
a_range = [30.0, 120.0]
step_down = 1.5"#);
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    for m in &tps[0].moves {
        if let marcam::Move::Cut { a, .. } = *m {
            assert!((30.0 - 1e-6..=120.0 + 1e-6).contains(&a), "A {a}");
        }
    }
}

#[test]
fn rest_cuts_only_what_the_operations_before_left() {
    // a rotary roughing first (levels 1.5 mm apart, 0.8 left): the pocket's rest takes what
    // it left, far fewer moves than from the whole bar, clean
    let rough = r#"
[[setups.operations]]
id = "op0"
type = "rotary_rough"
tool = 1
x_range = [-28.0, -2.0]
step_down = 1.5
step_x = 1.0
leave_radial = 0.8
sample_deg = 4.0
tolerance = 0.02
min_radius = 0.0
feed = 500.0
plunge_feed = 150.0"#;
    let pocket = |rest: bool| {
        format!(
            "x_range = [-28.0, -2.0]
step_down = 1.5
leave_radial = 0.3
rest = {rest}"
        )
    };
    let moves = |rest: bool| {
        let (job, mesh) = job_with(&pocket(rest), rough, r#"["op0", "op1"]"#);
        let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
        let n = tps[0]
            .moves
            .iter()
            .skip_while(|m| !matches!(m, marcam::Move::Operation { id: 1, .. }))
            .filter(|m| matches!(m, marcam::Move::Cut { .. }))
            .count();
        (job, mesh, tps, n)
    };
    let (_, _, _, whole) = moves(false);
    let (job, mesh, tps, rest) = moves(true);
    assert!(
        rest > 0 && rest * 2 < whole,
        "rest {rest} cuts, from the bar {whole}"
    );
    let sim = marcam::simulate_job(&job, &tps, mesh.as_ref(), 0.2, 180, 0.05).unwrap();
    let (st, dev) = (&sim.programs[0].1, &sim.programs[0].2);
    assert!(
        dev.max_gouge < 0.05 && st.rapid_hits.is_empty(),
        "{} {:?}",
        dev.max_gouge,
        st.rapid_hits
    );
}
