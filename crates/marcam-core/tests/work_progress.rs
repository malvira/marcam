//! Progress from work done: a long job (the simulation, the overload check, the stock in 3D)
//! tells how far it has got as per mille of its moves and rows, not a time estimate (Mar's
//! impeller sat at 97% for minutes): from 0, never back, to 1000 at the end. Its own test
//! binary: the phase hook is global.

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
operations = ["op2"]
"#;

static SAID: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

fn told(what: &str) {
    if let Some(pm) = what
        .strip_prefix("[work ")
        .and_then(|w| w.strip_suffix("/1000]"))
    {
        SAID.lock().unwrap().push(pm.parse().unwrap());
    }
}

/// What was told since the last call.
fn taken() -> Vec<u32> {
    std::mem::take(&mut *SAID.lock().unwrap())
}

fn steady(what: &str, said: &[u32]) {
    assert!(
        said.first() == Some(&0) && said.last() == Some(&1000),
        "{what}: from 0 to 1000: {:?}",
        &said[..said.len().min(5)]
    );
    assert!(said.windows(2).all(|w| w[0] < w[1]), "{what}: never back");
    // (steadily: many steps, none more than a tenth)
    let big = said.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    assert!(
        said.len() > 50 && big <= 100,
        "{what}: {} steps, the biggest {big}",
        said.len()
    );
}

#[test]
fn long_jobs_tell_their_work_done_from_0_to_1000() {
    marcam::set_phase_hook(told);
    let (job, mesh) = job(HUB);
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    taken();
    marcam::simulate_job(&job, &tps, mesh.as_ref(), 0.2, 180, 0.05).unwrap();
    steady("the simulation", &taken());
    marcam::heavy_bites(&job, &tps);
    steady("the overload check", &taken());
    marcam::left3d::stock_surface(&job, &tps, mesh.as_ref(), 0.4);
    steady("the stock in 3D", &taken());
}
