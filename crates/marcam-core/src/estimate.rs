//! Machining time from G-code text and a machine profile, for files marcam made or any other.
//!
//! Each motion line becomes a block. Its nominal time comes from F as the controller reads
//! it: G94 feeds as degrees-as-length (A degrees count like mm), linear-only (A degrees do
//! not count unless the move is pure rotation) or surface speed (the rotation counted as its
//! arc at the tool's distance from the axis plus the profile's pad), G93 as inverse time.
//! That time is capped by
//! each axis's top feed (and the controller's feed cap). Rapids run at the slowest axis's
//! rapid rate. A look-ahead planner then gives each block an entry and exit speed:
//! - junction speeds come from the junction-deviation rule (as in Grbl);
//! - acceleration is each axis's limit projected on the move;
//! - stops come at dwells, tool changes, spindle commands and the program's ends.
//!
//! Each block is a trapezoid (or triangle) at that acceleration. Dwells (`G4 P` seconds),
//! tool changes (`M6`) and spindle starts (`M3`/`M4`) add their times.
//!
//! The defaults for profiles without timing data are in [`Dynamics::from_profile`]. They
//! are estimates, not measurements.

use crate::det::Det;
use crate::machine::FeedMode;
use crate::profiles::MachineProfile;
use crate::verify::words;
use serde::Serialize;

/// Speeds and accelerations the estimate uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dynamics {
    /// X, Y, Z, A: top feed (mm/min, deg/min), rapid (mm/min, deg/min), acceleration
    /// (mm/s², deg/s²).
    pub max_feed: [f64; 4],
    pub rapid: [f64; 4],
    pub accel: [f64; 4],
    /// Controller feed cap (mm/min).
    pub feed_cap: f64,
    pub feed_mode: FeedMode,
    /// `surface_speed` radius pad (mm).
    pub surface_radius_pad: f64,
    /// Junction deviation (mm).
    pub junction_deviation: f64,
    pub tool_change_s: f64,
    pub spindle_start_s: f64,
}

impl Dynamics {
    /// From a profile; missing numbers take these defaults:
    /// - linear axes: 5000 mm/min feed, rapids at the feed, 500 mm/s²;
    /// - A axis: 21600 deg/min, 3600 deg/s²;
    /// - junction deviation 0.02 mm;
    /// - tool change 20 s (ATC) or 60 s (manual);
    /// - spindle start 0 s (the posts dwell explicitly).
    pub fn from_profile(p: &MachineProfile) -> Dynamics {
        let mut d = Dynamics {
            max_feed: [5000.0, 5000.0, 5000.0, 21600.0],
            rapid: [5000.0, 5000.0, 5000.0, 21600.0],
            accel: [500.0, 500.0, 500.0, 3600.0],
            feed_cap: p.max_feed.unwrap_or(f64::INFINITY),
            feed_mode: p.post.feed_mode,
            surface_radius_pad: p.post.surface_radius_pad,
            junction_deviation: 0.02,
            tool_change_s: match p.tool_changer.as_ref().map(|t| t.kind.as_str()) {
                Some("atc") => 20.0,
                _ => 60.0,
            },
            spindle_start_s: 0.0,
        };
        for (k, l) in ["X", "Y", "Z"].iter().enumerate() {
            if let Some(a) = p.kinematics.linear(l) {
                if let Some(f) = a.max_feed {
                    d.max_feed[k] = f;
                    d.rapid[k] = f;
                }
                if let Some(r) = a.max_rapid {
                    d.rapid[k] = r;
                }
                if let Some(acc) = a.accel {
                    d.accel[k] = acc;
                }
            }
        }
        if let Some(a) = p.kinematics.rotary_about_x() {
            if let Some(f) = a.max_feed {
                d.max_feed[3] = f;
                d.rapid[3] = f;
            }
            if let Some(r) = a.max_rapid {
                d.rapid[3] = r;
            }
            if let Some(acc) = a.accel {
                d.accel[3] = acc;
            }
        }
        if let Some(t) = &p.timing {
            if let Some(v) = t.junction_deviation {
                d.junction_deviation = v;
            }
            if let Some(v) = t.tool_change_s {
                d.tool_change_s = v;
            }
            if let Some(v) = t.spindle_start_s {
                d.spindle_start_s = v;
            }
        }
        d
    }
}

