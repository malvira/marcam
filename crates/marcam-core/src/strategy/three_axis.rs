//! First 3-axis operations (tool along +Z, work held in a vise): facing, rectangular pocket,
//! drilling (pecking with an end mill), 2D contour and a 3D drop-cutter raster over a mesh.
//! Moves are `Rapid3` / `Line3` in the setup's work coordinates (Z up, Z0 usually the stock
//! top). Every pass starts and ends at the safe Z; rapids never move XY below it.

use crate::det::Det;
use crate::dropcutter::{drop_triangle, Cutter};
use crate::job::{Contour, Drill, Face, Pocket, Raster3d, StockBox};
use crate::mesh::Mesh;
use crate::toolpath::Builder;

const UP: [f64; 3] = [0.0, 0.0, 1.0];

thread_local! {
    /// The highest material left in the way of the operation being planned (the stock as the
    /// operations before it left it), when lower than the stock's top: see [`with_material`].
    static MATERIAL: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Plan with the material in the tool's way no higher than `top` (Mar: in-process stock):
/// [`levels`] leaves out the passes wholly above it, [`material_top`] starts there.
pub fn with_material<T>(top: Option<f64>, f: impl FnOnce() -> T) -> T {
    MATERIAL.with(|m| m.set(top));
    let out = f();
    MATERIAL.with(|m| m.set(None));
    out
}

thread_local! {
    /// Stock left on the walls by the operation being planned (pockets and contours: see
    /// [`LeaveGuard`]).
    static LEAVE_XY: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// The walls' stock to leave set while it lives.
pub struct LeaveGuard;

impl LeaveGuard {
    pub fn set(v: f64) -> LeaveGuard {
        LEAVE_XY.with(|m| m.set(v));
        LeaveGuard
    }
}

impl Drop for LeaveGuard {
    fn drop(&mut self) {
        LEAVE_XY.with(|m| m.set(0.0));
    }
}

fn leave_xy() -> f64 {
    LEAVE_XY.with(|m| m.get())
}

/// [`MATERIAL`] set while it lives (cleared when it goes, on an error too).
pub struct MaterialGuard;

impl MaterialGuard {
    pub fn set(top: Option<f64>) -> MaterialGuard {
        MATERIAL.with(|m| m.set(top));
        MaterialGuard
    }
}

impl Drop for MaterialGuard {
    fn drop(&mut self) {
        MATERIAL.with(|m| m.set(None));
    }
}

/// Where the material in the way of the operation being planned starts: `top`, or lower
/// when the operations before it cut it down.
pub fn material_top(top: f64) -> f64 {
    MATERIAL.with(|m| m.get()).map_or(top, |t| t.min(top))
}

/// Levels from `top` down to `bottom`, at most `step` apart, ending exactly at `bottom`. (The
/// operations before it having cut the material down: those wholly above it left out, the
/// last always kept.)
pub fn levels(top: f64, bottom: f64, step: f64) -> Vec<f64> {
    if bottom >= top - 1e-9 {
        return vec![bottom];
    }
    let n = ((top - bottom) / step.max(1e-3)).ceil().max(1.0) as usize;
    let all = (1..=n).map(|k| top - (top - bottom) * k as f64 / n as f64);
    match MATERIAL.with(|m| m.get()) {
        // (a level cuts what is above it, down to it: in air when the material is no higher)
        Some(m) => all
            .filter(|&z| z < m - 1e-6 || (z - bottom).abs() < 1e-9)
            .collect(),
        None => all.collect(),
    }
}

/// Feed down to `z` at the plunge feed, from the safe height above (x, y).
fn enter(b: &mut Builder, safe: f64, x: f64, y: f64, z_start: f64, z: f64, plunge: f64) {
    b.rapid3(None, None, Some(safe));
    b.rapid3(Some(x), Some(y), None);
    b.rapid3(None, None, Some(z_start));
    b.line3(x, y, z, UP, plunge);
}

fn leave(b: &mut Builder, safe: f64) {
    b.rapid3(None, None, Some(safe));
}

/// Back-and-forth passes along X covering [x0, x1] x [y0, y1] (tool centre) at height z.
/// `area` is [x0, x1, y0, y1].
fn zigzag(b: &mut Builder, area: [f64; 4], z: f64, stepover: f64, feed: f64) {
    let [x0, x1, y0, y1] = area;
    let n = (((y1 - y0) / stepover.max(1e-3)).ceil() as usize).max(1);
    for k in 0..=n {
        if crate::budget::over() {
            break;
        }
        let y = y0 + (y1 - y0) * k as f64 / n as f64;
        let (xa, xb) = if k % 2 == 0 { (x0, x1) } else { (x1, x0) };
        b.line3(xa, y, z, UP, feed);
        b.line3(xb, y, z, UP, feed);
    }
}

/// (Mar: "a way to clear all the remaining stock") facing everything of the stock at each
/// level down to the face's depth, out to the stock's edges: zigzag rows across the stock,
/// broken (up and over) where the part stands above the level (`dc`: the part, with the tool).
pub fn face_clear(
    b: &mut Builder,
    d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Face,
    dc: &DropCutter3,
) -> Result<(), String> {
    let r = d / 2.0;
    let top = stock.max[2];
    let bottom = top - op.depth;
    let (x0, x1) = (stock.min[0] - r * 0.5, stock.max[0] + r * 0.5);
    let (y0, y1) = (stock.min[1] + r * 0.5, stock.max[1] - r * 0.5);
    if x1 <= x0 || y1 <= y0 {
        return Err("facing: the stock is narrower than the tool".into());
    }
    b.comment("--- facing to the stock's edges ---");
    let step = op.stepover * d;
    let n = (((y1 - y0) / step.max(1e-3)).ceil() as usize).max(1);
    let m = (((x1 - x0) / 0.25).ceil() as usize).max(1);
    let mut any = false;
    for z in levels(top, bottom, op.step_down) {
        let mut down = false;
        for k in 0..=n {
            if crate::budget::over() {
                break;
            }
            let y = y0 + (y1 - y0) * k as f64 / n as f64;
            let xs: Vec<f64> = (0..=m)
                .map(|i| x0 + (x1 - x0) * i as f64 / m as f64)
                .collect();
            let xs: Vec<f64> = if k % 2 == 0 {
                xs
            } else {
                xs.into_iter().rev().collect()
            };
            // the row's stretches where the tool at this level stays clear of the part
            let mut run: Vec<f64> = Vec::new();
            let flush = |run: &mut Vec<f64>, b: &mut Builder, down: &mut bool| {
                if run.len() >= 2 {
                    let (xa, xb) = (run[0], run[run.len() - 1]);
                    if !*down {
                        enter(b, safe, xa, y, z + 1.0, z, op.plunge_feed);
                        *down = true;
                    } else {
                        // over the part (or to the next row's start): up clear, across, down
                        b.rapid3(None, None, Some(safe));
                        b.rapid3(Some(xa), Some(y), None);
                        b.line3(xa, y, z, UP, op.plunge_feed);
                    }
                    b.line3(xb, y, z, UP, op.feed);
                }
                run.clear();
            };
            for &x in &xs {
                if dc.z_at(x, y, f64::NEG_INFINITY) <= z + 0.01 {
                    run.push(x);
                } else {
                    flush(&mut run, b, &mut down);
                }
            }
            flush(&mut run, b, &mut down);
        }
        if down {
            any = true;
            leave(b, safe);
        }
    }
    if !any {
        return Err(
            "facing to the stock's edges: the part stands above the whole stock at that depth"
                .into(),
        );
    }
    Ok(())
}

/// `paths` (even-odd: an outline and its openings) with the outline alone grown by `g`
/// (outward), the openings as they are.
fn grow_outline_only(paths: &[Vec<[f64; 2]>], g: f64) -> Vec<Vec<[f64; 2]>> {
    let area = |p: &Vec<[f64; 2]>| {
        let n = p.len();
        (0..n)
            .map(|i| p[i][0] * p[(i + 1) % n][1] - p[(i + 1) % n][0] * p[i][1])
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let Some(k) = (0..paths.len()).max_by(|&a, &b| area(&paths[a]).total_cmp(&area(&paths[b])))
    else {
        return paths.to_vec();
    };
    let field = crate::offset::DistanceField::padded(
        &paths[k..k + 1],
        POCKET_FIELD_CELL,
        f64::INFINITY,
        g + 1.0,
    );
    let grown = field.loops(-g);
    if grown.len() != 1 {
        return paths.to_vec();
    }
    let mut out = grown;
    out.extend(
        paths
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != k)
            .map(|(_, p)| p.clone()),
    );
    out
}

pub fn face(b: &mut Builder, d: f64, stock: &StockBox, safe: f64, op: &Face) -> Result<(), String> {
    if !op.paths.is_empty() {
        // bounded by a face: cleared like a shaped pocket from the stock top down
        let p = Pocket {
            tool: op.tool,
            index_a: op.index_a,
            rect: [0.0; 4],
            paths: op.paths.clone(),
            depth: op.depth,
            top: None,
            step_down: op.step_down,
            stepover: op.stepover,
            feed: op.feed,
            plunge_feed: op.plunge_feed,
            ramp: 0.0,
            adaptive: false,
            smoothing: None,
            regions: Vec::new(),
            in_process: op.in_process,
            leave_xy: 0.0,
            leave_z: 0.0,
        };
        b.comment("--- facing (bounded by a face) ---");
        // (outward only: the outline grown, its openings as they are)
        if op.outward_only && op.boundary_offset > 0.0 && op.paths.len() > 1 {
            let p = Pocket {
                paths: grow_outline_only(&op.paths, op.boundary_offset),
                ..p
            };
            return path_pocket(b, d, stock, safe, &p, 0.0);
        }
        return path_pocket(b, d, stock, safe, &p, op.boundary_offset);
    }
    let r = d / 2.0;
    let top = stock.max[2];
    let bottom = top - op.depth;
    // overhang the stock by the tool radius so the edges are cleared (and by the boundary
    // offset: past the stock, or short of its edges)
    let g = op.boundary_offset;
    let (x0, x1) = (stock.min[0] - r * 0.5 - g, stock.max[0] + r * 0.5 + g);
    let (y0, y1) = (stock.min[1] + r * 0.5 - g, stock.max[1] - r * 0.5 + g);
    if x1 <= x0 || y1 <= y0 {
        return Err(format!(
            "facing: a boundary offset of {g} mm leaves nothing of the stock to face"
        ));
    }
    b.comment("--- facing ---");
    for z in levels(top, bottom, op.step_down) {
        if crate::budget::over() {
            break;
        }
        enter(b, safe, x0 - r - 1.0, y0, z + 1.0, z, op.plunge_feed);
        b.line3(x0, y0, z, UP, op.feed);
        zigzag(b, [x0, x1, y0, y1], z, op.stepover * d, op.feed);
        leave(b, safe);
    }
    Ok(())
}

pub fn pocket(
    b: &mut Builder,
    d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Pocket,
) -> Result<(), String> {
    if op.adaptive {
        return adaptive_pocket(b, d, stock, safe, op);
    }
    if !op.paths.is_empty() {
        return path_pocket(b, d, stock, safe, op, -leave_xy());
    }
    // (the walls' stock to leave: the tool kept that much further in)
    let r = d / 2.0 + leave_xy();
    let [px0, py0, px1, py1] = op.rect;
    let (x0, x1, y0, y1) = (
        px0.min(px1) + r,
        px0.max(px1) - r,
        py0.min(py1) + r,
        py0.max(py1) - r,
    );
    if op.rect == [0.0; 4] {
        return Err("a pocket needs a rect or closed paths".into());
    }
    if x0 > x1 || y0 > y1 {
        return Err(format!(
            "pocket {:?} is narrower than the {d} mm tool",
            op.rect
        ));
    }
    let top = op.top.unwrap_or(stock.max[2]);
    b.comment("--- pocket ---");
    for z in levels(top, top - op.depth, op.step_down) {
        if crate::budget::over() {
            break;
        }
        // in: down along X, back and forth at the ramp's angle (or straight down at the
        // plunge feed), clear, then one finishing lap of the walls
        let z_start = z + op.step_down.max(0.5);
        if op.ramp > 0.0 && x1 - x0 > 1e-6 {
            ramp_in(
                b,
                safe,
                [x0, y0],
                [x1, y0],
                z_start,
                z,
                op.ramp,
                op.feed,
                op.plunge_feed,
            );
        } else {
            enter(b, safe, x0, y0, z_start, z, op.plunge_feed);
        }
        zigzag(b, [x0, x1, y0, y1], z, op.stepover * d, op.feed);
        for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)] {
            b.line3(x, y, z, UP, op.feed);
        }
        leave(b, safe);
    }
    Ok(())
}

