//! Reading G-code someone else wrote (a custom toolpath: an AI through MCP, a person's
//! Python script, another CAM) into steps marcam plans, simulates, checks and posts like its
//! own moves.
//!
//! What it reads: G0 G1 G2 G3 (arcs in G17/G18/G19, by I J K or R, helical too), G90 G91,
//! G20 G21, F, S, A, M3 M4 M5, T and M6, the work offsets G54..G59 (taken as the setup's
//! WCS), G4 (a dwell: no motion), the harmless modal codes (G40 G49 G64 G80 G94 G17..G19 and
//! the like), comments `( )` and `;`, block numbers `N`, `O` names and `%`. Everything else
//! is a warning with its line number and the line's motion is not read: nothing is dropped
//! silently.
//!
//! Frames: on a vise (3-axis) X Y Z are the setup's work coordinates (the tool tip). On the
//! 4th axis, as the posts write it: X along the bar, Z the radius from the axis, A degrees
//! about +X; Y must stay 0 (moves off the axis are refused).

use std::fmt::Write as _;

/// Which setup the program is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// A vise (or any 3-axis setup): X Y Z, the tool along +Z.
    Three,
    /// The 4th axis: X along the bar, Z the radius, A about +X.
    Rotary,
}

/// One motion read from the program: where it goes (every axis known so far, `None` until
/// first given), a rapid or a feed, and the line it came from (1-based).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line: usize,
    pub rapid: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub a: Option<f64>,
    /// mm/min (a feed move's; None for rapids, or a feed move before any F).
    pub feed: Option<f64>,
}

/// A program read: its motion, the tools and spindle speeds it names, and what it could not
/// read (`line N: ...`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub steps: Vec<Step>,
    /// T words, in order (each once).
    pub tools: Vec<u32>,
    /// S words, in order (each once).
    pub speeds: Vec<f64>,
    pub warnings: Vec<String>,
    /// Errors: the program can't be planned as it is (a move off the 4th axis, a feed move
    /// before the position is known, an arc that doesn't close).
    pub errors: Vec<String>,
    /// The program's lines, as given.
    pub lines: Vec<String>,
}

/// How finely arcs become lines: the most a chord strays from the arc (mm).
pub const ARC_TOL: f64 = 0.002;

#[derive(Clone, Copy, PartialEq)]
enum Plane {
    Xy,
    Zx,
    Yz,
}

