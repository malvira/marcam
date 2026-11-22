//! Mesh finishing with a radial drop-cutter: rasters along the axis (constant A per pass) or a
//! helix around it.

use crate::det::Det;
use crate::dropcutter::DropCutter;
use crate::job::{FinishPattern, RotaryFinish};
use crate::toolpath::{Builder, Move};

/// Emit a straight cut from `p0` to `p1` (`[x, r, a]`), split until the surface under the
/// move stays within `tol` below it (checked at the quarter points).
pub(crate) fn refine(
    b: &mut Builder,
    cl: &dyn Fn(f64, f64) -> f64,
    p0: [f64; 3],
    p1: [f64; 3],
    tol: f64,
    depth: u32,
    feeds: (f64, f64),
) {
    let at = |t: f64| {
        [
            p0[0] + (p1[0] - p0[0]) * t,
            p0[1] + (p1[1] - p0[1]) * t,
            p0[2] + (p1[2] - p0[2]) * t,
        ]
    };
    // highest surface point found above the straight move
    let mut top = f64::NEG_INFINITY;
    let mut rises = false;
    for t in [0.25, 0.5, 0.75] {
        let p = at(t);
        let need = cl(p[0], p[2]);
        top = top.max(need);
        rises |= need > p[1] + tol;
    }
    if rises && depth < 14 {
        let m = at(0.5);
        let mid = [m[0], cl(m[0], m[2]), m[2]];
        refine(b, cl, p0, mid, tol, depth + 1, feeds);
        refine(b, cl, mid, p1, tol, depth + 1, feeds);
        return;
    }
    if rises {
        // a wall (the surface jumps within a few microns): climb, cross, then descend
        let top = top.max(p0[1]).max(p1[1]);
        b.cut(p0[0], top, p0[2], feeds.0, feeds.1);
        b.cut(p1[0], top, p1[2], feeds.0, feeds.1);
    }
    b.cut(p1[0], p1[1], p1[2], feeds.0, feeds.1);
}

/// X positions just on the low side of every jump in the tool radius (a wall), found at
/// 12 angles and merged within 0.05 mm.
fn wall_feet(cl: &dyn Fn(f64, f64) -> f64, lo: f64, hi: f64) -> Vec<f64> {
    let step = 0.02;
    let n = ((hi - lo) / step).ceil() as usize;
    let mut feet: Vec<f64> = Vec::new();
    for k in 0..12 {
        let a = k as f64 * 30.0;
        for i in 0..n {
            let (xa, xb) = (lo + step * i as f64, (lo + step * (i + 1) as f64).min(hi));
            let (ra, rb) = (cl(xa, a), cl(xb, a));
            if (rb - ra).abs() < 0.1 {
                continue;
            }
            // bisect the jump; keep the low side
            let rising = rb > ra;
            let (mut p, mut q) = (xa, xb);
            for _ in 0..30 {
                let m = 0.5 * (p + q);
                let rm = cl(m, a);
                if (rm - ra).abs() < (rm - rb).abs() {
                    p = m;
                } else {
                    q = m;
                }
            }
            let foot = if rising { p } else { q };
            if !feet.iter().any(|f| (f - foot).abs() < 0.05) {
                feet.push(foot);
            }
        }
    }
    feet.sort_by(|a, b| a.total_cmp(b));
    feet
}

/// Plan a finishing pass. `r_max` is the part's largest radius (for `stepover_mm` in `along`).
/// Returns the lowest tip radius used.
pub fn rotary_finish(
    b: &mut Builder,
    dc: &DropCutter,
    r_max: f64,
    safe_r: f64,
    op: &RotaryFinish,
) -> Result<f64, String> {
    rotary_finish_on(b, dc, None, r_max, safe_r, op)
}

/// Where the tool rests on the selected faces: dropped on them alone it sits as high as on
/// the whole part (so the contact is theirs), and they are under it at all.
pub(crate) fn on_faces(full: f64, selected: f64, none: f64) -> bool {
    selected > none && selected >= full - 1e-6
}

