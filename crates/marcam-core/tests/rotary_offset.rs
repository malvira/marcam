//! Rotary parallel's tool offset in Y: the ball off the axis, its tool upright, so it cuts
//! off its tip (where it turns at speed). The ball's centre goes where it would over the
//! axis: the same cut, A turned to match. The machine gets Y, and its tip runs at the feed.

use marcam::job::Job;
use marcam::kinematics::{to_machine, to_part};
use serde_json::{json, Value};
use std::path::PathBuf;

const R: f64 = 1.5875;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The knob with a 3.175 ball (T3) and one program: a rotary parallel spiral over its body,
/// `y` off the axis.
fn knob(y: f64, style: &str) -> Job {
    let text = std::fs::read_to_string(root().join("examples/knob/knob.job.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["tools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"number": 3, "description": "3.175 ball",
        "kind": "ball", "diameter": 3.175, "flute_length": 12.0}));
    v["programs"] = json!([{"name": "p", "title": "p", "operations": [{"type": "rotary_parallel",
        "tool": 3, "pattern": style, "x_range": [-20.0, -5.0], "stepover_mm": 1.0, "stepover_deg": 20.0,
        "feed": 600.0, "plunge_feed": 100.0, "smoothing": 0.0, "y_offset": y}]}]);
    Job::from_json(&v.to_string()).unwrap()
}

/// Each feed move's ball centre in the part's frame, read back from the G-code (X Y Z A, the
/// tool upright: its centre a radius above its tip).
fn centres(g: &str) -> Vec<[f64; 3]> {
    let (mut p, mut out) = ([0.0f64; 4], Vec::new());
    for line in g.lines() {
        let line = line.split('(').next().unwrap_or("").trim();
        let mut moved = false;
        for w in line.split_whitespace() {
            let k = match w.chars().next() {
                Some('X') => 0,
                Some('Y') => 1,
                Some('Z') => 2,
                Some('A') => 3,
                _ => continue,
            };
            if let Ok(v) = w[1..].parse::<f64>() {
                p[k] = v;
                moved = true;
            }
        }
        if moved && line.starts_with("G1") {
            out.push(to_part([p[0], p[1], p[2] + R], p[3]));
        }
    }
    out
}

#[test]
fn an_offset_ball_s_centre_goes_where_it_would_over_the_axis() {
    // the pose itself: the centre at the same place on the part, the tool upright
    for (x, r, a, y) in [
        (-3.0, 8.0, 30.0, 0.8),
        (1.0, 4.0, 400.0, -1.2),
        (0.0, 12.0, -50.0, 1.5),
    ] {
        let (tip, axis) = marcam::post::offset_pose(x, r, a, y, R);
        let c = [
            tip[0] + axis[0] * R,
            tip[1] + axis[1] * R,
            tip[2] + axis[2] * R,
        ];
        let want = to_part([x, 0.0, r + R], a);
        for k in 0..3 {
            assert!((c[k] - want[k]).abs() < 1e-9, "{c:?} {want:?}");
        }
        // upright at the A it is turned to, the centre y off the axis in Y
        let a1 = marcam::kinematics::solve_a(axis, a).unwrap();
        let m = to_machine(c, a1);
        assert!((m[1] - y).abs() < 1e-9 && to_machine(axis, a1)[2] > 0.999_999);
    }
}

#[test]
fn the_g_code_has_the_tool_off_the_axis_and_the_same_cut() {
    let z1 = marcam::profiles::builtin_machine("makera-z1-4axis").unwrap();
    for style in ["spiral", "line", "circular"] {
        let plain = marcam::plan(&knob(0.0, style)).unwrap();
        let off = marcam::plan(&knob(0.8, style)).unwrap();
        // planned the same: only the post puts it off the axis
        let g0 = marcam::post(&plain[0], &z1.post);
        let g1 = marcam::post(&off[0], &z1.post);
        let words = |g: &str| -> Vec<String> {
            g.lines()
                .map(|l| l.split('(').next().unwrap_or("").to_string())
                .filter(|l| l.contains(" Y"))
                .collect()
        };
        assert!(words(&g0).is_empty(), "{style}: Y over the axis");
        assert!(g1.contains(" Y0.800"), "{style}: no Y0.8");
        // back over the axis at the end
        let last_y = g1
            .lines()
            .rev()
            .filter_map(|l| {
                l.split('(')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .find(|w| w.starts_with('Y'))
                    .map(str::to_string)
            })
            .next()
            .unwrap();
        assert_eq!(
            last_y[1..].parse::<f64>().unwrap(),
            0.0,
            "{style}: {last_y}"
        );
        // the verifier reads it clean: rapids turning only at the safe height, no descent
        // faster than the plunge feed
        let issues = marcam::verify_gcode(&g1, &marcam::limits_for(&off[0], &z1.post));
        assert!(
            issues.is_empty(),
            "{style}: {:?}
{}",
            &issues[..issues.len().min(5)],
            issues.first().map_or(String::new(), |i| g1
                .lines()
                .skip(i.line.saturating_sub(4))
                .take(5)
                .collect::<Vec<_>>()
                .join(
                    "
"
                ))
        );
        let (c0, c1) = (centres(&g0), centres(&g1));
        assert!(c0.len() > 100, "{style}: {} moves", c0.len());
        assert_eq!(c0.len(), c1.len(), "{style}");
        let worst = c0
            .iter()
            .zip(&c1)
            .map(|(p, q)| {
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
            })
            .fold(0.0, f64::max);
        // (the printed digits: a thousandth on X, Y, Z and A at the radius)
        assert!(
            worst < 0.005,
            "{style}: a centre {worst:.4} mm from where it would be"
        );
    }
}