/// Down from `z_start` to `z` along the line from `a` to `b` and back, at `angle` degrees below
/// the horizontal (fed so the tip falls no faster than `plunge`), ending at `a` at depth.
#[allow(clippy::too_many_arguments)]
fn ramp_in(
    bld: &mut Builder,
    safe: f64,
    a: [f64; 2],
    b: [f64; 2],
    z_start: f64,
    z: f64,
    angle: f64,
    feed: f64,
    plunge: f64,
) {
    bld.rapid3(None, None, Some(safe));
    bld.rapid3(Some(a[0]), Some(a[1]), None);
    bld.rapid3(None, None, Some(z_start));
    let len = (b[0] - a[0]).dhypot(b[1] - a[1]);
    let drop = len * angle.to_radians().dtan().max(1e-3);
    let mut at = a;
    let mut zz = z_start;
    let mut guard = 0;
    while zz > z + 1e-9 && guard < 10_000 {
        let to = if at == a { b } else { a };
        let next = (zz - drop).max(z);
        // (the last leg partway, where it reaches the depth)
        let frac = if zz - next < drop {
            (zz - next) / drop
        } else {
            1.0
        };
        let p = [
            at[0] + (to[0] - at[0]) * frac,
            at[1] + (to[1] - at[1]) * frac,
        ];
        let f = descent_feed([at[0], at[1], zz], [p[0], p[1], next], feed, plunge, 0.0);
        bld.line3(p[0], p[1], next, UP, f);
        at = if frac < 1.0 { p } else { to };
        zz = next;
        guard += 1;
    }
    // back to the start, at depth
    bld.line3(a[0], a[1], z, UP, feed);
}

/// The unit vector from `a` to `b` (zero when they meet).
fn unit2(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let l = (b[0] - a[0]).dhypot(b[1] - a[1]);
    if l < 1e-12 {
        [1.0, 0.0]
    } else {
        [(b[0] - a[0]) / l, (b[1] - a[1]) / l]
    }
}

/// Down from `z_start` to `z` along the closed loop `lp` (its last point repeats its first)
/// from its point `s`, at `angle` degrees below the horizontal (fed so the tip falls no faster
/// than `plunge`), round it as often as needed. Returns where it reached the depth: the
/// segment it is on (its first point's index) and the point.
#[allow(clippy::too_many_arguments)]
fn ramp_along(
    b: &mut Builder,
    safe: f64,
    lp: &[[f64; 2]],
    s: usize,
    z_start: f64,
    z: f64,
    angle: f64,
    feed: f64,
    plunge: f64,
) -> (usize, [f64; 2]) {
    let n = lp.len() - 1;
    let tan = angle.clamp(0.5, 45.0).to_radians().dtan();
    b.rapid3(None, None, Some(safe));
    b.rapid3(Some(lp[s][0]), Some(lp[s][1]), None);
    b.rapid3(None, None, Some(z_start));
    let (mut k, mut at, mut zz) = (s, lp[s], z_start);
    // (a loop shorter than a hair: nothing to ramp along)
    let perimeter: f64 = (0..n)
        .map(|i| (lp[i + 1][0] - lp[i][0]).dhypot(lp[i + 1][1] - lp[i][1]))
        .sum();
    if perimeter < 1e-3 {
        b.line3(at[0], at[1], z, UP, plunge);
        return (k, at);
    }
    let mut guard = 0;
    while zz > z + 1e-9 && guard < 100_000 {
        let to = lp[(k + 1) % n];
        let seg = (to[0] - at[0]).dhypot(to[1] - at[1]);
        let fall = seg * tan;
        if zz - fall > z {
            let next = [to[0], to[1], zz - fall];
            let f = descent_feed([at[0], at[1], zz], next, feed, plunge, 0.0);
            b.line3(to[0], to[1], zz - fall, UP, f);
            at = to;
            zz -= fall;
            k = (k + 1) % n;
        } else {
            // the depth reached partway along this segment
            let t = if fall > 0.0 { (zz - z) / fall } else { 1.0 };
            let p = [at[0] + (to[0] - at[0]) * t, at[1] + (to[1] - at[1]) * t];
            let f = descent_feed([at[0], at[1], zz], [p[0], p[1], z], feed, plunge, 0.0);
            b.line3(p[0], p[1], z, UP, f);
            at = p;
            zz = z;
        }
        guard += 1;
    }
    (k, at)
}

/// Largest spacing between a shaped pocket's offset loops, in tool diameters (see
/// [`path_pocket`]): a larger stepover is cut at this.
pub const MAX_LOOP_SPACING: f64 = 0.7;
/// Grid spacing of the distance field a shaped pocket is offset on (mm).
pub const POCKET_FIELD_CELL: f64 = 0.05;
/// Offset loops are simplified to within this (mm).
pub const POCKET_LOOP_TOL: f64 = 0.002;

/// The tool-centre loops that clear a shaped pocket at one depth, innermost first: offsets of
/// the outline (and round its islands) at the tool radius, then every `step` deeper in. The
/// last loop of each region is the finishing pass along the walls.
pub fn pocket_loops(
    paths: &[Vec<[f64; 2]>],
    r: f64,
    step: f64,
) -> Result<Vec<Vec<[f64; 2]>>, String> {
    pocket_loops_grown(paths, r, step, 0.0)
}

/// [`pocket_loops`] for the area `grow` mm past the outlines (shrunk when negative): the
/// outermost loop `r - grow` in from them, holes in it smaller by as much.
pub fn pocket_loops_grown(
    paths: &[Vec<[f64; 2]>],
    r: f64,
    step: f64,
    grow: f64,
) -> Result<Vec<Vec<[f64; 2]>>, String> {
    let field = crate::offset::DistanceField::padded(
        paths,
        POCKET_FIELD_CELL,
        f64::INFINITY,
        grow.max(0.0) + if grow > 0.0 { r } else { 0.0 },
    );
    let deepest = field.max();
    let first = r - grow;
    if deepest <= first + 1e-6 {
        return Err(format!(
            "the pocket is nowhere wide enough for the {} mm tool",
            2.0 * r
        ));
    }
    let mut levels = Vec::new();
    let mut l = first;
    while l < deepest {
        levels.push(l);
        l += step;
    }
    // the middle (up to `deepest`) must be within reach of the last loop
    if let Some(&last) = levels.last() {
        if deepest - last > 0.9 * r {
            levels.push(deepest - 0.5 * r);
        }
    }
    let mut loops: Vec<(f64, Vec<[f64; 2]>)> = Vec::new();
    for &level in &levels {
        if crate::budget::over() {
            break;
        }
        for lp in field.loops(level) {
            let lp = crate::offset::simplify(&lp, POCKET_LOOP_TOL);
            if lp.len() >= 4 {
                loops.push((level, lp));
            }
        }
    }
    // Ridges between two walls (or a wall and an island) can be less deep than the next
    // level yet out of reach of the last: there, add loops halfway between the levels, kept
    // only where they pass near what is left. Spacing at most 0.7 diameters makes one round
    // enough; up to three are tried.
    for _ in 0..3 {
        let refs: Vec<&[[f64; 2]]> = loops.iter().map(|(_, l)| l.as_slice()).collect();
        let left = field.uncovered(&refs, r);
        if left.is_empty() {
            break;
        }
        let mut have: Vec<f64> = loops.iter().map(|(l, _)| *l).collect();
        have.sort_by(f64::total_cmp);
        have.dedup();
        let mut added = Vec::new();
        for (k, &lo) in have.iter().enumerate() {
            let hi = have.get(k + 1).copied().unwrap_or(deepest);
            if !left.iter().any(|&(_, d)| d > lo && d <= hi) {
                continue;
            }
            let mid = 0.5 * (lo + hi);
            for lp in field.loops(mid) {
                let near = left.iter().any(|&(p, _)| {
                    lp.iter()
                        .any(|q| (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) <= 4.0 * r * r)
                });
                if near {
                    let lp = crate::offset::simplify(&lp, POCKET_LOOP_TOL);
                    if lp.len() >= 4 {
                        added.push((mid, lp));
                    }
                }
            }
        }
        if added.is_empty() {
            break;
        }
        loops.extend(added);
    }
    // innermost first; the walls' laps last
    loops.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(loops.into_iter().map(|(_, l)| l).collect())
}

/// A pocket of any shape: at each depth, the offset loops from the middle out, each entered
/// from above at the plunge feed and left to the safe height.
fn path_pocket(
    b: &mut Builder,
    d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Pocket,
    grow: f64,
) -> Result<(), String> {
    let r = d / 2.0;
    // Offset loops keep their corners sharp, so along a corner's diagonal they are √2 times
    // the spacing apart: more than 0.7 diameters would leave material there.
    let loops = pocket_loops_grown(
        &op.paths,
        r,
        (op.stepover * d).clamp(0.01, MAX_LOOP_SPACING * d),
        grow,
    )?;
    let top = op.top.unwrap_or(stock.max[2]);
    b.comment("--- pocket (outline) ---");
    let mut at = [f64::NAN; 2];
    // loops too short to ramp along (round less than the tool's diameter): plunged
    let mut plunged = 0usize;
    for z in levels(top, top - op.depth, op.step_down) {
        if crate::budget::over() {
            break;
        }
        for lp in &loops {
            // start where the last loop ended, as near as the loop allows
            let n = lp.len() - 1;
            let s = (0..n)
                .min_by(|&i, &j| {
                    let dd = |k: usize| (lp[k][0] - at[0]).powi(2) + (lp[k][1] - at[1]).powi(2);
                    dd(i).total_cmp(&dd(j))
                })
                .filter(|_| at[0].is_finite())
                .unwrap_or(0);
            let start = lp[s];
            let z_start = z + op.step_down.max(0.5);
            // in: down along the loop itself at the ramp's angle (it stays in the pocket),
            // round it again if one lap is not long enough, then a whole lap at depth from
            // where it got there; or straight down at the plunge feed
            let mut from = (s, start);
            let round: f64 = (0..n)
                .map(|i| (lp[i + 1][0] - lp[i][0]).dhypot(lp[i + 1][1] - lp[i][1]))
                .sum();
            if op.ramp > 0.0 && round < d {
                plunged += 1;
                enter(b, safe, start[0], start[1], z_start, z, op.plunge_feed);
            } else if op.ramp > 0.0 {
                from = ramp_along(b, safe, lp, s, z_start, z, op.ramp, op.feed, op.plunge_feed);
            } else {
                enter(b, safe, start[0], start[1], z_start, z, op.plunge_feed);
            }
            let (k0, p0) = from;
            for k in 1..=n {
                let q = lp[(k0 + k) % n];
                b.line3(q[0], q[1], z, UP, op.feed);
            }
            b.line3(p0[0], p0[1], z, UP, op.feed);
            leave(b, safe);
            at = start;
        }
    }
    if plunged > 0 {
        b.warnings.push(format!(
            "pocket: {plunged} loop(s) too short to ramp along (round less than the {d} mm tool): plunged at the plunge feed"
        ));
    }
    Ok(())
}

