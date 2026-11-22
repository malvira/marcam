//! Cutting data by tool × operation type × stock material: the most specific preset wins
//! (this operation in this material, then any operation in it, then this operation in any
//! material, then any), the project's own before the library's, and the computed defaults
//! when nothing fits. Changing a setup's material updates only the values not typed over.

use marcam::profiles::{self, pick_preset, Preset, ToolLibrary};
use marcam::project::{Project, ToolEntry};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn plate() -> Project {
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    Project::from_json_any(&text).unwrap().0
}

fn preset(op: Option<&str>, material: &str, feed: f64) -> Preset {
    Preset {
        material: material.into(),
        op: op.map(str::to_string),
        rpm: 12000.0,
        feed,
        plunge: 0.0,
        stepdown: None,
        stepover: None,
        ramp_angle: None,
        leave_xy: None,
        leave_z: None,
    }
}

/// The starter library, with T1 (the 3.175 mm metal end mill) also given pocket data in
/// aluminium: 450 mm/min, 0.4 mm a pass, 1 mm stepover.
fn libs() -> Vec<ToolLibrary> {
    let mut lib = profiles::builtin_libraries()
        .into_iter()
        .find(|l| l.id == "makera-starter")
        .unwrap();
    let t = lib
        .tools
        .iter_mut()
        .find(|t| t.id == "spiral-o-metal-3.175x12")
        .unwrap();
    t.presets.push(Preset {
        op: Some("pocket".into()),
        plunge: 150.0,
        stepdown: Some(0.4),
        stepover: Some(1.0),
        ..preset(None, "Aluminum", 450.0)
    });
    // and T3 (the 3.175 mm metal ball) raster data: 0.2 mm rows, 3° ramps
    let b = lib
        .tools
        .iter_mut()
        .find(|t| t.id == "ball-metal-3.175x10")
        .unwrap();
    b.presets.push(Preset {
        stepover: Some(0.2),
        ramp_angle: Some(3.0),
        ..preset(Some("raster3d"), "aluminium", 420.0)
    });
    vec![lib]
}

fn defaults(p: &Project, kind: &str, tool: u32) -> Value {
    p.op_defaults(kind, "vise1", Some(tool), &[], &libs(), None, &|_| None)
        .unwrap()
}

fn with_material(m: Option<&str>) -> Project {
    let mut p = plate();
    p.setups[0].material = m.map(str::to_string);
    p
}

#[test]
fn the_most_specific_preset_wins() {
    let generic = preset(None, "", 100.0);
    let op_any = preset(Some("pocket"), "", 200.0);
    let mat = preset(None, "aluminium", 300.0);
    let exact = preset(Some("pocket"), "aluminium", 400.0);
    let all = [&generic, &op_any, &mat, &exact];
    let feed = |ps: &[&Preset], op: &str, m: Option<&str>| pick_preset(ps, op, m).map(|p| p.feed);
    assert_eq!(feed(&all, "pocket", Some("aluminium")), Some(400.0));
    assert_eq!(
        feed(&all, "face", Some("aluminium")),
        Some(300.0),
        "any operation in the material"
    );
    assert_eq!(
        feed(&all, "pocket", Some("brass")),
        Some(200.0),
        "the operation in any material"
    );
    assert_eq!(feed(&all, "face", Some("brass")), Some(100.0));
    assert_eq!(
        feed(&all, "pocket", None),
        Some(200.0),
        "no material: material-less data only"
    );
    assert_eq!(feed(&[&mat, &exact], "pocket", None), None);
    // names compare loosely
    assert_eq!(feed(&[&exact], "pocket", Some(" Aluminum ")), Some(400.0));
    // equally specific: the first (the project's own go first)
    let other = preset(Some("pocket"), "aluminium", 999.0);
    assert_eq!(
        feed(&[&other, &exact], "pocket", Some("aluminium")),
        Some(999.0)
    );
}

