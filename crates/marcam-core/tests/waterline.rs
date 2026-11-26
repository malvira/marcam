//! 3D waterline finishing: the part's steep walls cut round at constant heights. On the
//! plate's dome (a sphere of radius 6.5 centred at (46, 20, -7), cut off at Z -4.5) with a
//! 3 mm ball: every point is where the ball rests on the dome at that height, nothing
//! shallower than the minimum slope is cut, nothing is gouged, the G-code verifies clean.

use marcam::job::{Operation, Raster3d};
use marcam::{limits_for, load_mesh, plan_with_mesh, post, verify_gcode, Job, Move};
use std::path::PathBuf;

fn plate() -> (Job, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plate");
    let text = std::fs::read_to_string(dir.join("plate.marcam.toml")).unwrap();
    let libs = marcam::profiles::builtin_libraries();
    let mut jobs =
        marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&dir).ok()).unwrap();
    (jobs.remove(0).1, dir)
}

/// The plate with its dome's raster replaced by a waterline (`min_slope`).
fn waterline_job(min_slope: f64) -> (Job, PathBuf) {
    let (mut job, dir) = plate();
    for p in &mut job.programs {
        for o in &mut p.operations {
            if let Operation::Raster3d(r) = o {
                let mut w: Raster3d = r.clone();
                w.x_range = [36.0, 56.0];
                w.y_range = [10.0, 30.0];
                w.step_down = 0.3;
                w.sample = 0.1;
                w.leave_z = 0.0;
                w.min_slope = min_slope;
                w.lead_in = 0.0;
                *o = Operation::Waterline(w);
            }
        }
    }
    (job, dir)
}

