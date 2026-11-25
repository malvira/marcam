//! The SolidCraft adapter without SolidCraft: the manifest it ships, and every entry run on
//! the knob fixture as `model.mesh` would hand it over, checked against the marcam CLI.

use marcam_solidcraft::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The knob fixture as a `model.mesh` result (positions and index triangles).
fn knob_mesh_result() -> Value {
    let bytes = std::fs::read(root().join("examples/knob/knob.stl")).unwrap();
    let mesh = marcam::Mesh::from_bytes(&bytes).unwrap();
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for t in &mesh.triangles {
        let base = positions.len();
        for p in t {
            positions.push(json!(p));
        }
        triangles.push(json!([base, base + 1, base + 2]));
    }
    json!({"body": "Knob", "component": "root", "frame": "component-local-mm",
           "positions": positions, "triangles": triangles})
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

#[test]
fn manifest_fits_solidcraft_rules() {
    let m = manifest();
    let text = serde_json::to_string(&m).unwrap();
    // SolidCraft refuses manifests over 16 KiB: plan takes any operation's fields as an
    // object, so a new operation costs its name, and the manifest stays well under
    assert!(
        text.len() <= 8 * 1024,
        "plugin.json is {} bytes",
        text.len()
    );
    assert!(valid_id(m["id"].as_str().unwrap()));
    assert_eq!(m["capabilities"], json!(["commands@0.1"]));
    let rt = &m["runtime"];
    assert_eq!(rt["commands"], json!(["model.mesh"]));
    assert_eq!(
        rt["permissions"]["files"],
        json!([{"name": "output", "read": false, "write": true}, {"name": "project", "read": true, "write": false}])
    );
    let entries = rt["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "plan",
            "describe",
            "plan-project",
            "rotary-rough",
            "rotary-finish",
            "helical-knurl",
            "flank-chamfer"
        ]
    );
    for e in entries {
        assert!(valid_id(e["name"].as_str().unwrap()));
        assert_eq!(e["effect"], "read");
        assert_eq!(e["input_schema"]["type"], "object");
    }
    // plan: every operation but turning from a profile and a custom toolpath (its own program)
    let plan = &entries[0]["input_schema"];
    let kinds: Vec<&str> = marcam::ops::registry()
        .iter()
        .map(|t| t.kind)
        .filter(|k| *k != "rotary_turning" && *k != "custom")
        .collect();
    assert_eq!(plan["properties"]["operation"]["enum"], json!(kinds));
    assert_eq!(plan["properties"]["bodies"]["type"], "array");
    assert_eq!(plan["properties"]["params"]["type"], "object");
    assert_eq!(plan["required"], json!(["bodies", "operation"]));
    // the operations' own entries: every field of theirs (but the tool, which the adapter
    // makes, and those left at marcam's defaults)
    for e in &entries[3..] {
        let kind = entry_kind(e["name"].as_str().unwrap()).unwrap();
        let s = &e["input_schema"];
        assert_eq!(s["required"], json!(["bodies"]));
        for p in marcam::ops::find(kind).unwrap().params {
            if p.name != "tool" && !marcam_solidcraft::LEFT_AT_DEFAULT.contains(&p.name) {
                assert!(s["properties"].get(p.name).is_some(), "{kind}.{}", p.name);
            }
        }
    }
    assert_eq!(entry_kind("rotary-turning"), None);
    assert_eq!(entry_kind("pocket"), None);
    assert_eq!(operation_kind("pocket"), Some("pocket"));
    assert_eq!(operation_kind("rotary-rough"), Some("rotary_rough"));
    assert_eq!(operation_kind("rotary_turning"), None);
}

#[test]
fn describe_lists_the_operations_and_their_fields() {
    let all = describe(&json!({})).unwrap();
    let ops = all["operations"].as_array().unwrap();
    assert_eq!(ops.len(), offered().len());
    let pocket = ops.iter().find(|o| o["operation"] == "pocket").unwrap();
    let fields: Vec<&str> = pocket["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"adaptive") && fields.contains(&"stepover") && !fields.contains(&"tool")
    );
    assert_eq!(pocket["own_entry"], Value::Null);
    let rough = ops
        .iter()
        .find(|o| o["operation"] == "rotary_rough")
        .unwrap();
    assert_eq!(rough["own_entry"], "rotary-rough");
    assert!(all["machines"].as_array().unwrap().len() >= 3);
    assert!(all["inputs"]["bodies"].is_object());
    let one = describe(&json!({"operation": "waterline"})).unwrap();
    assert_eq!(one["operations"].as_array().unwrap().len(), 1);
    assert!(
        matches!(describe(&json!({"operation": "nope"})), Err(Error::Input { field, .. }) if field == "operation")
    );
}