/// The point next to the boundary between `p` and `q` (one inside, one out), on the inside,
/// to within about 1 um of travel (`len` is the travel from `p` to `q`).
pub(crate) fn boundary<P: Copy>(
    p: P,
    q: P,
    p_inside: bool,
    lerp: &dyn Fn(P, P, f64) -> P,
    inside: &dyn Fn(P) -> bool,
    len: f64,
) -> P {
    let (mut lo, mut hi) = (0.0f64, 1.0f64); // lo on p's side
    let n = ((len.max(1e-9) / 1e-3).dlog2().ceil().max(1.0) as usize).min(40);
    for _ in 0..n {
        let m = 0.5 * (lo + hi);
        if inside(lerp(p, q, m)) == p_inside {
            lo = m;
        } else {
            hi = m;
        }
    }
    lerp(p, q, if p_inside { lo } else { hi })
}

/// [`rotary_finish`] limited to where the tool rests on the faces in `sel` (a drop-cutter on
/// just those faces): elsewhere it lifts to the safe radius.
pub fn rotary_finish_on(
    b: &mut Builder,
    dc: &DropCutter,
    sel: Option<&DropCutter>,
    r_max: f64,
    safe_r: f64,
    op: &RotaryFinish,
) -> Result<f64, String> {
    let floor = op.min_radius.max(0.0);
    let cl = |x: f64, a: f64| dc.radius_at(x, a, floor) + op.leave_radial;
    let [x0, x1] = op.x_range;
    if op.sample_mm <= 0.0 || op.sample_deg <= 0.0 {
        return Err("sample_mm and sample_deg must be > 0".into());
    }
    let first = b.moves.len();
    let tol = op.tolerance.max(1e-4);
    // feed to (x, a), subdividing where the surface rises above the straight move
    let cut = |b: &mut Builder, x: f64, a: f64| {
        let (xs, rs, as_) = (b.x.unwrap_or(x), b.r.unwrap_or(0.0), b.a);
        refine(
            b,
            &cl,
            [xs, rs, as_],
            [x, cl(x, a), a],
            tol,
            0,
            (op.feed, op.plunge_feed),
        );
    };
    // face selection: cut only where the tool rests on the selected faces, lifting between
    let inside = |x: f64, a: f64| {
        sel.is_none_or(|s| on_faces(dc.radius_at(x, a, -1.0), s.radius_at(x, a, -1.0), -1.0))
    };
    let lifted = std::cell::Cell::new(false);
    let last = std::cell::Cell::new(None::<(f64, f64)>);
    let lerp =
        |p: (f64, f64), q: (f64, f64), t: f64| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
    let in_p = |p: (f64, f64)| inside(p.0, p.1);
    let go = |b: &mut Builder, x: f64, a: f64| {
        if sel.is_none() {
            cut(b, x, a);
            return;
        }
        let prev = last.replace(Some((x, a)));
        let span = |p: (f64, f64)| (x - p.0).abs() + r_max * (a - p.1).to_radians().abs();
        match (lifted.get(), inside(x, a)) {
            (false, true) => cut(b, x, a),
            (true, false) => {}
            (false, false) => {
                // leaving the faces: cut to their edge, then up
                if let Some(p) = prev {
                    let e = boundary(p, (x, a), true, &lerp, &in_p, span(p));
                    cut(b, e.0, e.1);
                }
                b.rapid_r(safe_r);
                lifted.set(true);
            }
            (true, true) => {
                // back on them: down at their edge
                let e = prev.map_or((x, a), |p| {
                    boundary(p, (x, a), false, &lerp, &in_p, span(p))
                });
                b.rapid_r(safe_r);
                b.rapid(Some(e.0), None, Some(e.1));
                b.plunge(cl(e.0, e.1), op.plunge_feed);
                lifted.set(false);
                cut(b, x, a);
            }
        }
    };
    // the lead-in: down to `lead_in` above the surface a little before `a` (at 15 degrees
    // along the turn), then onto it turning (the move never goes below the surface)
    let lead_deg = |x: f64, a: f64| {
        let r = cl(x, a).max(0.5);
        (op.lead_in / 15f64.to_radians().dtan() / r).to_degrees()
    };
    let onto = |b: &mut Builder, x: f64, a: f64, dir: f64| {
        if op.lead_in > 0.0 {
            let a0 = a - dir * lead_deg(x, a);
            b.rapid(Some(x), None, Some(a0));
            b.plunge(cl(x, a0) + op.lead_in, op.plunge_feed);
            cut(b, x, a);
        } else {
            b.rapid(Some(x), None, Some(a));
            b.plunge(cl(x, a), op.plunge_feed);
        }
    };
    // the first point of a pass: from the safe radius
    let start = |b: &mut Builder, x: f64, a: f64| {
        b.rapid_r(safe_r);
        if sel.is_none() {
            onto(b, x, a, 1.0);
        } else {
            lifted.set(true);
            last.set(None);
            go(b, x, a);
        }
    };
    match op.pattern {
        FinishPattern::Along => {
            let step = match (op.stepover_deg, op.stepover_mm) {
                (Some(d), _) => d,
                (None, Some(mm)) => (mm / r_max.max(1e-9)).to_degrees(),
                (None, None) => return Err("along needs stepover_deg or stepover_mm".into()),
            };
            if step <= 0.0 {
                return Err("stepover must be > 0".into());
            }
            let [a0, a1] = op.a_range;
            let span = a1 - a0;
            let angles: Vec<f64> = if span.abs() >= 360.0 - 1e-9 {
                let n = (360.0 / step).ceil().max(1.0) as usize;
                (0..n)
                    .map(|i| a0 + span.signum() * 360.0 * i as f64 / n as f64)
                    .collect()
            } else {
                let n = (span.abs() / step).ceil() as usize + 1;
                (0..n)
                    .map(|i| a0 + span * i as f64 / (n - 1).max(1) as f64)
                    .collect()
            };
            let m = ((x1 - x0).abs() / op.sample_mm).ceil().max(1.0) as usize;
            let xs: Vec<f64> = (0..=m)
                .map(|i| x0 + (x1 - x0) * i as f64 / m as f64)
                .collect();
            start(b, xs[0], angles[0]);
            for (i, &a) in angles.iter().enumerate() {
                if crate::budget::over() {
                    break;
                }
                let forward = i % 2 == 0;
                let pass: Vec<f64> = if forward {
                    xs.clone()
                } else {
                    xs.iter().rev().copied().collect()
                };
                for &x in &pass {
                    if crate::budget::over() {
                        break;
                    }
                    go(b, x, a);
                }
                if let Some(&next) = angles.get(i + 1) {
                    let x_end = *pass.last().unwrap();
                    let k = ((next - a).abs() / op.sample_deg).ceil().max(1.0) as usize;
                    for j in 1..=k {
                        go(b, x_end, a + (next - a) * j as f64 / k as f64);
                    }
                }
            }
            b.rapid_r(safe_r);
        }
        FinishPattern::Around => {
            let step = op
                .stepover_mm
                .ok_or("around needs stepover_mm (X advance per turn)")?;
            if step <= 0.0 {
                return Err("stepover_mm must be > 0".into());
            }
            let a0 = op.a_range[0];
            let n_rev = (360.0 / op.sample_deg).ceil() as usize;
            let seg = 360.0 / n_rev as f64;
            // the helix's angle (the tool's while it cuts; kept going while it is lifted)
            let mut at = a0;
            start(b, x0, a0);
            for _ in 0..n_rev {
                at += seg;
                go(b, x0, at);
            }
            let len = (x1 - x0).abs();
            if len > 1e-9 {
                let n = ((len / step * 360.0 / seg).ceil() as usize).max(1);
                let da = 360.0 * len / step / n as f64;
                for i in 1..=n {
                    let x = x0 + (x1 - x0) * i as f64 / n as f64;
                    at += da;
                    go(b, x, at);
                }
            }
            for _ in 0..n_rev {
                at += seg;
                go(b, x1, at);
            }
            b.rapid_r(safe_r);
            if op.clean_walls {
                // A helix reaches the foot of a wall at only one angle per turn, leaving a
                // strip as wide as the stepover; finish each wall foot with a full turn.
                for xw in wall_feet(&cl, x0.min(x1), x0.max(x1)) {
                    if sel.is_none() {
                        let a = b.a;
                        b.rapid_r(safe_r);
                        b.rapid_x(xw);
                        if op.lead_in > 0.0 {
                            onto(b, xw, a, 1.0);
                        } else {
                            b.plunge(cl(xw, a), op.plunge_feed);
                        }
                        for k in 1..=n_rev {
                            cut(b, xw, a + seg * k as f64);
                        }
                    } else {
                        start(b, xw, at);
                        for k in 1..=n_rev {
                            go(b, xw, at + seg * k as f64);
                        }
                        at += seg * n_rev as f64;
                    }
                    b.rapid_r(safe_r);
                }
            }
        }
    }
    let r_min = b.moves[first..]
        .iter()
        .filter_map(|m| match m {
            Move::Cut { r, .. } | Move::Plunge { r, .. } => Some(*r),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min);
    Ok(r_min)
}
