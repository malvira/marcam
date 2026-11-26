//! The per-operation plan cache: planning with it gives exactly the toolpaths planning from
//! scratch gives, and after an edit only the operations it touches are planned again.

use marcam::{plan_with_mesh, plan_with_mesh_cached, Job, Mesh, PlanCache};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every setup of every example, as jobs with their meshes.
fn examples() -> Vec<(String, Job, Option<Mesh>, PathBuf)> {
    let libs = marcam::profiles::builtin_libraries();
    let mut out = Vec::new();
    for dir in std::fs::read_dir(root().join("examples"))
        .unwrap()
        .flatten()
    {
        let d = dir.path();
        for f in std::fs::read_dir(&d).unwrap().flatten() {
            let p = f.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if !(name.ends_with(".marcam.toml") || name.ends_with(".job.json")) {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let jobs = marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&d).ok())
                .unwrap();
            for (id, job) in jobs {
                let mesh = marcam::load_mesh(&job, &d).unwrap();
                out.push((format!("{name}/{id}"), job, mesh, d.clone()));
            }
        }
    }
    out
}

fn with(job: &Job, f: impl FnOnce(&mut Value)) -> Job {
    let mut v = serde_json::to_value(job).unwrap();
    f(&mut v);
    serde_json::from_value(v).unwrap()
}

fn ops(job: &Job) -> usize {
    job.programs.iter().map(|p| p.operations.len()).sum()
}

#[test]
fn cached_plans_are_the_same_as_fresh_ones_and_only_edits_are_planned() {
    let all = examples();
    assert!(all.len() >= 6, "{}", all.len());
    for (name, job, mesh, _) in &all {
        let _: &Path;
        let fresh = plan_with_mesh(job, mesh.as_ref()).unwrap();
        let mut cache = PlanCache::new();
        let first = plan_with_mesh_cached(job, mesh.as_ref(), &mut cache).unwrap();
        assert_eq!(first, fresh, "{name}: first cached plan");
        assert_eq!(
            cache.take_counts(),
            (0, ops(job)),
            "{name}: everything planned the first time"
        );
        let again = plan_with_mesh_cached(job, mesh.as_ref(), &mut cache).unwrap();
        assert_eq!(again, fresh, "{name}: replayed");
        assert_eq!(
            cache.take_counts(),
            (ops(job), 0),
            "{name}: nothing planned again"
        );
        // the last operation's feed: that operation alone
        let (pi, oi) = (
            job.programs.len() - 1,
            job.programs.last().unwrap().operations.len() - 1,
        );
        let edited = with(job, |v| {
            let o = &mut v["programs"][pi]["operations"][oi];
            if let Some(f) = o["feed"].as_f64() {
                o["feed"] = serde_json::json!(f * 0.9);
            }
        });
        let fresh2 = plan_with_mesh(&edited, mesh.as_ref()).unwrap();
        let cached2 = plan_with_mesh_cached(&edited, mesh.as_ref(), &mut cache).unwrap();
        assert_eq!(cached2, fresh2, "{name}: after an edit");
        let (hits, misses) = cache.take_counts();
        assert!(
            misses <= 1 && hits == ops(job) - misses,
            "{name}: the last operation only ({hits}, {misses})"
        );
        // the first operation's feed: it, and (only if its end changed) those after it
        let edited = with(job, |v| {
            let o = &mut v["programs"][0]["operations"][0];
            if let Some(f) = o["feed"].as_f64() {
                o["feed"] = serde_json::json!(f * 0.9);
            }
        });
        let fresh3 = plan_with_mesh(&edited, mesh.as_ref()).unwrap();
        let cached3 = plan_with_mesh_cached(&edited, mesh.as_ref(), &mut cache).unwrap();
        assert_eq!(cached3, fresh3, "{name}: after editing the first operation");
        let (_, misses) = cache.take_counts();
        assert!(
            misses <= 1,
            "{name}: a feed does not move where the operation ends ({misses} planned)"
        );
        // the stock: everything again
        let edited = with(job, |v| {
            v["clearance"] = serde_json::json!(v["clearance"].as_f64().unwrap_or(3.0) + 1.0)
        });
        let fresh4 = plan_with_mesh(&edited, mesh.as_ref()).unwrap();
        let cached4 = plan_with_mesh_cached(&edited, mesh.as_ref(), &mut cache).unwrap();
        assert_eq!(cached4, fresh4, "{name}: after a clearance change");
        assert_eq!(
            cache.take_counts(),
            (0, ops(job)),
            "{name}: a job-wide change plans everything"
        );
    }
}

