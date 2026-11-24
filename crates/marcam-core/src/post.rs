//! Post-processor: toolpath IR -> G-code text for a [`Machine`].

use crate::det::Det;
use crate::machine::{FeedMode, Machine};
use crate::toolpath::{true_length, Move, Toolpath};

struct State {
    x: Option<f64>,
    z: Option<f64>,
    a: f64,
    y: Option<f64>,
}

fn fill(template: &str, tool: u32, rpm: u32, safe: &str) -> String {
    template
        .replace("{tool}", &tool.to_string())
        .replace("{rpm}", &rpm.to_string())
        .replace("{safe}", safe)
}

/// Convert one program's toolpath to G-code. Lines end with `\n`.
/// F for linear-only controllers: a helix's linear part can be a few hundredths of a mm per
/// move, so F is small; enough decimals to keep it within 0.1 %.
fn fine_feed(f: f64) -> String {
    let digits = (3 - f.abs().max(1e-9).dlog10().floor() as i32).clamp(0, 4) as usize;
    format!("{f:.digits$}")
}

/// An arc the post writes for a run of straight moves.
struct Arc {
    /// How many moves it replaces.
    moves: usize,
    end: [f64; 3],
    center: [f64; 2],
    ccw: bool,
    feed: f64,
    /// Helical length (mm).
    length: f64,
}

/// The longest run of `Line3` moves (equal feed) from `p0` that lies on one circle in XY,
/// within `tol`, with Z linear in the angle (a helix), at least 3 moves and at most half a
/// turn.
fn fit_arc(p0: [f64; 3], moves: &[Move], tol: f64) -> Option<Arc> {
    let mut pts = vec![p0];
    let mut feed = None;
    for m in moves {
        match m {
            Move::Line3 {
                x, y, z, feed: f, ..
            } if feed.is_none_or(|g| g == *f) && pts.len() < 2000 => {
                feed = Some(*f);
                pts.push([*x, *y, *z]);
            }
            _ => break,
        }
    }
    let feed = feed?;
    let circle = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| -> Option<([f64; 2], f64)> {
        let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
        if d.abs() < 1e-12 {
            return None;
        }
        let (a2, b2, c2) = (
            a[0] * a[0] + a[1] * a[1],
            b[0] * b[0] + b[1] * b[1],
            c[0] * c[0] + c[1] * c[1],
        );
        let ux = (a2 * (b[1] - c[1]) + b2 * (c[1] - a[1]) + c2 * (a[1] - b[1])) / d;
        let uy = (a2 * (c[0] - b[0]) + b2 * (a[0] - c[0]) + c2 * (b[0] - a[0])) / d;
        Some(([ux, uy], (a[0] - ux).dhypot(a[1] - uy)))
    };
    let mut best: Option<Arc> = None;
    for m in 3..pts.len() {
        let Some((c, r)) = circle(pts[0], pts[m / 2], pts[m]) else {
            break;
        };
        if r < 10.0 * tol {
            break;
        }
        // one direction all the way, every point on the circle, at most half a turn
        let turn = |i: usize| {
            let (u, v) = (pts[i], pts[i + 1]);
            let (a0, a1) = (
                (u[1] - c[1]).datan2(u[0] - c[0]),
                (v[1] - c[1]).datan2(v[0] - c[0]),
            );
            let mut d = a1 - a0;
            if d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            } else if d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            d
        };
        let steps: Vec<f64> = (0..m).map(turn).collect();
        let ccw = steps[0] > 0.0;
        if steps.iter().any(|d| (*d > 0.0) != ccw || d.abs() < 1e-9) {
            break;
        }
        let sweep: f64 = steps.iter().sum();
        if sweep.abs() > std::f64::consts::PI + 1e-9 {
            break;
        }
        if pts[..=m]
            .iter()
            .any(|p| ((p[0] - c[0]).dhypot(p[1] - c[1]) - r).abs() > tol)
        {
            break;
        }
        // the straight moves it replaces stay within tol of it too (their sagitta): points
        // far apart on a big circle are not an arc
        if steps
            .iter()
            .any(|d| r * (1.0 - (d.abs() / 2.0).dcos()) > tol)
        {
            break;
        }
        // Z linear in the angle
        let (z0, z1) = (pts[0][2], pts[m][2]);
        let mut acc = 0.0;
        let mut helix = true;
        for (i, d) in steps.iter().enumerate() {
            acc += d;
            let want = z0 + (z1 - z0) * acc / sweep;
            if (pts[i + 1][2] - want).abs() > tol {
                helix = false;
                break;
            }
        }
        if !helix {
            break;
        }
        best = Some(Arc {
            moves: m,
            end: pts[m],
            center: c,
            ccw,
            feed,
            length: (r * sweep).dhypot(z1 - z0),
        });
    }
    best
}