#[test]
fn the_dome_is_cut_round_at_constant_heights_on_its_steep_part() {
    let (job, dir) = waterline_job(30.0);
    let mesh = load_mesh(&job, &dir).unwrap();
    let tps = plan_with_mesh(&job, mesh.as_ref()).unwrap();
    // the cuts of the waterline
    let mut on = false;
    let mut pts = Vec::new();
    let mut at: [Option<f64>; 3] = [None; 3];
    for tp in &tps {
        for m in &tp.moves {
            if let Move::Operation { kind, .. } = m {
                on = kind == "waterline";
            }
            // (not a feed straight down: the waterline on the uncut block comes down into
            // it by feed where a rapid would have gone into the stock)
            match *m {
                Move::Rapid3 { x, y, z } => {
                    at = [x.or(at[0]), y.or(at[1]), z.or(at[2])];
                }
                Move::Line3 { x, y, z, .. } => {
                    let down = at[0].is_some_and(|a| (a - x).abs() < 1e-9)
                        && at[1].is_some_and(|b| (b - y).abs() < 1e-9)
                        && at[2].is_some_and(|c| z < c);
                    if on && !down {
                        pts.push([x, y, z]);
                    }
                    at = [Some(x), Some(y), Some(z)];
                }
                _ => {}
            }
        }
    }
    assert!(pts.len() > 100, "{} points", pts.len());
    // the ball (r 1.5) rests on the sphere: its centre 8 from the dome's, its tip 1.5 below
    // that; nothing shallower than 30° (within 4 of the axis on that sphere)
    let mut worst: f64 = 0.0;
    for p in &pts {
        let rho = (p[0] - 46.0).hypot(p[1] - 20.0);
        let centre = p[2] + 1.5;
        // (on the sphere: above the dome's base at Z-4.5, where the ball rests on its rim)
        if p[2] > -4.4 && rho < 7.5 {
            let want = -7.0 + (64.0 - rho * rho).max(0.0).sqrt();
            worst = worst.max((centre - want).abs());
            assert!(
                rho > 3.9,
                "cut where the dome is shallow: rho {rho:.2} at {p:?}"
            );
        }
    }
    // (the STL's facets lie up to about 0.08 inside the true sphere)
    assert!(worst < 0.1, "off the surface by {worst:.3} mm");
    // climb, spindle clockwise: the wall on the tool's right, round the dome clockwise
    let turn: f64 = pts
        .windows(2)
        .filter(|w| (w[0][2] - w[1][2]).abs() < 1e-9)
        .map(|w| (w[0][0] - 46.0) * (w[1][1] - 20.0) - (w[1][0] - 46.0) * (w[0][1] - 20.0))
        .sum();
    assert!(turn < 0.0, "round the dome counter-clockwise ({turn:.0})");
    // not gouged; the G-code clean
    let g = marcam::gouge_check(&job, &tps, mesh.as_ref()).unwrap();
    assert!(g.iter().all(|r| r.max_depth < 0.02), "{g:?}");
    let profile = marcam::profiles::builtin_machine("makera-carvera-4axis").unwrap();
    for tp in &tps {
        let issues = verify_gcode(&post(tp, &profile.post), &limits_for(tp, &profile.post));
        assert!(
            issues.is_empty(),
            "{}: {:?}",
            tp.name,
            issues.iter().map(|i| i.to_string()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn without_a_minimum_slope_the_shallow_top_is_cut_too() {
    let (job, dir) = waterline_job(0.0);
    let mesh = load_mesh(&job, &dir).unwrap();
    let tps = plan_with_mesh(&job, mesh.as_ref()).unwrap();
    let mut on = false;
    let near_top = tps
        .iter()
        .flat_map(|tp| &tp.moves)
        .filter(|m| {
            if let Move::Operation { kind, .. } = m {
                on = kind == "waterline";
            }
            matches!(m, Move::Line3 { x, y, .. } if on && (x - 46.0).hypot(y - 20.0) < 3.0)
        })
        .count();
    assert!(near_top > 0);
}

/// How far operation `kind` cuts (mm along its feed moves) in `tps`.
fn cut_length(tps: &[marcam::Toolpath], kind: &str) -> f64 {
    let mut on = false;
    let mut at: Option<[f64; 3]> = None;
    let mut len = 0.0;
    for tp in tps {
        for m in &tp.moves {
            if let Move::Operation { kind: k, .. } = m {
                on = k == kind;
            }
            let to = match *m {
                Move::Line3 { x, y, z, .. } => Some([x, y, z]),
                Move::Rapid3 { x, y, z } => {
                    at.map(|a| [x.unwrap_or(a[0]), y.unwrap_or(a[1]), z.unwrap_or(a[2])])
                }
                _ => None,
            };
            if let (true, Some(a), Some(b), Move::Line3 { .. }) = (on, at, to, m) {
                len +=
                    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
            }
            if to.is_some() {
                at = to;
            }
        }
    }
    len
}

#[test]
fn a_rest_finish_cuts_only_where_the_larger_ball_could_not_reach() {
    // the dome finished with a 6 mm ball (T7), then the 3 mm ball over it: all of it, or only
    // where the 6 mm one could not go as deep
    let make = |rest: bool| {
        let (mut job, dir) = plate();
        let mut big = job
            .tools
            .iter()
            .find(|t| matches!(t.shape, marcam::job::ToolShape::Ball { .. }))
            .unwrap()
            .clone();
        let small_no = big.number;
        big.number = 7;
        big.shape = marcam::job::ToolShape::Ball { diameter: 6.0 };
        job.tools.push(big);
        for p in &mut job.programs {
            for o in &mut p.operations {
                if let Operation::Raster3d(r) = o {
                    r.tool = small_no;
                    r.lead_in = 0.0;
                    if rest {
                        r.rest_tool = 7;
                    }
                }
            }
        }
        let mesh = load_mesh(&job, &dir).unwrap();
        let tps = plan_with_mesh(&job, mesh.as_ref()).unwrap();
        (job, mesh, tps)
    };
    let (_, _, full) = make(false);
    let (job, mesh, rest) = make(true);
    let (all, left) = (cut_length(&full, "raster3d"), cut_length(&rest, "raster3d"));
    // the 6 mm ball cannot reach the corner where the dome meets the floor (Z-4.5): a ring
    // round its foot; its top and the floor away from it it finishes itself
    assert!(left > 0.0, "the 6 mm ball leaves corners to the 3 mm one");
    assert!(left < all, "rest {left:.0} mm of the full {all:.0} mm");
    let mut on = false;
    for m in rest.iter().flat_map(|tp| &tp.moves) {
        if let Move::Operation { kind, .. } = m {
            on = kind == "raster3d";
        }
        if let (true, Move::Line3 { x, y, z, .. }) = (on, m) {
            let rho = (x - 46.0).hypot(y - 20.0);
            if *z < 0.0 {
                assert!(
                    rho > 3.5 && rho < 10.0,
                    "a rest cut at rho {rho:.2} (z {z:.2})"
                );
            }
        }
    }
    let g = marcam::gouge_check(&job, &rest, mesh.as_ref()).unwrap();
    assert!(g.iter().all(|r| r.max_depth < 0.02), "{g:?}");
}