/// An operation refused as too fine to plan is refused again from the cache, at once: the
/// budget counts work, not time, so the same inputs give the same refusal (the browser plans
/// a project when it opens and again on Generate).
#[test]
fn a_refusal_is_cached_too() {
    let (_, job, mesh, _) = examples()
        .into_iter()
        .find(|(n, ..)| n.starts_with("plate.marcam.toml"))
        .unwrap();
    let fine = with(&job, |v| {
        for p in v["programs"].as_array_mut().unwrap() {
            for o in p["operations"].as_array_mut().unwrap() {
                if o["type"] == "raster3d" {
                    o["sample"] = 0.002.into();
                    o["stepover"] = 0.002.into();
                }
            }
        }
    });
    let mut cache = PlanCache::new();
    let t0 = std::time::Instant::now();
    let first = plan_with_mesh_cached(&fine, mesh.as_ref(), &mut cache)
        .unwrap_err()
        .to_string();
    let t1 = t0.elapsed();
    assert!(first.contains("too fine to plan"), "{first}");
    let t2 = std::time::Instant::now();
    let again = plan_with_mesh_cached(&fine, mesh.as_ref(), &mut cache)
        .unwrap_err()
        .to_string();
    assert_eq!(again, first);
    assert!(t2.elapsed() * 5 < t1, "{:?} then {:?}", t1, t2.elapsed());
}

/// (Mar: "changing a parameter in a later operation seems to force a regeneration of all the
/// toolpaths. Only the current toolpath and later toolpaths should get regenerated.") A hub
/// roughed at four angles, rest roughed on what that left, then finished: the finish edited,
/// it alone is planned; the roughing edited, it and the rest roughing (planned on what it
/// leaves) are, the finish (which does not depend on them) kept.
#[test]
fn an_edit_plans_that_operation_and_the_ones_on_its_stock_only() {
    let text = r#"
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
tool = "spiral-o-3.175x17"

[[tools]]
number = 2
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
tools = [1, 2]

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
type = "rough3d"
tool = 1
index_a = 0.0
x_range = [-30.0, 0.0]
y_range = [-14.0, 14.0]
stepover = 1.0
sample = 0.5
step_down = 2.0
floor = 4.0
leave_z = 0.3
leave_xy = 0.3
tolerance = 0.05
pattern = "offset"
index_count = 4
feed = 500.0
plunge_feed = 150.0

[[setups.operations]]
id = "op2"
type = "rotary_rough"
tool = 2
x_range = [-30.0, 0.0]
step_down = 1.0
step_x = 0.8
leave_radial = 0.3
sample_deg = 4.0
tolerance = 0.02
min_radius = 0.0
feed = 500.0
plunge_feed = 150.0
rest = true

[[setups.operations]]
id = "op3"
type = "rotary_finish"
tool = 2
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
operations = ["op1", "op2", "op3"]
"#;
    let dir = root().join("tests/fixtures");
    let libs = marcam::profiles::builtin_libraries();
    let load = |t: &str| {
        marcam::project::jobs_from_any_with(t, &libs, &|src| src.load(&dir).ok())
            .unwrap()
            .remove(0)
            .1
    };
    let job = load(text);
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    let mut cache = PlanCache::new();
    plan_with_mesh_cached(&job, mesh.as_ref(), &mut cache).unwrap();
    assert_eq!(cache.take_counts(), (0, 3));
    // the finish's stepover: it alone
    let finish = load(&text.replace("stepover_mm = 0.8", "stepover_mm = 0.6"));
    let cached = plan_with_mesh_cached(&finish, mesh.as_ref(), &mut cache).unwrap();
    assert_eq!(cached, plan_with_mesh(&finish, mesh.as_ref()).unwrap());
    assert_eq!(
        cache.take_counts(),
        (2, 1),
        "the finish edited: the two roughings kept"
    );
    // the rest roughing's step: the indexed roughing before it kept
    let rest = load(
        &text
            .replace("stepover_mm = 0.8", "stepover_mm = 0.6")
            .replace("step_x = 0.8", "step_x = 0.7"),
    );
    let cached = plan_with_mesh_cached(&rest, mesh.as_ref(), &mut cache).unwrap();
    assert_eq!(cached, plan_with_mesh(&rest, mesh.as_ref()).unwrap());
    let (hits, misses) = cache.take_counts();
    assert!(
        hits >= 1 && misses >= 1,
        "the rest roughing edited: the roughing before it kept ({hits}, {misses})"
    );
    // the roughing's floor: the rest roughing, planned on what it leaves, planned again too
    // (before this, kept from the cache on the old stock)
    let rough = load(
        &text
            .replace("stepover_mm = 0.8", "stepover_mm = 0.6")
            .replace("step_x = 0.8", "step_x = 0.7")
            .replace("floor = 4.0", "floor = 5.0"),
    );
    let cached = plan_with_mesh_cached(&rough, mesh.as_ref(), &mut cache).unwrap();
    assert_eq!(cached, plan_with_mesh(&rough, mesh.as_ref()).unwrap());
    let (_, misses) = cache.take_counts();
    assert!(
        misses >= 2,
        "the roughing edited: it and the rest roughing planned ({misses})"
    );
}