#[test]
fn new_operations_take_the_tools_cutting_data_for_the_setups_material() {
    // no material: the computed defaults (0.025 mm/tooth at 12000 rpm)
    let none = defaults(&with_material(None), "pocket", 1);
    assert_eq!(none["feed"], json!(300.0));
    // aluminium, pocket: the tool × operation × material data
    let al = with_material(Some("aluminium"));
    let pocket = defaults(&al, "pocket", 1);
    assert_eq!(pocket["feed"], json!(450.0));
    assert_eq!(pocket["plunge_feed"], json!(150.0));
    assert_eq!(pocket["step_down"], json!(0.4));
    assert_eq!(pocket["stepover"], json!(0.31), "1 mm of a 3.175 mm tool");
    assert!(
        pocket.get("ramp_angle").is_none(),
        "a pocket takes no ramp angle"
    );
    // a raster: stepover in mm, the ramp angle
    let raster = defaults(&al, "raster3d", 3);
    assert_eq!(raster["stepover"], json!(0.2));
    assert_eq!(raster["ramp_angle"], json!(3.0));
    assert_eq!(raster["feed"], json!(420.0));
    // aluminium, face: the tool's data for any operation in aluminium (the library's)
    let face = defaults(&al, "face", 1);
    assert_eq!(face["feed"], json!(500.0));
    assert_eq!(face["plunge_feed"], json!(200.0));
    assert_eq!(
        face["step_down"],
        none_face()["step_down"],
        "not given: computed"
    );
    // a material the tool has no data for: computed
    assert_eq!(
        defaults(&with_material(Some("hardwood")), "pocket", 1)["feed"],
        json!(300.0)
    );
    // the feeds follow the setup's spindle speed (same chip load)
    let mut slow = with_material(Some("aluminium"));
    slow.setups[0].spindle_rpm = 6000;
    assert_eq!(defaults(&slow, "pocket", 1)["feed"], json!(225.0));
    // the project's own data comes first
    let mut own = with_material(Some("aluminium"));
    if let ToolEntry::Library { cutting, .. } = &mut own.tools[0] {
        cutting.push(preset(Some("pocket"), "aluminium", 400.0));
    }
    assert_eq!(defaults(&own, "pocket", 1)["feed"], json!(400.0));
    assert_eq!(
        defaults(&own, "face", 1)["feed"],
        json!(500.0),
        "its pocket data only"
    );
    // the result is a valid operation
    for v in [&pocket, &face, &raster] {
        let op: marcam::job::Operation = serde_json::from_value(v.clone()).unwrap();
        let _ = op;
    }
}

fn none_face() -> Value {
    defaults(&with_material(None), "face", 1)
}

#[test]
fn a_material_change_updates_only_untouched_values() {
    let mut p = with_material(Some("hardwood"));
    // two pockets with T1, made with the hardwood defaults; the second one's feed typed over
    let ops = &mut p.setups[0].operations;
    let at = ops.iter().position(|o| o.op.tool() == 1).unwrap();
    let template = ops[at].clone();
    let mk = |id: &str, feed: Option<f64>| {
        let mut v = p_defaults_hardwood();
        if let Some(f) = feed {
            v["feed"] = json!(f);
        }
        let mut o = template.clone();
        o.id = id.into();
        o.op = serde_json::from_value(v).unwrap();
        o
    };
    ops.push(mk("auto", None));
    ops.push(mk("typed", Some(777.0)));
    let changed = p
        .set_material("vise1", Some("aluminium".into()), &libs(), None)
        .unwrap();
    assert_eq!(p.setups[0].material.as_deref(), Some("aluminium"));
    let get = |p: &Project, id: &str| {
        serde_json::to_value(
            &p.setups[0]
                .operations
                .iter()
                .find(|o| o.id == id)
                .unwrap()
                .op,
        )
        .unwrap()
    };
    let auto = get(&p, "auto");
    assert_eq!(auto["feed"], json!(450.0), "untouched: the new material's");
    assert_eq!(auto["step_down"], json!(0.4));
    assert_eq!(auto["stepover"], json!(0.31));
    let typed = get(&p, "typed");
    assert_eq!(typed["feed"], json!(777.0), "typed: kept");
    assert_eq!(
        typed["step_down"],
        json!(0.4),
        "its other values: the new material's"
    );
    assert!(changed.contains(&"auto: feed".to_string()), "{changed:?}");
    assert!(!changed.contains(&"typed: feed".to_string()), "{changed:?}");
    // and back: the computed values again, the typed one still kept
    p.set_material("vise1", None, &libs(), None).unwrap();
    assert_eq!(p.setups[0].material, None);
    assert_eq!(get(&p, "auto")["feed"], json!(300.0));
    assert_eq!(get(&p, "typed")["feed"], json!(777.0));
    // the project still plans
    assert!(
        p.check_operations().is_empty(),
        "{:?}",
        p.check_operations()
    );
    p.validate().unwrap();
}

fn p_defaults_hardwood() -> Value {
    defaults(&with_material(Some("hardwood")), "pocket", 1)
}

#[test]
fn material_and_cutting_data_round_trip_through_toml() {
    let mut p = with_material(Some("aluminium"));
    if let ToolEntry::Library { cutting, .. } = &mut p.tools[0] {
        cutting.push(Preset {
            stepdown: Some(0.3),
            ramp_angle: Some(2.0),
            ..preset(Some("contour"), "aluminium", 380.0)
        });
    }
    let text = marcam::project_file::project_to_toml(&p).unwrap();
    assert!(text.contains("material = \"aluminium\""), "{text}");
    let back = Project::from_json_any(&text).unwrap().0;
    assert_eq!(back, p);
    // a plain project writes neither
    let plain = marcam::project_file::project_to_toml(&plate()).unwrap();
    assert!(!plain.contains("material") && !plain.contains("cutting"));
    // a library with operation data: JSON round trip, and it is checked
    let lib = &libs()[0];
    let json = serde_json::to_string(lib).unwrap();
    assert_eq!(&ToolLibrary::from_json(&json).unwrap(), lib);
    let mut bad: Value = serde_json::from_str(&json).unwrap();
    bad["tools"][0]["presets"][0]["op"] = json!("no_such_op");
    assert!(ToolLibrary::from_json(&bad.to_string()).is_err());
}
