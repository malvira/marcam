//! Rotary wall finishing (4-axis simultaneous): waterlines on the walls that radiate from the
//! axis (an impeller's blade faces), A and X together along their twist. A ball dropped
//! radially only grazes such a wall, touching it at one height a pass (Mar's impeller: 0.1 to
//! 0.5 mm left on every blade face after the rotary finish). Here, at radii `step_down` apart,
//! the tool's tip stays at that radius and runs along each wall where the part rises past it:
//! the edges of the stretches round the axis that are clear at that radius
//! ([`super::rotary_flowline::channels_at`]), both faces of every blade, top down.

use super::rotary_finish::refine;
use super::rotary_flowline::channels_at;
use crate::dropcutter::DropCutter;
use crate::job::RotaryWall;
use crate::toolpath::{Builder, Move};

/// Plan the wall waterlines. Returns the lowest tip radius used.
pub fn rotary_wall(
    b: &mut Builder,
    dc: &DropCutter,
    safe_r: f64,
    op: &RotaryWall,
) -> Result<f64, String> {
    if op.step_down <= 0.0 || op.sample_mm <= 0.0 {
        return Err("step_down and sample_mm must be > 0".into());
    }
    let floor = op.min_radius.max(0.0);
    let cl = |x: f64, a: f64| dc.radius_at(x, a, floor) + op.leave_radial;
    let [x0, x1] = [
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    ];
    let m = ((x1 - x0) / op.sample_mm).ceil().max(1.0) as usize;
    let xs: Vec<f64> = (0..=m)
        .map(|i| x0 + (x1 - x0) * i as f64 / m as f64)
        .collect();
    // the radii the walls span: the lowest and highest of the part over the range
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &x in xs.iter().step_by(4) {
        for k in 0..180 {
            let r = cl(x, k as f64 * 2.0);
            lo = lo.min(r);
            hi = hi.max(r);
        }
    }
    let first = b.moves.len();
    let tol = op.tolerance.max(1e-4);
    let feeds = (op.feed, op.plunge_feed);
    let mut levels = Vec::new();
    let mut level = hi - 0.5 * op.step_down;
    while level > lo + 0.02 {
        levels.push(level);
        level -= op.step_down;
    }
    let mut any = false;
    for &level in &levels {
        if crate::budget::over() {
            break;
        }
        // (the tip at this radius, or on the part where it is higher: the wall's line is
        // found between samples)
        let zf = |x: f64, a: f64| cl(x, a).max(level);
        let go = |b: &mut Builder, x: f64, a: f64| {
            let p0 = [b.x.unwrap_or(x), b.r.unwrap_or(zf(x, a)), b.a];
            refine(b, &zf, p0, [x, zf(x, a), a], tol, 0, feeds);
        };
        for c in channels_at(&cl, &xs, level) {
            if crate::budget::over() {
                break;
            }
            any = true;
            let n = c.xs.len();
            // down beside the first wall's start, from the safe radius
            b.rapid_r(safe_r);
            let a0 = c.lo[0] + ((b.a - c.lo[0]) / 360.0).round() * 360.0;
            let turn = a0 - c.lo[0];
            b.rapid(Some(c.xs[0]), None, Some(a0));
            b.plunge(zf(c.xs[0], a0), op.plunge_feed);
            // along the one wall (the stretch's start), across at the far end, back along the
            // other (its end)
            for i in 1..n {
                go(b, c.xs[i], c.lo[i] + turn);
            }
            let (pa, a1) = (b.a, c.hi[n - 1] + turn);
            let steps = ((a1 - pa).abs().ceil() as usize).max(1);
            for s in 1..=steps {
                go(b, c.xs[n - 1], pa + (a1 - pa) * s as f64 / steps as f64);
            }
            for i in (0..n - 1).rev() {
                go(b, c.xs[i], c.hi[i] + turn);
            }
            b.rapid_r(safe_r);
        }
    }
    if !any {
        b.warnings.push(
            "rotary wall finishing: no wall rising from the axis in its X range, nothing to follow"
                .into(),
        );
        return Ok(f64::INFINITY);
    }
    Ok(b.moves[first..]
        .iter()
        .filter_map(|m| match m {
            Move::Cut { r, .. } | Move::Plunge { r, .. } => Some(*r),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min))
}