/// Read `text` for `frame`.
pub fn read(text: &str, frame: Frame) -> Program {
    let mut p = Program {
        lines: text.lines().map(str::to_string).collect(),
        ..Program::default()
    };
    // modal state
    let mut motion: Option<u8> = None;
    let mut absolute = true;
    let mut inch = false;
    let mut plane = Plane::Xy;
    let mut feed: Option<f64> = None;
    let mut pos: [Option<f64>; 4] = [None; 4];
    let mut offset_noted = false;
    let mut ended = false;
    for (n, raw) in text.lines().enumerate() {
        let ln = n + 1;
        let (words, bad) = words(raw);
        if let Some(b) = bad {
            p.warnings.push(format!("line {ln}: {b}"));
            continue;
        }
        if words.is_empty() {
            continue;
        }
        if ended {
            p.warnings.push(format!(
                "line {ln}: after the program's end (M2/M30): not read"
            ));
            continue;
        }
        let mut axis: [Option<f64>; 4] = [None; 4];
        let (mut ijk, mut rword): ([Option<f64>; 3], Option<f64>) = ([None; 3], None);
        let mut skip_motion = false;
        let mut dwell = false;
        let mut new_motion: Option<u8> = None;
        // modal G codes first (units and distance mode apply to this line's words)
        for (l, v) in &words {
            if *l != 'G' {
                continue;
            }
            match g_code(*v) {
                Some(20) => inch = true,
                Some(21) => inch = false,
                Some(90) => absolute = true,
                Some(91) => absolute = false,
                Some(17) => plane = Plane::Xy,
                Some(18) => plane = Plane::Zx,
                Some(19) => plane = Plane::Yz,
                Some(c @ 0..=3) => new_motion = Some(c as u8),
                Some(4) => dwell = true,
                Some(54..=59) => {
                    if !offset_noted {
                        p.warnings.push(format!(
                            "line {ln}: G{} read as the setup's work offset (its WCS)",
                            v
                        ));
                        offset_noted = true;
                    }
                }
                // harmless: cancel cutter and length compensation, cycles; feed per minute;
                // path control; length compensation (the machine's lengths)
                Some(40 | 43 | 49 | 61 | 64 | 80 | 94) => {}
                _ => {
                    // (decimal codes like G90.1 and anything else: not read)
                    p.warnings.push(format!(
                        "line {ln}: G{} not supported: the line's motion is not read",
                        fmt_num(*v)
                    ));
                    skip_motion = true;
                }
            }
        }
        let scale = if inch { 25.4 } else { 1.0 };
        for (l, v) in &words {
            match l {
                'G' => {}
                'X' => axis[0] = Some(*v * scale),
                'Y' => axis[1] = Some(*v * scale),
                'Z' => axis[2] = Some(*v * scale),
                'A' => axis[3] = Some(*v),
                'I' => ijk[0] = Some(*v * scale),
                'J' => ijk[1] = Some(*v * scale),
                'K' => ijk[2] = Some(*v * scale),
                'R' => rword = Some(*v * scale),
                'F' => feed = Some(*v * scale),
                'S' => {
                    if !p.speeds.contains(v) {
                        p.speeds.push(*v);
                    }
                }
                'T' => {
                    let t = *v as u32;
                    if !p.tools.contains(&t) {
                        p.tools.push(t);
                    }
                }
                'M' => match *v as i64 {
                    // spindle on (M4: reverse, read as on), off, tool change, coolant,
                    // optional and program stops
                    3 | 5 | 6 | 7 | 8 | 9 | 0 | 1 => {}
                    4 => p
                        .warnings
                        .push(format!("line {ln}: M4 (reverse spindle) read as M3")),
                    2 | 30 => ended = true,
                    m => p
                        .warnings
                        .push(format!("line {ln}: M{m} not supported: ignored")),
                },
                'N' | 'O' | 'H' | 'D' => {}
                'P' if dwell => {}
                c => p.warnings.push(format!(
                    "line {ln}: word {c}{} not supported: ignored",
                    fmt_num(*v)
                )),
            }
        }
        if let Some(m) = new_motion {
            motion = Some(m);
        }
        let moves_axes = axis.iter().any(Option::is_some);
        if dwell || skip_motion || !moves_axes {
            if skip_motion && moves_axes {
                // (its axes not taken either: where the machine would be is unknown after it)
            }
            continue;
        }
        let Some(m) = motion else {
            p.warnings.push(format!(
                "line {ln}: axis words with no G0/G1/G2/G3 yet: not read"
            ));
            continue;
        };
        // the target
        let mut target = pos;
        for k in 0..4 {
            if let Some(v) = axis[k] {
                target[k] = Some(if absolute {
                    v
                } else {
                    match pos[k] {
                        Some(cur) => cur + v,
                        None => {
                            p.errors.push(format!(
                                "line {ln}: an incremental {} before its position is known",
                                ['X', 'Y', 'Z', 'A'][k]
                            ));
                            v
                        }
                    }
                });
            }
        }
        // (the 4th axis: the tool on the axis's plane, Y 0)
        if frame == Frame::Rotary {
            if let Some(y) = target[1] {
                if y.abs() > 1e-6 {
                    p.errors.push(format!(
                        "line {ln}: Y{} is off the 4th axis: a custom toolpath on it moves in X, Z (the radius) and A only",
                        fmt_num(y)
                    ));
                    continue;
                }
            }
        } else if let Some(a) = target[3] {
            if a.abs() > 1e-9 {
                p.errors.push(format!(
                    "line {ln}: A{} on a vise setup (no rotary axis)",
                    fmt_num(a)
                ));
                continue;
            }
        }
        match m {
            0 | 1 => {
                if m == 1 && feed.is_none() {
                    p.warnings.push(format!(
                        "line {ln}: G1 with no F yet: the operation's feed is used"
                    ));
                }
                p.steps
                    .push(step(ln, m == 0, target, if m == 1 { feed } else { None }));
            }
            _ => {
                if ijk.iter().all(Option::is_none) && rword.is_none() {
                    p.errors.push(format!("line {ln}: G{m} with no I/J/K or R"));
                    continue;
                }
                if axis[3].is_some() && target[3] != pos[3] {
                    p.errors
                        .push(format!("line {ln}: A changes along an arc: not supported"));
                    continue;
                }
                match arc(&pos, &target, plane, m == 2, ijk, rword) {
                    Ok(points) => {
                        for q in points {
                            p.steps.push(step(ln, false, q, feed));
                        }
                    }
                    Err(e) => {
                        p.errors.push(format!("line {ln}: {e}"));
                        continue;
                    }
                }
            }
        }
        pos = target;
    }
    p
}

