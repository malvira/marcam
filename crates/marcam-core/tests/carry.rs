//! Stock carried from one setup to the next: the plate faced and pocketed on its top, turned
//! over about Y for its bottom, and back again; the knob turned, then re-chucked end for end.
//! Each setup starts from what the one before left, turned and moved, and its part turns
//! with it.

use marcam::carry::{carry_stock, Carried};
use marcam::job::{Job, Turn};
use marcam::project::Project;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The plate, then the same block turned over (about Y), then turned back.
fn plate_three_setups() -> Value {
    let text = std::fs::read_to_string(root().join("examples/plate/plate.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    let s1 = v["setups"][0].clone();
    // setup 1: the face and the pocket (20 x 30, 3 deep from -0.5) only
    v["setups"][0]["operations"] = json!(s1["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["type"] == "face" || o["type"] == "pocket")
        .cloned()
        .collect::<Vec<_>>());
    let ids: Vec<Value> = v["setups"][0]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].clone())
        .collect();
    v["setups"][0]["outputs"] = json!([{"name": "top", "title": "top", "operations": ids}]);
    // setup 2: over, the bottom faced (no operation that reaches the pocket)
    let mut s2 = s1.clone();
    s2["id"] = json!("vise2");
    s2["name"] = json!("Vise, bottom side");
    s2["stock"] = json!({"shape": "from_previous", "turn": "y180"});
    s2["operations"] = json!([{"id": "b1", "type": "face", "tool": 1, "depth": 0.5, "step_down": 0.5, "stepover": 0.6, "feed": 300.0, "plunge_feed": 60.0}]);
    s2["outputs"] = json!([{"name": "bottom", "title": "bottom", "operations": ["b1"]}]);
    // setup 3: back over (nothing cut)
    let mut s3 = s2.clone();
    s3["id"] = json!("vise3");
    s3["operations"] = json!([]);
    s3["outputs"] = json!([]);
    v["setups"] = json!([v["setups"][0], s2, s3]);
    v
}

fn jobs(v: &Value, dir: &Path) -> Vec<(String, Job)> {
    let libs = marcam::profiles::builtin_libraries();
    marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(dir).ok()).unwrap()
}

