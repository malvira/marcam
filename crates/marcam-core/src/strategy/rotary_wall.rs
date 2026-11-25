//! Rotary wall finishing (4-axis simultaneous): waterlines on the walls that radiate from the
//! axis (an impeller's blade faces), A and X together along their twist. A ball dropped
//! radially only grazes such a wall, touching it at one height a pass (Mar's impeller: 0.1 to
//! 0.5 mm left on every blade face after the rotary finish). Here, at radii `step_down` apart,
//! the tool's tip stays at that radius and runs along each wall where the part rises past it:
//! the edges of the stretches round the axis that are clear at that radius
//! ([`super::rotary_flowline::channels_at`]), both faces of every blade, top down.

use super::rotary_finish::refine;
use super::rotary_flowline::{channels_at, SEARCH_DEG};
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
    // (the part round the axis at each X once, for every level's search: the impeller's
    // 60 levels each searching it again was past the planning budget)
    let na = (360.0 / SEARCH_DEG).round() as usize;
    let rows: Vec<Vec<f64>> = xs
        .iter()
        .map(|&x| (0..na).map(|k| cl(x, k as f64 * SEARCH_DEG)).collect())
        .collect();
    let dxs = (x1 - x0) / m as f64;
    let cached = |x: f64, a: f64| {
        let i = (((x - x0) / dxs.max(1e-12)).round().max(0.0) as usize).min(rows.len() - 1);
        let k = ((a.rem_euclid(360.0) / SEARCH_DEG).round() as usize) % na;
        rows[i][k]
    };
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
        for c in channels_at(&cached, &xs, level) {
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

/// A feed move of the upright tool `y` off the axis from `p0` to `p1` (X, tip Z, A), split
/// where the part rises above the straight move (`zf` the tip's lowest height there), as
/// poses; each step turns A at most `max_da` degrees (its feed timed along the chord).
#[allow(clippy::too_many_arguments)]
fn offset_refine(
    out: &mut Vec<Move>,
    zf: &dyn Fn(f64, f64) -> f64,
    p0: [f64; 3],
    p1: [f64; 3],
    y: f64,
    tol: f64,
    depth: u32,
    feeds: (f64, f64, f64),
) {
    let (feed, plunge, ramp_tan) = feeds;
    let at = |t: f64| [0, 1, 2].map(|k| p0[k] + (p1[k] - p0[k]) * t);
    let rises = [0.25, 0.5, 0.75].iter().any(|&t| {
        let p = at(t);
        zf(p[0], p[2]) > p[1] + tol
    });
    if (rises && depth < 14) || (p1[2] - p0[2]).abs() > MAX_DA {
        let m = at(0.5);
        // (on the part where it rises, else on the straight move: only cut up for turning)
        let mid = [m[0], if rises { zf(m[0], m[2]) } else { m[1] }, m[2]];
        offset_refine(out, zf, p0, mid, y, tol, depth + 1, feeds);
        offset_refine(out, zf, mid, p1, y, tol, depth + 1, feeds);
        return;
    }
    // (down steeper than the ramp angle, as the verifier reads the machine's move: no
    // faster than the plunge feed, a tenth under and as the written digits may make it)
    let dz = p1[1] - p0[1];
    let mut f = feed;
    if dz < 0.0 {
        let radius = 0.5 * (y.hypot(p0[1]) + y.hypot(p1[1]));
        let across = (p1[0] - p0[0]).hypot(radius.max(0.5) * (p1[2] - p0[2]).to_radians());
        if !crate::toolpath::gentle_descent(-dz, across, ramp_tan) {
            let t0 = crate::kinematics::to_part([p0[0], y, p0[1]], p0[2]);
            let t1 = crate::kinematics::to_part([p1[0], y, p1[1]], p1[2]);
            let len = ((t1[0] - t0[0]).powi(2) + (t1[1] - t0[1]).powi(2) + (t1[2] - t0[2]).powi(2))
                .sqrt();
            f = f.min(0.9 * plunge * (len - 0.002).max(1e-4) / (0.001 - dz));
        }
    }
    out.push(upright(p1[0], y, p1[1], p1[2], Some(f)));
}

/// At the walls' feet, how far in across the floor the offset finish goes (mm), and its
/// passes' distance apart there.
const FLOOR_REACH: f64 = 3.0;
const FLOOR_STEP: f64 = 0.4;
/// The floor at the walls' feet: within this of its lowest at each X (mm).
const FLOOR_WALL: f64 = 0.3;

/// The offset finish's grid round the axis: this many points to [`SEARCH_DEG`].
const FINE: usize = 2;

/// The most A turns in one pose of the offset finish (degrees): the replay and the checks
/// follow the turn from where it starts, and the post times the feed along the chord.
const MAX_DA: f64 = 1.0;

/// The tool upright at machine (x, y, z) with the part at A `a`, as a pose in the part frame.
fn upright(x: f64, y: f64, z: f64, a: f64, feed: Option<f64>) -> Move {
    Move::Pose {
        tip: crate::kinematics::to_part([x, y, z], a),
        axis: crate::kinematics::to_part([0.0, 0.0, 1.0], a),
        feed,
    }
}

/// [`rotary_wall`] with the tool upright `op.y_offset` off the axis (the faces either side
/// of each wall, the offset to one side, then the other): its shank leans off the radius,
/// clear of a wall leaning over the face it cuts (Mar's impeller: the faces near the hub,
/// which a tool over the axis cannot reach for the blade above). `dc` must reach that far.
pub fn rotary_wall_offset(
    b: &mut Builder,
    dc: &DropCutter,
    safe_r: f64,
    op: &RotaryWall,
) -> Result<f64, String> {
    if op.step_down <= 0.0 || op.sample_mm <= 0.0 {
        return Err("step_down and sample_mm must be > 0".into());
    }
    let floor = op.min_radius.max(0.0);
    let [x0, x1] = [
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    ];
    let m = ((x1 - x0) / op.sample_mm).ceil().max(1.0) as usize;
    let xs: Vec<f64> = (0..=m)
        .map(|i| x0 + (x1 - x0) * i as f64 / m as f64)
        .collect();
    let tol = op.tolerance.max(1e-4);
    let feeds = (
        op.feed,
        op.plunge_feed,
        op.ramp_angle.clamp(0.0, 89.0).to_radians().tan(),
    );
    let na = (360.0 / SEARCH_DEG).round() as usize;
    let dxs = (x1 - x0) / m as f64;
    let mut out: Vec<Move> = Vec::new();
    let mut lowest = f64::INFINITY;
    let mut any = false;
    // where the machine is: (x, z, a), y
    let mut at: Option<([f64; 3], f64)> = None;
    for &y in &[op.y_offset.abs(), -op.y_offset.abs()] {
        // the tip's lowest height at each X and A, the tool this far off
        // (on a grid FINE_DEG apart round the axis at each X sample, once: dropping the
        // offset tool afresh at every point of every pass was past the planning budget on
        // the impeller; between grid points, the highest of those round it)
        let nf = na * FINE;
        // (the whole turn at the search's spacing, for the stretches; the finer points only
        // where a pass goes, as it needs them)
        let rows: Vec<Vec<f64>> = xs
            .iter()
            .map(|&x| {
                (0..na)
                    .map(|k| {
                        dc.radius_at_offset(x, k as f64 * SEARCH_DEG, y, floor) + op.leave_radial
                    })
                    .collect()
            })
            .collect();
        let memo: std::cell::RefCell<std::collections::HashMap<(usize, usize), f64>> =
            Default::default();
        let fine_at = |i: usize, k: usize| -> f64 {
            if k.is_multiple_of(FINE) {
                return rows[i][k / FINE];
            }
            if let Some(&v) = memo.borrow().get(&(i, k)) {
                return v;
            }
            let v = dc.radius_at_offset(xs[i], k as f64 * SEARCH_DEG / FINE as f64, y, floor)
                + op.leave_radial;
            memo.borrow_mut().insert((i, k), v);
            v
        };
        let cached = |x: f64, a: f64| {
            let i = (((x - x0) / dxs.max(1e-12)).round().max(0.0) as usize).min(rows.len() - 1);
            let k = ((a.rem_euclid(360.0) / SEARCH_DEG).round() as usize) % na;
            rows[i][k]
        };
        let cl = |x: f64, a: f64| {
            let fx = ((x - x0) / dxs.max(1e-12)).clamp(0.0, (rows.len() - 1) as f64);
            let fa = a.rem_euclid(360.0) / (SEARCH_DEG / FINE as f64);
            let (i0, i1) = (
                fx.floor() as usize,
                (fx.ceil() as usize).min(rows.len() - 1),
            );
            let (k0, k1) = (fa.floor() as usize % nf, fa.ceil() as usize % nf);
            fine_at(i0, k0)
                .max(fine_at(i0, k1))
                .max(fine_at(i1, k0))
                .max(fine_at(i1, k1))
        };
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for row in &rows {
            for &r in row {
                lo = lo.min(r);
                hi = hi.max(r);
            }
        }
        // (top down; then the floor itself at the walls' feet, followed as it rises and falls
        // along X (an impeller's hub is a cone): None)
        let mut levels = Vec::new();
        let mut l = hi - 0.5 * op.step_down;
        while l > lo + 0.02 {
            levels.push(Some(l));
            l -= op.step_down;
        }
        levels.push(None);
        let br = dc.cutter().radius();
        for level in levels {
            if crate::budget::over() {
                break;
            }
            let zf = |x: f64, a: f64| match level {
                Some(l) => cl(x, a).max(l),
                None => cl(x, a),
            };
            // (at the walls' feet, passes in across the floor too, up to 3 mm or halfway: the
            // floor under a wall leaning over it, which nothing over the axis reaches)
            let floor_passes = if level.is_none() {
                (FLOOR_REACH / FLOOR_STEP).ceil() as usize
            } else {
                0
            };
            // (a step on the floor round the axis: at the radius the ball touches it, its
            // centre `y` off and the tip's height plus its radius up, less its radius)
            let dstep_at = |x: f64, a: f64| {
                let touch = (y.hypot(zf(x, a) + br) - br).max(0.5);
                (FLOOR_STEP / touch).to_degrees()
            };
            let chans = match level {
                Some(l) => channels_at(&cached, &xs, l),
                None => super::rotary_flowline::channels(&cached, &xs, FLOOR_WALL, false),
            };
            for c in chans {
                any = true;
                let n = c.xs.len();
                // (the face this offset leans off: its own side of the stretch; the other
                // side's face is the other offset's)
                let edge = |i: usize| if y > 0.0 { c.hi[i] } else { c.lo[i] };
                let start = edge(0);
                // up, round at the safe height, down beside the wall
                let a0 = match at {
                    Some((p, py)) => {
                        out.push(upright(p[0], py, safe_r, p[2], None));
                        start + ((p[2] - start) / 360.0).round() * 360.0
                    }
                    None => start,
                };
                let turn = a0 - start;
                out.push(upright(c.xs[0], y, safe_r, a0, None));
                let z0 = zf(c.xs[0], a0);
                out.push(upright(c.xs[0], y, z0 + 0.5, a0, None));
                out.push(upright(c.xs[0], y, z0, a0, Some(op.plunge_feed)));
                let mut p = [c.xs[0], z0, a0];
                // inward across the floor: from the higher edge down, from the lower up
                let inward = if y > 0.0 { -1.0 } else { 1.0 };
                for k in 0..=floor_passes {
                    let pass_a = |i: usize| {
                        let half = 0.5 * (c.hi[i] - c.lo[i]);
                        let d = dstep_at(c.xs[i], edge(i));
                        edge(i) + inward * (k as f64 * d).min(half) + turn
                    };
                    let idx: Vec<usize> = if k % 2 == 0 {
                        (0..n).collect()
                    } else {
                        (0..n).rev().collect()
                    };
                    for (j, &i) in idx.iter().enumerate() {
                        if k == 0 && j == 0 {
                            continue;
                        }
                        let a = pass_a(i);
                        let q = [c.xs[i], zf(c.xs[i], a), a];
                        offset_refine(&mut out, &zf, p, q, y, tol, 0, feeds);
                        // (its tip's distance from the axis, for the flutes' reach)
                        lowest = lowest.min(y.hypot(q[1]));
                        p = q;
                    }
                }
                out.push(upright(p[0], y, safe_r, p[2], None));
                at = Some((p, y));
            }
        }
    }
    if !any {
        b.warnings.push(
            "rotary wall finishing: no wall rising from the axis in its X range, nothing to follow"
                .into(),
        );
        return Ok(f64::INFINITY);
    }
    // back over the axis at the safe height
    if let Some((p, _)) = at {
        out.push(upright(p[0], 0.0, safe_r, p[2], None));
        b.moves.extend(out);
        b.rapid(Some(p[0]), Some(safe_r), Some(p[2]));
    }
    Ok(lowest)
}
