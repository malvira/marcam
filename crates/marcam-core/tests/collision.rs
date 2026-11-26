//! Holder and shank collisions: the geometry of a contact, then whole jobs simulated with tools
//! too short to clear the stock (rotary, 3-axis and indexed), against the same jobs with the
//! default tool bodies, which clear.

use marcam::collision::{contact_block, contact_radial, Contact, ToolBody};
use marcam::job::{StockBox, Tool, ToolShape};
use marcam::sim::{Grid, RadialMap};
use marcam::sim3::Heightmap3;
use marcam::{collision_check, plan_with_mesh, post, profiles::builtin_machine, Job};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(path: &str) -> Job {
    let text = std::fs::read_to_string(root().join(path)).unwrap();
    let dir = root().join(path).parent().unwrap().to_path_buf();
    let libs = marcam::profiles::builtin_libraries();
    let mut jobs =
        marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&dir).ok()).unwrap();
    jobs.remove(0).1
}

fn body(flute: f64, shank_d: f64, stickout: f64) -> ToolBody {
    ToolBody::of(&Tool {
        flute_length: Some(flute),
        shank_diameter: Some(shank_d),
        stickout: Some(stickout),
        ..Tool::new(1, "t", ToolShape::Flat { diameter: 3.175 })
    })
}

#[test]
fn defaults_fill_every_field() {
    let b = ToolBody::of(&Tool::new(1, "t", ToolShape::Ball { diameter: 2.0 }));
    assert_eq!(b.flute_length, 6.0);
    assert_eq!(b.shank_radius, 1.0);
    assert_eq!(b.stickout, 11.0);
    assert_eq!(b.holder_radius, 10.0);
    assert_eq!(b.holder_length, 30.0);
    // a given stickout shorter than the flutes is taken as the flute length
    let short = ToolBody::of(&Tool {
        flute_length: Some(12.0),
        stickout: Some(5.0),
        ..Tool::new(1, "t", ToolShape::Flat { diameter: 3.0 })
    });
    assert_eq!(short.stickout, 12.0);
}

#[test]
fn radial_contact_geometry() {
    // a whole bar of radius 10 from X -20 to 0, mapped from X -20 to 0
    let grid = Grid::new(-20.0, 0.0, 0.05, 720);
    let bar = RadialMap::filled(grid, 10.0);
    let b = body(1.0, 3.175, 2.0);
    // tip 5 mm out over the bar: holder from Z 7, the bar reaches Z 10
    let (c, d) = contact_radial(&bar, (10.0, -20.0, 0.0), [-10.0, 0.0, 5.0], 37.0, &b).unwrap();
    assert_eq!(c, Contact::Holder);
    assert!((d - 3.0).abs() < 0.01, "{d}");
    // tip off to the side (Y 15): the holder (radius 10) reaches Y 5, where the bar is
    // sqrt(100 - 25) high
    let (_, d) = contact_radial(&bar, (10.0, -20.0, 0.0), [-10.0, 15.0, 5.0], 0.0, &b).unwrap();
    // (cells are 0.5° apart: about 0.05 mm at this radius)
    assert!((d - (75f64.sqrt() - 7.0)).abs() < 0.05, "{d}");
    // the same contact off the map's X range comes from the uncut bar
    let small = RadialMap::filled(Grid::new(-5.0, 0.0, 0.05, 720), 10.0);
    let (_, d) = contact_radial(&small, (10.0, -20.0, 0.0), [-12.0, 0.0, 5.0], 0.0, &b).unwrap();
    assert!((d - 3.0).abs() < 0.01, "{d}");
    // above the bar, or past its end: nothing. (Tip 8.5 out is clear too: the shank is no
    // wider than the cut, and inside the cutting radius the flutes went first.)
    assert!(contact_radial(&bar, (10.0, -20.0, 0.0), [-10.0, 0.0, 8.5], 0.0, &b).is_none());
    assert!(contact_radial(&bar, (10.0, -20.0, 0.0), [-10.0, 0.0, 9.5], 0.0, &b).is_none());
    assert!(contact_radial(&small, (10.0, -20.0, 0.0), [12.0, 0.0, 5.0], 0.0, &b).is_none());
    // a shank thicker than the cut, inside the bar (checked outside the cutting radius,
    // where the flutes went first; beside it the bar is full height)
    let (c, d) = contact_radial(
        &bar,
        (10.0, -20.0, 0.0),
        [-10.0, 0.0, 5.0],
        0.0,
        &body(1.0, 6.0, 30.0),
    )
    .unwrap();
    assert_eq!(c, Contact::Shank);
    assert!((d - 4.0).abs() < 0.01, "{d}");
    // a shank no thicker than the cut does not take its own hole for material
    assert!(contact_radial(
        &bar,
        (10.0, -20.0, 0.0),
        [-10.0, 0.0, 5.0],
        0.0,
        &body(1.0, 3.175, 30.0)
    )
    .is_none());
}