fn step(line: usize, rapid: bool, t: [Option<f64>; 4], feed: Option<f64>) -> Step {
    Step {
        line,
        rapid,
        x: t[0],
        y: t[1],
        z: t[2],
        a: t[3],
        feed,
    }
}

/// The points along an arc from `from` to `to` (both its ends known in the plane's axes),
/// clockwise (`cw`) about the centre given by `ijk` (from the start, incremental) or by the
/// radius `r` (negative: the larger arc); the plane's normal axis (and A) moves linearly
/// (a helix). The last point is `to`.
fn arc(
    from: &[Option<f64>; 4],
    to: &[Option<f64>; 4],
    plane: Plane,
    cw: bool,
    ijk: [Option<f64>; 3],
    r: Option<f64>,
) -> Result<Vec<[Option<f64>; 4]>, String> {
    // the plane's axes (u, v) and the normal (w), as indices into XYZ, and the centre words
    let (u, v, w, iu, iv) = match plane {
        Plane::Xy => (0, 1, 2, 0, 1),
        // G18: Z X, the arc turned from Z towards X
        Plane::Zx => (2, 0, 1, 2, 0),
        Plane::Yz => (1, 2, 0, 1, 2),
    };
    let need = |k: usize, at: &[Option<f64>; 4]| {
        at[k].ok_or_else(|| format!("an arc before {} is known", ['X', 'Y', 'Z'][k]))
    };
    let (su, sv) = (need(u, from)?, need(v, from)?);
    let (eu, ev) = (need(u, to)?, need(v, to)?);
    // cw as seen from the normal's positive side (G2 in G17: clockwise looking down -Z)
    let (cu, cv) = if let Some(rad) = r {
        let (dx, dy) = (eu - su, ev - sv);
        let d = (dx * dx + dy * dy).sqrt();
        if d < 1e-9 {
            return Err("an R arc whose ends are the same point".into());
        }
        let h2 = rad * rad - d * d / 4.0;
        if h2 < -1e-6 {
            return Err(format!("R{} is too small for the arc's ends", fmt_num(rad)));
        }
        let h = h2.max(0.0).sqrt();
        let (mx, my) = (su + dx / 2.0, sv + dy / 2.0);
        // the centre to the right of the chord for a cw minor arc
        let side = if cw == (rad > 0.0) { 1.0 } else { -1.0 };
        (mx + side * h * dy / d, my - side * h * dx / d)
    } else {
        (su + ijk[iu].unwrap_or(0.0), sv + ijk[iv].unwrap_or(0.0))
    };
    let rs = ((su - cu).powi(2) + (sv - cv).powi(2)).sqrt();
    let re = ((eu - cu).powi(2) + (ev - cv).powi(2)).sqrt();
    if rs < 1e-9 {
        return Err("an arc of radius 0".into());
    }
    if (rs - re).abs() > 0.01_f64.max(rs * 1e-4) {
        return Err(format!(
            "the arc's ends are {} and {} from its centre: not one circle",
            fmt_num(rs),
            fmt_num(re)
        ));
    }
    let a0 = (sv - cv).atan2(su - cu);
    let a1 = (ev - cv).atan2(eu - cu);
    let tau = std::f64::consts::TAU;
    let mut sweep = if cw { a0 - a1 } else { a1 - a0 };
    if sweep <= 1e-9 {
        sweep += tau; // (same ends: a full circle)
    }
    let step = 2.0 * (1.0 - ARC_TOL / rs).clamp(-1.0, 1.0).acos();
    let n = ((sweep / step.max(1e-3)).ceil() as usize).clamp(1, 100_000);
    let (sw, ew) = (from[w], to[w]);
    let (sa, ea) = (from[3], to[3]);
    let mut out = Vec::with_capacity(n);
    for k in 1..=n {
        let t = k as f64 / n as f64;
        let ang = if cw { a0 - sweep * t } else { a0 + sweep * t };
        let mut q = *to;
        if k < n {
            q[u] = Some(cu + rs * ang.cos());
            q[v] = Some(cv + rs * ang.sin());
            q[w] = match (sw, ew) {
                (Some(a), Some(b)) => Some(a + (b - a) * t),
                (a, b) => b.or(a),
            };
            q[3] = match (sa, ea) {
                (Some(a), Some(b)) => Some(a + (b - a) * t),
                (a, b) => b.or(a),
            };
        }
        out.push(q);
    }
    Ok(out)
}

