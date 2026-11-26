//! Rotary pocket roughing (4-axis simultaneous): a pocket round the bar (an impeller's
//! channels, a recess in a cylinder) cleared level by level in radius, each level as an
//! adaptive (constant-engagement) clearing on the cylinder at that radius, unrolled: X along
//! the bar, the arc round it. The tool stays square to the axis (over it), X and A moving
//! together along each cut; a steady load (the stepover's width), never a full-width slot but
//! where it comes in, down a helix round where it has room.

use crate::dropcutter::DropCutter;
use crate::job::RotaryPocket;
use crate::strategy::adaptive::{clear_level, Grid, Params, Step, SLOT_FEED};
use crate::toolpath::Builder;

/// What a rotary pocket's planning came to (for its notes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PocketReport {
    pub levels: usize,
    /// Plunges straight in (no room for a helix).
    pub plunges: usize,
    /// Cells of material the tool could reach but left (crumbs).
    pub left: usize,
    /// The lowest radius it cut at.
    pub floor: f64,
}

/// The pocket's levels: from just under the stock's top down to its floor, `step` apart, the
/// floor the last.
fn levels(top: f64, floor: f64, step: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut r = top - step;
    while r > floor + 1e-6 {
        out.push(r);
        r -= step;
    }
    if top > floor + 1e-6 {
        out.push(floor);
    }
    out
}

