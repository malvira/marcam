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
/// Rest roughing on the 3D stock with a limit on the bite: the stock as the operations
/// before left it (taken from as the passes are planned), the tool's solid, and the most a
/// pass may take per mm of travel (mm3/mm).
pub struct Bite<'a> {
    pub stock: &'a mut crate::dexel::Dexel,
    pub tool: crate::dexel::ToolSolid,
    pub limit: f64,
}

/// Where a pass's bite is sampled along it (mm).
const BITE_STEP: f64 = 0.25;
/// The slowest a pass over its bite goes (of the feed).
const BITE_SLOW: f64 = 0.6;

type RayLog = Vec<(usize, usize, Vec<[f32; 2]>)>;

/// A pass along X at A `a` from `xa` to `xb`, its tip at `zf`, taken out of the stock: what
/// it takes per mm at points at most [`BITE_STEP`] apart (X, mm3/mm; its start, where it
/// comes down, not a bite). With `log`, what it changed is kept for
/// [`crate::dexel::Dexel::restore`].
fn take_pass(
    bt: &mut Bite,
    zf: &dyn Fn(f64, f64) -> f64,
    xa: f64,
    xb: f64,
    a: f64,
    mut log: Option<&mut RayLog>,
) -> Vec<(f64, f64)> {
    let n = (((xb - xa).abs() / BITE_STEP).ceil() as usize).max(1);
    let mut out = Vec::with_capacity(n + 1);
    let mut scratch = RayLog::new();
    for i in 0..=n {
        let x = xa + (xb - xa) * i as f64 / n as f64;
        let (tip, dir) = crate::dexel::pose(x, 0.0, zf(x, a), a);
        let v = match log.as_deref_mut() {
            Some(l) => bt.stock.subtract_logged(&bt.tool, tip, dir, l),
            None => {
                scratch.clear();
                bt.stock.subtract_logged(&bt.tool, tip, dir, &mut scratch)
            }
        };
        // (per mm the tip moved: a short pass's step is short; the impeller's 0.11 mm one
        // beside a blade's wall took 1 mm3)
        if i > 0 {
            let moved = ((xb - xa).abs() / n as f64)
                .hypot(zf(x, a) - zf(xa + (xb - xa) * (i - 1) as f64 / n as f64, a));
            out.push((x, v / moved.max(0.05)));
        }
    }
    out
}

/// The feed's share at X for a pass whose bites are `bites`: under the limit, all of it;
/// over, in proportion, to [`BITE_SLOW`] (in twentieths, down).
fn bite_scale(bites: &[(f64, f64)], limit: f64, x: f64) -> f64 {
    let worst = bites
        .iter()
        .filter(|(bx, _)| (bx - x).abs() <= 0.5 + 1e-9)
        .map(|b| b.1)
        .fold(0.0f64, f64::max);
    if worst <= limit {
        return 1.0;
    }
    ((limit / worst * 20.0).floor() / 20.0).clamp(BITE_SLOW, 1.0)
}