/// The toolpath with its work zero at `o` (in its own coordinates): every work XYZ position
/// less `o` (its rapids, cuts, poses and drilling cycles, and the safe height). The rotary
/// moves (X along the axis, the radius) are not moved: a rotary setup's zero is the bar's
/// front on its axis.
pub fn shift_work_zero(tp: &Toolpath, o: [f64; 3]) -> Toolpath {
    let mut t = tp.clone();
    for m in &mut t.moves {
        match m {
            Move::Rapid3 { x, y, z } => {
                *x = x.map(|v| v - o[0]);
                *y = y.map(|v| v - o[1]);
                *z = z.map(|v| v - o[2]);
            }
            Move::Line3 { x, y, z, .. } => {
                *x -= o[0];
                *y -= o[1];
                *z -= o[2];
            }
            Move::Pose { tip, .. } => {
                for k in 0..3 {
                    tip[k] -= o[k];
                }
            }
            Move::Operation { cycle: Some(c), .. } => {
                for p in &mut c.points {
                    p[0] -= o[0];
                    p[1] -= o[1];
                }
                c.bottom -= o[2];
                c.r -= o[2];
                c.safe -= o[2];
            }
            _ => {}
        }
    }
    t.safe_r -= o[2];
    t.notes.insert(
        0,
        format!(
            "Work zero: X{:.3} Y{:.3} Z{:.3} from the setup's (the stock's low X and Y corner, its top)",
            o[0], o[1], o[2]
        ),
    );
    t
}

pub fn post(tp: &Toolpath, machine: &Machine) -> String {
    post_marked(tp, machine).0
}

/// [`post`], plus where each operation starts: (operation id, kind, index of its first
/// line). The text is the same.
pub fn post_marked(tp: &Toolpath, machine: &Machine) -> (String, Vec<(u32, String, usize)>) {
    let (text, marks, _) = post_lines(tp, machine);
    (text, marks)
}