#[test]
fn block_contact_geometry() {
    let sb = StockBox {
        min: [0.0, 0.0, -10.0],
        max: [40.0, 40.0, 0.0],
    };
    let hm = Heightmap3::new(&sb, 0.25);
    let b = body(1.0, 3.175, 2.0);
    let (c, d) = contact_block(&hm, 0.0, [20.0, 20.0, -5.0], &b).unwrap();
    assert_eq!(c, Contact::Holder);
    assert!((d - 3.0).abs() < 1e-9, "{d}");
    // just past the block's edge the holder still overhangs it
    assert!(contact_block(&hm, 0.0, [48.0, 20.0, -5.0], &b).is_some());
    assert!(contact_block(&hm, 0.0, [51.0, 20.0, -5.0], &b).is_none());
    // tip 1.5 mm into uncut block, 1 mm of flute: the shank is in the material
    assert_eq!(
        contact_block(&hm, 0.0, [20.0, 20.0, -1.5], &b).unwrap().0,
        Contact::Shank
    );
    assert!(contact_block(&hm, 0.0, [20.0, 20.0, -0.5], &b).is_none());
}

/// Every reported line is a motion line of the posted program.
fn lines_are_moves(
    job: &Job,
    tps: &[marcam::Toolpath],
    reps: &[marcam::collision::CollisionReport],
) {
    let m = builtin_machine(job.machine.as_deref().unwrap_or("makera-z1-4axis")).unwrap();
    for (tp, rep) in tps.iter().zip(reps) {
        let text = post(tp, &m.post);
        let lines: Vec<&str> = text.lines().collect();
        for c in &rep.collisions {
            let l = lines[c.line.unwrap() - 1];
            assert!(
                ["G0", "G1", "G2", "G3"].iter().any(|g| l.starts_with(g)),
                "line {:?}: {l}",
                c.line
            );
        }
    }
}

#[test]
fn rotary_holder_hits_the_knob_body() {
    let job = load("examples/knob/knob.marcam.toml");
    let m = builtin_machine("makera-z1-4axis").unwrap();
    let tps = plan_with_mesh(&job, None).unwrap();
    // as given (12 mm flutes, default stickout 17 mm): clear
    for rep in collision_check(&job, &tps, &m.post).unwrap() {
        assert_eq!(
            rep.count,
            0,
            "{}: {:?}",
            rep.program,
            rep.collisions.first()
        );
    }
    // T1 held 3.5 mm out: its 20 mm holder reaches the 10 mm body while turning the 3 mm neck
    let mut short = job.clone();
    let t1 = short.tools.iter_mut().find(|t| t.number == 1).unwrap();
    t1.flute_length = Some(2.0);
    t1.stickout = Some(3.5);
    let reps = collision_check(&short, &tps, &m.post).unwrap();
    let turning = &reps[0];
    assert!(turning.count > 0, "{turning:?}");
    assert!(turning.collisions.iter().all(|c| c.tool == 1));
    assert!(turning
        .collisions
        .iter()
        .any(|c| c.contact == Contact::Holder));
    // deepest: tip at the 3 mm neck, holder from 6.5 mm, the uncut 12.5 mm bar behind the part
    assert!(
        (turning.max_depth - 6.0).abs() < 0.1,
        "{}",
        turning.max_depth
    );
    lines_are_moves(&short, &tps, &reps);
    // the simulation reports the same moves
    let sim = marcam::simulate_job(&short, &tps, None, 0.2, 360, 0.05).unwrap();
    assert!(sim.collisions[0].count > 0);
    let a: Vec<usize> = sim.collisions[0]
        .collisions
        .iter()
        .map(|c| c.move_index)
        .collect();
    let b: Vec<usize> = turning.collisions.iter().map(|c| c.move_index).collect();
    // (the two grids differ, so the first contact may be a move apart)
    assert!(a[0].abs_diff(b[0]) <= 3, "{a:?} {b:?}");
}