/// Drill `op`'s holes, pecking. With `canned`, the operation's marker also carries them as a
/// drilling cycle, for a post that writes canned cycles (the moves stay: the simulation and
/// every other check use them).
pub fn drill(b: &mut Builder, stock: &StockBox, safe: f64, op: &Drill, canned: bool) {
    let top = op.top.unwrap_or(stock.max[2]);
    let bottom = top - op.depth;
    if canned && !op.points.is_empty() {
        if let Some(crate::Move::Operation { cycle, .. }) = b.moves.last_mut() {
            *cycle = Some(Box::new(crate::toolpath::DrillCycle {
                points: op.points.clone(),
                bottom,
                r: top + op.retract,
                peck: (op.peck > 1e-9 && op.peck < op.depth - 1e-9).then_some(op.peck),
                feed: op.feed,
                safe,
            }));
        }
    }
    b.comment("--- drilling ---");
    // (the operations before it having cut the material down: the pecks start there)
    let from = material_top(top);
    for &[x, y] in &op.points {
        b.rapid3(None, None, Some(safe));
        b.rapid3(Some(x), Some(y), None);
        b.rapid3(None, None, Some(from + op.retract));
        // peck: feed down a peck, rapid back out, rapid down to just above the last depth
        let mut z = from;
        while z > bottom + 1e-9 {
            let next = (z - op.peck).max(bottom);
            b.line3(x, y, next, UP, op.feed);
            b.rapid3(None, None, Some(from + op.retract));
            if next > bottom + 1e-9 {
                b.rapid3(None, None, Some(next + 0.2));
            }
            z = next;
        }
        b.rapid3(None, None, Some(safe));
    }
}

/// Offset a closed polygon by `dist` (outward for a counter-clockwise polygon when positive),
/// joining edges with miters.
pub fn offset_polygon(path: &[[f64; 2]], dist: f64) -> Vec<[f64; 2]> {
    let n = path.len();
    let area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0;
    let s = if area >= 0.0 { dist } else { -dist }; // outward for either winding
    let lines: Vec<([f64; 2], [f64; 2])> = (0..n)
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = dx.dhypot(dy).max(1e-12);
            let nrm = [dy / l * s, -dx / l * s]; // right-hand normal = outward for CCW
            ([a[0] + nrm[0], a[1] + nrm[1]], [dx, dy])
        })
        .collect();
    (0..n)
        .map(|i| {
            let (p1, d1) = lines[(i + n - 1) % n];
            let (p2, d2) = lines[i];
            let den = d1[0] * d2[1] - d1[1] * d2[0];
            if den.abs() < 1e-12 {
                p2
            } else {
                let t = ((p2[0] - p1[0]) * d2[1] - (p2[1] - p1[1]) * d2[0]) / den;
                [p1[0] + t * d1[0], p1[1] + t * d1[1]]
            }
        })
        .collect()
}

pub fn contour(
    b: &mut Builder,
    d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Contour,
) -> Result<(), String> {
    if op.path.len() < 3 {
        return Err("contour needs a closed path of at least 3 points".into());
    }
    let r = d / 2.0;
    // a closed path may repeat its first point at the end (picked loops do): offset without
    // it, or the zero-length edge there sends the tool back onto the outline
    let mut src = op.path.clone();
    while src.len() > 3 && {
        let (a, b) = (src[0], src[src.len() - 1]);
        (a[0] - b[0]).dhypot(a[1] - b[1]) < 1e-9
    } {
        src.pop();
    }
    // (the outline as written: where tabs placed by hand are measured along)
    let src0 = src.clone();
    // (and the boundary offset: out from the outline when positive, whichever side)
    let lv = leave_xy();
    let g = op.boundary_offset
        + match op.side.as_str() {
            "outside" => lv,
            "inside" => -lv,
            _ => 0.0,
        };
    let mut path = match op.side.as_str() {
        "outside" => offset_polygon(&src, if g == 0.0 { r } else { r + g }),
        "inside" => offset_polygon(&src, if g == 0.0 { -r } else { -r + g }),
        _ if g != 0.0 => offset_polygon(&src, g),
        _ => src,
    };
    // Climb milling, spindle clockwise, keeps the wall it cuts on the tool's right: round an
    // outline (outside, or on it) clockwise, round a hole counter-clockwise (as a bore).
    // Conventional: the other way.
    let area: f64 = (0..path.len())
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    let climb = op.direction.as_deref() != Some("conventional");
    let want_ccw = (op.side == "inside") == climb;
    if (area > 0.0) != want_ccw {
        path.reverse();
    }
    // with leads: start in the middle of the longest edge, so the arcs lie along a straight
    // wall (from a corner, the lead-in's start would be past the next wall)
    if op.lead_in > 0.0 || op.lead_out > 0.0 {
        let n = path.len();
        let k = (0..n)
            .max_by(|&i, &j| {
                let l = |i: usize| {
                    let (a, b) = (path[i], path[(i + 1) % n]);
                    (b[0] - a[0]).dhypot(b[1] - a[1])
                };
                l(i).total_cmp(&l(j))
            })
            .unwrap_or(0);
        let (a, b) = (path[k], path[(k + 1) % n]);
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let mut rotated = vec![mid];
        rotated.extend((1..=n).map(|i| path[(k + i) % n]));
        path = rotated;
    }
    let top = op.top.unwrap_or(stock.max[2]);
    let bottom = top - op.depth;
    // tabs: the last levels go over them, at the tab's top, for the tab's length plus the tool
    let tab_top = bottom + op.tab_height;
    let mut tabs = tab_spans(&path, op.tabs, op.tab_width + d);
    tabs.extend(placed_spans(&src0, &path, &op.tab_at, op.tab_width + d));
    let mut zs = levels(top, bottom, op.step_down);
    if !tabs.is_empty() && tab_top < top && !zs.iter().any(|z| (z - tab_top).abs() < 1e-6) {
        // a level at the tabs' top, so the cut above them is a whole one
        zs.push(tab_top);
        zs.sort_by(|a, b| b.total_cmp(a));
    }
    b.comment(if tabs.is_empty() {
        "--- contour ---"
    } else {
        "--- contour (with tabs) ---"
    });
    // the leads: quarter arcs on the waste side (away from the part: outside an outline,
    // inside a hole), tangent to the contour where it starts and ends
    let ccw = (0..path.len())
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        > 0.0;
    let waste_left = (op.side == "inside") == ccw;
    let unit = |a: [f64; 2], b: [f64; 2]| {
        let l = (b[0] - a[0]).dhypot(b[1] - a[1]).max(1e-12);
        [(b[0] - a[0]) / l, (b[1] - a[1]) / l]
    };
    let side = |t: [f64; 2]| {
        if waste_left {
            [-t[1], t[0]]
        } else {
            [t[1], -t[0]]
        }
    };
    let p0 = path[0];
    let t_in = unit(p0, path[1]);
    let t_out = unit(path[path.len() - 1], p0);
    const ARC: usize = 8;
    let arc_in: Vec<[f64; 2]> = if op.lead_in > 0.0 {
        let (n, r) = (side(t_in), op.lead_in);
        let c = [p0[0] + n[0] * r, p0[1] + n[1] * r];
        (0..=ARC)
            .map(|k| {
                let th = std::f64::consts::FRAC_PI_2 * k as f64 / ARC as f64;
                let (cs, sn) = (th.dcos(), th.dsin());
                [
                    c[0] - r * (cs * t_in[0] + sn * n[0]),
                    c[1] - r * (cs * t_in[1] + sn * n[1]),
                ]
            })
            .collect()
    } else {
        Vec::new()
    };
    let arc_out: Vec<[f64; 2]> = if op.lead_out > 0.0 {
        let (n, r) = (side(t_out), op.lead_out);
        let c = [p0[0] + n[0] * r, p0[1] + n[1] * r];
        (1..=ARC)
            .map(|k| {
                let th = std::f64::consts::FRAC_PI_2 * k as f64 / ARC as f64;
                let (cs, sn) = (th.dcos(), th.dsin());
                [
                    c[0] + r * (-cs * n[0] + sn * t_out[0]),
                    c[1] + r * (-cs * n[1] + sn * t_out[1]),
                ]
            })
            .collect()
    } else {
        Vec::new()
    };
    for z in zs {
        if crate::budget::over() {
            break;
        }
        // down beside the contour (on the waste side) and in along the arc, or onto it
        let [x, y] = arc_in.first().copied().unwrap_or(p0);
        enter(b, safe, x, y, z + op.step_down.max(0.5), z, op.plunge_feed);
        for q in arc_in.iter().skip(1) {
            b.line3(q[0], q[1], z, UP, op.feed);
        }
        if z >= tab_top - 1e-9 || tabs.is_empty() {
            for p in path.iter().skip(1).chain(std::iter::once(&path[0])) {
                b.line3(p[0], p[1], z, UP, op.feed);
            }
        } else {
            contour_with_tabs(b, &path, &tabs, z, tab_top, op);
        }
        // off along the arc (at the tab's top if it ended over one)
        let zo = b.p3.map_or(z, |p| p[2]);
        for q in &arc_out {
            b.line3(q[0], q[1], zo, UP, op.feed);
        }
        leave(b, safe);
    }
    Ok(())
}

/// Where tabs go along a closed path: `n` spans of `len` (arc length from the path's first
/// point), evenly spaced, each centred half a spacing in (none when the path is too short).
fn tab_spans(path: &[[f64; 2]], n: u32, len: f64) -> Vec<(f64, f64)> {
    if n == 0 || path.len() < 2 {
        return Vec::new();
    }
    let total: f64 = (0..path.len())
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            (b[0] - a[0]).dhypot(b[1] - a[1])
        })
        .sum();
    let gap = total / n as f64;
    if len >= gap * 0.8 {
        return Vec::new();
    }
    (0..n)
        .map(|k| {
            let c = gap * (k as f64 + 0.5);
            (c - len / 2.0, c + len / 2.0)
        })
        .collect()
}

