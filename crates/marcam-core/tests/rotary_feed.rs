//! Simultaneous 4-axis moves (X, Z and A together): on every built-in 4th-axis machine, the
//! tool's tip runs over the part at the programmed feed. The posted G-code is read back the
//! way each controller reads F (degrees as length, surface speed with its radius pad, G93
//! inverse time), and each move's time is compared with the tip's path on the part (in the
//! part's frame, as it turns) over the programmed feed.

use marcam::machine::{feed_length, FeedMode};
use marcam::toolpath::{Builder, Toolpath};

const FEED: f64 = 600.0;

/// Moves that turn while X and the radius change: a helix, a ramp down while turning, a turn
/// alone, X alone, a long turn at a small radius and a short one at a large radius.
fn program(y_offset: f64) -> Toolpath {
    let mut b = Builder::new();
    b.tool_change(1, 12000);
    b.rapid_r(20.0);
    if y_offset != 0.0 {
        // (rotary parallel's tool offset in Y: a 3.175 ball, off the axis)
        b.rapid(Some(-30.0), None, Some(0.0));
        b.moves.push(marcam::Move::YOffset {
            y: y_offset,
            ball_r: 1.5875,
        });
    }
    b.rapid(Some(-30.0), None, Some(0.0));
    b.rapid_r(12.0);
    b.plunge(10.0, 100.0);
    b.cut(-28.0, 10.0, 90.0, FEED, FEED); // X + A
    b.cut(-27.0, 9.0, 120.0, FEED, FEED); // X + Z + A
    b.cut(-27.0, 9.0, 300.0, FEED, FEED); // A alone
    b.cut(-20.0, 9.0, 300.0, FEED, FEED); // X alone
    b.cut(-19.0, 3.0, 340.0, FEED, FEED); // down to a small radius, turning
    b.cut(-18.5, 3.0, 700.0, FEED, FEED); // a turn at radius 3
    b.cut(-18.0, 20.0, 702.0, FEED, FEED); // up to radius 20, a little turn
    b.cut(-10.0, 20.0, 760.0, FEED, FEED); // X + A at radius 20
    b.rapid_r(25.0);
    if y_offset != 0.0 {
        b.moves.push(marcam::Move::YOffset {
            y: 0.0,
            ball_r: 1.5875,
        });
    }
    Toolpath {
        name: "feeds".into(),
        title: "feeds".into(),
        tool: Some((1, "ball".into())),
        notes: Vec::new(),
        reset_a: true,
        safe_r: 25.0,
        moves: b.moves,
        origin_comment: None,
        plunge_limit: None,
        ramp_angle: Some(90.0),
        post: None,
    }
}

/// The tip's path on the part from machine (x0, y0, z0) at A a0 to (x1, y1, z1) at a1, all
/// moving linearly: its length in the part's frame (mm).
fn tip_path(p0: [f64; 4], p1: [f64; 4]) -> f64 {
    let n = 2000;
    let at = |t: f64| {
        let q: Vec<f64> = (0..4).map(|k| p0[k] + (p1[k] - p0[k]) * t).collect();
        // part = Rx(-A) machine
        marcam::kinematics::to_part([q[0], q[1], q[2]], q[3])
    };
    let mut len = 0.0;
    let mut q = at(0.0);
    for i in 1..=n {
        let p = at(i as f64 / n as f64);
        len += ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
        q = p;
    }
    len
}

/// Each feed move of `g` along X or round A as the controller reads it: (its time in
/// minutes, the tip's path on the part in mm, the line).
fn read_back(g: &str, mode: FeedMode, pad: f64) -> Vec<(f64, f64, String)> {
    let (mut x, mut y, mut z, mut a, mut f) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut inverse = false;
    let mut out = Vec::new();
    for raw in g.lines() {
        let line = raw.split(['(', ';']).next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<(char, f64)> = line
            .split_whitespace()
            .filter_map(|w| {
                let c = w.chars().next()?;
                Some((c.to_ascii_uppercase(), w[1..].parse().ok()?))
            })
            .collect();
        let g_of = |n: f64| words.iter().any(|&(c, v)| c == 'G' && (v - n).abs() < 1e-9);
        if g_of(93.0) {
            inverse = true;
        }
        if g_of(94.0) {
            inverse = false;
        }
        let (mut nx, mut ny, mut nz, mut na) = (x, y, z, a);
        let mut moved = false;
        for &(c, v) in &words {
            match c {
                'X' => (nx, moved) = (v, true),
                'Y' => (ny, moved) = (v, true),
                'Z' => (nz, moved) = (v, true),
                'A' => (na, moved) = (v, true),
                'F' => f = v,
                _ => {}
            }
        }
        let feed_move = g_of(1.0) || (!g_of(0.0) && moved && words.iter().all(|w| w.0 != 'G'));
        if moved && feed_move && f > 0.0 {
            let d = [nx - x, ny - y, nz - z, na - a];
            let time = if inverse || mode == FeedMode::InverseTime {
                1.0 / f
            } else {
                let radius = 0.5 * (y.hypot(z) + ny.hypot(nz));
                feed_length(mode, pad, d, radius) / f
            };
            let path = tip_path([x, y, z, a], [nx, ny, nz, na]);
            // (the plunge, down at its own feed, left out: off the axis A turns a little
            // with it)
            let plunge = (nx - x).abs() < 1e-9 && (na - a).abs() < 5.0 && nz < z - 0.5;
            if !plunge && ((nx - x).abs() > 1e-9 || (na - a).abs() > 1e-9) {
                out.push((time, path, line.to_string()));
            }
        }
        (x, y, z, a) = (nx, ny, nz, na);
    }
    out
}

#[test]
fn simultaneous_x_z_a_moves_run_at_the_programmed_feed_on_every_4th_axis_machine() {
    let mut seen = 0;
    for y in [0.0, 0.8] {
        let tp = program(y);
        for id in [
            "makera-z1-4axis",
            "makera-carvera-4axis",
            "makera-carvera-air-4axis",
            "linuxcnc-4axis",
        ] {
            let profile = marcam::profiles::builtin_machine(id).unwrap();
            let m = &profile.post;
            let g = marcam::post(&tp, m);
            let moves = read_back(&g, m.feed_mode, m.surface_radius_pad);
            // the cuts at the cutting feed (not the plunge): over them all, and each one
            // long enough for its printed digits (off the axis they come 2 degrees at a time)
            let (t, l) = moves
                .iter()
                .fold((0.0, 0.0), |(t, l), m| (t + m.0, l + m.1));
            assert!(
                (l / t - FEED).abs() <= 0.01 * FEED,
                "{id}, Y {y}: the tip at {:.1} mm/min on the part overall",
                l / t
            );
            let cuts: Vec<_> = moves.iter().filter(|(_, path, _)| *path > 0.3).collect();
            assert!(cuts.len() >= 8, "{id}: {} cuts\n{g}", cuts.len());
            assert_eq!(g.contains(" Y0.800"), y != 0.0, "{id}");
            for (time, path, line) in cuts {
                let speed = path / time;
                assert!(
                    (speed - FEED).abs() <= 0.02 * FEED,
                    "{id}: the tip at {speed:.1} mm/min on the part, not {FEED}: {line}"
                );
            }
            seen += 1;
        }
    }
    assert_eq!(seen, 8);
}
