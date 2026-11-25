//! (Mar's knob3) A vise's 2.5D operation cutting into the part is said so (a GOUGE warning
//! naming where and how deep, shown in the app as an error); one along the part's walls is not.

use marcam::project::Project;
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The plate's vise setup with its own operations, or `ops` instead.
fn plate_notes(ops: Option<Value>) -> Vec<String> {
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    if let Some(ops) = ops {
        let ids: Vec<Value> = ops
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].clone())
            .collect();
        v["setups"][0]["operations"] = ops;
        v["setups"][0]["outputs"] = json!([{"name": "all", "title": "all", "operations": ids}]);
    }
    v["setups"] = json!([v["setups"][0]]);
    let libs = marcam::profiles::builtin_libraries();
    let dir = root().join("examples/plate");
    let mut js =
        marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(&dir).ok())
            .unwrap();
    let j = js.remove(0).1;
    let mesh = marcam::load_mesh(&j, &dir).unwrap();
    let tps = marcam::plan_with_mesh(&j, mesh.as_ref()).unwrap();
    tps.iter().flat_map(|t| t.notes.clone()).collect()
}

#[test]
fn a_contour_sunk_into_the_part_is_said_so_the_plates_own_are_not() {
    let own = plate_notes(None);
    assert!(!own.iter().any(|n| n.contains("GOUGE")), "{own:?}");
    // the dome's middle: a small outline round it, 3 mm down, outside: through the dome
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let libs = marcam::profiles::builtin_libraries();
    let dir = root().join("examples/plate");
    let j =
        marcam::project::jobs_from_any_with(&serde_json::to_string(&p).unwrap(), &libs, &|src| {
            src.load(&dir).ok()
        })
        .unwrap()
        .remove(0)
        .1;
    let (lo, hi) = marcam::load_mesh(&j, &dir)
        .unwrap()
        .unwrap()
        .bounds()
        .unwrap();
    let (cx, cy) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    let path = json!([
        [cx - 1.0, cy - 1.0],
        [cx + 1.0, cy - 1.0],
        [cx + 1.0, cy + 1.0],
        [cx - 1.0, cy + 1.0]
    ]);
    let notes = plate_notes(Some(
        json!([{"id": "op9", "type": "contour", "tool": 1, "path": path, "side": "outside", "depth": 3.0, "step_down": 1.0, "feed": 400.0, "plunge_feed": 100.0}]),
    ));
    let g = notes
        .iter()
        .find(|n| n.contains("GOUGE"))
        .unwrap_or_else(|| panic!("{notes:?}"));
    assert!(
        g.contains("into the part") && g.contains("boundary offset"),
        "{g}"
    );
}