/// Arc length along a closed path to its point nearest `q`.
fn arc_of(path: &[[f64; 2]], q: [f64; 2]) -> f64 {
    let n = path.len();
    let (mut best, mut at, mut s) = (f64::INFINITY, 0.0, 0.0);
    for i in 0..n {
        let (a, b) = (path[i], path[(i + 1) % n]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 {
            (((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let p = [a[0] + dx * t, a[1] + dy * t];
        let d = (p[0] - q[0]).dhypot(p[1] - q[1]);
        if d < best {
            best = d;
            at = s + t * l2.sqrt();
        }
        s += l2.sqrt();
    }
    at
}

/// The point a fraction `f` of the way round a closed path from its first point.
fn point_at(path: &[[f64; 2]], f: f64) -> [f64; 2] {
    let n = path.len();
    let len = |i: usize| {
        let (a, b) = (path[i], path[(i + 1) % n]);
        (b[0] - a[0]).dhypot(b[1] - a[1])
    };
    let total: f64 = (0..n).map(len).sum();
    let mut want = f.rem_euclid(1.0) * total;
    for i in 0..n {
        let l = len(i);
        if want <= l || i + 1 == n {
            let (a, b) = (path[i], path[(i + 1) % n]);
            let t = if l > 0.0 { (want / l).min(1.0) } else { 0.0 };
            return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        }
        want -= l;
    }
    path[0]
}

/// Tabs placed by hand (`at`: fractions of the outline `src` as written) as spans of `len`
/// along the tool's `path`: each where the path passes nearest its point on the outline. A
/// span across the path's start is split in two.
fn placed_spans(src: &[[f64; 2]], path: &[[f64; 2]], at: &[f64], len: f64) -> Vec<(f64, f64)> {
    if src.len() < 2 || path.len() < 2 {
        return Vec::new();
    }
    let total: f64 = (0..path.len())
        .map(|i| {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            (b[0] - a[0]).dhypot(b[1] - a[1])
        })
        .sum();
    let mut out = Vec::new();
    for &f in at {
        let c = arc_of(path, point_at(src, f));
        let (a, e) = (c - len / 2.0, c + len / 2.0);
        if a < 0.0 {
            out.push((a + total, total));
            out.push((0.0, e));
        } else if e > total {
            out.push((a, total));
            out.push((0.0, e - total));
        } else {
            out.push((a, e));
        }
    }
    out
}

/// One pass round `path` at `z`, rising to `tab_top` over each tab span (straight up and
/// down at the span's ends).
fn contour_with_tabs(
    b: &mut Builder,
    path: &[[f64; 2]],
    tabs: &[(f64, f64)],
    z: f64,
    tab_top: f64,
    op: &Contour,
) {
    let n = path.len();
    let mut s = 0.0;
    let on_tab = |s: f64| tabs.iter().any(|&(a, e)| s > a + 1e-9 && s < e - 1e-9);
    for i in 0..n {
        let (a, c) = (path[i], path[(i + 1) % n]);
        let l = (c[0] - a[0]).dhypot(c[1] - a[1]);
        if l < 1e-12 {
            continue;
        }
        // the segment's breakpoints: its tab starts and ends
        let mut cuts: Vec<f64> = tabs
            .iter()
            .flat_map(|&(t0, t1)| [t0, t1])
            .filter(|&t| t > s && t < s + l)
            .collect();
        cuts.push(s + l);
        let mut at = s;
        for t in cuts {
            let mid = 0.5 * (at + t);
            let zz = if on_tab(mid) { tab_top } else { z };
            let f = (t - s) / l;
            let p = [a[0] + (c[0] - a[0]) * f, a[1] + (c[1] - a[1]) * f];
            // up or down at a tab's end, where the height changes: straight
            let fa = (at - s) / l;
            let pa = [a[0] + (c[0] - a[0]) * fa, a[1] + (c[1] - a[1]) * fa];
            b.line3(
                pa[0],
                pa[1],
                zz,
                UP,
                if zz > z { op.feed } else { op.plunge_feed },
            );
            b.line3(p[0], p[1], zz, UP, op.feed);
            at = t;
        }
        s += l;
    }
    // back down at the start, closed
    b.line3(path[0][0], path[0][1], z, UP, op.plunge_feed);
}

/// Vertical drop-cutter over a mesh, with a simple XY bin index.
pub struct DropCutter3 {
    cutter: Cutter,
    tris: Vec<[[f64; 3]; 3]>,
    /// XY box of each triangle: [x_min, y_min, x_max, y_max].
    boxes: Vec<[f64; 4]>,
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    bins: Vec<Vec<u32>>,
}

impl DropCutter3 {
    pub fn new(mesh: &Mesh, cutter: Cutter) -> DropCutter3 {
        let r = cutter.radius();
        let (lo, hi) = mesh.bounds().unwrap_or(([0.0; 3], [1.0; 3]));
        // bins of half the radius, each listing the triangles within the radius of it: a
        // query looks at one bin, so small bins mean few candidates
        let cell = (0.5 * r).max(0.25);
        let (x0, y0) = (lo[0] - r, lo[1] - r);
        let nx = (((hi[0] + r - x0) / cell).ceil() as usize).max(1);
        let ny = (((hi[1] + r - y0) / cell).ceil() as usize).max(1);
        let mut bins = vec![Vec::new(); nx * ny];
        for (k, t) in mesh.triangles.iter().enumerate() {
            let (mut a, mut b) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for p in t {
                for c in 0..2 {
                    a[c] = a[c].min(p[c]);
                    b[c] = b[c].max(p[c]);
                }
            }
            let i0 = (((a[0] - r - x0) / cell).floor().max(0.0)) as usize;
            let i1 = ((((b[0] + r - x0) / cell).floor()) as usize).min(nx - 1);
            let j0 = (((a[1] - r - y0) / cell).floor().max(0.0)) as usize;
            let j1 = ((((b[1] + r - y0) / cell).floor()) as usize).min(ny - 1);
            for i in i0..=i1 {
                for j in j0..=j1 {
                    bins[j * nx + i].push(k as u32);
                }
            }
        }
        // highest triangles first: a query stops at the first one that cannot beat its best
        let top = |k: u32| {
            let t = &mesh.triangles[k as usize];
            t[0][2].max(t[1][2]).max(t[2][2])
        };
        for b in &mut bins {
            b.sort_by(|&p, &q| top(q).total_cmp(&top(p)));
        }
        let boxes = mesh
            .triangles
            .iter()
            .map(|t| {
                [
                    t[0][0].min(t[1][0]).min(t[2][0]),
                    t[0][1].min(t[1][1]).min(t[2][1]),
                    t[0][0].max(t[1][0]).max(t[2][0]),
                    t[0][1].max(t[1][1]).max(t[2][1]),
                ]
            })
            .collect();
        DropCutter3 {
            cutter,
            tris: mesh.triangles.clone(),
            boxes,
            x0,
            y0,
            cell,
            nx,
            ny,
            bins,
        }
    }

    /// Lowest tip Z at (x, y) that does not cut into the mesh (never below `floor`).
    /// The part's own top over (x, y) (no tool round it): the highest triangle there, or
    /// `floor`.
    pub fn part_top(&self, x: f64, y: f64, floor: f64) -> f64 {
        let (i, j) = ((x - self.x0) / self.cell, (y - self.y0) / self.cell);
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return floor;
        }
        let mut best = floor;
        for &k in &self.bins[j as usize * self.nx + i as usize] {
            let bb = &self.boxes[k as usize];
            if bb[0] > x || bb[2] < x || bb[1] > y || bb[3] < y {
                continue;
            }
            let t = &self.tris[k as usize];
            if t[0][2].max(t[1][2]).max(t[2][2]) <= best {
                break;
            }
            // (x, y) in the triangle seen from above: its height there
            let (ax, ay) = (t[1][0] - t[0][0], t[1][1] - t[0][1]);
            let (bx, by) = (t[2][0] - t[0][0], t[2][1] - t[0][1]);
            let det = ax * by - ay * bx;
            if det.abs() < 1e-12 {
                continue;
            }
            let (px, py) = (x - t[0][0], y - t[0][1]);
            let u = (px * by - py * bx) / det;
            let v = (ax * py - ay * px) / det;
            if u < -1e-9 || v < -1e-9 || u + v > 1.0 + 1e-9 {
                continue;
            }
            best = best.max(t[0][2] + u * (t[1][2] - t[0][2]) + v * (t[2][2] - t[0][2]));
        }
        best
    }

    pub fn z_at(&self, x: f64, y: f64, floor: f64) -> f64 {
        let (i, j) = ((x - self.x0) / self.cell, (y - self.y0) / self.cell);
        // (past the planning budget: the operation is failing, see crate::budget)
        if crate::budget::over() || i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64
        {
            return floor;
        }
        let mut best = floor;
        let r = self.cutter.radius();
        let bin = &self.bins[j as usize * self.nx + i as usize];
        // the work: the triangles looked at (at most the bin)
        crate::budget::spend(crate::budget::PER_QUERY + bin.len() as u64);
        for &k in bin {
            let bb = &self.boxes[k as usize];
            if bb[0] > x + r || bb[2] < x - r || bb[1] > y + r || bb[3] < y - r {
                continue;
            }
            let t = &self.tris[k as usize];
            let lt = [
                [t[0][0] - x, t[0][1] - y, t[0][2]],
                [t[1][0] - x, t[1][1] - y, t[1][2]],
                [t[2][0] - x, t[2][1] - y, t[2][2]],
            ];
            if lt.iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max) <= best {
                // sorted by top: no later triangle can be higher
                break;
            }
            best = best.max(drop_triangle(&self.cutter, &lt));
        }
        best
    }
}

/// Stepover for a raster: from the scallop target when one is set and the tool has a
/// curved or pointed tip ([`Cutter::scallop_width`]: ball 2 sqrt(h (2R - h)), V-bit
/// 2 h tan(half angle), tapered ball by its ball and then its cone), else the operation's
/// stepover.
pub fn raster_stepover(op: &Raster3d, c: &Cutter) -> f64 {
    c.scallop_width(op.scallop).unwrap_or(op.stepover).max(0.01)
}

/// Clip the line t*u + v*w (u, w unit, perpendicular) to the rectangle; the t range.
fn clip_line(u: [f64; 2], w: [f64; 2], v: f64, x: [f64; 2], y: [f64; 2]) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
    for (k, lim) in [(0, x), (1, y)] {
        let p = v * w[k];
        if u[k].abs() < 1e-12 {
            if p < lim[0].min(lim[1]) - 1e-9 || p > lim[0].max(lim[1]) + 1e-9 {
                return None;
            }
        } else {
            let (a, b) = ((lim[0] - p) / u[k], (lim[1] - p) / u[k]);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
    }
    (t1 >= t0 - 1e-9).then_some((t0, t1.max(t0)))
}

/// Parallel passes over the part mesh at `angle`, zigzag or one-way, in step-down levels from
/// the top of the stock to the part (+ `leave`). Zigzag rows are linked at depth along the
/// surface; one-way passes lift to the safe height and re-enter from above the stock.
pub fn raster3d(
    b: &mut Builder,
    dc: &DropCutter3,
    stock: &StockBox,
    safe: f64,
    op: &Raster3d,
) -> Result<(), String> {
    raster3d_on(b, dc, None, None, None, stock, safe, op, false)
}

/// Material thicker than this (mm) left by the earlier finish is cut by a rest finish.
pub const REST_FINISH_MIN: f64 = 0.02;

/// Where a rest finish cuts: the surface the earlier finish's ball (radius `big`) leaves (the
/// lower envelope of the ball everywhere it rests on the part), where it stands more than
/// [`REST_FINISH_MIN`] above the part, widened by the rest finish's tool radius (`small`).
#[derive(Debug, Clone)]
pub struct RestFinish {
    x0: f64,
    y0: f64,
    h: f64,
    nx: usize,
    ny: usize,
    cut: Vec<bool>,
}

impl RestFinish {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        big_dc: &DropCutter3,
        part: &DropCutter3,
        x_range: [f64; 2],
        y_range: [f64; 2],
        big: f64,
        small: f64,
        floor: f64,
    ) -> RestFinish {
        // (the balls placed on a grid: between them the envelope stands proud of a curved
        // surface by about the square of the spacing; a sixth of the small radius keeps that
        // well under the threshold)
        let h = (small / 6.0).clamp(0.03, 0.25);
        // a margin of the big ball round the area: its envelope there reaches in
        let (x0, x1) = (
            x_range[0].min(x_range[1]) - big,
            x_range[0].max(x_range[1]) + big,
        );
        let (y0, y1) = (
            y_range[0].min(y_range[1]) - big,
            y_range[0].max(y_range[1]) + big,
        );
        let nx = ((x1 - x0) / h).ceil() as usize + 1;
        let ny = ((y1 - y0) / h).ceil() as usize + 1;
        let at = |i: usize, j: usize| (x0 + i as f64 * h, y0 + j as f64 * h);
        // the big ball's centre over each point, and the part's top there
        let mut centre = vec![f64::NEG_INFINITY; nx * ny];
        let mut top = vec![f64::NEG_INFINITY; nx * ny];
        for j in 0..ny {
            for i in 0..nx {
                let (x, y) = at(i, j);
                centre[j * nx + i] = big_dc.z_at(x, y, floor) + big;
                top[j * nx + i] = part.z_at(x, y, floor);
            }
        }
        // what it leaves: at each point the lowest of the balls over it
        let k = (big / h).ceil() as isize;
        let mut left_over = vec![false; nx * ny];
        for j in 0..ny as isize {
            for i in 0..nx as isize {
                let mut low = f64::INFINITY;
                for dj in -k..=k {
                    for di in -k..=k {
                        let (a, c) = (i + di, j + dj);
                        if a < 0 || c < 0 || a >= nx as isize || c >= ny as isize {
                            continue;
                        }
                        let d2 = ((di * di + dj * dj) as f64) * h * h;
                        if d2 > big * big {
                            continue;
                        }
                        low =
                            low.min(centre[c as usize * nx + a as usize] - (big * big - d2).sqrt());
                    }
                }
                let t = top[j as usize * nx + i as usize];
                left_over[j as usize * nx + i as usize] =
                    t.is_finite() && low - t > REST_FINISH_MIN;
            }
        }
        // the small tool cuts there with any part of it: widened by its radius
        let s = (small / h).ceil() as isize;
        let mut cut = vec![false; nx * ny];
        for j in 0..ny as isize {
            for i in 0..nx as isize {
                if !left_over[j as usize * nx + i as usize] {
                    continue;
                }
                for dj in -s..=s {
                    for di in -s..=s {
                        let (a, c) = (i + di, j + dj);
                        if a < 0 || c < 0 || a >= nx as isize || c >= ny as isize {
                            continue;
                        }
                        if ((di * di + dj * dj) as f64) * h * h <= small * small {
                            cut[c as usize * nx + a as usize] = true;
                        }
                    }
                }
            }
        }
        RestFinish {
            x0,
            y0,
            h,
            nx,
            ny,
            cut,
        }
    }

    /// Whether the rest finish cuts with its tool over (x, y).
    pub fn cuts(&self, x: f64, y: f64) -> bool {
        let i = ((x - self.x0) / self.h).round();
        let j = ((y - self.y0) / self.h).round();
        if i < 0.0 || j < 0.0 || i as usize >= self.nx || j as usize >= self.ny {
            return false;
        }
        self.cut[j as usize * self.nx + i as usize]
    }
}

/// Material thinner than this (mm) over what a pass leaves is not cut by rest roughing: the
/// finish takes it.
pub const REST_MIN: f64 = 0.05;
/// How far above the material left a rest roughing comes down to, or passes over it (mm).
pub const REST_CLEAR: f64 = 0.3;

/// What the operations before a rest roughing left of the block, as the highest top under a
/// tool of `radius` at each cell (the block's heightmap after them, widened by the tool): a
/// pass at Z cuts material at (x, y) only where this is above Z.
#[derive(Debug, Clone)]
pub struct RestMap {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    pub top: Vec<f64>,
    /// The lowest bottom of the material under the tool (a block turned over: what an earlier
    /// side cut is gone below it); None: the block's bottom everywhere.
    pub bot: Option<Vec<f64>>,
}

impl RestMap {
    /// [`RestMap::new`], with each cell's bottom (`bottoms`, a block carried over from an
    /// earlier setup: its other side's cuts).
    pub fn with_bottoms(
        hm: &crate::sim3::Heightmap3,
        radius: f64,
        bottoms: Option<&[f64]>,
    ) -> RestMap {
        let mut m = RestMap::new(hm, radius);
        if let Some(b) = bottoms {
            let (nx, ny, cell) = (hm.nx, hm.ny, hm.cell);
            let k = (radius / cell).ceil() as isize;
            let mut low = vec![f64::INFINITY; nx * ny];
            for j in 0..ny as isize {
                for i in 0..nx as isize {
                    let mut v = f64::INFINITY;
                    for dj in -k..=k {
                        for di in -k..=k {
                            let (a, c) = (i + di, j + dj);
                            if a < 0 || c < 0 || a >= nx as isize || c >= ny as isize {
                                continue;
                            }
                            let ci = c as usize * nx + a as usize;
                            // (a cell cut through holds nothing)
                            if hm.z[ci] > b[ci] {
                                v = v.min(b[ci]);
                            }
                        }
                    }
                    low[j as usize * nx + i as usize] = v;
                }
            }
            m.bot = Some(low);
        }
        m
    }

    /// The lowest the material goes under the tool centred at (x, y) (minus infinity: down to
    /// the block's bottom).
    pub fn bottom_at(&self, x: f64, y: f64) -> f64 {
        let Some(b) = &self.bot else {
            return f64::NEG_INFINITY;
        };
        let i = (((x - self.x0) / self.cell).max(0.0) as usize).min(self.nx - 1);
        let j = (((y - self.y0) / self.cell).max(0.0) as usize).min(self.ny - 1);
        b[j * self.nx + i]
    }

    /// From the block left (`hm`) and the rest tool's radius.
    pub fn new(hm: &crate::sim3::Heightmap3, radius: f64) -> RestMap {
        let (nx, ny, cell) = (hm.nx, hm.ny, hm.cell);
        let k = (radius / cell).ceil() as isize;
        // the disc's cells, as offsets (its centre cell included)
        let disc: Vec<(isize, isize)> = (-k..=k)
            .flat_map(|dj| (-k..=k).map(move |di| (di, dj)))
            .filter(|&(di, dj)| {
                let (dx, dy) = (
                    (di.unsigned_abs() as f64 - 0.5).max(0.0) * cell,
                    (dj.unsigned_abs() as f64 - 0.5).max(0.0) * cell,
                );
                dx * dx + dy * dy <= radius * radius
            })
            .collect();
        // a row's maximum over each run of cells first, then the disc's rows (separable
        // per row width): kept simple, the disc's offsets directly
        let mut top = vec![f64::NEG_INFINITY; nx * ny];
        for j in 0..ny as isize {
            for i in 0..nx as isize {
                let mut m = f64::NEG_INFINITY;
                for &(di, dj) in &disc {
                    let (a, c) = (i + di, j + dj);
                    if a < 0 || c < 0 || a >= nx as isize || c >= ny as isize {
                        continue;
                    }
                    m = m.max(hm.z[c as usize * nx + a as usize]);
                }
                top[j as usize * nx + i as usize] = m;
            }
        }
        RestMap {
            x0: hm.x0,
            y0: hm.y0,
            cell,
            nx,
            ny,
            top,
            bot: None,
        }
    }

    /// [`RestMap::at`] at a point.
    pub fn at_point(&self, p: [f64; 2]) -> f64 {
        self.at(p[0], p[1])
    }

    /// The highest material under the tool centred at (x, y) (beside the block: its edge).
    pub fn at(&self, x: f64, y: f64) -> f64 {
        let fi = (x - self.x0) / self.cell;
        let fj = (y - self.y0) / self.cell;
        if fi < 0.0 || fj < 0.0 || fi >= self.nx as f64 || fj >= self.ny as f64 {
            // (beside the block the tool's edge still reaches it: the nearest cell)
            let i = (fi.max(0.0) as usize).min(self.nx - 1);
            let j = (fj.max(0.0) as usize).min(self.ny - 1);
            return self.top[j * self.nx + i];
        }
        self.top[fj as usize * self.nx + fi as usize]
    }
}

/// [`raster3d`] limited to where the tool rests on the faces in `sel` (a drop-cutter on just
/// those faces, following the whole part through `dc`): it lifts to the safe height between
/// runs, and links rows at depth only when the link stays on the faces.
#[allow(clippy::too_many_arguments)]
pub fn raster3d_on(
    b: &mut Builder,
    dc: &DropCutter3,
    sel: Option<&DropCutter3>,
    rest: Option<&RestMap>,
    bigger: Option<&RestFinish>,
    stock: &StockBox,
    safe: f64,
    op: &Raster3d,
    rough: bool,
) -> Result<(), String> {
    let [x0, x1] = op.x_range;
    let [y0, y1] = op.y_range;
    let floor = op.floor;
    let d = 2.0 * dc.cutter.radius();
    let step = raster_stepover(op, &dc.cutter);
    let one_way = match op.pattern.as_str() {
        "zigzag" => false,
        "one_way" => true,
        p => return Err(format!("pattern {p:?}: use \"zigzag\" or \"one_way\"")),
    };
    // one-way climb (M3 spindle): the uncut side (+w, the next passes) on the tool's right,
    // so the passes run along -u
    let backwards = match op.direction.as_str() {
        "climb" => true,
        "conventional" => false,
        p => {
            return Err(format!(
                "direction {p:?}: use \"climb\" or \"conventional\""
            ))
        }
    };
    let (sa, ca) = op.angle.to_radians().dsin_cos();
    let (u, w) = ([ca, sa], [-sa, ca]);
    let corners = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]];
    let ws: Vec<f64> = corners.iter().map(|c| c[0] * w[0] + c[1] * w[1]).collect();
    let (w0, w1) = (
        ws.iter().cloned().fold(f64::INFINITY, f64::min),
        ws.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    // angle 0: the rows run from y0 to y1, as given
    let (w0, w1) = if op.angle == 0.0 { (y0, y1) } else { (w0, w1) };
    let n = (((w1 - w0).abs() / step).ceil() as usize).max(1);
    // the surface under each point, once for all the levels (the same values every time)
    let cache = std::cell::RefCell::new(std::collections::HashMap::<(u64, u64), f64>::new());
    let surf = |x: f64, y: f64| {
        let key = (x.to_bits(), y.to_bits());
        if let Some(&z) = cache.borrow().get(&key) {
            return z;
        }
        let z = dc.z_at(x, y, floor) + op.leave_z;
        // (past the budget nothing more is kept: the operation is failing)
        if !crate::budget::over() {
            cache.borrow_mut().insert(key, z);
        }
        z
    };
    let top = stock.max[2];
    let feeds = [op.feed, op.plunge_feed];
    b.comment("--- 3D raster ---");
    for level in levels(top, floor + op.leave_z, op.step_down.max(d * 0.05)) {
        if crate::budget::over() {
            break;
        }
        // a level that holds the tool up nowhere is the surface pass itself: every deeper
        // level would cut exactly the same path again, so it is the last
        let held = std::cell::Cell::new(false);
        let z_of = |x: f64, y: f64| {
            let s = surf(x, y);
            if level > s {
                held.set(true);
            }
            s.max(level)
        };
        let mut first = true;
        // face selection: at the safe height until the first run on the faces
        let mut lifted = true;
        // rest roughing: the tool left down where the material ended (see masked_row)
        let mut air: Option<[f64; 2]> = None;
        let mut emitted = 0usize;
        for k in 0..=n {
            if crate::budget::over() {
                break;
            }
            let wk = w0 + (w1 - w0) * k as f64 / n as f64;
            let row: Vec<[f64; 2]> = if op.angle == 0.0 {
                let m = (((x1 - x0).abs() / op.sample.max(0.01)).ceil() as usize).max(1);
                (0..=m)
                    .map(|i| [x0 + (x1 - x0) * i as f64 / m as f64, wk])
                    .collect()
            } else {
                let Some((t0, t1)) = clip_line(u, w, wk, [x0, x1], [y0, y1]) else {
                    continue;
                };
                let m = (((t1 - t0) / op.sample.max(0.01)).ceil() as usize).max(1);
                (0..=m)
                    .map(|i| {
                        let t = t0 + (t1 - t0) * i as f64 / m as f64;
                        [t * u[0] + wk * w[0], t * u[1] + wk * w[1]]
                    })
                    .collect()
            };
            let reverse = if one_way { backwards } else { emitted % 2 == 1 };
            emitted += 1;
            let row: Vec<[f64; 2]> = if reverse {
                row.into_iter().rev().collect()
            } else {
                row
            };
            // (roughing, the walkthrough: a square base as wide as the block held the tool on
            // its top at every level below it, the same pass again and again, 4 h 55 min)
            // only where this level goes deeper than the one above it
            let deeper = |p: [f64; 2]| {
                !rough || surf(p[0], p[1]) < level + op.step_down.max(d * 0.05) - 1e-6
            };
            if sel.is_some()
                || rest.is_some()
                || bigger.is_some()
                || (rough && row.iter().any(|&p| !deeper(p)))
            {
                // on the selected faces, (rest roughing) where material is left above the
                // pass, and (rest finishing) where the earlier finish's larger tool could not
                // go as deep
                // (rest roughing keeps its own: what is left above the pass; skipping there too
                // let its links at depth cut 0.3 mm into a part)
                let inside = |p: [f64; 2]| {
                    (rest.is_some() || deeper(p))
                        && sel.is_none_or(|s| on_sel(dc, s, p[0], p[1]))
                        && rest.is_none_or(|r| {
                            r.at(p[0], p[1]) > z_of(p[0], p[1]) + REST_MIN
                                // (and not all of it below this level's layer)
                                && r.bottom_at(p[0], p[1]) < level + op.step_down
                        })
                        && bigger.is_none_or(|g| g.cuts(p[0], p[1]))
                };
                masked_row(
                    b,
                    &inside,
                    &z_of,
                    &row,
                    safe,
                    top,
                    op,
                    &mut lifted,
                    rest,
                    &mut air,
                    false,
                    rough.then_some(level + op.step_down.max(d * 0.05)),
                );
                continue;
            }
            let [sx, sy] = row[0];
            let z0 = z_of(sx, sy);
            // the lead-in: from `lead_in` above the surface, a little behind the pass's start,
            // down along the pass at the ramp angle (never below the surface)
            let lead = |b: &mut Builder| {
                let tan = op.ramp_angle.clamp(1.0, 45.0).to_radians().dtan();
                let len = op.lead_in / tan;
                let u = unit2(row[0], row.get(1).copied().unwrap_or(row[0]));
                let n = ((len / op.sample.max(0.05)).ceil() as usize).clamp(2, 200);
                let pts: Vec<[f64; 3]> = (0..=n)
                    .map(|k| {
                        let back = len * (1.0 - k as f64 / n as f64);
                        let (x, y) = (sx - u[0] * back, sy - u[1] * back);
                        [x, y, z_of(x, y) + op.lead_in * (1.0 - k as f64 / n as f64)]
                    })
                    .collect();
                enter(
                    b,
                    safe,
                    pts[0][0],
                    pts[0][1],
                    top + 1.0,
                    pts[0][2],
                    op.plunge_feed,
                );
                for w in pts.windows(2) {
                    let f = descent_feed(w[0], w[1], op.feed, op.plunge_feed, tan);
                    b.line3(w[1][0], w[1][1], w[1][2].max(z_of(w[1][0], w[1][1])), UP, f);
                }
            };
            if first {
                // one entry per level (per pass one-way), from above the uncut stock, so the
                // tool never drops beside material outside the raster area
                if op.lead_in > 0.0 {
                    lead(b);
                } else {
                    enter(b, safe, sx, sy, top + 1.0, z0, op.plunge_feed);
                }
                first = false;
            } else if one_way {
                leave(b, safe);
                if op.lead_in > 0.0 {
                    lead(b);
                } else {
                    enter(b, safe, sx, sy, top + 1.0, z0, op.plunge_feed);
                }
            } else {
                let p = b.p3.expect("inside a level");
                line_refined(b, &z_of, p, [sx, sy, z0], op.tolerance, 0, feeds);
            }
            for q in row.windows(2) {
                // split where the surface rises above the straight move
                line_refined(
                    b,
                    &z_of,
                    [q[0][0], q[0][1], z_of(q[0][0], q[0][1])],
                    [q[1][0], q[1][1], z_of(q[1][0], q[1][1])],
                    op.tolerance,
                    0,
                    feeds,
                );
            }
        }
        leave(b, safe);
        if !held.get() {
            break;
        }
    }
    Ok(())
}

/// Most grid points a waterline samples the part at.
pub const WATERLINE_MAX_POINTS: usize = 4_000_000;

/// 3D waterline finishing: at each level from the part's top down to `floor` (every
/// `step_down`), the tool goes round where it touches the part at that height: the level's
/// contour on a grid of drop-cutter heights (`sample` apart), followed with the part on the
/// tool's right (climb with a clockwise spindle; conventional: the other way). With
/// `min_slope`, only where the surface is at least that steep (the walls a parallel finish
/// passes down too far apart); shallow stretches are left for it.
pub fn waterline(
    b: &mut Builder,
    dc: &DropCutter3,
    stock: &StockBox,
    safe: f64,
    op: &Raster3d,
) -> Result<(), String> {
    let (x0, x1) = (
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    );
    let (y0, y1) = (
        op.y_range[0].min(op.y_range[1]),
        op.y_range[0].max(op.y_range[1]),
    );
    let h = op.sample.max(0.02);
    let nx = ((x1 - x0) / h).ceil() as usize + 1;
    let ny = ((y1 - y0) / h).ceil() as usize + 1;
    if nx * ny > WATERLINE_MAX_POINTS {
        return Err(format!(
            "waterline: a {h} mm sample over {:.0} x {:.0} mm is {} points (at most {WATERLINE_MAX_POINTS}): sample coarser",
            x1 - x0,
            y1 - y0,
            nx * ny
        ));
    }
    let floor = op.floor + op.leave_z;
    let px = |i: usize| x0 + (x1 - x0) * i as f64 / (nx - 1).max(1) as f64;
    let py = |j: usize| y0 + (y1 - y0) * j as f64 / (ny - 1).max(1) as f64;
    // the tool's height over each grid point (where it rests on the part, plus what is left)
    let mut v = vec![floor; nx * ny];
    for j in 0..ny {
        if crate::budget::over() {
            return Ok(());
        }
        for i in 0..nx {
            v[j * nx + i] = (dc.z_at(px(i), py(j), op.floor) + op.leave_z).max(floor);
        }
    }
    let top = v
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max)
        .min(stock.max[2]);
    let at = |i: usize, j: usize| v[j * nx + i];
    // the surface's steepness at a cell (degrees), from its corners
    let steep = |i: usize, j: usize| {
        let gx = ((at(i + 1, j) - at(i, j)) + (at(i + 1, j + 1) - at(i, j + 1))) / (2.0 * h);
        let gy = ((at(i, j + 1) - at(i, j)) + (at(i + 1, j + 1) - at(i + 1, j))) / (2.0 * h);
        gx.dhypot(gy).datan().to_degrees()
    };
    // a grid edge's id: 2 * point + (0 along X, 1 along Y)
    let edge_point = |e: usize, z: f64| -> [f64; 2] {
        let (k, dir) = (e / 2, e % 2);
        let (i, j) = (k % nx, k / nx);
        let (a, bv, q) = if dir == 0 {
            (at(i, j), at(i + 1, j), [px(i), px(i + 1), py(j), py(j)])
        } else {
            (at(i, j), at(i, j + 1), [px(i), px(i), py(j), py(j + 1)])
        };
        let t = if (bv - a).abs() < 1e-12 {
            0.5
        } else {
            ((z - a) / (bv - a)).clamp(0.0, 1.0)
        };
        [q[0] + (q[1] - q[0]) * t, q[2] + (q[3] - q[2]) * t]
    };
    let climb = op.direction != "conventional";
    let feeds = [op.feed, op.plunge_feed];
    let _ = feeds;
    b.comment("--- 3D waterline ---");
    let mut last: Option<[f64; 2]> = None;
    for z in levels(top, floor, op.step_down.max(0.01)) {
        if crate::budget::over() {
            break;
        }
        // the level's contour: per cell, oriented segments (edge to edge) with the part
        // (above the level) on the right when climbing
        let mut segs: Vec<(usize, usize)> = Vec::new();
        for j in 0..ny - 1 {
            for i in 0..nx - 1 {
                let c = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                let up = c.map(|vv| vv > z);
                if up.iter().all(|&u| u) || up.iter().all(|&u| !u) {
                    continue;
                }
                if op.min_slope > 0.0 && steep(i, j) < op.min_slope {
                    continue;
                }
                let k = j * nx + i;
                // the cell's edges: bottom, right, top, left (between corners 0-1, 1-2, 2-3, 3-0)
                let edges = [2 * k, 2 * (k + 1) + 1, 2 * (k + nx), 2 * k + 1];
                let cross: Vec<usize> = (0..4).filter(|&e| up[e] != up[(e + 1) % 4]).collect();
                let pairs: Vec<(usize, usize)> = if cross.len() == 2 {
                    vec![(cross[0], cross[1])]
                } else {
                    // a saddle: the middle decides which corners join
                    let mid = c.iter().sum::<f64>() / 4.0 > z;
                    if mid == up[0] {
                        vec![(0, 1), (2, 3)]
                    } else {
                        vec![(0, 3), (1, 2)]
                    }
                };
                for (ea, eb) in pairs {
                    // which side of a -> b the part (above) is on
                    let (pa, pb) = (edge_point(edges[ea], z), edge_point(edges[eb], z));
                    let corner = (0..4)
                        .find(|&q| up[q])
                        .map(|q| {
                            [
                                [px(i), py(j)],
                                [px(i + 1), py(j)],
                                [px(i + 1), py(j + 1)],
                                [px(i), py(j + 1)],
                            ][q]
                        })
                        .unwrap();
                    let cross_z = (pb[0] - pa[0]) * (corner[1] - pa[1])
                        - (pb[1] - pa[1]) * (corner[0] - pa[0]);
                    let (sa, sb) = if (cross_z < 0.0) == climb {
                        (edges[ea], edges[eb])
                    } else {
                        (edges[eb], edges[ea])
                    };
                    segs.push((sa, sb));
                }
            }
        }
        // chained into runs: each segment's end is the next one's start
        let mut by_start: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        let mut ends: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (k, &(a, e)) in segs.iter().enumerate() {
            by_start.insert(a, k);
            ends.insert(e);
        }
        let mut used = vec![false; segs.len()];
        let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();
        // open runs first (from a segment nothing leads into), then the closed loops
        let order: Vec<usize> = (0..segs.len())
            .filter(|&k| !ends.contains(&segs[k].0))
            .chain(0..segs.len())
            .collect();
        for k0 in order {
            if used[k0] {
                continue;
            }
            let mut pts = vec![edge_point(segs[k0].0, z)];
            let mut k = k0;
            loop {
                used[k] = true;
                pts.push(edge_point(segs[k].1, z));
                match by_start.get(&segs[k].1) {
                    Some(&n) if !used[n] => k = n,
                    _ => break,
                }
            }
            if pts.len() >= 2 {
                runs.push(pts);
            }
        }
        // nearest first
        while !runs.is_empty() {
            let from = last.unwrap_or(runs[0][0]);
            let k = (0..runs.len())
                .min_by(|&a, &c| {
                    let d = |r: &Vec<[f64; 2]>| (r[0][0] - from[0]).dhypot(r[0][1] - from[1]);
                    d(&runs[a]).total_cmp(&d(&runs[c]))
                })
                .unwrap();
            let run = runs.swap_remove(k);
            let [sx, sy] = run[0];
            enter(
                b,
                safe,
                sx,
                sy,
                z + op.step_down.max(0.5),
                z,
                op.plunge_feed,
            );
            for q in run.iter().skip(1) {
                b.line3(q[0], q[1], z, UP, op.feed);
            }
            leave(b, safe);
            last = run.last().copied();
        }
    }
    Ok(())
}