/// Plan a rotary pocket. `stock_top(x, a)`: the stock's radius there (the bar, or what the
/// operations before left); `safe_r`: the radius the tool crosses at.
pub fn rotary_pocket(
    b: &mut Builder,
    dc: &DropCutter,
    stock_top: &dyn Fn(f64, f64) -> f64,
    safe_r: f64,
    op: &RotaryPocket,
) -> Result<PocketReport, String> {
    let r_tool = dc.cutter().radius();
    let d = 2.0 * r_tool;
    if op.step_down <= 0.0 {
        return Err("a rotary pocket needs a step down above 0".into());
    }
    let (x0, x1) = (
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    );
    let (a0, a1) = (
        op.a_range[0].min(op.a_range[1]),
        op.a_range[0].max(op.a_range[1]),
    );
    if x1 - x0 < d || a1 - a0 <= 0.0 {
        return Err("a rotary pocket's range is narrower than its tool".into());
    }
    let a_span = (a1 - a0).min(360.0);
    let full = a_span >= 360.0 - 1e-9;
    // the part (and what to leave) and the stock over a grid in (X, A), sampled once: the range
    // and the tool's reach round it (the stock there is cut too, and counts in the load)
    let cell = (d / 8.0).clamp(0.05, 0.5);
    let m = r_tool + 2.0 * cell;
    let ma = if full {
        0.0
    } else {
        (m / op.min_radius.max(2.0)).to_degrees().min(60.0)
    };
    let (ex0, ex1) = (x0 - m, x1 + m);
    let (ea0, espan) = (a0 - ma, if full { 360.0 } else { a_span + 2.0 * ma });
    let r_max = (0..=8)
        .flat_map(|i| (0..=8).map(move |j| (i, j)))
        .map(|(i, j)| {
            stock_top(
                x0 + (x1 - x0) * i as f64 / 8.0,
                a0 + a_span * j as f64 / 8.0,
            )
        })
        .fold(0.0f64, f64::max)
        .max(op.min_radius + op.step_down);
    let nx = ((ex1 - ex0) / cell).ceil() as usize + 1;
    let na = ((espan.to_radians() * r_max) / cell).ceil().max(8.0) as usize + 1;
    let xa = |i: usize, j: usize| {
        (
            ex0 + (ex1 - ex0) * i as f64 / (nx - 1) as f64,
            ea0 + espan * j as f64 / (na - 1) as f64,
        )
    };
    let mut part = vec![0.0f64; nx * na];
    let mut top = vec![0.0f64; nx * na];
    for j in 0..na {
        if crate::budget::over() {
            return Ok(PocketReport::default());
        }
        for i in 0..nx {
            let (x, a) = xa(i, j);
            part[j * nx + i] =
                (dc.radius_at(x, a, op.min_radius) + op.leave_radial).max(op.min_radius);
            top[j * nx + i] = stock_top(x, a);
        }
    }
    let idx = |x: f64, a: f64| -> usize {
        let i = (((x - ex0) / (ex1 - ex0) * (nx - 1) as f64).round().max(0.0) as usize).min(nx - 1);
        let u = if full {
            (a - ea0).rem_euclid(360.0)
        } else {
            a - ea0
        };
        let j = ((u / espan * (na - 1) as f64).round().max(0.0) as usize).min(na - 1);
        j * nx + i
    };
    let at = |v: &[f64], x: f64, a: f64| -> f64 { v[idx(x, a)] };
    // (the floor: the part's lowest inside the range)
    let mut floor = f64::INFINITY;
    for j in 0..na {
        for i in 0..nx {
            let (x, a) = xa(i, j);
            if x >= x0 - 1e-9 && x <= x1 + 1e-9 && (full || (a >= a0 - 1e-9 && a <= a1 + 1e-9)) {
                floor = floor.min(part[j * nx + i]);
            }
        }
    }
    let climb = true;
    let params = Params {
        r: r_tool,
        ae: (op.stepover * d).clamp(0.02 * d, d),
        climb,
        helix: if op.helix { 0.75 * r_tool } else { 0.0 },
    };
    let mut report = PocketReport::default();
    let a_mid = a0 + 0.5 * a_span;
    b.comment("--- rotary pocket (adaptive, 4-axis simultaneous) ---");
    let lv = levels(r_max, floor, op.step_down);
    let mut above = r_max;
    for &r in &lv {
        if crate::budget::over() {
            break;
        }
        // the level unrolled: X across, the arc at this radius along (its middle at A a_mid)
        let s_half = 0.5 * a_span.to_radians() * r + m;
        let h = cell;
        let g = Grid {
            x0: ex0,
            y0: -s_half,
            h,
            nx: ((ex1 - ex0) / h).floor() as usize + 1,
            ny: ((2.0 * s_half) / h).floor() as usize + 1,
        };
        let a_of = |s: f64| a_mid + (s / r).to_degrees();
        let n = g.len();
        let mut allowed = vec![false; n];
        let mut stock = vec![false; n];
        let mut open = vec![false; n];
        for c in 0..n {
            let p = g.point(c);
            let a = a_of(p[1]);
            let t = at(&top, p[0], a);
            // (the grid's edges too: the range's ends, and round the axis the seam, which the
            // clearing does not cross; its helices kept inside)
            let inside = p[0] >= x0 - 1e-9
                && p[0] <= x1 + 1e-9
                && (p[1].abs() <= s_half - m + 1e-9)
                && (!full || p[1].abs() < s_half - m - 0.5 * h);
            allowed[c] = inside && at(&part, p[0], a) <= r + 1e-9;
            stock[c] = t > r + 1e-3;
            // (nothing left above the level before: come in from there)
            open[c] = t <= above + 1e-3;
        }
        if !stock.iter().zip(&allowed).any(|(&s, &a)| s && a) {
            above = r;
            continue;
        }
        let cl = clear_level(g, &allowed, &stock, &mut open, params, None);
        report.levels += 1;
        report.floor = r;
        let _ = cl.plunges;
        report.left += cl.left;
        // (the stock's top under the tool at a point: a way in comes down by rapid to just
        // above it, where the levels before did not reach)
        let top_under = |x: f64, s: f64| -> f64 {
            let mut t: f64 = at(&top, x, a_of(s));
            for k in 0..12 {
                let w = std::f64::consts::TAU * k as f64 / 12.0;
                t = t.max(at(&top, x + r_tool * w.cos(), a_of(s + r_tool * w.sin())));
            }
            t
        };
        report.plunges += emit(b, &cl, &g, r, above, &a_of, &top_under, safe_r, op);
        // what this level cut, in the (X, A) grid: down to it
        for (c, &cut) in cl.cut.iter().enumerate() {
            if cut {
                let p = g.point(c);
                let a = a_of(p[1]);
                let k = idx(p[0], a);
                top[k] = top[k].min(r);
            }
        }
        above = r;
    }
    b.rapid_r(safe_r);
    Ok(report)
}