#[test]
fn bad_inputs_are_named() {
    let mesh = knob_mesh_result();
    let err = |params: Value| run_entry("rotary-rough", &params, &mesh).unwrap_err();
    assert!(
        matches!(mesh_request(&json!({})), Err(Error::Input { field, .. }) if field == "bodies")
    );
    assert!(
        matches!(mesh_request(&json!({"bodies": ["A"], "tolerance": -1})), Err(Error::Input { field, .. }) if field == "tolerance")
    );
    assert!(
        matches!(err(json!({"bodies": ["K"], "machine": "nope"})), Error::Input { field, .. } if field == "machine")
    );
    assert!(
        matches!(err(json!({"bodies": ["K"], "tool_kind": "drill"})), Error::Input { field, .. } if field == "tool_kind")
    );
    assert!(
        matches!(err(json!({"bodies": ["K"], "name": "../x"})), Error::Input { field, .. } if field == "name")
    );
    assert!(
        matches!(err(json!({"bodies": ["K"], "count": 3})), Error::Input { field, .. } if field == "count")
    );
    assert!(
        matches!(err(json!({"bodies": ["K"], "step_down": -2})), Error::Input { field, .. } if field == "rotary_rough")
    );
    assert!(run_entry("no-such-op", &json!({"bodies": ["K"]}), &mesh).is_err());
    let plan = |params: Value| run_entry("plan", &params, &mesh).unwrap_err();
    assert!(
        matches!(plan(json!({"bodies": ["K"]})), Error::Input { field, .. } if field == "operation")
    );
    assert!(
        matches!(plan(json!({"bodies": ["K"], "operation": "rotary_turning"})), Error::Input { field, .. } if field == "operation")
    );
    assert!(
        matches!(plan(json!({"bodies": ["K"], "operation": "pocket", "params": 3})), Error::Input { field, .. } if field == "params")
    );
    assert!(
        matches!(plan(json!({"bodies": ["K"], "operation": "pocket", "name": "a", "params": {"name": "b"}})), Error::Input { field, .. } if field == "name")
    );
    assert!(
        matches!(plan(json!({"bodies": ["K"], "operation": "pocket", "params": {"nope": 1}})), Error::Input { field, .. } if field == "nope")
    );
    assert!(
        stl_from_mesh_result(&json!({"positions": [[0, 0, 0]], "triangles": [[0, 1, 2]]})).is_err()
    );
}