/// The estimate for one program.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    /// Seconds in total, cutting (G1/G2/G3), rapids (G0), and the rest (dwells, tool
    /// changes, spindle starts).
    pub total_s: f64,
    pub feed_s: f64,
    pub rapid_s: f64,
    pub other_s: f64,
    /// Distances (mm; A degrees counted as mm where the feed mode does).
    pub feed_mm: f64,
    pub rapid_mm: f64,
    /// Time of each line (same order as the text).
    #[serde(skip)]
    pub per_line: Vec<f64>,
    /// Things the estimate assumed or skipped.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct Block {
    line: usize,
    rapid: bool,
    /// Path length in planner units, its unit vector over X, Y, Z, A.
    len: f64,
    dir: [f64; 4],
    /// Cruise speed and acceleration (units/s, units/s²).
    v: f64,
    a: f64,
    /// Stop before this block (dwell, tool change, spindle).
    stop_before: bool,
}

/// Estimate the machining time of G-code text.
pub fn estimate_gcode(text: &str, dy: &Dynamics) -> Estimate {
    let mut est = Estimate {
        total_s: 0.0,
        feed_s: 0.0,
        rapid_s: 0.0,
        other_s: 0.0,
        feed_mm: 0.0,
        rapid_mm: 0.0,
        per_line: vec![0.0; text.lines().count()],
        notes: Vec::new(),
    };
    let mut pos: [Option<f64>; 4] = [None; 4];
    let mut motion = 0u32;
    let (mut inverse, mut absolute, mut scale) = (false, true, 1.0f64);
    let mut feed: Option<f64> = None;
    let mut blocks: Vec<Block> = Vec::new();
    let mut stop = true;
    let note = |est: &mut Estimate, s: &str| {
        if !est.notes.iter().any(|n| n == s) {
            est.notes.push(s.to_string());
        }
    };
    for (li, line) in crate::verify::expand_cycles(text) {
        let w = words(&line);
        if w.is_empty() {
            continue;
        }
        let mut target: [Option<f64>; 4] = [None; 4];
        let (mut ij, mut dwell, mut g92) = ([0.0f64, 0.0f64], None::<f64>, false);
        let mut has_motion_word = false;
        for &(c, v) in &w {
            match c {
                'G' => match (v * 10.0).round() as i64 {
                    0 | 10 | 20 | 30 => {
                        motion = (v.round()) as u32;
                        has_motion_word = true;
                    }
                    40 => dwell = Some(0.0),
                    200 => scale = 25.4,
                    210 => scale = 1.0,
                    900 => absolute = true,
                    910 => absolute = false,
                    920 => g92 = true,
                    930 => inverse = true,
                    940 => inverse = false,
                    _ => {}
                },
                'X' => target[0] = Some(v * scale),
                'Y' => target[1] = Some(v * scale),
                'Z' => target[2] = Some(v * scale),
                'A' => target[3] = Some(v),
                'I' => ij[0] = v * scale,
                'J' => ij[1] = v * scale,
                'F' => feed = Some(if inverse { v } else { v * scale }),
                'P' if dwell.is_some() => dwell = Some(v),
                'M' => match v.round() as i64 {
                    6 => {
                        est.per_line[li] += dy.tool_change_s;
                        est.other_s += dy.tool_change_s;
                        stop = true;
                    }
                    3 | 4 => {
                        est.per_line[li] += dy.spindle_start_s;
                        est.other_s += dy.spindle_start_s;
                        stop = true;
                    }
                    0 | 1 | 2 | 5 | 30 => stop = true,
                    _ => {}
                },
                _ => {}
            }
        }
        let _ = has_motion_word;
        if let Some(p) = dwell {
            est.per_line[li] += p;
            est.other_s += p;
            stop = true;
            continue;
        }
        if g92 {
            for k in 0..4 {
                if target[k].is_some() {
                    pos[k] = target[k];
                }
            }
            continue;
        }
        if target.iter().all(Option::is_none) {
            continue;
        }
        // resolve the target; unknown start positions do not move
        let mut d = [0.0f64; 4];
        let mut end = pos;
        for k in 0..4 {
            if let Some(t) = target[k] {
                let to = if absolute {
                    t
                } else {
                    pos[k].unwrap_or(0.0) + t
                };
                if let Some(p) = pos[k] {
                    d[k] = to - p;
                }
                end[k] = Some(to);
            }
        }
        // arcs in XY (G17): the arc length replaces the chord's XY part
        let mut xy_len = d[0].dhypot(d[1]);
        if motion == 2 || motion == 3 {
            if let (Some(x0), Some(y0)) = (pos[0], pos[1]) {
                let (cx, cy) = (x0 + ij[0], y0 + ij[1]);
                let r = ij[0].dhypot(ij[1]);
                let a0 = (y0 - cy).datan2(x0 - cx);
                let a1 = (y0 + d[1] - cy).datan2(x0 + d[0] - cx);
                let mut sweep = a1 - a0;
                if motion == 2 && sweep >= 0.0 {
                    sweep -= std::f64::consts::TAU;
                } else if motion == 3 && sweep <= 0.0 {
                    sweep += std::f64::consts::TAU;
                }
                xy_len = r * sweep.abs();
                note(
                    &mut est,
                    "arcs (G2/G3) timed along their length, without look-ahead inside the arc",
                );
            }
        }
        // the tool's mean distance from the rotary axis (surface-speed feeds)
        let yz = |p: &[Option<f64>; 4]| p[1].unwrap_or(0.0).dhypot(p[2].unwrap_or(0.0));
        let radius = (yz(&pos) + yz(&end)) / 2.0;
        pos = end;
        let lin = (xy_len * xy_len + d[2] * d[2]).sqrt();
        let len = (lin * lin + d[3] * d[3]).sqrt();
        if len < 1e-9 {
            continue;
        }
        let dir = [d[0] / len, d[1] / len, d[2] / len, d[3] / len];
        let rapid = motion == 0;
        // nominal time (s): F as the controller reads it, then the axis limits
        let mut t = if rapid {
            0.0
        } else {
            match feed.filter(|f| *f > 0.0) {
                None => {
                    note(
                        &mut est,
                        "feed moves before any F are timed at the axes' top feed",
                    );
                    0.0
                }
                Some(f) if inverse => 60.0 / f,
                Some(f) => {
                    let f = f.min(dy.feed_cap);
                    match dy.feed_mode {
                        FeedMode::LinearOnly if lin > 1e-9 => 60.0 * lin / f,
                        FeedMode::SurfaceSpeed => {
                            let arc = (radius + dy.surface_radius_pad) * d[3].to_radians().abs();
                            60.0 * (lin * lin + arc * arc).sqrt() / f
                        }
                        _ => 60.0 * len / f,
                    }
                }
            }
        };
        let lims = if rapid { &dy.rapid } else { &dy.max_feed };
        for (dk, lim) in d.iter().zip(lims) {
            if *dk != 0.0 && *lim > 0.0 {
                t = t.max(60.0 * dk.abs() / lim);
            }
        }
        if t <= 0.0 {
            continue;
        }
        // acceleration along the move: each axis's limit over its share of the path
        let a = (0..4)
            .filter(|&k| dir[k].abs() > 1e-12)
            .map(|k| dy.accel[k] / dir[k].abs())
            .fold(f64::INFINITY, f64::min);
        if rapid {
            est.rapid_mm += len;
        } else {
            est.feed_mm += len;
        }
        blocks.push(Block {
            line: li,
            rapid,
            len,
            dir,
            v: len / t,
            a,
            stop_before: stop,
        });
        stop = false;
    }
    // look-ahead: junction limits, then backward and forward passes
    let n = blocks.len();
    let mut entry = vec![0.0f64; n + 1];
    for i in 0..n {
        let b = &blocks[i];
        entry[i] = if i == 0 || b.stop_before {
            0.0
        } else {
            let p = &blocks[i - 1];
            let cos = -(0..4).map(|k| p.dir[k] * b.dir[k]).sum::<f64>();
            let vj = if cos <= -0.999_999 {
                f64::INFINITY // straight on
            } else if cos >= 0.999_999 {
                0.0 // reversal
            } else {
                let s = (0.5 * (1.0 - cos)).sqrt();
                (b.a.min(p.a) * dy.junction_deviation * s / (1.0 - s)).sqrt()
            };
            vj.min(b.v).min(p.v)
        };
    }
    for i in (0..n).rev() {
        let b = &blocks[i];
        entry[i] = entry[i].min((entry[i + 1].powi(2) + 2.0 * b.a * b.len).sqrt());
    }
    for i in 0..n {
        let b = &blocks[i];
        entry[i + 1] = entry[i + 1].min((entry[i].powi(2) + 2.0 * b.a * b.len).sqrt());
    }
    for (i, b) in blocks.iter().enumerate() {
        let t = trapezoid(b.len, entry[i], entry[i + 1], b.v, b.a);
        est.per_line[b.line] += t;
        if b.rapid {
            est.rapid_s += t;
        } else {
            est.feed_s += t;
        }
    }
    est.total_s = est.feed_s + est.rapid_s + est.other_s;
    est
}