/// Where the tool rests on the selected faces (see [`super::rotary_finish::on_faces`]).
fn on_sel(dc: &DropCutter3, sel: &DropCutter3, x: f64, y: f64) -> bool {
    super::rotary_finish::on_faces(
        dc.z_at(x, y, f64::NEG_INFINITY),
        sel.z_at(x, y, f64::NEG_INFINITY),
        f64::NEG_INFINITY,
    )
}

/// About how fast the machine rapids (mm/min), for choosing between a link at depth and a
/// lift: a guess on the slow side.
const RAPID_GUESS: f64 = 3000.0;
/// And what each rapid costs besides its length (minutes: about 0.15 s of speeding up and
/// slowing down).
const RAPID_OVERHEAD: f64 = 0.0025;

/// One raster row on selected faces, or (rest roughing) where material is left: cut its runs
/// there, finding their edges to 1 um, lifting between runs; a row's first run is reached at
/// depth from the last one when the straight link stays on the faces. With `rest`, the tool
/// comes down from just above the material left (not the block's top), and crosses a gap at
/// depth when that is quicker than lifting (nothing is left there to cut).
#[allow(clippy::too_many_arguments)]
fn masked_row(
    b: &mut Builder,
    inside: &dyn Fn([f64; 2]) -> bool,
    z_of: &dyn Fn(f64, f64) -> f64,
    row: &[[f64; 2]],
    safe: f64,
    top: f64,
    op: &Raster3d,
    lifted: &mut bool,
    rest: Option<&RestMap>,
    air: &mut Option<[f64; 2]>,
    // (roughing: across a gap at depth or over it, whichever is quicker, as rest roughing)
    stay: bool,
    // (roughing: what the level above left here, so a descent comes in just above it)
    cleared: Option<f64>,
) {
    let feeds = [op.feed, op.plunge_feed];
    let lerp =
        |p: [f64; 2], q: [f64; 2], t: f64| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
    let dist = |p: [f64; 2], q: [f64; 2]| ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
    let at = |p: [f64; 2]| [p[0], p[1], z_of(p[0], p[1])];
    // where a descent starts: above the block, or just above what is left there
    let start = |p: [f64; 2], z: f64| {
        rest.map_or(
            cleared.map_or(top + 1.0, |c| (c + REST_CLEAR).min(top + 1.0)),
            |r| (r.at(p[0], p[1]) + REST_CLEAR).min(top + 1.0),
        )
        .max(z)
    };
    let mut prev: Option<[f64; 2]> = None;
    for &q in row {
        let here = inside(q);
        match (*lifted, here) {
            (false, true) => {
                let from = b.p3.expect("cutting");
                // from the last row's end: at depth only if the link stays on the faces (rest
                // roughing: always, as a full pass would)
                let link_ok = prev.is_some() || rest.is_some() || {
                    let a = [from[0], from[1]];
                    let n = ((dist(a, q) / op.sample.max(0.01)).ceil() as usize).max(1);
                    (1..n).all(|k| inside(lerp(a, q, k as f64 / n as f64)))
                };
                if link_ok {
                    line_refined(b, z_of, from, at(q), op.tolerance, 0, feeds);
                } else {
                    leave(b, safe);
                    let z = z_of(q[0], q[1]);
                    enter(b, safe, q[0], q[1], start(q, z), z, op.plunge_feed);
                }
            }
            (true, false) => {}
            (false, false) => {
                // leaving: cut to the edge, then up (rest roughing: maybe not)
                if let Some(p) = prev {
                    let e = super::rotary_finish::boundary(p, q, true, &lerp, inside, dist(p, q));
                    let from = b.p3.expect("cutting");
                    line_refined(b, z_of, from, at(e), op.tolerance, 0, feeds);
                }
                if rest.is_some() || stay {
                    *air = b.p3.map(|p| [p[0], p[1]]);
                } else {
                    leave(b, safe);
                }
                *lifted = true;
            }
            (true, true) => {
                // onto the faces (or the material): at their edge
                let e = prev.map_or(q, |p| {
                    super::rotary_finish::boundary(p, q, false, &lerp, inside, dist(p, q))
                });
                let z = z_of(e[0], e[1]);
                let from = b.p3;
                match (air.take(), from, rest) {
                    (Some(a), Some(f), Some(r)) => {
                        // across the gap at depth, or over it just above what is left along
                        // the way: whichever is quicker
                        let s = start(e, z);
                        let n = ((dist(a, e) / r.cell).ceil() as usize).max(1);
                        let hop = (0..=n)
                            .map(|k| r.at_point(lerp(a, e, k as f64 / n as f64)))
                            .fold(f[2].max(s), f64::max)
                            + REST_CLEAR;
                        let hop = hop.min(safe);
                        let lift = ((hop - f[2]) + dist(a, e) + (hop - s)) / RAPID_GUESS
                            + 3.0 * RAPID_OVERHEAD
                            + (s - z) / op.plunge_feed.max(1e-6);
                        if dist(a, e) / op.feed.max(1e-6) <= lift {
                            line_refined(b, z_of, f, at(e), op.tolerance, 0, feeds);
                        } else {
                            b.rapid3(None, None, Some(hop));
                            b.rapid3(Some(e[0]), Some(e[1]), None);
                            b.rapid3(None, None, Some(s));
                            b.line3(e[0], e[1], z, UP, op.plunge_feed);
                        }
                    }
                    (Some(a), Some(f), None) if stay => {
                        // (the same, over the part itself: the tool held up on it)
                        let s = start(e, z);
                        let n = ((dist(a, e) / op.sample.max(0.05)).ceil() as usize).max(1);
                        let hop = (0..=n)
                            .map(|k| {
                                let p = lerp(a, e, k as f64 / n as f64);
                                z_of(p[0], p[1])
                            })
                            .fold(f[2].max(s), f64::max)
                            + REST_CLEAR;
                        let hop = hop.min(safe);
                        let lift = ((hop - f[2]) + dist(a, e) + (hop - s)) / RAPID_GUESS
                            + 3.0 * RAPID_OVERHEAD
                            + (s - z) / op.plunge_feed.max(1e-6);
                        if dist(a, e) / op.feed.max(1e-6) <= lift {
                            line_refined(b, z_of, f, at(e), op.tolerance, 0, feeds);
                        } else {
                            b.rapid3(None, None, Some(hop));
                            b.rapid3(Some(e[0]), Some(e[1]), None);
                            b.rapid3(None, None, Some(s));
                            b.line3(e[0], e[1], z, UP, op.plunge_feed);
                        }
                    }
                    _ => enter(b, safe, e[0], e[1], start(e, z), z, op.plunge_feed),
                }
                *lifted = false;
                if dist(e, q) > 1e-9 {
                    line_refined(b, z_of, at(e), at(q), op.tolerance, 0, feeds);
                }
            }
        }
        prev = Some(q);
    }
    // one-way passes always come back from above
    if op.pattern == "one_way" && !*lifted {
        leave(b, safe);
        *lifted = true;
    }
}