/// Rest roughing along X: at each level, passes along X at angles `op.step_x` mm apart at the
/// level's radius, each only over the X runs where more than [`REST_MIN`] of the stock left
/// (`st`, its top at `xs` x the `seg`-degree angles) stands above the level and the part
/// (with its allowance, `clg`/`cl`). Each pass comes down onto the stock from just above it,
/// rides `max(level, part)`, and leaves upward; the next angle's pass follows at the safe
/// radius. The levels top-down, so a pass takes at most a level's depth and a stepover.
/// With `bite`, a pass that would take more than its limit per mm goes in two, half a level
/// apart, and slower where the second still would.
#[allow(clippy::too_many_arguments)]
fn rest_along(
    b: &mut Builder,
    op: &RotaryRough,
    xs: &[f64],
    seg: f64,
    clg: &[Vec<f64>],
    _st: &[Vec<f64>],
    levels: &[f64],
    safe_r: f64,
    cl: &dyn Fn(f64, f64) -> f64,
    tol: f64,
    feeds: (f64, f64),
    takes: StockNear,
    mut bite: Option<Bite>,
) {
    let n_rev = clg.first().map_or(0, |r| r.len());
    if n_rev == 0 || xs.len() < 2 {
        return;
    }
    let k_of = |a: f64| ((a.rem_euclid(360.0) / seg).round() as usize) % n_rev;
    let mut forward = true;
    // one pass along X at A `a` from `xa` to `xb` at `level`, its feed's share at X `scale`;
    // `live`: the stock as the passes before it left it (else the operations' before)
    let pass = |b: &mut Builder,
                live: Option<(&crate::dexel::Dexel, &crate::dexel::ToolSolid)>,
                level: f64,
                a: f64,
                xa: f64,
                xb: f64,
                scale: &dyn Fn(f64) -> f64| {
        let zf = |x: f64, a: f64| py_max(level, cl(x, a));
        // the tool at the tip radius `r` there takes nothing
        let free = |x: f64, aa: f64, r: f64| match live {
            Some((dx, t)) => {
                let (tip, dir) = crate::dexel::pose(x, 0.0, r, aa);
                dx.inside(t, tip, dir) <= 0.05
            }
            None => takes(x, x, aa, aa, r) <= 0.0,
        };
        // down from just above what stands there: the level above where a pass of it went,
        // else the stock left
        // (down by rapid only to where the tool takes nothing: up from the pass a half mm at
        // a time; the stock's top along a ray alone missed what stood beside it)
        let mut above = zf(xa, a);
        while above < safe_r && !free(xa, a, above) {
            above += 0.5;
        }
        let z0 = zf(xa, a);
        let start = py_max(above + 0.5, z0 + 0.5).min(safe_r);
        // (Mar's impeller: 18,000 rapids up to the safe radius and back between short passes,
        // 53 min) from the end of the pass before, near here: up to where the tool takes
        // nothing there and on the way, over at the feed, and down
        let linked = match (b.x, b.r) {
            (Some(px), Some(pr)) if (px - xa).abs() <= LINK_REACH && pr < safe_r - 1e-9 => {
                let pa = b.a;
                let mut rl = pr.max(start);
                // (clear of the stock left, and of the part itself, at points a third of a mm
                // apart along the way: the overhang fixture's ledge between two passes'
                // angles, gouged 3 mm by a link)
                let way = (xa - px)
                    .abs()
                    .max((a - pa).abs().to_radians() * pr.max(1.0));
                let n = ((way / 0.3).ceil() as usize).max(4);
                let clear = |r: f64| {
                    (0..=n).all(|t| {
                        let f = t as f64 / n as f64;
                        let (x, aa) = (px + (xa - px) * f, pa + (a - pa) * f);
                        r >= cl(x, aa) + 0.05 && free(x, aa, r)
                    })
                };
                while rl < safe_r && !clear(rl) {
                    rl += 0.5;
                }
                (rl < safe_r).then_some(rl)
            }
            _ => None,
        };
        let top = match linked {
            Some(rl) => {
                b.cut(b.x.unwrap_or(xa), rl, b.a, feeds.0, feeds.1);
                b.cut(xa, rl, a, feeds.0, feeds.1);
                rl
            }
            None => {
                b.rapid_r(safe_r);
                b.rapid(Some(xa), None, Some(a));
                b.rapid_r(start);
                start
            }
        };
        let fd = |x: f64| (feeds.0 * scale(x), feeds.1);
        // (no plunge through what stands there: down a ramp along the pass at the operation's
        // ramp angle, then back over the ramp at depth)
        let tan = op.ramp_angle.clamp(2.0, 45.0).to_radians().tan();
        let len = (xb - xa).abs();
        let ramp_len = ((top - z0).max(0.0) / tan).min(len);
        let dir = if xb >= xa { 1.0 } else { -1.0 };
        if ramp_len > 0.05 && len > 0.5 {
            // (the ramp or the part, whichever is higher, followed as a pass is: the impeller's
            // blade wall rose 8 mm between two ramp points a quarter mm apart, and the
            // straight move between them gouged 5.4 mm)
            let zr = |x: f64, aa: f64| {
                let f = ((x - xa) * dir / ramp_len).clamp(0.0, 1.0);
                py_max(top + (z0 - top) * f, zf(x, aa))
            };
            let steps = (ramp_len / 0.25).ceil().max(1.0) as usize;
            for j in 1..=steps {
                let f = j as f64 / steps as f64;
                let x = xa + dir * ramp_len * f;
                let p0 = [b.x.unwrap_or(xa), b.r.unwrap_or(top), b.a];
                refine(b, &zr, p0, [x, zr(x, a), a], tol, 0, fd(x));
            }
            // the rest of the way down there, if the ramp ran out of pass
            let xe = xa + dir * ramp_len;
            b.plunge(zf(xe, a), feeds.1);
            // back over the ramp at depth, then the pass
            let n = (ramp_len / 0.5).ceil().max(1.0) as usize;
            for j in 1..=n {
                let x = xe - dir * ramp_len * j as f64 / n as f64;
                let p0 = [b.x.unwrap_or(x), b.r.unwrap_or(z0), b.a];
                refine(b, &zf, p0, [x, zf(x, a), a], tol, 0, fd(x));
            }
        } else {
            b.plunge(z0, feeds.1);
        }
        let n = (len / 0.5).ceil().max(1.0) as usize;
        for j in 1..=n {
            let x = xa + (xb - xa) * j as f64 / n as f64;
            let p0 = [b.x.unwrap_or(x), b.r.unwrap_or(z0), b.a];
            refine(b, &zf, p0, [x, zf(x, a), a], tol, 0, fd(x));
        }
    };
    let mut prev_level: Option<f64> = None;
    for &level in levels {
        if crate::budget::over() {
            break;
        }
        // half a step above this level, where a pass would take too much
        let half = prev_level.map_or(level + 0.5 * op.step_down, |p| 0.5 * (p + level));
        prev_level = Some(level);
        // angles a stepover apart at this radius
        let da = (op.step_x / level.max(0.5))
            .to_degrees()
            .clamp(0.2, seg.max(0.2) * 4.0);
        let n_a = (360.0 / da).ceil() as usize;
        for ia in 0..n_a {
            let a = ia as f64 * 360.0 / n_a as f64;
            let k = k_of(a);
            // (where the tool at this level would take any of what is left: the stock's top
            // along a ray alone also met the blades beside it)
            let every = 10;
            let mut mask = vec![false; xs.len()];
            let mut i = 0;
            // (the tool's width inside the range: the bar past the part's ends, towards the
            // chuck, is not the rest of the part's roughing; at every angle and level there
            // it went round 5 h)
            let (x_in0, x_in1) = (xs[0] + TOOL_R_GUESS, xs[xs.len() - 1] - TOOL_R_GUESS);
            while i < xs.len() {
                let c = clg[i][k];
                let z = level.max(c);
                // (by what the tool would take there, not the stock's top along a ray alone:
                // the impeller's hub between the blades, its ray's top on the hub, the stock
                // left beside it)
                let here = (x_in0..=x_in1).contains(&xs[i])
                    && c < level - 1e-6
                    // (a negative radius: more than a trace of it, see [`StockNear`])
                    && takes(xs[i], xs[i], a, a, -z) > 0.0;
                for m in mask.iter_mut().skip(i).take(every) {
                    *m = here;
                }
                i += every;
            }
            let runs = intervals(&mask, xs);
            for (xa, xb) in runs {
                let (xa, xb) = if forward { (xa, xb) } else { (xb, xa) };
                let Some(bt) = bite.as_mut() else {
                    pass(b, None, level, a, xa, xb, &|_| 1.0);
                    forward = !forward;
                    continue;
                };
                let limit = bt.limit;
                let zf = |x: f64, a: f64| py_max(level, cl(x, a));
                // the whole step: within the limit, as it is
                let mut log = RayLog::new();
                let full = take_pass(bt, &zf, xa, xb, a, Some(&mut log));
                bt.stock.restore(log);
                if full.iter().all(|s| s.1 <= limit) {
                    pass(b, Some((&*bt.stock, &bt.tool)), level, a, xa, xb, &|_| 1.0);
                    take_pass(bt, &zf, xa, xb, a, None);
                    forward = !forward;
                    continue;
                }
                // over: half the step first, then the rest of it, each slower where still over
                let zh = |x: f64, a: f64| py_max(half, cl(x, a));
                let mut log = RayLog::new();
                let first = take_pass(bt, &zh, xa, xb, a, Some(&mut log));
                bt.stock.restore(log);
                let (mut xa, mut xb) = (xa, xb);
                if first.iter().any(|s| s.1 > 0.05) {
                    pass(b, Some((&*bt.stock, &bt.tool)), half, a, xa, xb, &|x| {
                        bite_scale(&first, limit, x)
                    });
                    take_pass(bt, &zh, xa, xb, a, None);
                    forward = !forward;
                    (xa, xb) = (xb, xa);
                }
                let mut log = RayLog::new();
                let second = take_pass(bt, &zf, xa, xb, a, Some(&mut log));
                bt.stock.restore(log);
                pass(b, Some((&*bt.stock, &bt.tool)), level, a, xa, xb, &|x| {
                    bite_scale(&second, limit, x)
                });
                take_pass(bt, &zf, xa, xb, a, None);
                forward = !forward;
            }
        }
    }
    b.rapid_r(safe_r);
}