#[test]
fn three_axis_shank_and_holder_in_the_plate_pocket() {
    let job = load("examples/plate/plate.marcam.toml");
    let dir = root().join("examples/plate");
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    let tps = plan_with_mesh(&job, mesh.as_ref()).unwrap();
    let m = builtin_machine(job.machine.as_deref().unwrap()).unwrap();
    for rep in collision_check(&job, &tps, &m.post).unwrap() {
        assert_eq!(
            rep.count,
            0,
            "{}: {:?}",
            rep.program,
            rep.collisions.first()
        );
    }
    // a 6 mm shank over 1 mm of flute: it rubs the pocket walls once the pocket is deeper
    let mut thick = job.clone();
    let t1 = thick.tools.iter_mut().find(|t| t.number == 1).unwrap();
    t1.flute_length = Some(1.0);
    t1.shank_diameter = Some(6.0);
    t1.stickout = Some(30.0);
    let reps = collision_check(&thick, &tps, &m.post).unwrap();
    let shank: Vec<_> = reps
        .iter()
        .flat_map(|r| &r.collisions)
        .filter(|c| c.contact == Contact::Shank)
        .collect();
    assert!(!shank.is_empty(), "{reps:?}");
    lines_are_moves(&thick, &tps, &reps);
    // held 2 mm out: the holder meets the block top beside the 3.5 mm deep pocket
    let mut stubby = job.clone();
    let t1 = stubby.tools.iter_mut().find(|t| t.number == 1).unwrap();
    t1.flute_length = Some(1.0);
    t1.stickout = Some(2.0);
    let reps = collision_check(&stubby, &tps, &m.post).unwrap();
    assert!(reps
        .iter()
        .flat_map(|r| &r.collisions)
        .any(|c| c.contact == Contact::Holder));
    assert!(reps.iter().map(|r| r.max_depth).fold(0.0, f64::max) > 1.0);
}

#[test]
fn indexed_holder_on_the_knob() {
    let job = load("examples/knob/knob_indexed.marcam.toml");
    let m = builtin_machine("makera-z1-4axis").unwrap();
    let tps = plan_with_mesh(&job, None).unwrap();
    for rep in collision_check(&job, &tps, &m.post).unwrap() {
        assert_eq!(
            rep.count,
            0,
            "{}: {:?}",
            rep.program,
            rep.collisions.first()
        );
    }
    // the indexed program's tool held very short
    let mut short = job.clone();
    let indexed_tool = match &tps[2]
        .moves
        .iter()
        .find(|m| matches!(m, marcam::Move::ToolChange { .. }))
    {
        Some(marcam::Move::ToolChange { tool, .. }) => *tool,
        _ => panic!("no tool change"),
    };
    let t = short
        .tools
        .iter_mut()
        .find(|t| t.number == indexed_tool)
        .unwrap();
    t.flute_length = Some(0.5);
    t.stickout = Some(0.6);
    let reps = collision_check(&short, &tps, &m.post).unwrap();
    assert!(reps[2].count > 0, "{:?}", reps[2]);
    assert!(reps[2].collisions.iter().all(|c| c.tool == indexed_tool));
    lines_are_moves(&short, &tps, &reps);
}

