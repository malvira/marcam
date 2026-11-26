//! Overloaded operations: the simulation says which feed moves take more than
//! [`marcam::dexel::HEAVY`] times their tool's normal bite on the 3D stock (Mar's impeller-6:
//! its roughing stopped where its tool reaches, the ball finish after it took up to 5.7 times
//! its bite).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn job(text: &str) -> (marcam::Job, Option<marcam::Mesh>) {
    let dir = root().join("tests/fixtures");
    let libs = marcam::profiles::builtin_libraries();
    let job = marcam::project::jobs_from_any_with(text, &libs, &|src| src.load(&dir).ok())
        .unwrap()
        .remove(0)
        .1;
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    (job, mesh)
}

const HUB: &str = r#"
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
tool = "ball-3.175x22"

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

ROUGH

[[setups.operations]]
id = "op2"
type = "rotary_finish"
tool = 1
pattern = "along"
x_range = [-30.0, 0.0]
a_range = [0.0, 360.0]
stepover_mm = 0.8
sample_mm = 0.2
sample_deg = 3.0
tolerance = 0.01
min_radius = 0.0
feed = 500.0
plunge_feed = 150.0

[[setups.outputs]]
name = "hub"
title = "hub"
reset_a = false
operations = OPS
"#;

const ROUGH: &str = r#"
[[setups.operations]]
id = "op1"
type = "rotary_rough"
tool = 1
x_range = [-30.0, 0.0]
step_down = 1.0
step_x = 0.8
leave_radial = 0.3
sample_deg = 4.0
tolerance = 0.02
min_radius = 0.0
feed = 500.0
plunge_feed = 150.0
"#;

#[test]
fn a_finish_straight_into_the_bar_is_overloaded_one_after_roughing_is_not() {
    let sim = |text: String| {
        let (job, mesh) = job(&text);
        let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
        marcam::simulate_job(&job, &tps, mesh.as_ref(), 0.2, 180, 0.05).unwrap()
    };
    // the finish alone, on the whole bar: overloaded, said
    let bare = sim(HUB.replace("ROUGH", "").replace("OPS", r#"["op2"]"#));
    let h = &bare.programs[0].1.heavy;
    assert!(
        h.len() == 1 && h[0].moves > 50 && h[0].worst > 3.0 && h[0].tool == 1,
        "{h:?}"
    );
    // and the same said without the simulation (for the GPU's, which has no 3D replay)
    let (job, mesh) = job(&HUB.replace("ROUGH", "").replace("OPS", r#"["op2"]"#));
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    assert_eq!(&marcam::heavy_bites(&job, &tps)[0], h);
    // roughed first: neither operation over
    let roughed = sim(HUB
        .replace("ROUGH", ROUGH)
        .replace("OPS", r#"["op1", "op2"]"#));
    let h = &roughed.programs[0].1.heavy;
    assert!(
        h.iter().all(|h| h.op != 2),
        "the finish after roughing: {h:?}"
    );
}
