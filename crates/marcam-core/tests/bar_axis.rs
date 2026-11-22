//! (Mar) A bar in a vise along X, Y or standing up (Z): the block round it, the stock the
//! simulation starts from, the jaws' grip, and a flat faced on a bar lying down.

use marcam::job::Job;
use marcam::project::Project;
use marcam::sim3::Heightmap3;
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The plate's vise setup holding a round bar Ø20 x 50 along `along`, with `ops`.
fn bar(along: Option<&str>, ops: Value) -> Job {
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    let ids: Vec<Value> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].clone())
        .collect();
    let s = &mut v["setups"][0];
    s["stock"] = json!({"shape": "cylinder", "radius": 10.0, "length": 50.0});
    s["holding"] = json!({"kind": "vise", "tailstock": false});
    if let Some(a) = along {
        s["holding"]["along"] = json!(a);
    }
    s["part"] = json!([]);
    s["operations"] = ops;
    s["outputs"] = json!([{"name": "all", "title": "all", "operations": ids}]);
    v["setups"] = json!([v["setups"][0]]);
    let libs = marcam::profiles::builtin_libraries();
    let mut js = marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|_| None).unwrap();
    js.remove(0).1
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn the_block_round_the_bar_follows_its_axis() {
    let x = bar(None, json!([]));
    let y = bar(Some("y"), json!([]));
    let z = bar(Some("z"), json!([]));
    let sb = |j: &Job| j.stock_box.unwrap();
    assert!(
        close(sb(&x).max[0], 50.0) && close(sb(&x).max[1], 20.0) && close(sb(&x).min[2], -20.0)
    );
    assert!(
        close(sb(&y).max[0], 20.0) && close(sb(&y).max[1], 50.0) && close(sb(&y).min[2], -20.0)
    );
    assert!(
        close(sb(&z).max[0], 20.0) && close(sb(&z).max[1], 20.0) && close(sb(&z).min[2], -50.0)
    );
    // (along X: as before, nothing new written)
    assert_eq!(x.stock_along, None);
    assert_eq!(z.stock_along.as_deref(), Some("z"));
    // the axis: along Y at (X 10, Z -10); standing at (X 10, Y 10)
    let ay = y.stock_axis.unwrap();
    let az = z.stock_axis.unwrap();
    assert!(
        (ay[0] - 10.0).abs() < 1e-3 && (ay[1] + 10.0).abs() < 1e-3,
        "{ay:?}"
    );
    assert!(
        (az[0] - 10.0).abs() < 1e-3 && (az[1] - 10.0).abs() < 1e-3,
        "{az:?}"
    );
    // the jaws: 40% of a bar lying down (8 of 20); standing, as a block's (3 mm)
    assert!(
        close(x.jaws.unwrap(), 8.0) && close(y.jaws.unwrap(), 8.0) && close(z.jaws.unwrap(), 3.0)
    );
}

#[test]
fn the_stock_simulated_is_the_bar_each_way() {
    let cell = 0.25;
    // standing: full height inside the circle, nothing outside: pi r^2 L
    let z = bar(Some("z"), json!([]));
    let sb = z.stock_box.unwrap();
    let hm = Heightmap3::of_job(&z, &sb, cell);
    let vol: f64 = hm.z.iter().map(|t| t - hm.bottom).sum::<f64>() * cell * cell;
    let want = std::f64::consts::PI * 100.0 * 50.0;
    assert!((vol - want).abs() < 0.02 * want, "{vol} vs {want}");
    let at = |h: &Heightmap3, x: f64, y: f64| {
        h.z[((y - h.y0) / h.cell) as usize * h.nx + ((x - h.x0) / h.cell) as usize]
    };
    assert!(close(at(&hm, 10.0, 10.0), 0.0) && close(at(&hm, 0.5, 0.5), -50.0));
    // along Y: each column along X at the circle's top (0 over the axis, -4 at 8 from it)
    let y = bar(Some("y"), json!([]));
    let sb = y.stock_box.unwrap();
    let hm = Heightmap3::of_job(&y, &sb, cell);
    for yy in [5.0, 25.0, 45.0] {
        assert!(at(&hm, 10.1, yy).abs() < 0.02, "{}", at(&hm, 10.1, yy));
        // (at the cell's centre: X 18.125)
        let want = (100.0f64 - 8.125 * 8.125).sqrt() - 10.0;
        assert!(
            (at(&hm, 18.1, yy) - want).abs() < 0.01,
            "{}",
            at(&hm, 18.1, yy)
        );
    }
}

#[test]
fn a_flat_faced_on_a_bar_lying_along_y() {
    // faced 2 down: a flat 12 wide (2 sqrt(100 - 64)) along the bar, at -2
    let j = bar(
        Some("y"),
        json!([{"id": "f", "type": "face", "tool": 1, "depth": 2.0, "step_down": 1.0, "stepover": 0.5, "feed": 300.0, "plunge_feed": 60.0}]),
    );
    let sb = j.stock_box.unwrap();
    let tps = marcam::plan(&j).unwrap();
    let cell = 0.1;
    let mut hm = Heightmap3::of_job(&j, &sb, cell);
    let cutters = |t: u32| j.tool(t).and_then(|d| marcam::cutter_for(t, d.shape).ok());
    for tp in &tps {
        marcam::sim3::simulate3(&mut hm, tp, &cutters, 0.1);
    }
    let at = |x: f64, y: f64| {
        hm.z[((y - hm.y0) / hm.cell) as usize * hm.nx + ((x - hm.x0) / hm.cell) as usize]
    };
    for yy in [5.0, 25.0, 45.0] {
        // the flat, its edges at 6 from the axis; the round below them untouched
        assert!((at(10.05, yy) + 2.0).abs() < 0.01, "{}", at(10.05, yy));
        assert!((at(15.0, yy) + 2.0).abs() < 0.01);
        let r = (100.0f64 - 7.55 * 7.55).sqrt() - 10.0;
        assert!((at(17.55, yy) - r).abs() < 0.05, "{} vs {r}", at(17.55, yy));
    }
}