/// Time to cover `len` starting at `v0` and ending at `v1`, cruising at most at `v`, with
/// acceleration `a`.
fn trapezoid(len: f64, v0: f64, v1: f64, v: f64, a: f64) -> f64 {
    if !a.is_finite() || a <= 0.0 {
        return len / v;
    }
    let (v0, v1) = (v0.min(v), v1.min(v));
    let (d0, d1) = ((v * v - v0 * v0) / (2.0 * a), (v * v - v1 * v1) / (2.0 * a));
    if d0 + d1 <= len {
        (v - v0) / a + (v - v1) / a + (len - d0 - d1) / v
    } else {
        let vp = ((2.0 * a * len + v0 * v0 + v1 * v1) / 2.0).sqrt();
        (vp - v0).max(0.0) / a + (vp - v1).max(0.0) / a
    }
}

/// Time of one planned program, per operation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProgramTime {
    pub estimate: Estimate,
    /// (operation id, kind, seconds) in planning order.
    pub ops: Vec<(u32, String, f64)>,
    /// Time outside any operation (preamble, postamble).
    pub overhead_s: f64,
}

/// Post `tp` for `profile` and estimate it, splitting the time by operation.
pub fn estimate_toolpath(tp: &crate::Toolpath, profile: &MachineProfile) -> ProgramTime {
    let (text, marks) = crate::post::post_marked(tp, &profile.post);
    let estimate = estimate_gcode(&text, &Dynamics::from_profile(profile));
    let mut ops = Vec::new();
    let mut overhead = 0.0;
    let first = marks.first().map_or(usize::MAX, |m| m.2);
    overhead += estimate.per_line[..first.min(estimate.per_line.len())]
        .iter()
        .sum::<f64>();
    for (k, (id, kind, start)) in marks.iter().enumerate() {
        let end = marks.get(k + 1).map_or(estimate.per_line.len(), |m| m.2);
        ops.push((
            *id,
            kind.clone(),
            estimate.per_line[*start..end].iter().sum(),
        ));
    }
    // the postamble after the last operation's moves is overhead too (it has none of its
    // own markers); it is counted in the last operation above, which is fine for a total
    ProgramTime {
        estimate,
        ops,
        overhead_s: overhead,
    }
}