fn line_refined(
    b: &mut Builder,
    z_of: &dyn Fn(f64, f64) -> f64,
    p0: [f64; 3],
    p1: [f64; 3],
    tol: f64,
    depth: u32,
    feed: [f64; 2],
) {
    if depth < 12 {
        let m = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
        let zm = z_of(m[0], m[1]);
        if zm > (p0[2] + p1[2]) / 2.0 + tol {
            let mid = [m[0], m[1], zm];
            line_refined(b, z_of, p0, mid, tol, depth + 1, feed);
            line_refined(b, z_of, mid, p1, tol, depth + 1, feed);
            return;
        }
    }
    b.line3(
        p1[0],
        p1[1],
        p1[2],
        UP,
        descent_feed(p0, p1, feed[0], feed[1], b.ramp_tan),
    );
}

/// Feed for a move from `p0` to `p1`: a descent no steeper than the ramp (`ramp_tan` =
/// tan(ramp angle)) runs at `feed`; a steeper one so Z never falls faster than `plunge`
/// mm/min, even after the post rounds coordinates to 0.001 mm (whole mm/min, rounded down).
pub fn descent_feed(p0: [f64; 3], p1: [f64; 3], feed: f64, plunge: f64, ramp_tan: f64) -> f64 {
    const ROUND: f64 = 1e-3;
    let dz = p0[2] - p1[2];
    if dz <= -ROUND {
        return feed;
    }
    let across = (p1[0] - p0[0]).dhypot(p1[1] - p0[1]);
    // the post's rounding may make a move up to 2 microns steeper than planned
    if crate::toolpath::gentle_descent(dz + 2.0 * ROUND, across, ramp_tan) {
        return feed;
    }
    let len = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2) + dz * dz).sqrt();
    feed.min(
        (plunge * (len - ROUND).max(0.0) / (dz + ROUND))
            .floor()
            .max(1.0),
    )
}