#[test]
fn placement_and_defaults() {
    let mesh = knob_mesh_result();
    let stl = stl_from_mesh_result(&mesh).unwrap();
    let (p, name) = build_project("rotary-rough", &json!({"bodies": ["Knob"]}), &stl).unwrap();
    assert_eq!(name, "knob_rotary_rough");
    let model = &p["models"][0];
    assert_eq!(model["axis"], "x");
    // the knob fixture's front is already at X0
    assert!(model["translate"][0].as_f64().unwrap().abs() < 1e-6);
    let setup = &p["setups"][0];
    assert_eq!(setup["machine"], DEFAULT_MACHINE);
    // knob: largest radius 10 -> 10.5 mm bar; 21.7 mm long -> 37 mm
    assert_eq!(
        setup["stock"],
        json!({"shape": "cylinder", "radius": 10.5, "length": 37.0})
    );
    let op = &setup["operations"][0];
    assert_eq!(op["type"], "rotary_rough");
    assert_eq!(op["tool"], 1);
    // a field the user gives replaces the default
    let (p2, _) = build_project(
        "rotary-rough",
        &json!({"bodies": ["Knob"], "step_down": 0.3, "stock_radius": 12.5}),
        &stl,
    )
    .unwrap();
    assert_eq!(p2["setups"][0]["operations"][0]["step_down"], 0.3);
    assert_eq!(p2["setups"][0]["stock"]["radius"], 12.5);

    // a body modelled along Z is turned onto X and its front moved to X0
    let moved: Vec<Value> = mesh["positions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!([p[1], p[2], p[0].as_f64().unwrap() + 40.0]))
        .collect();
    let along_z = json!({"positions": moved, "triangles": mesh["triangles"]});
    let stl_z = stl_from_mesh_result(&along_z).unwrap();
    let (pz, _) = build_project(
        "rotary-rough",
        &json!({"bodies": ["Knob"], "axis": "Z"}),
        &stl_z,
    )
    .unwrap();
    assert_eq!(pz["models"][0]["axis"], "z");
    assert!((pz["models"][0]["translate"][0].as_f64().unwrap() + 40.0).abs() < 1e-4);
    assert_eq!(pz["setups"][0]["stock"], p["setups"][0]["stock"]);
}

/// `plan` plans every operation on the knob, and its G-code is what `marcam run` makes from
/// the files it wrote; an operation's own entry gives what `plan` does; `plan-project` plans
/// the project `plan` wrote, from its folder, to the same G-code.
#[test]
fn every_entry_matches_the_cli() {
    let mesh = knob_mesh_result();
    let cli = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    // the most recently built marcam CLI (check.sh builds it before the tests)
    let exe = [
        "release/marcam.exe",
        "release/marcam",
        "debug/marcam.exe",
        "debug/marcam",
    ]
    .iter()
    .map(|p| cli.join(p))
    .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
    .max()
    .map(|(_, p)| p);
    if exe.is_none() {
        eprintln!("no marcam CLI built: G-code not compared with `marcam run`");
    }
    let tmp = std::env::temp_dir().join(format!("marcam-solidcraft-{}", std::process::id()));
    for t in offered() {
        let entry = entry_name(t.kind);
        let out = run_entry(
            "plan",
            &json!({"bodies": ["Knob"], "operation": t.kind}),
            &mesh,
        )
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
        if let Some(kind) = entry_kind(&entry) {
            let own = run_entry(&entry, &json!({"bodies": ["Knob"]}), &mesh).unwrap();
            assert_eq!(own, out, "{entry}: its own entry and plan differ");
            assert_eq!(kind, t.kind);
        }
        let dir = tmp.join(&entry);
        std::fs::create_dir_all(&dir).unwrap();
        for f in &out.files {
            std::fs::write(dir.join(&f.path), &f.data).unwrap();
        }
        let nc: Vec<_> = out
            .files
            .iter()
            .filter(|f| f.path.ends_with(".nc") || f.path.ends_with(".nc.gz"))
            .collect();
        assert_eq!(nc.len(), 1, "{entry}");
        assert!(
            out.files.iter().all(|f| f.data.len() <= MAX_WRITE),
            "{entry}"
        );
        let text = if nc[0].path.ends_with(".gz") {
            let mut s = String::new();
            let mut z = flate2::read::GzDecoder::new(&nc[0].data[..]);
            std::io::Read::read_to_string(&mut z, &mut s).unwrap();
            assert!(
                s.len() > MAX_WRITE,
                "{entry}: only long programs are compressed"
            );
            s
        } else {
            String::from_utf8(nc[0].data.clone()).unwrap()
        };
        let nc_name = nc[0].path.trim_end_matches(".gz").to_string();
        // plan-project: the project plan wrote, read back from its folder
        let read = |p: &str| {
            std::fs::read(dir.join(p)).map_err(|e| Error::Input {
                field: "project".into(),
                reason: e.to_string(),
            })
        };
        let again = plan_project(
            &json!({"project": format!("knob_{}.marcam.toml", t.kind)}),
            &read,
        )
        .unwrap_or_else(|e| panic!("{entry}: plan-project: {e}"));
        let same = again.files.iter().find(|f| f.path == nc[0].path).unwrap();
        assert!(
            same.data == nc[0].data,
            "{entry}: plan-project's G-code differs from plan's"
        );
        assert_eq!(out.result["programs"][0]["file"], nc[0].path.as_str());
        assert!(text.lines().count() > 10, "{entry}: {text}");
        let programs = out.result["programs"].as_array().unwrap();
        assert_eq!(
            programs[0]["collisions"]["count"], 0,
            "{entry}: {}",
            out.result
        );
        assert!(
            programs[0]["issues"].as_array().unwrap().is_empty(),
            "{entry}: {}",
            out.result
        );
        let Some(exe) = &exe else { continue };
        let project = dir.join(format!("knob_{}.marcam.toml", t.kind));
        let cli_out = dir.join("cli");
        let status = std::process::Command::new(exe)
            .arg("run")
            .arg(&project)
            .arg("--out")
            .arg(&cli_out)
            .output()
            .unwrap();
        let from_cli = std::fs::read_to_string(cli_out.join(&nc_name)).unwrap_or_else(|e| {
            panic!("{entry}: {e}\n{}", String::from_utf8_lossy(&status.stdout))
        });
        assert!(
            from_cli == text,
            "{entry}: the plugin's G-code differs from marcam run's"
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn plan_project_reads_only_inside_its_folder() {
    let read = |_: &str| -> Result<Vec<u8>, Error> { Ok(Vec::new()) };
    for bad in [
        "",
        "/etc/x.toml",
        "../x.toml",
        "a/../../x.toml",
        "a\\b.toml",
    ] {
        assert!(
            matches!(plan_project(&json!({"project": bad}), &read), Err(Error::Input { field, .. }) if field == "project"),
            "{bad:?}"
        );
    }
    assert!(plan_project(&json!({}), &read).is_err());
}