/// [`post_marked`], plus each move's G-code line: the 1-based number of the last line it
/// wrote (the line before it if it wrote none). The text is the same.
pub fn post_lines(
    tp: &Toolpath,
    machine: &Machine,
) -> (String, Vec<(u32, String, usize)>, Vec<usize>) {
    // the work zero moved (a vise setup's WCS): its numbers written from there
    let shifted;
    let tp = match tp.post.as_ref().map(|p| p.origin) {
        Some(o) if o != [0.0; 3] => {
            shifted = shift_work_zero(tp, o);
            &shifted
        }
        _ => tp,
    };
    let mut marks = Vec::new();
    let mut before = Vec::with_capacity(tp.moves.len() + 1);
    let d = machine.decimals;
    let num = |v: f64| format!("{v:.d$}");
    // axis words as this machine names and signs them (identity for XYZ + A about +X)
    let word = |k: usize, v: f64| machine.axes.word(k, v, d);
    let safe = num(tp.safe_r);
    let mut out: Vec<String> = Vec::new();

    out.push(format!("({})", tp.title));
    if let Some((n, desc)) = &tp.tool {
        out.push(format!("(T{n} {desc})"));
    }
    if let Some(c) = tp
        .origin_comment
        .as_ref()
        .or(machine.origin_comment.as_ref())
    {
        out.push(format!("({c})"));
    }
    out.extend(machine.preamble.iter().cloned());
    // the setup's own: its work offset, its header lines
    let opts = tp.post.clone().unwrap_or_default();
    let tool0 = tp.tool.as_ref().map_or(0, |t| t.0);
    if let Some(w) = &opts.work_offset {
        out.push(w.clone());
    }
    for line in &opts.header {
        out.push(fill(line, tool0, 0, &safe));
    }
    for note in &tp.notes {
        out.push(format!("({note})"));
    }
    if tp.reset_a {
        if let Some(r) = &machine.reset_rotary {
            out.push(r.clone());
        }
    }

    let mut s = State {
        x: None,
        z: None,
        a: 0.0,
        y: None,
    };
    // inside an operation that asks for arcs: its tolerance; moves folded into an arc
    let mut arc_tol: Option<f64> = None;
    let mut skip = 0usize;
    // a custom toolpath posted verbatim: its lines, written at its first motion
    let mut verbatim_lines: Option<&Vec<String>> = None;
    for (mi, mv) in tp.moves.iter().enumerate() {
        before.push(out.len());
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if !matches!(
            mv,
            Move::Operation { .. } | Move::ToolChange { .. } | Move::Comment { .. }
        ) {
            if let Some(lines) = verbatim_lines.take() {
                // its own lines for its moves (planned all the same: simulated and checked)
                let rest = tp.moves[mi..]
                    .iter()
                    .take_while(|m| !matches!(m, Move::Operation { .. }))
                    .count();
                out.push("(custom toolpath: its own lines, as given)".into());
                out.extend(lines.iter().cloned());
                // (its modes back to the post's, if it changed them)
                let changed = lines.iter().any(|l| {
                    let u = l.to_ascii_uppercase();
                    ["G91", "G20", "G18", "G19", "G93"]
                        .iter()
                        .any(|g| u.contains(g))
                });
                if changed {
                    out.push("G90 G21 G17 G94".into());
                }
                for m in &tp.moves[mi..mi + rest] {
                    match m {
                        Move::Rapid { a: Some(a), .. } | Move::Cut { a, .. } => s.a = *a,
                        _ => {}
                    }
                }
                // where it left the machine: written in full by the next move
                s.x = None;
                s.y = None;
                s.z = None;
                skip = rest.saturating_sub(1);
                continue;
            }
        }
        match mv {
            Move::Comment { text } => out.push(format!("({text})")),
            Move::Operation {
                id,
                kind,
                arcs: fit,
                cycle,
                verbatim,
            } => {
                marks.push((*id, kind.clone(), out.len()));
                verbatim_lines = verbatim.as_deref();
                arc_tol = *fit;
                if let Some(c) = cycle {
                    if machine.canned_cycles && machine.axes.is_identity() {
                        // the holes as one drilling cycle, from the safe height and back to
                        // it (G98); the planned moves are what it does
                        if s.z.is_none_or(|z| (z - c.safe).abs() > 1e-9) {
                            out.push(format!("G0 {}", word(2, c.safe)));
                        }
                        let inverse = machine.feed_mode == FeedMode::InverseTime;
                        if inverse {
                            // (canned cycles take units per minute)
                            out.push("G94".into());
                        }
                        let g = if c.peck.is_some() { "G83" } else { "G81" };
                        let q = c.peck.map(|q| format!(" Q{}", num(q))).unwrap_or_default();
                        for p in &c.points {
                            out.push(format!(
                                "G98 {g} {} {} {} R{}{q} F{:.0}",
                                word(0, p[0]),
                                word(1, p[1]),
                                word(2, c.bottom),
                                num(c.r),
                                c.feed
                            ));
                        }
                        out.push("G80".into());
                        if inverse {
                            out.push("G93".into());
                        }
                        if let Some(p) = c.points.last() {
                            s.x = Some(p[0]);
                            s.y = Some(p[1]);
                        }
                        s.z = Some(c.safe);
                        // its moves: written by the cycle
                        skip = tp.moves[mi + 1..]
                            .iter()
                            .take_while(|m| {
                                !matches!(m, Move::Operation { .. } | Move::ToolChange { .. })
                            })
                            .count();
                    } else {
                        out.push(
                            "(drilling: written as moves; this machine's post has no canned cycles)"
                                .into(),
                        );
                    }
                }
            }
            Move::Pose { tip, axis, feed } => {
                let a = match crate::kinematics::solve_a(*axis, s.a) {
                    Ok(a) => a,
                    Err(e) => {
                        // planning checks this; a hand-made toolpath gets a visible stop
                        out.push(format!("(marcam: {e})"));
                        out.push("M0".into());
                        continue;
                    }
                };
                let m = crate::kinematics::to_machine(*tip, a);
                // a value counts as changed when it really moved: the part-frame round trip
                // leaves ~1e-12 noise that must not flip a printed digit into a sideways rapid
                let changed =
                    |old: Option<f64>, new: f64| old.is_none_or(|o| (o - new).abs() > 1e-6);
                match feed {
                    None => {
                        let mut w = String::from("G0");
                        for (k, v, old) in [
                            (0, m[0], s.x),
                            (1, m[1], s.y),
                            (2, m[2], s.z),
                            (3, a, Some(s.a)),
                        ] {
                            if changed(old, v) {
                                w += &format!(" {}", word(k, v));
                            }
                        }
                        if w != "G0" {
                            out.push(w);
                        }
                    }
                    Some(feed) => {
                        // the feed is the tip's speed along the part; F as this controller reads it
                        let prev = [
                            s.x.unwrap_or(m[0]),
                            s.y.unwrap_or(m[1]),
                            s.z.unwrap_or(m[2]),
                        ];
                        let p0 = crate::kinematics::to_part(prev, s.a);
                        let len = ((tip[0] - p0[0]).powi(2)
                            + (tip[1] - p0[1]).powi(2)
                            + (tip[2] - p0[2]).powi(2))
                        .sqrt();
                        if len < 1e-6 && (a - s.a).abs() < 1e-9 {
                            continue;
                        }
                        let time = len.max(1e-9) / feed;
                        let (dx, dy, dz, da) =
                            (m[0] - prev[0], m[1] - prev[1], m[2] - prev[2], a - s.a);
                        let lin = (dx * dx + dy * dy + dz * dz).sqrt();
                        let fw = match machine.feed_mode {
                            FeedMode::InverseTime => format!("{:.3}", 1.0 / time),
                            FeedMode::DegreesAsLength => {
                                format!("{:.0}", (lin * lin + da * da).sqrt() / time)
                            }
                            FeedMode::LinearOnly => {
                                fine_feed(if lin > 1e-9 { lin } else { da.abs() } / time)
                            }
                            FeedMode::SurfaceSpeed => {
                                let radius = (prev[1].dhypot(prev[2]) + m[1].dhypot(m[2])) / 2.0;
                                let len = crate::machine::feed_length(
                                    FeedMode::SurfaceSpeed,
                                    machine.surface_radius_pad,
                                    [dx, dy, dz, da],
                                    radius,
                                );
                                format!("{:.0}", len / time)
                            }
                        };
                        let aw = if changed(Some(s.a), a) {
                            format!(" {}", word(3, a))
                        } else {
                            String::new()
                        };
                        out.push(format!(
                            "G1 {} {} {}{aw} F{fw}",
                            word(0, m[0]),
                            word(1, m[1]),
                            word(2, m[2])
                        ));
                    }
                }
                s.x = Some(m[0]);
                s.y = Some(m[1]);
                s.z = Some(m[2]);
                s.a = a;
            }
            Move::Rapid3 { x, y, z } => {
                let mut w = String::from("G0");
                for (k, v, st) in [(0, x, &mut s.x), (1, y, &mut s.y), (2, z, &mut s.z)] {
                    if let Some(v) = v {
                        w += &format!(" {}", word(k, *v));
                        *st = Some(*v);
                    }
                }
                out.push(w);
            }
            Move::Line3 { x, y, z, feed, .. } => {
                let p0 = [s.x.unwrap_or(*x), s.y.unwrap_or(*y), s.z.unwrap_or(*z)];
                let len = ((x - p0[0]).powi(2) + (y - p0[1]).powi(2) + (z - p0[2]).powi(2)).sqrt();
                if len < 1e-6 {
                    continue;
                }
                // a run of moves on one circle (a helix): one G2/G3
                if let Some(tol) = arc_tol.filter(|_| machine.axes.is_identity()) {
                    if let (true, Some(arc)) = (
                        s.x.is_some() && s.y.is_some() && s.z.is_some(),
                        fit_arc(p0, &tp.moves[mi..], tol),
                    ) {
                        let fw = if machine.feed_mode == FeedMode::InverseTime {
                            format!("{:.3}", arc.feed / arc.length)
                        } else {
                            format!("{:.0}", arc.feed)
                        };
                        out.push(format!(
                            "{} {} {} {} I{} J{} F{fw}",
                            if arc.ccw { "G3" } else { "G2" },
                            word(0, arc.end[0]),
                            word(1, arc.end[1]),
                            word(2, arc.end[2]),
                            num(arc.center[0] - p0[0]),
                            num(arc.center[1] - p0[1])
                        ));
                        s.x = Some(arc.end[0]);
                        s.y = Some(arc.end[1]);
                        s.z = Some(arc.end[2]);
                        skip = arc.moves - 1;
                        continue;
                    }
                }
                let fw = if machine.feed_mode == FeedMode::InverseTime {
                    format!("{:.3}", feed / len)
                } else {
                    format!("{feed:.0}")
                };
                out.push(format!(
                    "G1 {} {} {} F{fw}",
                    word(0, *x),
                    word(1, *y),
                    word(2, *z)
                ));
                s.x = Some(*x);
                s.y = Some(*y);
                s.z = Some(*z);
            }
            Move::ToolChange { tool, rpm } => {
                out.push(fill(&machine.tool_change, *tool, *rpm, &safe));
                out.push(fill(&machine.spindle_on, *tool, *rpm, &safe));
                if let Some(dw) = &machine.dwell {
                    out.push(dw.clone());
                }
            }
            Move::Rapid { x, r, a } => {
                let mut w = String::from("G0");
                if let Some(z) = r {
                    w += &format!(" {}", word(2, *z));
                    s.z = Some(*z);
                }
                if let Some(x) = x {
                    w += &format!(" {}", word(0, *x));
                    s.x = Some(*x);
                }
                if let Some(a) = a {
                    w += &format!(" {}", word(3, *a));
                    s.a = *a;
                }
                out.push(w);
            }
            Move::Plunge { r, feed } => {
                if machine.feed_mode == FeedMode::InverseTime {
                    // G93: F = 1 / minutes for this move
                    let dz = s.z.map_or(0.0, |z| (r - z).abs()).max(1e-6);
                    out.push(format!("G1 {} F{:.3}", word(2, *r), feed / dz));
                } else {
                    out.push(format!("G1 {} F{feed:.0}", word(2, *r)));
                }
                s.z = Some(*r);
            }
            Move::Cut {
                x,
                r,
                a,
                feed,
                plunge_cap,
            } => {
                let (x0, z0) = (s.x.unwrap_or(*x), s.z.unwrap_or(*r));
                // G93 feeds are per move: time the move the controller will actually run,
                // i.e. between the printed (rounded) coordinates
                let q = |v: f64| {
                    if machine.feed_mode == FeedMode::InverseTime {
                        // exactly the printed value
                        num(v).parse::<f64>().unwrap_or(v)
                    } else {
                        v
                    }
                };
                let (dx, dz, da) = (q(*x) - q(x0), q(*r) - q(z0), q(*a) - q(s.a));
                let len = true_length(dx, dz, da, q(z0), q(*r));
                if len < 1e-6 {
                    continue;
                }
                let mut dt = len / feed;
                if dz < 0.0 {
                    // never descend faster than the plunge feed
                    dt = crate::numeric::py_max(dt, -dz / plunge_cap);
                }
                let f = match machine.feed_mode {
                    FeedMode::DegreesAsLength => (dx * dx + dz * dz + da * da).sqrt() / dt,
                    FeedMode::LinearOnly => {
                        // a pure rotation: the controller reads F as deg/min
                        let lin = dx.dhypot(dz);
                        if lin > 1e-6 {
                            lin / dt
                        } else {
                            da.abs() / dt
                        }
                    }
                    FeedMode::InverseTime => 1.0 / dt,
                    FeedMode::SurfaceSpeed => {
                        crate::machine::feed_length(
                            FeedMode::SurfaceSpeed,
                            machine.surface_radius_pad,
                            [dx, 0.0, dz, da],
                            (q(z0) + q(*r)) / 2.0,
                        ) / dt
                    }
                };
                let mut fw = match machine.feed_mode {
                    FeedMode::InverseTime => format!("{f:.3}"),
                    FeedMode::LinearOnly => fine_feed(f),
                    _ => format!("{f:.0}"),
                };
                // on very short moves the printed (rounded) coordinates can descend faster than
                // the exact ones: slow such a move so the program as written stays within the
                // plunge cap (and its 2% allowance for F rounding)
                let p = |v: f64| num(v).parse::<f64>().unwrap_or(v);
                let (px, pz, pa) = (p(*x) - p(x0), p(*r) - p(z0), p(*a) - p(s.a));
                if pz < 0.0 {
                    let span = match machine.feed_mode {
                        FeedMode::DegreesAsLength => (px * px + pz * pz + pa * pa).sqrt(),
                        FeedMode::LinearOnly => px.dhypot(pz),
                        FeedMode::InverseTime => 1.0,
                        FeedMode::SurfaceSpeed => crate::machine::feed_length(
                            FeedMode::SurfaceSpeed,
                            machine.surface_radius_pad,
                            [px, 0.0, pz, pa],
                            (p(z0) + p(*r)) / 2.0,
                        ),
                    };
                    let fv: f64 = fw.parse().unwrap_or(f);
                    if fv > 0.0 && -pz * fv / span > plunge_cap * 1.02 {
                        let slow = span * plunge_cap / -pz;
                        fw = if machine.feed_mode == FeedMode::InverseTime {
                            format!("{:.3}", (slow * 1000.0).floor() / 1000.0)
                        } else {
                            format!("{:.0}", slow.floor().max(1.0))
                        };
                    }
                }
                out.push(format!(
                    "G1 {} {} {} F{fw}",
                    word(0, *x),
                    word(2, *r),
                    word(3, *a)
                ));
                s.x = Some(*x);
                s.z = Some(*r);
                s.a = *a;
            }
        }
    }
    before.push(out.len());
    let tool = tp.tool.as_ref().map_or(0, |t| t.0);
    for line in &opts.footer {
        out.push(fill(line, tool, 0, &safe));
    }
    for line in &machine.postamble {
        out.push(fill(line, tool, 0, &safe));
    }
    // without comments, numbered: the lines kept, and where each old line went
    if opts.no_comments || opts.line_numbers {
        let mut kept = Vec::with_capacity(out.len());
        // (the number of lines kept before each old line)
        let mut to_new = Vec::with_capacity(out.len() + 1);
        for line in out {
            to_new.push(kept.len());
            let line = if opts.no_comments {
                strip_comments(&line)
            } else {
                line
            };
            if line.trim().is_empty() {
                continue;
            }
            kept.push(line);
        }
        to_new.push(kept.len());
        if opts.line_numbers {
            for (i, l) in kept.iter_mut().enumerate() {
                *l = format!("N{} {l}", 10 * (i + 1));
            }
        }
        for m in &mut marks {
            m.2 = to_new[m.2.min(to_new.len() - 1)];
        }
        for b in &mut before {
            *b = to_new[(*b).min(to_new.len() - 1)];
        }
        out = kept;
    }
    let mut text = out.join("\n");
    text.push('\n');
    (text, marks, before[1..].to_vec())
}

/// A G-code line without its comments (`(...)` and from `;` on); lines of their own go.
fn strip_comments(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ';' if depth == 0 => break,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