/// Grid cells for adaptive clearing: a 24th of the tool (coarser if the area is huge).
fn adaptive_cell(d: f64, w: f64, l: f64) -> f64 {
    let mut h = (d / 24.0).clamp(0.02, 0.5);
    while (w / h) * (l / h) > ADAPTIVE_MAX_CELLS as f64 {
        h *= 1.25;
    }
    h
}

/// Most grid cells an adaptive clearing works on per level.
pub const ADAPTIVE_MAX_CELLS: usize = 1_500_000;

/// The moves of a level's adaptive clearing at height `z` (the material from `z_top` down):
/// rapids over the top between runs, helices at the ramp's angle (or plunges), cuts and
/// links at depth. It ends at the safe height.
#[allow(clippy::too_many_arguments)]
fn emit_adaptive(
    b: &mut Builder,
    cl: &crate::strategy::adaptive::Cleared,
    z: f64,
    entry: &dyn Fn(f64, f64, f64) -> f64,
    safe: f64,
    feed: f64,
    plunge: f64,
    ramp: f64,
    climb: bool,
) {
    use crate::strategy::adaptive::Step;
    // (each way in from just above what is there: the level above, or, where that level
    // never reached, the material still standing under the tool)
    for st in &cl.steps {
        match st {
            Step::Down(q) => enter(
                b,
                safe,
                q[0],
                q[1],
                entry(q[0], q[1], 0.0).max(z + 0.5),
                z,
                plunge,
            ),
            Step::Plunge(q) => enter(
                b,
                safe,
                q[0],
                q[1],
                entry(q[0], q[1], 0.0).max(z + 0.5),
                z,
                plunge,
            ),
            Step::Helix {
                centre,
                radius,
                start,
            } => {
                let side = if climb { 1.0 } else { -1.0 };
                let n = 36;
                let lp: Vec<[f64; 2]> = (0..=n)
                    .map(|i| {
                        let a = start + side * std::f64::consts::TAU * i as f64 / n as f64;
                        [centre[0] + radius * a.dcos(), centre[1] + radius * a.dsin()]
                    })
                    .collect();
                let z_start = entry(centre[0], centre[1], *radius).max(z + 0.5);
                let (k, _) = ramp_along(b, safe, &lp, 0, z_start, z, ramp, feed, plunge);
                // a whole turn at depth, on round to the start
                for i in 1..=(n + (n - k) % n) {
                    let q = lp[(k + i) % n];
                    b.line3(q[0], q[1], z, UP, feed);
                }
            }
            Step::Cut(pts) | Step::Link(pts) => {
                for q in pts.iter().skip(1) {
                    b.line3(q[0], q[1], z, UP, feed);
                }
            }
        }
    }
    leave(b, safe);
}