#[test]
fn the_plate_turned_over_shows_its_pocket_underneath_and_back_on_top() {
    let dir = root().join("examples/plate");
    let v = plate_three_setups();
    let mut js = jobs(&v, &dir);
    assert_eq!(js.len(), 3);
    // the block re-zeroed where it was: X 0..60, Y 0..40, its top at Z0
    let sb2 = js[1].1.stock_box.unwrap();
    assert_eq!((sb2.min, sb2.max), ([0.0, 0.0, -10.0], [60.0, 40.0, 0.0]));
    let c = js[1].1.carry.clone().unwrap();
    assert_eq!(
        (c.from.as_str(), c.turn, c.shift),
        ("vise1", Turn::Y180, [60.0, 0.0, -10.0])
    );
    // the part turned and moved the same: X mirrored about 30, upside down
    let m1 = marcam::load_mesh(&js[0].1, &dir).unwrap().unwrap();
    let m2 = marcam::load_mesh(&js[1].1, &dir).unwrap().unwrap();
    let (lo1, hi1) = m1.bounds().unwrap();
    let (lo2, hi2) = m2.bounds().unwrap();
    assert!((lo2[0] - (60.0 - hi1[0])).abs() < 1e-6 && (hi2[2] - (-10.0 - lo1[2])).abs() < 1e-6);
    assert!((lo2[1] - lo1[1]).abs() < 1e-6);
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    // setup 2 starts on the old bottom: flat at Z0 all over; its bottom has the pocket (old
    // X 5..25 -> 35..55), 3.5 below the old top: -6.5; elsewhere the faced top, -9.5
    let Some(c2) = js[1].1.carried.as_deref() else {
        panic!("no stock carried")
    };
    let h2 = c2.heightmap(&sb2, 0.5).unwrap();
    let at = |h: &marcam::sim3::Heightmap3, x: f64, y: f64| {
        let i = ((x - h.x0) / h.cell) as usize;
        let j = ((y - h.y0) / h.cell) as usize;
        h.z[j * h.nx + i]
    };
    assert!((at(&h2, 45.0, 20.0) - 0.0).abs() < 1e-9 && (at(&h2, 10.0, 20.0)).abs() < 1e-9);
    let b2 = c2.bottoms(&h2);
    let bat = |h: &marcam::sim3::Heightmap3, x: f64, y: f64, b: &[f64]| {
        let i = ((x - h.x0) / h.cell) as usize;
        let j = ((y - h.y0) / h.cell) as usize;
        b[j * h.nx + i]
    };
    assert!(
        (bat(&h2, 45.0, 20.0, &b2) + 6.5).abs() < 0.05,
        "{}",
        bat(&h2, 45.0, 20.0, &b2)
    );
    assert!((bat(&h2, 10.0, 20.0, &b2) + 9.5).abs() < 0.05);
    // setup 3, back over: the bottom faced 0.5 is now under it, the pocket on top again
    let Some(c3) = js[2].1.carried.as_deref() else {
        panic!()
    };
    let sb3 = js[2].1.stock_box.unwrap();
    let h3 = c3.heightmap(&sb3, 0.5).unwrap();
    // (re-zeroed by the block, its top where it was: the pocket's floor at -3.5 again, the
    // faced top at -0.5; the bottom faced 0.5 in setup 2: -9.5)
    assert!(
        (at(&h3, 15.0, 20.0) + 3.5).abs() < 0.05,
        "{}",
        at(&h3, 15.0, 20.0)
    );
    assert!(
        (at(&h3, 45.0, 20.0) + 0.5).abs() < 0.05,
        "{}",
        at(&h3, 45.0, 20.0)
    );
    let b3 = c3.bottoms(&h3);
    assert!(
        (bat(&h3, 45.0, 20.0, &b3) + 9.5).abs() < 0.05,
        "{}",
        bat(&h3, 45.0, 20.0, &b3)
    );
}

#[test]
fn a_carried_block_is_roughed_only_where_material_is_left() {
    // setup 2 (over) roughs over the pocket's footprint (X 35..55, Y 5..35 turned over) 8
    // deep: the carried block has the pocket's hollow from -6.5 down, so no level below the
    // one that takes its floor's last sliver; a fresh block is roughed all the way
    let dir = root().join("examples/plate");
    let mut v = plate_three_setups();
    v["setups"][1]["operations"] = json!([{"id": "r1", "type": "rough3d", "tool": 1, "x_range": [38.0, 52.0], "y_range": [8.0, 32.0],
        "stepover": 1.2, "sample": 0.4, "step_down": 1.0, "floor": -8.0, "leave": 0.0, "feed": 600.0, "plunge_feed": 100.0}]);
    v["setups"][1]["outputs"] =
        json!([{"name": "bottom", "title": "bottom", "operations": ["r1"]}]);
    v["setups"].as_array_mut().unwrap().truncate(2);
    let mut js = jobs(&v, &dir);
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    let fresh = js[1].1.clone();
    carry_stock(&mut js, &meshes, None).unwrap();
    let m = marcam::load_mesh(&js[1].1, &dir).unwrap();
    let carried = marcam::plan_with_mesh(&js[1].1, m.as_ref()).unwrap();
    let all = marcam::plan_with_mesh(&fresh, m.as_ref()).unwrap();
    let lowest = |tps: &[marcam::Toolpath]| {
        tps.iter()
            .flat_map(|t| &t.moves)
            .filter_map(|m| match m {
                marcam::Move::Line3 { z, .. } => Some(*z),
                _ => None,
            })
            .fold(f64::INFINITY, f64::min)
    };
    assert!(lowest(&all) <= -8.0 + 1e-9, "{}", lowest(&all));
    assert!(lowest(&carried) > -7.5, "{}", lowest(&carried));
    let g = marcam::gouge_check(&js[1].1, &carried, m.as_ref()).unwrap();
    assert!(g.iter().all(|r| r.max_depth < 0.02), "{g:?}");
}

