//! The simulation's rapids through stock, confirmed in 3D: the radial map keeps only the
//! stock's highest point along each ray from the axis, so a rapid through a pocket cut from
//! the side (under stock that still stands over it) reads as through stock when it is not
//! (Mar's impeller: 9 in red, none in 3D). A rapid through stock that is really there is
//! still counted.

use marcam::job::Job;
use marcam::toolpath::{Builder, Toolpath};
use marcam::Move;
use std::path::PathBuf;

fn knob() -> Job {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("examples/knob/knob.job.json")).unwrap();
    Job::from_json(&text).unwrap()
}

/// The tool upright at machine (x, y, z) with the part at A `a`, as a pose.
fn upright(x: f64, y: f64, z: f64, a: f64, feed: Option<f64>) -> Move {
    Move::Pose {
        tip: marcam::kinematics::to_part([x, y, z], a),
        axis: marcam::kinematics::to_part([0.0, 0.0, 1.0], a),
        feed,
    }
}

/// A slot cut down beside the axis (the tool 8 mm off it, its tip 3 mm up, along X from -20
/// to -10): under the bar's stock that still stands over it seen from the axis. Then a rapid
/// to X `to` with the tool `dy` off the slot's line: back along the slot (clear), back along
/// it but a few hundredths into its wall (a graze), into its wall by a tenth, or on along the
/// bar where nothing was cut (through it).
fn program(to: f64, dy: f64) -> Toolpath {
    let mut b = Builder::new();
    b.tool_change(1, 12000);
    b.moves.push(upright(-20.0, 8.0, 30.0, 0.0, None));
    b.moves.push(upright(-20.0, 8.0, 3.0, 0.0, Some(60.0)));
    b.moves.push(upright(-10.0, 8.0, 3.0, 0.0, Some(300.0)));
    // (over to the rapid's line at feed: only the rapid along it is looked at)
    b.moves
        .push(upright(-10.0, 8.0 + dy, 3.0, 0.0, Some(300.0)));
    b.moves.push(upright(to, 8.0 + dy, 3.0, 0.0, None));
    b.moves.push(upright(to, 8.0 + dy, 30.0, 0.0, None));
    Toolpath {
        name: "p".into(),
        title: "p".into(),
        tool: Some((1, "flat".into())),
        notes: Vec::new(),
        reset_a: true,
        safe_r: 30.0,
        moves: b.moves,
        origin_comment: None,
        plunge_limit: None,
        ramp_angle: None,
        post: None,
    }
}

#[test]
fn a_rapid_in_a_slot_cut_from_the_side_is_clear_one_through_the_bar_is_not() {
    let job = knob();
    // the radial map alone flags both
    let radial = |tp: &Toolpath| {
        let grid = marcam::sim::Grid::new(-40.0, 2.0, 0.1, 240);
        let mut stock = marcam::sim::RadialMap::of_job(grid, &job);
        let cutters = |t: u32| {
            job.tool(t)
                .and_then(|d| marcam::cutter_for(t, d.shape).ok())
        };
        marcam::sim::simulate(&mut stock, tp, &cutters, 0.1)
            .rapid_hits
            .len()
    };
    assert_eq!(
        radial(&program(-19.0, 0.0)),
        1,
        "the radial map sees the slot's rapid in stock"
    );
    assert_eq!(radial(&program(-2.0, 0.0)), 1);
    // confirmed in 3D: the one in the slot is clear, one a few hundredths into its wall a
    // graze (said, in yellow, not counted as clear), one a tenth into it or on through the
    // bar through stock
    let sim = |tp: Toolpath| {
        let s = marcam::simulate_job(&job, &[tp], None, 0.1, 240, 0.02).unwrap();
        let st = &s.programs[0].1;
        (st.rapid_hits.len(), st.rapid_grazes.len())
    };
    assert_eq!(
        sim(program(-19.0, 0.0)),
        (0, 0),
        "back along the slot it cut"
    );
    assert_eq!(
        sim(program(-19.0, 0.03)),
        (0, 1),
        "0.03 mm into the slot's wall"
    );
    assert_eq!(
        sim(program(-19.0, -0.03)),
        (0, 1),
        "0.03 mm into its other wall"
    );
    assert_eq!(sim(program(-19.0, 0.12)), (1, 0), "0.12 mm into its wall");
    assert_eq!(sim(program(-2.0, 0.0)).0, 1, "on through the bar");
}

#[test]
fn the_gpus_flagged_rapids_are_confirmed_the_same_way_with_their_moves() {
    // (the GPU's simulation has no 3D replay: its red fold had a count and no entries) the
    // rapid the radial map flags, by its segment of the job: through the bar, confirmed with
    // its move; back along the slot, cleared
    let job = knob();
    let flag = |tp: &Toolpath| {
        let segs = marcam::sim::segments(std::slice::from_ref(tp));
        let k = segs
            .iter()
            .rposition(|s| {
                s.rapid && s.to[0] < -1.0 && s.from.is_some_and(|f| (f[0] - s.to[0]).abs() > 1.0)
            })
            .unwrap();
        (k, segs[k].move_index)
    };
    let tp = program(-2.0, 0.0);
    let (k, mv) = flag(&tp);
    let out = marcam::confirm_gpu(&job, std::slice::from_ref(&tp), &[(k, 2)]);
    assert_eq!(
        out[0].0,
        vec![mv],
        "through the bar: confirmed, with its move"
    );
    let tp = program(-19.0, 0.0);
    let (k, _) = flag(&tp);
    let out = marcam::confirm_gpu(&job, std::slice::from_ref(&tp), &[(k, 2)]);
    assert!(
        out[0].0.is_empty() && out[0].1.is_empty(),
        "back along the slot: clear"
    );
}