/// A pocket cleared adaptively: at each depth the tool comes down a helix (or plunges, with
/// no ramp) and cuts at a steady load (the stepover's width) out to the walls, then laps the
/// walls once.
fn adaptive_pocket(
    b: &mut Builder,
    d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Pocket,
) -> Result<(), String> {
    use crate::strategy::adaptive::{clear_level, Grid, Params};
    let r = d / 2.0;
    let paths: Vec<Vec<[f64; 2]>> = if op.paths.is_empty() {
        let [x0, y0, x1, y1] = op.rect;
        if op.rect == [0.0; 4] {
            return Err("a pocket needs a rect or closed paths".into());
        }
        let (x0, x1, y0, y1) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
        vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]]
    } else {
        op.paths.clone()
    };
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for q in paths.iter().flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    let h = adaptive_cell(d, hi[0] - lo[0], hi[1] - lo[1]);
    let field = crate::offset::DistanceField::new(&paths, h, r + 4.0 * h);
    if field.max() <= r + 1e-6 {
        return Err(format!(
            "the pocket is nowhere wide enough for the {d} mm tool"
        ));
    }
    let g = Grid {
        x0: field.x0,
        y0: field.y0,
        h: field.h,
        nx: field.nx,
        ny: field.ny,
    };
    let allowed: Vec<bool> = field.d.iter().map(|&v| v >= r).collect();
    let inside: Vec<bool> = field.d.iter().map(|&v| v > 0.0).collect();
    let mut open = vec![true; g.len()];
    // (pockets climb-mill)
    let climb = true;
    let params = Params {
        r,
        ae: (op.stepover * d).clamp(0.02 * d, d),
        climb,
        helix: if op.ramp > 0.0 { 0.75 * r } else { 0.0 },
    };
    // the walls' laps: climb counter-clockwise round the outline, clockwise round islands
    let laps: Vec<Vec<[f64; 2]>> = field
        .loops(r)
        .into_iter()
        .map(|l| crate::offset::simplify(&l, POCKET_LOOP_TOL))
        .filter(|l| l.len() >= 4)
        .collect();
    let top = op.top.unwrap_or(stock.max[2]);
    b.comment("--- pocket (adaptive) ---");
    let mut plunges = 0;
    let mut z_top = top;
    for z in levels(top, top - op.depth, op.step_down) {
        if crate::budget::over() {
            break;
        }
        let cl = clear_level(g, &allowed, &inside, &mut open, params, None);
        plunges += cl.plunges;
        // (a pocket's every level is the same area: in from just above the one before)
        let above = z_top;
        emit_adaptive(
            b,
            &cl,
            z,
            &|_, _, _| above + 0.5,
            safe,
            op.feed,
            op.plunge_feed,
            op.ramp,
            climb,
        );
        for lp in &laps {
            enter(
                b,
                safe,
                lp[0][0],
                lp[0][1],
                z_top.max(z) + 0.5,
                z,
                op.plunge_feed,
            );
            for q in lp.iter().skip(1) {
                b.line3(q[0], q[1], z, UP, op.feed);
            }
            leave(b, safe);
        }
        z_top = z;
    }
    if plunges > 0 {
        b.warnings.push(format!(
            "adaptive pocket: {plunges} plunge(s) straight into the material (no room for a helix, or no ramp)"
        ));
    }
    Ok(())
}

/// 3D roughing cleared adaptively: at each level, the tool cuts at a steady load (the
/// stepover's width) wherever it can stand without cutting into the part (plus what is
/// left for the finish), coming down a helix (or plunging) where nothing is open, linking at
/// depth across what it has cleared. `stock_top` is the material's top at a point (the stock,
/// or what the operations before left), `stock_bottom` its bottom (a block carried over from
/// an earlier setup: cut from its other side). `stock_x`: where along X the stock really is
/// when `stock` is only the box round the part (a bar on the 4th axis, indexed: it goes on
/// past the part's ends, towards the chuck).
#[allow(clippy::too_many_arguments)]
pub fn adaptive_rough(
    b: &mut Builder,
    dc: &DropCutter3,
    stock_top: &dyn Fn(f64, f64) -> f64,
    stock_bottom: &dyn Fn(f64, f64) -> f64,
    stock: &StockBox,
    stock_x: Option<[f64; 2]>,
    safe: f64,
    op: &Raster3d,
) -> Result<(), String> {
    use crate::strategy::adaptive::{clear_level, Grid, Params};
    let r = dc.cutter.radius();
    let d = 2.0 * r;
    let (x0, x1) = (
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    );
    let (y0, y1) = (
        op.y_range[0].min(op.y_range[1]),
        op.y_range[0].max(op.y_range[1]),
    );
    // the tool's reach round the range (and its centre there, beside the stock: in the air,
    // to come in from the side)
    let m = 2.0 * r + d / 12.0;
    let h = adaptive_cell(d, x1 - x0 + 2.0 * m, y1 - y0 + 2.0 * m);
    let g = Grid {
        x0: x0 - m,
        y0: y0 - m,
        h,
        nx: ((x1 - x0 + 2.0 * m) / h).ceil() as usize + 1,
        ny: ((y1 - y0 + 2.0 * m) / h).ceil() as usize + 1,
    };
    let n = g.len();
    // the tool's lowest over each point (inside the range), and the material's top
    let mut surf = vec![f64::INFINITY; n];
    let mut top_at = vec![f64::NEG_INFINITY; n];
    let mut bot_at = vec![f64::NEG_INFINITY; n];
    let mut part_at = vec![f64::NEG_INFINITY; n];
    for c in 0..n {
        if c % g.nx == 0 && crate::budget::over() {
            return Ok(());
        }
        let p = g.point(c);
        let in_range =
            p[0] >= x0 - 1e-9 && p[0] <= x1 + 1e-9 && p[1] >= y0 - 1e-9 && p[1] <= y1 + 1e-9;
        // (the overhang fixture's 3+1 roughing: the bar past the part's end, towards the chuck,
        // taken for air: the tool came in from the side there and rapids went into it)
        let [sx0, sx1] = stock_x.unwrap_or([stock.min[0], stock.max[0]]);
        let inside_block = p[0] >= sx0.min(stock.min[0])
            && p[0] <= sx1.max(stock.max[0])
            && p[1] >= stock.min[1]
            && p[1] <= stock.max[1];
        // beside the range only off the stock (within the tool's radius of the range)
        let near_range = (p[0] - p[0].clamp(x0, x1)).dhypot(p[1] - p[1].clamp(y0, y1)) <= r;
        // (the impeller past its nose: the tool kept inside the range reached the stock there
        // only late, beside the blades, and slotted it 6-9 mm deep) within a tool's radius of
        // the range the tool may stand over the stock too, where it misses the part: that
        // stock is cleared a level at a time
        if in_range || near_range {
            surf[c] = dc.z_at(p[0], p[1], op.floor) + op.leave_z;
        }
        if inside_block {
            top_at[c] = stock_top(p[0], p[1]);
            bot_at[c] = stock_bottom(p[0], p[1]);
            // (the walkthrough: a square base as wide as the block was roughed round at every
            // level, touching nothing, 4 h 55 min) the part's own top: no material to cut
            // where the part stands above the level
            part_at[c] = dc.part_top(p[0], p[1], f64::NEG_INFINITY);
        }
    }
    let top = top_at
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max)
        .min(stock.max[2]);
    let floor = op.floor + op.leave_z;
    if top <= floor + 1e-6 {
        return Ok(());
    }
    let climb = op.direction != "conventional";
    let params = Params {
        r,
        ae: raster_stepover(op, &dc.cutter).clamp(0.02 * d, d),
        climb,
        helix: if op.ramp_angle > 0.0 { 0.75 * r } else { 0.0 },
    };
    let mut open = vec![true; n];
    b.comment("--- 3D roughing (adaptive) ---");
    let mut plunges = 0;
    // (the overhang fixture's 3+1 roughing: 26 rapids down to "just above the level before"
    // where that level had not reached, into the stock) the material's top in each cell as
    // the levels cut it: a way in comes down by rapid only to just above what stands under
    // the tool there
    let mut mat_top: Vec<f64> = top_at.clone();
    for z in levels(top, floor, op.step_down.max(d * 0.05)) {
        if crate::budget::over() {
            break;
        }
        let allowed: Vec<bool> = surf.iter().map(|&s| s <= z + 1e-9).collect();
        // (material in this level's layer: above it and not all of it below)
        let layer = op.step_down.max(d * 0.05);
        let stock_here: Vec<bool> = top_at
            .iter()
            .zip(&bot_at)
            .zip(&part_at)
            .map(|((&t, &u), &pt)| t > z + 1e-6 && u < z + layer && pt < z + 1e-6)
            .collect();
        let cl = clear_level(g, &allowed, &stock_here, &mut open, params, None);
        plunges += cl.plunges;
        let standing = |x: f64, y: f64, extra: f64| -> f64 {
            // the cells under the tool (and a helix's circle), a cell more for the grid
            let rr = r + extra + g.h;
            let (i0, i1) = (
                ((x - rr - g.x0) / g.h).floor().max(0.0) as usize,
                (((x + rr - g.x0) / g.h).ceil().max(0.0) as usize).min(g.nx - 1),
            );
            let (j0, j1) = (
                ((y - rr - g.y0) / g.h).floor().max(0.0) as usize,
                (((y + rr - g.y0) / g.h).ceil().max(0.0) as usize).min(g.ny - 1),
            );
            let mut hi = f64::NEG_INFINITY;
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let c = j * g.nx + i;
                    let p = g.point(c);
                    if (p[0] - x).dhypot(p[1] - y) <= rr {
                        hi = hi.max(mat_top[c]);
                    }
                }
            }
            hi + 0.5
        };
        emit_adaptive(
            b,
            &cl,
            z,
            &standing,
            safe,
            op.feed,
            op.plunge_feed,
            op.ramp_angle,
            climb,
        );
        for (c, &cut) in cl.cut.iter().enumerate() {
            if cut {
                mat_top[c] = mat_top[c].min(z);
            }
        }
    }
    if plunges > 0 {
        b.warnings.push(format!(
            "adaptive roughing: {plunges} plunge(s) straight into the material (no room for a helix, or no ramp angle)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    #[test]
    fn levels_end_at_the_bottom() {
        assert_eq!(levels(0.0, -1.0, 0.4), vec![-1.0 / 3.0, -2.0 / 3.0, -1.0]);
        assert_eq!(levels(0.0, 0.0, 0.4), vec![0.0]);
    }

    #[test]
    fn offsets_are_outward_for_both_windings() {
        let sq = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let out = offset_polygon(&sq, 1.0);
        assert_eq!(out[0], [-1.0, -1.0]);
        assert_eq!(out[2], [11.0, 11.0]);
        let mut cw = sq;
        cw.reverse();
        let out = offset_polygon(&cw, 1.0);
        assert!(out.contains(&[-1.0, -1.0]) && out.contains(&[11.0, 11.0]));
        let inn = offset_polygon(&sq, -1.0);
        assert_eq!(inn[0], [1.0, 1.0]);
    }
}