#[test]
fn an_offset_needs_a_ball_no_wider_than_it() {
    let mut j = knob(2.0, "spiral");
    assert!(marcam::plan(&j)
        .unwrap_err()
        .to_string()
        .contains("ball's radius"));
    if let marcam::job::Operation::RotaryParallel(o) = &mut j.programs[0].operations[0] {
        o.tool = 1;
        o.y_offset = 0.5;
    }
    assert!(marcam::plan(&j)
        .unwrap_err()
        .to_string()
        .contains("needs a ball"));
}

/// The holder and shank check sees an offset tool where the machine has it: upright beside
/// the axis. A fin of stock that the radial line misses but the upright shank passes through
/// is a hit with the offset, none without it.
#[test]
fn the_holder_check_sees_the_offset_tool_upright_beside_the_axis() {
    use marcam::collision::{simulate_rotary, ToolBody};
    use marcam::sim::{Grid, RadialMap};
    use marcam::toolpath::{Builder, Toolpath};
    let tool: marcam::job::Tool = serde_json::from_value(json!({"number": 1,
        "description": "3.175 ball, 6 mm shank", "kind": "ball", "diameter": 3.175, "flute_length": 12.0,
        "shank_diameter": 6.0}))
    .unwrap();
    let body = ToolBody::of(&tool);
    let cutter = marcam::cutter_for(1, tool.shape).unwrap();
    // a bar of radius 10 with a fin of stock out to 26 (below the holder, past the flutes) at part angles 100..108 degrees
    let grid = Grid::new(-20.0, 2.0, 0.3, 360);
    let mut stock = RadialMap::filled(grid, 10.0);
    for i in 0..grid.nx {
        for j in 0..grid.na {
            let phi = grid.phi(j).to_degrees();
            if (100.0..=108.0).contains(&phi) {
                stock.r[i * grid.na + j] = 26.0;
            }
        }
    }
    stock.top = 26.0;
    // the ball's centre at part angle 90 (A 0), 12.59 from the axis, along X; with and without
    // the tool 6 mm off the axis (upright there, its shank crossing the fin)
    let program = |y: f64| {
        let mut b = Builder::new();
        b.tool_change(1, 12000);
        b.rapid_r(40.0);
        b.rapid(Some(-12.0), None, Some(0.0));
        if y != 0.0 {
            b.moves.push(marcam::Move::YOffset { y, ball_r: R });
        }
        b.rapid_r(11.5);
        b.plunge(11.0, 100.0);
        b.cut(-6.0, 11.0, 0.0, 300.0, 300.0);
        b.rapid_r(40.0);
        if y != 0.0 {
            b.moves.push(marcam::Move::YOffset { y: 0.0, ball_r: R });
        }
        Toolpath {
            name: "p".into(),
            title: "p".into(),
            tool: Some((1, "ball".into())),
            notes: Vec::new(),
            reset_a: true,
            safe_r: 40.0,
            moves: b.moves,
            origin_comment: None,
            plunge_limit: None,
            ramp_angle: Some(30.0),
            post: None,
        }
    };
    let hits = |tp: &Toolpath| {
        let mut st = stock.clone();
        let (_, rep) = simulate_rotary(
            &mut st,
            tp,
            &|_| Some(cutter),
            &|_| Some(body),
            0.25,
            (26.0, -40.0, 0.0),
            true,
            None,
        );
        rep.count
    };
    assert_eq!(
        hits(&program(0.0)),
        0,
        "over the axis: the fin is beside its shank"
    );
    assert!(
        hits(&program(6.0)) > 0,
        "off the axis: its shank crosses the fin"
    );
}
