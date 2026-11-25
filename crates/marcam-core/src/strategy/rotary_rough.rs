//! Roughing from a mesh: radial step-down levels, each cut as a helix along X. At every point
//! the tool sits at `max(level, drop-cutter radius + allowance)`, so it never cuts deeper than
//! one step and never into the part.

use super::rotary_finish::refine;
use crate::dropcutter::DropCutter;
use crate::job::RotaryRough;
use crate::numeric::py_max;
use crate::toolpath::{Builder, Move};

/// The fastest a move through the air goes (mm/min), at most five times the cutting feed.
const AIR_FEED: f64 = 2000.0;
/// Through the air round the axis: at most this many mm/min per mm from it (about 3,400
/// degrees a minute, under the 4,000 the Z1's controller takes).
const AIR_TURN: f64 = 60.0;
/// Rest roughing: the least stock above a level worth going round for (mm); less is the
/// allowance the operations before left on the part.
const REST_MIN: f64 = 0.25;
/// How far clear of the stock a move must stay to count as in the air (mm).
const AIR_GAP: f64 = 0.2;

fn intervals(mask: &[bool], xs: &[f64]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &m) in mask.iter().enumerate() {
        match (m, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((xs[s], xs[i - 1]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((xs[s], xs[xs.len() - 1]));
    }
    out
}

/// The highest stock under a tool moving from X `x0` to `x1` and A `a0` to `a1` (degrees)
/// with its tip `r` from the axis (its width included): a stock that is not round.
pub type StockNear<'a> = &'a dyn Fn(f64, f64, f64, f64, f64) -> f64;

/// Returns the lowest tip radius used. `stock_at` gives the bar's largest radius at an X when
/// it is not round from end to end (a bar carried from an earlier setup, turned down in
/// places): a level is cut only along X where the bar stands above it. `stock_near` (a stock
/// not round about the axis: a carried piece) says where round a level there is none: those
/// stretches are crossed at the air feed.
pub fn rotary_rough(
    b: &mut Builder,
    dc: &DropCutter,
    stock_r: f64,
    safe_r: f64,
    op: &RotaryRough,
    stock_at: Option<&dyn Fn(f64) -> f64>,
    stock_near: Option<StockNear>,
) -> Result<f64, String> {
    if op.step_down <= 0.0 || op.step_x <= 0.0 || op.sample_deg <= 0.0 {
        return Err("step_down, step_x and sample_deg must be > 0".into());
    }
    let floor = op.min_radius.max(0.0);
    let cl = |x: f64, a: f64| dc.radius_at(x, a, floor) + op.leave_radial;
    let [x_lo, x_hi] = [
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    ];
    let first = b.moves.len();
    let n_rev = (360.0 / op.sample_deg).ceil() as usize;
    let seg = 360.0 / n_rev as f64;

    // lowest surface over all angles at each x: where a level has material to remove
    let m = ((x_hi - x_lo) / 0.05).ceil().max(1.0) as usize;
    let xs: Vec<f64> = (0..=m)
        .map(|i| x_lo + (x_hi - x_lo) * i as f64 / m as f64)
        .collect();
    let clg: Vec<Vec<f64>> = xs
        .iter()
        .map(|&x| (0..n_rev).map(|k| cl(x, k as f64 * seg)).collect())
        .collect();
    let need: Vec<f64> = clg
        .iter()
        .map(|row| row.iter().copied().fold(f64::INFINITY, f64::min))
        .collect();
    // (rest roughing: the stock as the operations before left it, at each x and angle)
    let rest_stock: Option<Vec<Vec<f64>>> = (op.rest && stock_near.is_some()).then(|| {
        let f = stock_near.expect("checked");
        xs.iter()
            .map(|&x| {
                (0..n_rev)
                    .map(|k| f(x, x, k as f64 * seg, k as f64 * seg, stock_r))
                    .collect()
            })
            .collect()
    });
    let z_min = need.iter().copied().fold(f64::INFINITY, f64::min);
    // the bar's largest radius at each x (round from end to end: the stock's)
    let top: Vec<f64> = xs
        .iter()
        .map(|&x| stock_at.map_or(stock_r, |f| f(x)))
        .collect();

    // (Mar's knob4: radial 20 left on the part put the whole path 20-45 mm from the axis,
    // round a bar 27 mm at most: 6 h in the air) nothing below the stock: said so
    let highest = top.iter().copied().fold(0.0f64, f64::max);
    if z_min >= highest - 1e-6 {
        return Err(format!(
            "nothing to rough: the part with {:.2} mm of radial stock left on it (Radial) stands above the stock everywhere ({:.2} mm from the axis at most)",
            op.leave_radial, highest
        ));
    }
    let top_at = |x: f64| stock_at.map_or(stock_r, |f| f(x));
    let mut levels = Vec::new();
    let mut level = stock_r - op.step_down;
    while level > z_min - op.step_down + 1e-6 {
        levels.push(py_max(level, z_min));
        level -= op.step_down;
    }

    let tol = op.tolerance.max(1e-4);
    let feeds = (op.feed, op.plunge_feed);
    // (Mar's knob4: a bar laid across A, its rough round the whole turn at every level, 4 h
    // of it in the air) where the tool meets no stock: crossed faster, at the air feed
    let air_feed = op.feed.max((5.0 * op.feed).min(AIR_FEED));
    let mut prev = stock_r;
    let mut direction = 1.0;
    b.comment("--- mesh roughing ---");
    for &level in &levels {
        if crate::budget::over() {
            break;
        }
        // material to remove at this level: the part below it, the bar above it; (rest
        // roughing) at some angle more of it than a trace, not the allowance the operations
        // before left on the part
        let mask: Vec<bool> = match &rest_stock {
            Some(st) => (0..xs.len())
                .map(|i| {
                    (0..n_rev).any(|k| {
                        let c = clg[i][k];
                        c < level - 1e-6 && st[i][k] - level.max(c) > REST_MIN
                    })
                })
                .collect(),
            None => need
                .iter()
                .zip(&top)
                .map(|(&z, &t)| z < level - 1e-6 && t > level + 1e-6)
                .collect(),
        };
        for (mut xa, mut xb) in intervals(&mask, &xs) {
            if direction < 0.0 {
                std::mem::swap(&mut xa, &mut xb);
            }
            // (never above the bar: where the part with what it leaves stands higher, along the
            // bar's top, not round the air above it)
            // (and never below the part itself: a simulated stock's top can be under a blade
            // that leans over the hub)
            let zf = |x: f64, a: f64| {
                let c = cl(x, a);
                py_max(level, c.min(top_at(x) + 0.5).max(c - op.leave_radial))
            };
            let go = |b: &mut Builder, x: f64, r: f64, a: f64| {
                let p0 = [b.x.unwrap_or(x), b.r.unwrap_or(r), b.a];
                // (the tip nowhere lower than the lower end: nothing of the stock up to it)
                let low = p0[1].min(r);
                let air = stock_near.is_some_and(|f| {
                    f(p0[0].min(x), p0[0].max(x), p0[2].min(a), p0[2].max(a), low) < low - AIR_GAP
                });
                // (turning, not cutting: no faster round the axis than the controller's cap)
                let f = if air {
                    (air_feed.min(AIR_TURN * low).max(op.feed), feeds.1)
                } else {
                    feeds
                };
                refine(b, &zf, p0, [x, r, a], tol, 0, f);
            };
            let a0 = b.a;
            // enter above the previous level (already cleared here), then ramp down over a turn
            let zs = py_max(prev + 0.5, zf(xa, a0));
            b.rapid_r(safe_r);
            b.rapid_x(xa);
            // (rest roughing: down by rapid where nothing stands under the tool, else at the
            // plunge feed)
            if stock_near.is_some_and(|f| f(xa, xa, a0, a0, zs) < zs - AIR_GAP) {
                b.rapid_r(zs);
            } else {
                b.plunge(zs, op.plunge_feed);
            }
            for i in 1..=n_rev {
                let a = a0 + direction * seg * i as f64;
                let target = zf(xa, a);
                let r = py_max(zs + (target - zs) * i as f64 / n_rev as f64, target);
                go(b, xa, r, a);
            }
            for _ in 0..n_rev {
                let a = b.a + direction * seg;
                go(b, xa, zf(xa, a), a);
            }
            let len = (xb - xa).abs();
            if len > 1e-9 {
                let n = ((len / op.step_x * 360.0 / seg).ceil() as usize).max(1);
                let da = direction * 360.0 * len / op.step_x / n as f64;
                for i in 1..=n {
                    let x = xa + (xb - xa) * i as f64 / n as f64;
                    let a = b.a + da;
                    go(b, x, zf(x, a), a);
                }
            }
            for _ in 0..n_rev {
                let a = b.a + direction * seg;
                go(b, xb, zf(xb, a), a);
            }
            b.rapid_r(safe_r);
        }
        prev = level;
        direction = -direction;
    }
    Ok(b.moves[first..]
        .iter()
        .filter_map(|m| match m {
            Move::Cut { r, .. } | Move::Plunge { r, .. } => Some(*r),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min))
}