/// A G code's integer number (G1, G01, G17), or None for decimal ones (G90.1).
fn g_code(v: f64) -> Option<i64> {
    ((v - v.round()).abs() < 1e-9).then(|| v.round() as i64)
}

fn fmt_num(v: f64) -> String {
    let mut s = String::new();
    let _ = write!(s, "{}", (v * 1e6).round() / 1e6);
    s
}

/// A line's words (letter, number), its comments and block delete left out; or what is
/// wrong with it.
fn words(line: &str) -> (Vec<(char, f64)>, Option<String>) {
    let mut out = Vec::new();
    let mut s = String::with_capacity(line.len());
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ';' if depth == 0 => break,
            _ if depth > 0 => {}
            _ => s.push(c),
        }
    }
    let s = s.trim();
    if s.is_empty() || s == "%" {
        return (out, None);
    }
    if s.starts_with('/') {
        return (out, Some("block delete (/): not read".into()));
    }
    let b: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    let mut i = 0;
    while i < b.len() {
        let l = b[i].to_ascii_uppercase();
        if !l.is_ascii_alphabetic() {
            return (
                Vec::new(),
                Some(format!("unexpected {:?}: the line is not read", b[i])),
            );
        }
        let mut j = i + 1;
        while j < b.len() && (b[j].is_ascii_digit() || matches!(b[j], '.' | '-' | '+')) {
            j += 1;
        }
        let num: String = b[i + 1..j].iter().collect();
        if l == 'O' {
            // a program's name: the rest of the line
            break;
        }
        match num.parse::<f64>() {
            Ok(v) => out.push((l, v)),
            Err(_) => {
                return (
                    Vec::new(),
                    Some(format!("{l}{num}: not a number: the line is not read")),
                )
            }
        }
        i = j;
    }
    (out, None)
}

/// JSON moves for a custom toolpath (an alternative to G-code): each `{rapid?, x?, y?, z?,
/// a?, f?}` in the same frame and units (mm, degrees, mm/min), every axis absolute.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JsonMove {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rapid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<f64>,
}

