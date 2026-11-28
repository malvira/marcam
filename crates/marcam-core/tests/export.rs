//! Named NC exports: the picked operations' planned toolpaths posted to a file of their own.

use marcam::export::{fingerprint, numbers_of, splice};
use marcam::{limits_for, plan, post, verify_gcode, Job, Machine, Move};
use std::path::PathBuf;

fn knob() -> (Vec<marcam::Toolpath>, Machine) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("examples/knob/knob.job.json")).unwrap();
    let job = Job::from_json(&text).unwrap();
    (plan(&job).unwrap(), Machine::makera_z1())
}

fn ops_of(tp: &marcam::Toolpath) -> Vec<u32> {
    tp.moves
        .iter()
        .filter_map(|m| match m {
            Move::Operation { id, .. } => Some(*id),
            _ => None,
        })
        .collect()
}

/// Every operation of one program, exported: that program's G-code, its title aside.
#[test]
fn a_whole_program_exports_as_itself() {
    let (tps, machine) = knob();
    for tp in &tps {
        let ids = ops_of(tp);
        let ex = splice(&tps, &|k| ids.contains(&k), "mine").unwrap();
        let a = post(tp, &machine);
        let b = post(&ex, &machine);
        let skip_title = |s: &str| s.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(skip_title(&a), skip_title(&b), "{}", tp.name);
        assert!(b.starts_with("(mine)"));
    }
}

/// An operation taken without the one before it (same tool, so the original loads nothing):
/// the export loads its tool, retracts and comes to where it was planned from; the verifier
/// finds nothing wrong.
#[test]
fn an_operation_alone_gets_its_tool_and_its_way_in() {
    let (tps, machine) = knob();
    let last = *ops_of(tps.last().unwrap()).last().unwrap();
    let ex = splice(&tps, &|k| k == last, "chamfer").unwrap();
    assert!(matches!(ex.moves[0], Move::Operation { id, .. } if id == last));
    let tool = ex.moves.iter().find_map(|m| match m {
        Move::ToolChange { tool, .. } => Some(*tool),
        _ => None,
    });
    assert_eq!(tool, Some(2), "the V-bit loaded");
    assert_eq!(ex.tool.as_ref().map(|t| t.0), Some(2));
    let g = post(&ex, &machine);
    assert!(g.contains("T2"), "{g}");
    let issues = verify_gcode(&g, &limits_for(&ex, &machine));
    assert!(issues.is_empty(), "{issues:?}");
    // its cuts are the planned ones, all of them
    let cuts = |moves: &[Move]| moves.iter().filter(|m| matches!(m, Move::Cut { .. })).count();
    let tp = tps.last().unwrap();
    let start = tp
        .moves
        .iter()
        .position(|m| matches!(m, Move::Operation { id, .. } if *id == last))
        .unwrap();
    assert_eq!(cuts(&ex.moves), cuts(&tp.moves[start..]));
}

/// Operations of two programs in one file: both tools, in planning order, and the verifier
/// finds nothing wrong.
#[test]
fn operations_of_two_programs_in_one_file() {
    let (tps, machine) = knob();
    let first = ops_of(&tps[0])[0];
    let last = *ops_of(tps.last().unwrap()).last().unwrap();
    let ex = splice(&tps, &|k| k == first || k == last, "both").unwrap();
    let tools: Vec<u32> = ex
        .moves
        .iter()
        .filter_map(|m| match m {
            Move::ToolChange { tool, .. } => Some(*tool),
            _ => None,
        })
        .collect();
    assert_eq!(tools, vec![1, 2]);
    assert_eq!(ops_of(&ex), vec![first, last]);
    let g = post(&ex, &machine);
    let issues = verify_gcode(&g, &limits_for(&ex, &machine));
    assert!(issues.is_empty(), "{issues:?}");
    // nothing picked: no file
    assert!(splice(&tps, &|_| false, "none").is_none());
}

#[test]
fn fingerprints_and_numbers() {
    assert_eq!(fingerprint("G0 X1\n"), fingerprint("G0 X1\n"));
    assert_ne!(fingerprint("G0 X1\n"), fingerprint("G0 X2\n"));
    assert_eq!(fingerprint("").len(), 16);
    let planned: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    let (nums, missing) = numbers_of(&planned, &["c".into(), "x".into(), "a".into()]);
    assert_eq!(nums, vec![2, 0]);
    assert_eq!(missing, vec!["x".to_string()]);
}


/// On a vise (3-axis): the whole flat program exports as itself; the holes and the dome
/// (another program, another tool) together post clean G-code, up to the safe Z between them.
#[test]
fn three_axis_exports() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plate");
    let text = std::fs::read_to_string(dir.join("plate.marcam.toml")).unwrap();
    let libs = marcam::profiles::builtin_libraries();
    let job = marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&dir).ok())
        .unwrap()
        .remove(0)
        .1;
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    let machine = Machine::makera_z1();
    let ids = ops_of(&tps[0]);
    let ex = splice(&tps, &|k| ids.contains(&k), "flat").unwrap();
    let skip_title = |s: &str| s.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(skip_title(&post(&tps[0], &machine)), skip_title(&post(&ex, &machine)));
    let holes = ids[2];
    let dome = ops_of(&tps[1])[0];
    let ex = splice(&tps, &|k| k == holes || k == dome, "holes+dome").unwrap();
    assert_eq!(ops_of(&ex), vec![holes, dome]);
    let g = post(&ex, &machine);
    let issues = verify_gcode(&g, &limits_for(&ex, &machine));
    assert!(issues.is_empty(), "{issues:?}");
    // between them: up to the safe Z before moving over
    let k = ex
        .moves
        .iter()
        .position(|m| matches!(m, Move::Operation { id, .. } if *id == dome))
        .unwrap();
    let safe = ex.safe_r;
    let up = ex.moves[k..]
        .iter()
        .position(|m| matches!(m, Move::Rapid3 { x: None, y: None, z: Some(z) } if *z >= safe - 1e-9));
    let over = ex.moves[k..]
        .iter()
        .position(|m| matches!(m, Move::Rapid3 { x: Some(_), .. } | Move::Line3 { .. }));
    assert!(up.is_some() && up < over, "{:?}", &ex.moves[k..k + 6]);
}