#[test]
fn the_chuck_and_the_tailstock_are_checked_when_the_setup_has_them() {
    // (Mar's Fixture) the knob: without a chuck or a tailstock, as before; a chuck gripping
    // the bar up to near the part's back: the tool reaches it
    let mut job = load("examples/knob/knob.job.json");
    let m = builtin_machine("makera-z1-4axis").unwrap();
    let tps = plan_with_mesh(&job, None).unwrap();
    let plain = collision_check(&job, &tps, &m.post).unwrap();
    let any = |reps: &[marcam::collision::CollisionReport], c: Contact| {
        reps.iter()
            .any(|r| r.collisions.iter().any(|k| k.contact == c))
    };
    assert!(!any(&plain, Contact::OnTailstock) && !any(&plain, Contact::OnChuck));
    // the tailstock: its centre's cone (60° across) from its point 3 mm into the front
    // face, then the quill (Ø24): a tool low over the front face meets it, one high does not
    let fx = marcam::job::Fixture {
        tailstock: true,
        centre_depth: Some(3.0),
        ..Default::default()
    };
    let b = ToolBody::of(&Tool::new(1, "t", ToolShape::Flat { diameter: 3.175 }));
    let at = |x: f64, r: f64| marcam::collision::contact_holders(&fx, 100.0, [x, 0.0, r], &b);
    assert!(
        matches!(at(0.5, 1.0), Some((Contact::OnTailstock, _))),
        "{:?}",
        at(0.5, 1.0)
    );
    assert!(at(-10.0, 1.0).is_none() && at(0.5, 13.0).is_none());
    assert!(matches!(at(25.0, 8.0), Some((Contact::OnTailstock, _))));
    job.fixture = Some(marcam::job::Fixture {
        chuck_diameter: Some(65.0),
        grip: Some(job.stock.length - 15.0),
        ..Default::default()
    });
    let chuck = collision_check(&job, &tps, &m.post).unwrap();
    assert!(
        any(&chuck, Contact::OnChuck),
        "{:?}",
        chuck.iter().map(|r| r.count).collect::<Vec<_>>()
    );
}

/// A near miss: no contact, but the holder or shank within [`marcam::collision::NEAR_MISS`] of
/// the stock, said (Mar's impeller turned end for end: its roughing shank 0.02 mm from the
/// stock, the check "clear"). The knob's T1 held so its holder passes 0.3 mm over the bar
/// behind the part: a near miss, no collision; 1 mm over it: clear.
#[test]
fn a_holder_just_clear_of_the_stock_is_a_near_miss() {
    let job = load("examples/knob/knob.marcam.toml");
    let m = builtin_machine("makera-z1-4axis").unwrap();
    let tps = plan_with_mesh(&job, None).unwrap();
    // (as given: clear, no near miss either)
    for rep in collision_check(&job, &tps, &m.post).unwrap() {
        assert_eq!((rep.count, rep.near), (0, 0), "{}", rep.program);
    }
    // the turning's deepest: tip at the 3 mm neck, the uncut 12.5 mm bar behind it: the holder
    // touches with less than 9.5 mm out
    let held = |out: f64| {
        let mut j = job.clone();
        let t1 = j.tools.iter_mut().find(|t| t.number == 1).unwrap();
        t1.flute_length = Some(2.0);
        t1.stickout = Some(out);
        // (a thin shank, well inside the cut: only the holder can come near)
        t1.shank_diameter = Some(1.0);
        collision_check(&j, &tps, &m.post).unwrap().remove(0)
    };
    let near = held(9.8);
    assert_eq!(near.count, 0, "{:?}", near.collisions.first());
    assert!(near.near > 0 && near.near_line.is_some(), "{near:?}");
    let clear = held(10.5);
    assert_eq!((clear.count, clear.near), (0, 0), "{clear:?}");
    let touching = held(9.0);
    assert!(touching.count > 0 && touching.near == 0, "{touching:?}");
}