/// "1 h 02 min", "12 min 05 s", "48 s".
pub fn format_duration(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    if s >= 3600 {
        // (the minutes rounded first, then split: 2 h 59 min 45 s is 3 h 00 min, not "2 h 60")
        let m = (s + 30) / 60;
        format!("{} h {:02} min", m / 60, m % 60)
    } else if s >= 60 {
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    /// Fast axes with the given acceleration (inf: none), degrees-as-length.
    fn dynamics(accel: f64) -> Dynamics {
        Dynamics {
            max_feed: [1e9; 4],
            rapid: [6000.0, 3000.0, 3000.0, 3600.0],
            accel: [accel; 4],
            feed_cap: f64::INFINITY,
            feed_mode: FeedMode::DegreesAsLength,
            surface_radius_pad: 0.0,
            junction_deviation: 0.02,
            tool_change_s: 30.0,
            spindle_start_s: 2.0,
        }
    }

    fn t(g: &str, d: &Dynamics) -> f64 {
        estimate_gcode(g, d).total_s
    }

    #[test]
    fn straight_feed_and_rapid() {
        let d = dynamics(f64::INFINITY);
        assert!((t("G0 X0 Y0\nG1 X100 F600\n", &d) - 10.0).abs() < 1e-9);
        // rapid: X at 100 mm/s and Y at 50 mm/s: 100 x 50 mm takes 1 s on each
        assert!((t("G0 X0 Y0\nG0 X100 Y50\n", &d) - 1.0).abs() < 1e-9);
        // with acceleration 100 mm/s²: 0.5 mm to reach 10 mm/s at each end
        let d = dynamics(100.0);
        assert!((t("G0 X0 Y0\nG1 X100 F600\n", &d) - 10.1).abs() < 1e-9);
        // a straight line split in two runs through the joint at full speed
        assert!((t("G0 X0 Y0\nG1 X50 F600\nG1 X100\n", &d) - 10.1).abs() < 1e-9);
    }

    #[test]
    fn short_moves_are_acceleration_limited() {
        // 1 mm at 100 mm/s with 100 mm/s²: a triangle, 2 sqrt(L / a) = 0.2 s
        let d = dynamics(100.0);
        assert!((t("G0 X0\nG1 X1 F6000\n", &d) - 0.2).abs() < 1e-9);
        // a right-angle corner slows the junction: slower than going straight on
        let corner = t("G0 X0 Y0\nG1 X10 F6000\nG1 Y10\n", &d);
        let straight = t("G0 X0 Y0\nG1 X10 F6000\nG1 X20\n", &d);
        assert!(corner > straight + 0.05, "{corner} vs {straight}");
        // and a reversal stops: two 10 mm triangles of 2 sqrt(10 / 100) s
        let rev = t("G0 X0\nG1 X10 F6000\nG1 X0\n", &d);
        assert!((rev - 4.0 * 0.1f64.sqrt()).abs() < 1e-9, "{rev}");
    }

    #[test]
    fn rotary_moves_in_each_feed_mode() {
        let mut d = dynamics(f64::INFINITY);
        // degrees-as-length: 90 degrees at F360 -> 15 s; X10 + A90 -> 90.55 "mm" at F600
        assert!((t("G0 X0 A0\nG1 A90 F360\n", &d) - 15.0).abs() < 1e-9);
        let both = t("G0 X0 A0\nG1 X10 A90 F600\n", &d);
        assert!((both - 60.0 * (100.0f64 + 8100.0).sqrt() / 600.0).abs() < 1e-9);
        // linear-only: the A part does not count while X moves; pure A uses deg/min
        d.feed_mode = FeedMode::LinearOnly;
        assert!((t("G0 X0 A0\nG1 X10 A90 F600\n", &d) - 1.0).abs() < 1e-9);
        assert!((t("G0 X0 A0\nG1 A90 F360\n", &d) - 15.0).abs() < 1e-9);
        // ... unless the A axis cannot turn that fast: 90 deg at 3600 deg/min takes 1.5 s
        d.max_feed[3] = 3600.0;
        assert!((t("G0 X0 A0\nG1 X10 A90 F600\n", &d) - 1.5).abs() < 1e-9);
        // inverse time: F2 = half a minute per move
        assert!((t("G0 X0 A0\nG93 G1 X10 A90 F2\n", &d) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn dwells_tool_changes_spindle_and_units() {
        let d = dynamics(f64::INFINITY);
        let e = estimate_gcode("M6 T1\nM3 S10000\nG4 P3\nG20\nG0 X0\nG1 X1 F60\n", &d);
        // 30 s change, 2 s spin-up, 3 s dwell, 1 inch at 60 in/min = 1 s
        assert!(
            (e.other_s - 35.0).abs() < 1e-9 && (e.feed_s - 1.0).abs() < 1e-9,
            "{e:?}"
        );
        assert_eq!(e.per_line[0], 30.0);
        assert!((format_duration(3725.0) == "1 h 02 min") && format_duration(65.0) == "1 min 05 s");
    }

    /// The minutes rounded before the hours are split off: the impeller's finish, 2 h 59 min
    /// 45 s, printed "2 h 60 min".
    #[test]
    fn durations_round_the_minutes_before_the_hours() {
        assert_eq!(format_duration(3.0 * 3600.0 - 15.0), "3 h 00 min");
        assert_eq!(format_duration(3.0 * 3600.0 - 31.0), "2 h 59 min");
        assert_eq!(format_duration(3600.0), "1 h 00 min");
        assert_eq!(format_duration(2.0 * 3600.0 + 29.0), "2 h 00 min");
        assert_eq!(format_duration(2.0 * 3600.0 + 30.0), "2 h 01 min");
        assert_eq!(format_duration(3599.0), "59 min 59 s");
    }

    #[test]
    fn arcs_use_their_length() {
        let d = dynamics(f64::INFINITY);
        // a half circle of radius 10 at F600 (10 mm/s): pi x 10 mm = 3.14 s
        let e = estimate_gcode("G0 X0 Y0\nG3 X20 Y0 I10 J0 F600\n", &d);
        assert!((e.feed_s - std::f64::consts::PI).abs() < 1e-9, "{e:?}");
    }
}