/// JSON moves as a [`Program`] (each its own "line", 1-based), with the same checks.
pub fn from_json(moves: &[JsonMove], frame: Frame) -> Program {
    let mut text = String::new();
    for m in moves {
        let mut l = String::from(if m.rapid { "G0" } else { "G1" });
        for (c, v) in [('X', m.x), ('Y', m.y), ('Z', m.z), ('A', m.a), ('F', m.f)] {
            if let Some(v) = v {
                let _ = write!(l, " {c}{}", fmt_num(v));
            }
        }
        text.push_str(&l);
        text.push('\n');
    }
    let mut p = read(&text, frame);
    // (G90 G21 assumed: the defaults)
    p.lines.clear();
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_rapids_feeds_units_and_modes() {
        let p = read(
            "%\nO1000 (demo)\nG21 G90 G17\nG54\nT1 M6\nS12000 M3\nG0 X0 Y0 Z5\nG1 Z-1 F300\nX10\nG91 Y5\nG20 G90 X1\nM5\nM30\n",
            Frame::Three,
        );
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.tools, vec![1]);
        assert_eq!(p.speeds, vec![12000.0]);
        let s: Vec<_> = p
            .steps
            .iter()
            .map(|s| (s.rapid, s.x, s.y, s.z, s.feed))
            .collect();
        assert_eq!(
            s,
            vec![
                (true, Some(0.0), Some(0.0), Some(5.0), None),
                (false, Some(0.0), Some(0.0), Some(-1.0), Some(300.0)),
                (false, Some(10.0), Some(0.0), Some(-1.0), Some(300.0)),
                (false, Some(10.0), Some(5.0), Some(-1.0), Some(300.0)),
                (false, Some(25.4), Some(5.0), Some(-1.0), Some(300.0)),
            ]
        );
        // the work offset noted, nothing else
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    }

    #[test]
    fn arcs_by_ijk_and_r_stay_on_their_circle() {
        // a full circle (same ends) and a quarter by R
        let p = read(
            "G0 X10 Y0 Z0\nG2 X10 Y0 I-10 J0 F100\nG3 X0 Y10 R10\n",
            Frame::Three,
        );
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let on = |s: &Step| ((s.x.unwrap()).hypot(s.y.unwrap()) - 10.0).abs();
        assert!(p.steps[1..].iter().all(|s| on(s) < 1e-9));
        // the full circle clockwise: through (0,-10) first
        let q = p
            .steps
            .iter()
            .find(|s| s.line == 2 && s.y.unwrap() < -9.99)
            .unwrap();
        assert!(q.x.unwrap().abs() < 0.2);
        // the quarter ends at (0, 10), counter-clockwise from (10, 0): through (7.07, 7.07)
        let last = p.steps.last().unwrap();
        assert!((last.x.unwrap()).abs() < 1e-9 && (last.y.unwrap() - 10.0).abs() < 1e-9);
        assert!(p
            .steps
            .iter()
            .filter(|s| s.line == 3)
            .all(|s| s.x.unwrap() >= -1e-9 && s.y.unwrap() >= -1e-9));
        // chords within the tolerance: the middle of each within ARC_TOL of the circle
        for w in p.steps[1..].windows(2) {
            let (mx, my) = (
                (w[0].x.unwrap() + w[1].x.unwrap()) / 2.0,
                (w[0].y.unwrap() + w[1].y.unwrap()) / 2.0,
            );
            assert!(10.0 - mx.hypot(my) <= ARC_TOL + 1e-9);
        }
    }

    #[test]
    fn a_helix_descends_linearly() {
        let p = read("G0 X5 Y0 Z0\nG3 X5 Y0 Z-2 I-5 F200\n", Frame::Three);
        let zs: Vec<f64> = p.steps[1..].iter().map(|s| s.z.unwrap()).collect();
        assert!(zs.windows(2).all(|w| w[1] < w[0]));
        assert!((zs.last().unwrap() + 2.0).abs() < 1e-12);
    }

    #[test]
    fn the_4th_axis_reads_x_z_a_and_refuses_y() {
        let p = read(
            "G0 X0 Z15 A0\nG1 Z9 F100\nG1 X-20 A720\nG18 G2 X-24 Z5 R4\n",
            Frame::Rotary,
        );
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.steps[2].a, Some(720.0));
        let bad = read("G0 X0 Y2 Z15\n", Frame::Rotary);
        assert_eq!(bad.errors.len(), 1, "{:?}", bad.errors);
        assert!(bad.errors[0].contains("off the 4th axis"));
        let vise = read("G0 X0 Y0 Z5 A90\n", Frame::Three);
        assert!(vise.errors[0].contains("no rotary axis"));
    }

    #[test]
    fn what_it_cannot_read_is_said_with_its_line() {
        let p = read(
            "G0 X0 Y0 Z5\nG81 X1 Y1 Z-3 R1 F100\nM98 P100\nQ5\nG1 X2 F100\n/G1 X3\nM30\nG1 X9\n",
            Frame::Three,
        );
        let w = p.warnings.join(" | ");
        for want in [
            "line 2: G81",
            "line 3: M98",
            "line 4: word Q5",
            "line 6: block delete",
            "line 8: after the program's end",
        ] {
            assert!(w.contains(want), "{want}: {w}");
        }
        // the canned cycle's line moved nothing
        assert_eq!(p.steps.len(), 2);
    }

    #[test]
    fn json_moves_read_as_g_code() {
        let mv = vec![
            JsonMove {
                rapid: true,
                x: Some(0.0),
                y: Some(0.0),
                z: Some(5.0),
                a: None,
                f: None,
            },
            JsonMove {
                rapid: false,
                x: None,
                y: None,
                z: Some(-1.0),
                a: None,
                f: Some(200.0),
            },
        ];
        let p = from_json(&mv, Frame::Three);
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[1].feed, Some(200.0));
    }
}