/// Indexed roughing deeper than the tool reaches: no deeper, at each angle, than its flutes
/// (and its holder) clear the stock round the cut, the rest left and said (Mar's impeller on
/// a 24 mm bar, roughed to Z 0 with a 3.175 x 12 tool: 6845 moves with the holder up to
/// 6.7 mm in the bar). The finned hub on a 14 mm bar, roughed at 4 angles to Z 2 with a
/// 2 x 8 mm tool: no collision and no near miss, each angle's floor raised and said.
#[test]
fn indexed_roughing_goes_no_deeper_than_the_tool_reaches() {
    let text = r#"
schema_version = "0.2"
name = "deep indexed roughing"

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
tool = "spiral-o-metal-2x8"

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
name = "Indexed roughing"
type = "rough3d"
tool = 1
index_a = 0.0
x_range = [-30.0, 0.0]
y_range = [-14.0, 14.0]
stepover = 0.8
sample = 0.5
step_down = 1.5
floor = 2.0
leave_z = 0.2
leave_xy = 0.2
tolerance = 0.05
pattern = "offset"
index_count = 4
feed = 500.0
plunge_feed = 150.0

[[setups.outputs]]
name = "deep"
title = "deep"
reset_a = false
operations = ["op1"]
"#;
    let dir = root().join("tests/fixtures");
    let libs = marcam::profiles::builtin_libraries();
    let job = marcam::project::jobs_from_any_with(text, &libs, &|src| src.load(&dir).ok())
        .unwrap()
        .remove(0)
        .1;
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    let tps = plan_with_mesh(&job, mesh.as_ref()).unwrap();
    let m = builtin_machine("makera-z1-4axis").unwrap();
    let rep = &collision_check(&job, &tps, &m.post).unwrap()[0];
    assert_eq!((rep.count, rep.near), (0, 0), "{rep:?}");
    let capped = tps[0]
        .notes
        .iter()
        .filter(|w| w.contains("no deeper than Z 7.00"))
        .count();
    assert_eq!(capped, 4, "{:?}", tps[0].notes);
}

#[test]
fn a_knurl_on_a_bar_not_turned_down_comes_in_from_the_bar() {
    // (the knob's part replaced: its profile turning gone, the knurl on the Ø25 bar) its
    // grooves came in by rapids to the knurl's body (Ø20 + 0.5 mm), 6 mm into the bar, 13 of
    // them through it on the 3D stock. From the bar now, and said
    let mut job = load("examples/knob/knob.job.json");
    job.programs.retain(|p| p.name == "knob_2_knurl");
    let tps = plan_with_mesh(&job, None).unwrap();
    let sim = marcam::simulate_job(&job, &tps, None, 0.25, 180, 0.05).unwrap();
    let hits: usize = sim.programs.iter().map(|p| p.1.rapid_hits.len()).sum();
    assert_eq!(hits, 0, "rapids through the bar");
    assert!(
        tps[0].notes
            .iter()
            .any(|w| w.contains("above the knurl's body")),
        "{:?}",
        tps[0].notes
    );
    // turned first (the whole knob): its knurl as it was, its rapids clear too
    let job = load("examples/knob/knob.job.json");
    let tps = plan_with_mesh(&job, None).unwrap();
    assert!(
        !tps[1].notes
            .iter()
            .any(|w| w.contains("above the knurl's body")),
        "{:?}",
        tps[1].notes
    );
    let sim = marcam::simulate_job(&job, &tps, None, 0.25, 180, 0.05).unwrap();
    assert_eq!(
        sim.programs
            .iter()
            .map(|p| p.1.rapid_hits.len())
            .sum::<usize>(),
        0
    );
}