/// A level's clearing as rotary moves at radius `r` (the points' arc `s` to A by `a_of`).
#[allow(clippy::too_many_arguments)]
fn emit(
    b: &mut Builder,
    cl: &crate::strategy::adaptive::Cleared,
    g: &Grid,
    r: f64,
    above: f64,
    a_of: &dyn Fn(f64) -> f64,
    top_under: &dyn Fn(f64, f64) -> f64,
    safe_r: f64,
    op: &RotaryPocket,
) -> usize {
    let _ = g;
    let mut plunged = 0;
    // (in from just above the level before, or where it never reached, the stock there)
    let entry_at = |q: [f64; 2]| (top_under(q[0], q[1]).max(above) + 0.5).min(safe_r);
    let come_in = |b: &mut Builder, q: [f64; 2]| -> f64 {
        let e = entry_at(q);
        b.rapid_r(safe_r);
        b.rapid(Some(q[0]), None, Some(a_of(q[1])));
        b.rapid_r(e);
        e
    };
    for (si, st) in cl.steps.iter().enumerate() {
        match st {
            Step::Down(q) | Step::Plunge(q) => {
                // (a way in with nothing to cut after it: left out)
                if !matches!(cl.steps.get(si + 1), Some(Step::Cut(_) | Step::Link(_))) {
                    continue;
                }
                let e = come_in(b, *q);
                // (no straight plunge where the cut that follows is long enough: down a ramp
                // along it, out and back, at the ramp angle; the impeller's channels had 69
                // plunges)
                let path = match cl.steps.get(si + 1) {
                    Some(Step::Cut(pts)) if pts.len() >= 2 => Some(pts),
                    _ => None,
                };
                let len: f64 = path.map_or(0.0, |pts| {
                    pts.windows(2)
                        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                        .sum()
                });
                let tan = op.ramp_angle.clamp(1.0, 45.0).to_radians().tan();
                match path {
                    Some(pts) if e > r + 0.05 && len * tan > 0.2 * (e - r) => {
                        // out along the cut and back, as often as it takes
                        let mut lp: Vec<[f64; 2]> = pts.clone();
                        lp.extend(pts.iter().rev().skip(1).copied());
                        let (mut z, mut k) = (e, 0usize);
                        let mut at = lp[0];
                        while z > r + 1e-9 {
                            let to = lp[(k + 1) % lp.len()];
                            let seg = (to[0] - at[0]).hypot(to[1] - at[1]);
                            let fall = (seg * tan).min(z - r);
                            z -= fall;
                            b.cut(to[0], z, a_of(to[1]), op.feed * SLOT_FEED, op.plunge_feed);
                            at = to;
                            k = (k + 1) % lp.len();
                            if k == 0 {
                                at = lp[0];
                            }
                        }
                        // back to the cut's start at depth
                        let back: Vec<[f64; 2]> = if k == 0 {
                            Vec::new()
                        } else if k < pts.len() {
                            pts[..k].iter().rev().copied().collect()
                        } else {
                            lp[k..].to_vec()
                        };
                        for q in back {
                            b.cut(q[0], r, a_of(q[1]), op.feed * SLOT_FEED, op.plunge_feed);
                        }
                    }
                    _ => {
                        if matches!(st, Step::Plunge(_)) {
                            plunged += 1;
                        }
                        b.plunge(r, op.plunge_feed)
                    }
                }
            }
            Step::Helix {
                centre,
                radius,
                start,
            } => {
                let n = 36;
                let pt = |k: usize| {
                    let t = start + std::f64::consts::TAU * k as f64 / n as f64;
                    [centre[0] + radius * t.cos(), centre[1] + radius * t.sin()]
                };
                let p0 = pt(0);
                // (from above the stock all round the helix)
                let mut entry = come_in(b, p0);
                for k in 1..n {
                    entry = entry.max(entry_at(pt(k)));
                }
                b.rapid_r(entry);
                // down a turn (or more, gentle) from the entry to the level, then one at depth
                let drop = entry - r;
                let per_turn =
                    (std::f64::consts::TAU * radius * op.ramp_angle.to_radians().tan()).max(0.05);
                let turns = (drop / per_turn).ceil().max(1.0) as usize;
                for k in 1..=turns * n {
                    let q = pt(k % n);
                    let z = entry - drop * k as f64 / (turns * n) as f64;
                    b.cut(q[0], z, a_of(q[1]), op.feed * SLOT_FEED, op.plunge_feed);
                }
                for k in 1..=n {
                    let q = pt(k % n);
                    b.cut(q[0], r, a_of(q[1]), op.feed * SLOT_FEED, op.plunge_feed);
                }
            }
            Step::Cut(pts) => {
                let heavy = cl.heavy.iter().find(|h| h.0 == si).map(|h| &h.1);
                for (i, q) in pts.iter().enumerate() {
                    let k = if i == 0 {
                        1.0
                    } else {
                        heavy.and_then(|h| h.get(i - 1).copied()).unwrap_or(1.0)
                    };
                    b.cut(q[0], r, a_of(q[1]), op.feed * k, op.plunge_feed);
                }
            }
            Step::Link(pts) => {
                for q in pts {
                    b.cut(q[0], r, a_of(q[1]), op.feed, op.plunge_feed);
                }
            }
        }
    }
    plunged
}