/// Rest roughing along X: the next pass's start this near the last one's end (mm, along X)
/// is reached at the feed, over the stock, not by way of the safe radius.
const LINK_REACH: f64 = 3.0;
/// Rest roughing along X: how far round its pass a tool reaches (mm), for the stock it comes
/// down over: as wide as the largest ball it is used with (3.175 mm).
const TOOL_R_GUESS: f64 = 1.6;
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
/// with its tip `r` from the axis (its width included): a stock that is not round. (Rest
/// roughing on the 3D stock: `r` negative, the tip at `-r`: whether the tool would take more
/// than a trace there, worth a pass.)
pub type StockNear<'a> = &'a dyn Fn(f64, f64, f64, f64, f64) -> f64;

/// Returns the lowest tip radius used. `stock_at` gives the bar's largest radius at an X when
/// it is not round from end to end (a bar carried from an earlier setup, turned down in
/// places): a level is cut only along X where the bar stands above it. `stock_near` (a stock
/// not round about the axis: a carried piece) says where round a level there is none: those
/// stretches are crossed at the air feed.
#[allow(clippy::too_many_arguments)]
pub fn rotary_rough(
    b: &mut Builder,
    dc: &DropCutter,
    stock_r: f64,
    safe_r: f64,
    op: &RotaryRough,
    stock_at: Option<&dyn Fn(f64) -> f64>,
    stock_near: Option<StockNear>,
    bite: Option<Bite>,
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
    // (Mar's impeller: rest roughing in full turns, 2 h 23, the A axis the bottleneck) the
    // rest along X at set angles: only where the stock is left
    if let Some(st) = &rest_stock {
        b.comment("--- rest roughing (along X) ---");
        if let Some(bt) = bite.as_ref().filter(|bt| bt.limit.is_finite()) {
            b.comment(format!("at most {:.2} mm3 a mm a pass", bt.limit));
        }
        rest_along(
            b,
            op,
            &xs,
            seg,
            &clg,
            st,
            &levels,
            safe_r,
            &cl,
            tol,
            feeds,
            stock_near.expect("rest"),
            bite,
        );
        return Ok(b.moves[first..]
            .iter()
            .filter_map(|m| match m {
                Move::Cut { r, .. } | Move::Plunge { r, .. } => Some(*r),
                _ => None,
            })
            .fold(f64::INFINITY, f64::min));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// (Mar's impeller) a pass's ramp in from above that crosses a blade's wall: the ball's
    /// path rises 10 mm in its radius, over the wall's edge, between two of the ramp's
    /// points; no move may go into it.
    #[test]
    fn rest_ramp_into_a_wall_follows_it() {
        let (edge, rb) = (-37.45 + 1.5875, 1.5875);
        let wall = |x: f64| {
            if x >= edge {
                16.0
            } else if x > edge - rb {
                (16.0 - rb + (rb * rb - (x - edge) * (x - edge)).sqrt()).max(6.0)
            } else {
                6.0
            }
        };
        let cl = |x: f64, _a: f64| wall(x);
        let op = RotaryRough {
            tool: 1,
            leave_axial: 0.0,
            x_range: [-40.0, -30.0],
            step_down: 1.0,
            step_x: 3.0,
            leave_radial: 0.0,
            sample_deg: 30.0,
            tolerance: 0.01,
            min_radius: 0.0,
            feed: 500.0,
            plunge_feed: 150.0,
            ramp_angle: 2.0,
            smoothing: None,
            rest: true,
            max_bite: None,
        };
        let xs: Vec<f64> = (0..=200).map(|i| -40.0 + i as f64 * 0.05).collect();
        let seg = 30.0;
        let clg: Vec<Vec<f64>> = xs.iter().map(|&x| vec![wall(x); 12]).collect();
        // stock up to radius 9 everywhere over the part
        let takes =
            |_: f64, _: f64, _: f64, _: f64, r: f64| if r.abs() < 9.0 { r.abs() } else { 0.0 };
        let mut b = Builder::new();
        rest_along(
            &mut b,
            &op,
            &xs,
            seg,
            &clg,
            &[],
            &[8.0],
            25.0,
            &cl,
            0.01,
            (500.0, 150.0),
            &takes,
            None,
        );
        let (mut x, mut r, mut a) = (None::<f64>, None::<f64>, 0.0);
        let mut cuts = 0;
        for m in &b.moves {
            match *m {
                Move::Rapid {
                    x: nx,
                    r: nr,
                    a: na,
                } => {
                    x = nx.or(x);
                    r = nr.or(r);
                    a = na.unwrap_or(a);
                }
                Move::Plunge { r: nr, .. } => r = Some(nr),
                Move::Cut {
                    x: nx,
                    r: nr,
                    a: na,
                    ..
                } => {
                    if let (Some(x0), Some(r0)) = (x, r) {
                        cuts += 1;
                        for k in 0..=20 {
                            let t = k as f64 / 20.0;
                            let (xi, ri) = (x0 + (nx - x0) * t, r0 + (nr - r0) * t);
                            // (as the gouge check measures: below the part, and that far
                            // along X from where the tool clears it)
                            let deficit = wall(xi) - ri;
                            let along = (0..=200)
                                .map(|s| s as f64 * 0.001)
                                .find(|d| wall(xi - d) <= ri || wall(xi + d) <= ri)
                                .unwrap_or(0.2);
                            assert!(
                                deficit.min(along) < 0.02,
                                "into the wall: X{xi:.3} R{ri:.3}, the part at {:.3} (from X{x0:.3} R{r0:.3} to X{nx:.3} R{nr:.3})",
                                wall(xi)
                            );
                        }
                    }
                    x = Some(nx);
                    r = Some(nr);
                    a = na;
                }
                _ => {}
            }
        }
        assert!(cuts > 10, "{cuts} cuts");
    }

    /// A limit on the bite: a pass over it goes in two, half a level apart, and slower where
    /// still over (not below 0.6 of the feed); without one, every pass a full step.
    #[test]
    fn rest_passes_over_the_bite_go_in_two_and_slower() {
        use crate::dexel::{pose, Dexel, ToolSolid};
        let tool = ToolSolid {
            radius: 1.5875,
            ball: true,
            height: 20.0,
        };
        let bar = Dexel::bar(10.0, 20.0, 0.2);
        let cl = |_x: f64, _a: f64| 7.0;
        let op = RotaryRough {
            tool: 1,
            leave_axial: 0.0,
            x_range: [-15.0, -5.0],
            step_down: 1.0,
            step_x: 0.6,
            leave_radial: 0.0,
            sample_deg: 6.0,
            tolerance: 0.01,
            min_radius: 0.0,
            feed: 500.0,
            plunge_feed: 150.0,
            ramp_angle: 30.0,
            smoothing: None,
            rest: true,
            max_bite: Some(1.5),
        };
        let xs: Vec<f64> = (0..=200).map(|i| -15.0 + i as f64 * 0.05).collect();
        let clg: Vec<Vec<f64>> = xs.iter().map(|_| vec![7.0; 60]).collect();
        let takes = |x: f64, _: f64, a: f64, _: f64, r: f64| {
            let (rr, least) = if r < 0.0 { (-r, 0.5) } else { (r, 0.05) };
            let (tip, dir) = pose(x, 0.0, rr, a);
            if bar.inside(&tool, tip, dir) > least {
                rr
            } else {
                0.0
            }
        };
        let plan = |limit: Option<f64>| {
            let mut live = bar.clone();
            let mut b = Builder::new();
            let bite = limit.map(|limit| Bite {
                stock: &mut live,
                tool,
                limit,
            });
            rest_along(
                &mut b,
                &op,
                &xs,
                6.0,
                &clg,
                &[],
                &[9.0],
                25.0,
                &cl,
                0.01,
                (500.0, 150.0),
                &takes,
                bite,
            );
            b.moves
        };
        let cuts = |m: &[Move]| -> Vec<(f64, f64)> {
            m.iter()
                .filter_map(|m| match *m {
                    Move::Cut { r, feed, .. } => Some((r, feed)),
                    _ => None,
                })
                .collect()
        };
        let plain = cuts(&plan(None));
        assert!(
            !plain.iter().any(|c| (c.0 - 9.5).abs() < 1e-6),
            "a half level without a limit"
        );
        assert!(plain.iter().all(|c| c.1 >= 500.0 - 1e-9 || c.1 > 600.0));
        let limited = cuts(&plan(Some(1.5)));
        let halves = limited.iter().filter(|c| (c.0 - 9.5).abs() < 1e-6).count();
        assert!(halves > 5, "{halves} cuts at the half level");
        let slow: Vec<f64> = limited
            .iter()
            .filter(|c| c.1 < 500.0 - 1e-9)
            .map(|c| c.1)
            .collect();
        assert!(!slow.is_empty(), "nothing slower over the limit");
        assert!(slow.iter().all(|&f| f >= 300.0 - 1e-9), "{slow:?}");
    }
}