#[test]
fn the_knob_re_chucked_end_for_end_starts_from_the_turned_bar() {
    let text =
        std::fs::read_to_string(root().join("examples/knob/knob_two_setups.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    v["setups"][1]["stock"]["turn"] = json!("y180");
    let dir = root().join("examples/knob");
    let mut js = jobs(&v, &dir);
    let c = js[1].1.carry.clone().unwrap();
    assert_eq!(c.turn, Turn::Y180);
    // the profile end for end: its front (X0) now at the back
    let p1 = js[0].1.part.profile.clone().unwrap();
    let p2 = js[1].1.part.profile.clone().unwrap();
    let (a, b) = (p1.points.first().unwrap(), p2.points.last().unwrap());
    assert!((b[0] - (c.shift[0] - a[0])).abs() < 1e-9 && b[1] == a[1]);
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let Some(Carried::Bar(m)) = js[1].1.carried.as_deref() else {
        panic!()
    };
    // the knob turned by the first setup (its front, X -1..0, down to radius 7.2 or so), now
    // at the other end: old X x -> shift - x
    let g = m.grid;
    let at = |x: f64| {
        let i = ((x - g.x0) / g.dx) as usize;
        m.r[i * g.na..(i + 1) * g.na]
            .iter()
            .fold(0.0f32, |a, &b| a.max(b)) as f64
    };
    let front = at(c.shift[0] + 0.5);
    assert!(front < 8.5, "{front}");
    // the rest of the bar (the chuck end) as it came
    assert!(
        (at(c.shift[0] + 30.0) - 12.5).abs() < 1e-3,
        "{}",
        at(c.shift[0] + 30.0)
    );
    // and it plans from there
    let tps = marcam::plan(&js[1].1).unwrap();
    assert!(!tps.is_empty());
}

/// A rough of the knob after it was turned (setup 2 carrying the turned bar): where the bar is
/// turned down already it finds nothing to cut, so it cuts far less than on a fresh bar.
#[test]
fn rotary_roughing_of_a_carried_bar_cuts_only_what_is_left() {
    let text =
        std::fs::read_to_string(root().join("examples/knob/knob_two_setups.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    let defaults = |job: &Value| -> Value {
        let libs = marcam::profiles::builtin_libraries();
        let (p, _) = Project::from_json_any(&job.to_string()).unwrap();
        p.op_defaults("rotary_rough", "setup2", None, &[], &libs, None, &|_| None)
            .unwrap()
    };
    let mut op = defaults(&v);
    op["id"] = json!("rr");
    v["setups"][1]["operations"] = json!([op]);
    v["setups"][1]["outputs"] = json!([{"name": "rough", "title": "rough", "operations": ["rr"]}]);
    let dir = root().join("examples/knob");
    let mut js = jobs(&v, &dir);
    let fresh = js[1].1.clone();
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let cut = |tps: &[marcam::Toolpath]| {
        tps.iter()
            .flat_map(|t| &t.moves)
            .filter(|m| matches!(m, marcam::Move::Cut { .. }))
            .count()
    };
    let carried = marcam::plan(&js[1].1).unwrap();
    let all = marcam::plan(&fresh).unwrap();
    assert!(cut(&all) > 100, "{}", cut(&all));
    assert!(
        cut(&carried) * 4 < cut(&all),
        "{} vs {}",
        cut(&carried),
        cut(&all)
    );
    let g = marcam::gouge_check(&js[1].1, &carried, None).unwrap();
    assert!(g.iter().all(|r| r.max_depth < 0.02), "{g:?}");
}

#[test]
fn a_setup_can_take_the_stock_of_any_setup_before_it() {
    // (Mar) setup 3 takes setup 1's stock, not setup 2's: the pocket on top, its bottom as
    // setup 1 left it (setup 2 faced its copy, turned over)
    let dir = root().join("examples/plate");
    let mut v = plate_three_setups();
    v["setups"][2]["stock"] = json!({"shape": "from_previous", "from": "vise1"});
    let mut js = jobs(&v, &dir);
    let c = js[2].1.carry.clone().unwrap();
    assert_eq!((c.from.as_str(), c.turn), ("vise1", Turn::None));
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let c3 = js[2].1.carried.as_deref().unwrap();
    let sb3 = js[2].1.stock_box.unwrap();
    let h3 = c3.heightmap(&sb3, 0.5).unwrap();
    let at = |h: &marcam::sim3::Heightmap3, x: f64, y: f64, b: &[f64]| {
        let i = ((x - h.x0) / h.cell) as usize;
        let j = ((y - h.y0) / h.cell) as usize;
        (h.z[j * h.nx + i], b[j * h.nx + i])
    };
    let b3 = c3.bottoms(&h3);
    let (pocket, _) = at(&h3, 15.0, 20.0, &b3);
    let (top, bottom) = at(&h3, 45.0, 20.0, &b3);
    assert!(
        (pocket + 3.5).abs() < 0.05 && (top + 0.5).abs() < 0.05,
        "{pocket} {top}"
    );
    // (not faced: setup 2's facing was of its own copy)
    assert!((bottom + 10.0).abs() < 0.05, "{bottom}");
    // a setup after it, or one that is not there: refused
    let mut bad = plate_three_setups();
    bad["setups"][1]["stock"] = json!({"shape": "from_previous", "from": "vise3"});
    let libs = marcam::profiles::builtin_libraries();
    let e =
        marcam::project::jobs_from_any_with(&bad.to_string(), &libs, &|src| src.load(&dir).ok())
            .unwrap_err();
    assert!(e.contains("not a setup before it"), "{e}");
}

/// The top of column (x, y) of a carried block, and its bottom.
fn column(c: &Carried, sb: &marcam::job::StockBox, x: f64, y: f64) -> (f64, f64) {
    let h = c.heightmap(sb, 0.5).unwrap();
    let b = c.bottoms(&h);
    let i = ((x - h.x0) / h.cell) as usize;
    let j = ((y - h.y0) / h.cell) as usize;
    (h.z[j * h.nx + i], b[j * h.nx + i])
}

#[test]
fn the_plate_stood_on_its_end_goes_on_as_a_mesh() {
    // (Mar) setup 2 takes the plate tipped 90 degrees about Y: its old top (faced, pocketed)
    // faces +X, its old X runs down. Re-zeroed where it was: X 0..10, Y 0..40, Z -60..0;
    // every column full height (a pocket in a side is solid seen from above)
    let dir = root().join("examples/plate");
    let mut v = plate_three_setups();
    v["setups"][1]["stock"] = json!({"shape": "from_previous", "rotate": [0.0, 90.0, 0.0]});
    v["setups"].as_array_mut().unwrap().truncate(2);
    let mut js = jobs(&v, &dir);
    let sb = js[1].1.stock_box.unwrap();
    for a in 0..3 {
        let (lo, hi) = ([0.0, 0.0, -60.0][a], [10.0, 40.0, 0.0][a]);
        assert!(
            (sb.min[a] - lo).abs() < 1e-9 && (sb.max[a] - hi).abs() < 1e-9,
            "{sb:?}"
        );
    }
    let c = js[1].1.carry.clone().unwrap();
    assert_eq!((c.turn, c.rotate), (Turn::None, [0.0, 90.0, 0.0]));
    // the part tipped the same: within the block, Z 60 tall
    let m2 = marcam::load_mesh(&js[1].1, &dir).unwrap().unwrap();
    let (lo, hi) = m2.bounds().unwrap();
    assert!(
        lo[0] > -1e-6 && hi[0] < 10.0 + 1e-6 && lo[2] > -60.0 - 1e-6 && hi[2] < 1e-6,
        "{lo:?} {hi:?}"
    );
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let c2 = js[1].1.carried.as_deref().unwrap();
    assert!(matches!(c2, Carried::Block { .. }));
    let (top, bot) = column(c2, &sb, 5.0, 20.0);
    assert!(top.abs() < 0.05 && (bot + 60.0).abs() < 0.05, "{top} {bot}");
    // the faced top, now the +X side: nothing beyond X 9.5
    let (top, bot) = column(c2, &sb, 9.8, 20.0);
    assert!(top <= bot + 1e-9, "{top} {bot}");
    // and a facing pass plans on it, cutting the old X 60 end only
    let tps = marcam::plan(&js[1].1).unwrap();
    assert!(!tps.is_empty());
}

#[test]
fn the_knob_lies_in_a_vise_on_its_side() {
    // (Mar) the knob, turned, goes into a vise lying along X: the cut bar as a mesh, sampled
    // as columns. X 0..100, Y 0..25, its top at Z0; the front (old X0, turned down to radius
    // 7.2 or so) at X 100
    let text =
        std::fs::read_to_string(root().join("examples/knob/knob_two_setups.marcam.toml")).unwrap();
    let (p, _) = Project::from_json_any(&text).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    v["setups"][1]["kind"] = json!("vise");
    v["setups"][1]["holding"] = json!({"kind": "vise", "tailstock": false});
    v["setups"][1]["part"] = json!([]);
    v["setups"][1]["operations"] = json!([{"id": "f1", "type": "face", "tool": 1, "depth": 0.5, "step_down": 0.5, "stepover": 1.5, "feed": 300.0, "plunge_feed": 60.0}]);
    v["setups"][1]["outputs"] = json!([{"name": "face", "title": "face", "operations": ["f1"]}]);
    let dir = root().join("examples/knob");
    let mut js = jobs(&v, &dir);
    let sb = js[1].1.stock_box.unwrap();
    assert!(
        (sb.min[0]).abs() < 1e-9 && (sb.max[0] - 100.0).abs() < 1e-9,
        "{sb:?}"
    );
    assert!(
        (sb.max[1] - sb.min[1] - 25.0).abs() < 1e-6 && sb.max[2].abs() < 1e-9,
        "{sb:?}"
    );
    assert!(js[1].1.part.profile.is_none());
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let c2 = js[1].1.carried.as_deref().unwrap();
    let yc = 0.5 * (sb.min[1] + sb.max[1]);
    // the chuck end as it came: top at Z0, bottom at -25
    let (top, bot) = column(c2, &sb, 30.0, yc);
    assert!(top.abs() < 0.1 && (bot + 25.0).abs() < 0.1, "{top} {bot}");
    // the front, turned down: well below the bar's top
    let (top, _) = column(c2, &sb, 99.5, yc);
    assert!(top < -4.0, "{top}");
    let tps = marcam::plan(&js[1].1).unwrap();
    assert!(!tps.is_empty());
}

#[test]
fn a_vise_cut_plate_goes_on_as_a_bar() {
    // (Mar) the plate, faced and pocketed in the vise, chucked in the rotary: centred on the
    // axis, its front at X0, X -60..0; the radius to its corners (20, 5)
    let dir = root().join("examples/plate");
    let mut v = plate_three_setups();
    v["setups"][1]["stock"] = json!({"shape": "from_previous"});
    v["setups"][1]["kind"] = json!("rotary");
    v["setups"][1]["holding"] = json!({"kind": "rotary_chuck", "tailstock": false});
    v["setups"][1]["operations"] = json!([]);
    v["setups"][1]["outputs"] = json!([]);
    v["setups"].as_array_mut().unwrap().truncate(2);
    let mut js = jobs(&v, &dir);
    assert!(js[1].1.stock_box.is_none());
    let st = &js[1].1.stock;
    assert!((st.length - 60.0).abs() < 1e-9 && (st.radius - 20f64.hypot(5.0)).abs() < 1e-9);
    let meshes = |j: &Job| marcam::load_mesh(j, &dir).ok().flatten();
    carry_stock(&mut js, &meshes, None).unwrap();
    let Some(Carried::Bar(m)) = js[1].1.carried.as_deref() else {
        panic!("no bar carried")
    };
    let g = m.grid;
    // the radius at x toward angle a (0: +Y, 90: +Z)
    let at = |x: f64, a: f64| {
        let i = ((x - g.x0) / g.dx) as usize;
        let j = ((a / 360.0 * g.na as f64) as usize).min(g.na - 1);
        m.r[i * g.na + j] as f64
    };
    // its sides (Y +-20) and its faced top (old Z -0.5: 4.5 up from the axis), its bottom 5
    assert!((at(-10.0, 0.0) - 20.0).abs() < 0.3, "{}", at(-10.0, 0.0));
    assert!((at(-10.0, 90.0) - 4.5).abs() < 0.3, "{}", at(-10.0, 90.0));
    assert!((at(-10.0, 270.0) - 5.0).abs() < 0.3, "{}", at(-10.0, 270.0));
    // the pocket (old X 5..25: -55..-35), 3 deep: its floor 1.5 up
    assert!((at(-45.0, 90.0) - 1.5).abs() < 0.3, "{}", at(-45.0, 90.0));
}

/// (Mar's walkthrough) A square bar stood in the vise, its top turned to a Ø25 cylinder with
/// a Ø15 boss; then on the 4th axis, continuing, turned 90° about Y: the part goes with the
/// stock (its axis on the rotary axis, its boss at the front), and what a new operation
/// starts from (its defaults) and what is picked on it are the part where this setup holds
/// it, not where the vise had it.
fn stepped_bar_vise_then_4th() -> (Value, PathBuf) {
    let v = json!({
        "schema_version": "0.2",
        "name": "stepped bar",
        "models": [{"id": "part", "name": "stepped_bar.stl", "kind": "mesh", "file": "stepped_bar.stl", "axis": "x", "translate": [15.0, 15.0, -60.5]}],
        "tools": [
            {"number": 1, "library": "makera-starter", "tool": "spiral-o-metal-3.175x12"},
            {"number": 4, "library": "makera-starter", "tool": "vbit-90-3.175"}
        ],
        "setups": [
            {"id": "vise1", "name": "Vise", "kind": "vise", "machine": "makera-z1-4axis", "clearance": 3.0, "part": ["part"], "spindle_rpm": 12000,
             "stock": {"shape": "box", "min": [0.0, 0.0, -62.0], "max": [30.0, 30.0, 0.0]},
             "holding": {"kind": "vise", "tailstock": false}, "operations": [], "outputs": []},
            {"id": "rot", "name": "4th axis", "kind": "rotary", "machine": "makera-z1-4axis", "clearance": 3.0, "part": ["part"], "spindle_rpm": 12000,
             "stock": {"shape": "from_previous", "rotate": [0.0, 90.0, 0.0]},
             "holding": {"kind": "rotary_chuck", "tailstock": false}, "operations": [], "outputs": []}
        ]
    });
    (v, root().join("tests/fixtures"))
}

#[test]
fn a_bar_turned_in_the_vise_goes_on_the_4th_axis_with_its_part_where_it_is_held() {
    let (v, dir) = stepped_bar_vise_then_4th();
    let (p, _) = Project::from_json_any(&v.to_string()).unwrap();
    let rot = p.setups.iter().find(|s| s.id == "rot").unwrap();
    // the part where the 4th axis holds it: along X, its front (the boss) at X -0.5, its axis
    // on the rotary axis
    let src = p.model_source_in(rot, "part").unwrap();
    let m = src.load(&dir).unwrap();
    let (lo, hi) = m.bounds().unwrap();
    assert!(
        (lo[0] + 60.5).abs() < 1e-6 && (hi[0] + 0.5).abs() < 1e-6,
        "{lo:?} {hi:?}"
    );
    for a in 1..3 {
        assert!(
            (lo[a] + hi[a]).abs() < 1e-6 && (hi[a] - 15.0).abs() < 1e-6,
            "{lo:?} {hi:?}"
        );
    }
    // the vise's own: as written (standing, Z -60.5..-0.5)
    let vise = p.setups.iter().find(|s| s.id == "vise1").unwrap();
    let (lo1, hi1) = p
        .model_source_in(vise, "part")
        .unwrap()
        .load(&dir)
        .unwrap()
        .bounds()
        .unwrap();
    assert!(
        (lo1[2] + 60.5).abs() < 1e-6 && (hi1[2] + 0.5).abs() < 1e-6,
        "{lo1:?} {hi1:?}"
    );
    // a knurl's defaults on it: within the part (were X 2..32 and radii 60..68, the part where
    // the vise had it seen from the rotary axis)
    let libs = marcam::profiles::builtin_libraries();
    let meshes = |id: &str| p.model_source_in(rot, id)?.load(&dir).ok();
    let d = p
        .op_defaults("helical_knurl", "rot", Some(4), &[], &libs, None, &meshes)
        .unwrap();
    for k in ["body_back", "body_front"] {
        let xr = d[k].as_array().unwrap();
        let (x, r) = (xr[0].as_f64().unwrap(), xr[1].as_f64().unwrap());
        assert!(
            (-60.5 - 1e-6..=1e-6).contains(&x) && r > 0.0 && r <= 21.3,
            "{k} {xr:?}"
        );
    }
    // the faces a pick resolves on: the setup's part, a cylinder along X of radius 12.5 and one
    // of 7.5
    let regions = p.model_regions("part", &meshes).unwrap();
    let cyl = |r: f64| {
        regions
            .iter()
            .any(|g| g.shape == "cylinder" && (g.r[1] - r).abs() < 0.05 && g.a.is_none())
    };
    assert!(
        cyl(12.5) && cyl(7.5),
        "{:?}",
        regions.iter().map(|g| (&g.shape, g.r)).collect::<Vec<_>>()
    );
}

#[test]
fn a_knurl_on_the_step_next_to_the_base_keeps_off_the_base() {
    // (Mar's walkthrough) the knurl on the Ø25 step (X -40.5..-10.5) ran its grooves 0.6 mm
    // past the square base's face, the holder 2.7 mm into it: at a shoulder no run out, and the
    // V-bit's widest (Ø3.175) kept off the face
    let (mut v, dir) = stepped_bar_vise_then_4th();
    v["setups"][1]["operations"] = json!([{"id": "k", "type": "helical_knurl", "tool": 4,
        "body_back": [-40.5, 12.5], "body_front": [-10.5, 12.5], "count": 39, "helix_angle": 30.0,
        "fill": 0.95, "passes": [0.6, 1.0], "families": [1, -1], "runout": 0.6, "samples": 120,
        "approach_clearance": 0.5, "feed": 200.0, "plunge_feed": 60.0}]);
    v["setups"][1]["outputs"] = json!([{"name": "knurl", "title": "knurl", "operations": ["k"]}]);
    let js = jobs(&v, &dir);
    let rot = &js.iter().find(|(id, _)| id == "rot").unwrap().1;
    let mesh = marcam::load_mesh(rot, &dir).unwrap();
    let tps = marcam::plan_with_mesh(rot, mesh.as_ref()).unwrap();
    let g = marcam::post(&tps[0], &marcam::Machine::makera_z1());
    let xs: Vec<f64> = g
        .lines()
        // (each groove starts at a rapid to its X, then cuts along)
        .filter(|l| l.starts_with("G1") || l.starts_with("G0 X"))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with('X'))
                .and_then(|w| w[1..].parse().ok())
        })
        .collect();
    let (lo, hi) = xs
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| {
            (a.min(x), b.max(x))
        });
    // the back end off the base by the V-bit's radius; the front (the boss is smaller) runs out
    assert!((lo - (-40.5 + 3.175 / 2.0)).abs() < 0.01, "{lo}");
    assert!((hi - (-10.5 + 0.6)).abs() < 0.01, "{hi}");
}
