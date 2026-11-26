//! Toolpath smoothing: fewer, smoother moves within a tolerance.
//!
//! - Point reduction (Douglas-Peucker): of a run of feed moves at one feed, a point is
//!   dropped while the straight move that replaces it stays within the tolerance of every
//!   point dropped. 3-axis moves are measured in X, Y, Z; rotary moves in X, radius and arc
//!   length (A at the mean radius), the way the controller interpolates them (linearly in
//!   X, radius and A), so a helix at one radius becomes one move.
//! - Corner blending (3-axis, a run at one Z): a sharp corner is replaced by a few points on
//!   an arc tangent to both moves, within the tolerance of the corner, so the machine need
//!   not stop there; the post then writes such arcs as G2/G3.
//!
//! A tolerance of 0 leaves the moves as they are.

use crate::det::Det;
use crate::toolpath::Move;

/// Largest turn of A in one smoothed rotary move (degrees).
pub const MAX_A: f64 = 45.0;

/// Smooth the feed moves of `moves` (one operation's) within `tol` mm; `blend`: also round
/// the corners of 3-axis runs at one Z.
pub fn smooth(moves: &mut Vec<Move>, tol: f64, blend: bool) {
    smooth_from(moves, tol, blend, (None, None, 0.0))
}

/// [`smooth`] for moves that start with the tool at rotary `(x, r, a)` (an operation after
/// others: its first move from where they left the machine, not from A 0; Mar's impeller-6
/// parting, after a flowline that left A at 20862 degrees, was split into 58 turns from 0).
pub fn smooth_from(
    moves: &mut Vec<Move>,
    tol: f64,
    blend: bool,
    start: (Option<f64>, Option<f64>, f64),
) {
    if tol <= 0.0 || moves.is_empty() {
        return;
    }
    let mut out: Vec<Move> = Vec::with_capacity(moves.len());
    let mut i = 0;
    // the position before each move (3-axis tip; rotary x, r, a)
    let mut p3: Option<[f64; 3]> = None;
    let mut rot: (Option<f64>, Option<f64>, f64) = start;
    while i < moves.len() {
        match &moves[i] {
            Move::Line3 { feed, axis, .. } if p3.is_some() => {
                let (f0, ax0) = (*feed, *axis);
                let mut pts = vec![p3.unwrap()];
                let mut j = i;
                while let Some(Move::Line3 {
                    x,
                    y,
                    z,
                    feed,
                    axis,
                }) = moves.get(j)
                {
                    if *feed != f0 || *axis != ax0 {
                        break;
                    }
                    pts.push([*x, *y, *z]);
                    j += 1;
                }
                let mut keep = reduce(&pts, tol, 2, |p| *p);
                if blend {
                    keep = blend_corners(&keep, tol);
                }
                for q in &keep[1..] {
                    out.push(Move::Line3 {
                        x: q[0],
                        y: q[1],
                        z: q[2],
                        axis: ax0,
                        feed: f0,
                    });
                }
                p3 = Some(*pts.last().unwrap());
                i = j;
            }
            Move::Cut {
                feed, plunge_cap, ..
            } if rot.0.is_some() && rot.1.is_some() => {
                let (f0, cap0) = (*feed, *plunge_cap);
                let mut pts = vec![[rot.0.unwrap(), rot.1.unwrap(), rot.2]];
                let mut j = i;
                while let Some(Move::Cut {
                    x,
                    r,
                    a,
                    feed,
                    plunge_cap,
                }) = moves.get(j)
                {
                    if *feed != f0 || *plunge_cap != cap0 {
                        break;
                    }
                    pts.push([*x, *r, *a]);
                    j += 1;
                }
                // measured in X, radius and arc length at the run's mean radius
                let rm = (pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64).max(0.5);
                let keep = reduce(&pts, tol, 1, |p| [p[0], p[1], p[2].to_radians() * rm]);
                // at most MAX_A per move (split evenly: the same helix), so checks and
                // controllers see the turn in steps
                for w in keep.windows(2) {
                    let n = ((w[1][2] - w[0][2]).abs() / MAX_A).ceil().max(1.0) as usize;
                    for k in 1..=n {
                        let t = k as f64 / n as f64;
                        let q = [0, 1, 2].map(|c| w[0][c] + (w[1][c] - w[0][c]) * t);
                        out.push(Move::Cut {
                            x: q[0],
                            r: q[1],
                            a: q[2],
                            feed: f0,
                            plunge_cap: cap0,
                        });
                    }
                }
                let last = *pts.last().unwrap();
                rot = (Some(last[0]), Some(last[1]), last[2]);
                i = j;
            }
            m => {
                track(m, &mut p3, &mut rot);
                out.push(m.clone());
                i += 1;
            }
        }
    }
    *moves = out;
}

/// Follow the position through moves that are not smoothed.
fn track(m: &Move, p3: &mut Option<[f64; 3]>, rot: &mut (Option<f64>, Option<f64>, f64)) {
    match m {
        Move::Rapid3 { x, y, z } => {
            let p = p3.get_or_insert([0.0; 3]);
            if let Some(v) = x {
                p[0] = *v;
            }
            if let Some(v) = y {
                p[1] = *v;
            }
            if let Some(v) = z {
                p[2] = *v;
            }
        }
        Move::Line3 { x, y, z, .. } => *p3 = Some([*x, *y, *z]),
        Move::Rapid { x, r, a } => {
            if let Some(v) = x {
                rot.0 = Some(*v);
            }
            if let Some(v) = r {
                rot.1 = Some(*v);
            }
            if let Some(v) = a {
                rot.2 = *v;
            }
        }
        Move::Plunge { r, .. } => rot.1 = Some(*r),
        Move::Cut { x, r, a, .. } => *rot = (Some(*x), Some(*r), *a),
        _ => {}
    }
}

/// Douglas-Peucker on `pts` with distances measured on `metric(p)`; keeps the first and
/// last points. Coordinate `up` is along the tool (Z, or the radius): a point is dropped
/// only if it is within `tol` of the new move both square to it and along the tool, at its
/// place across (on a steep wall a point can be close to the move yet far above or below
/// it, which would gouge).
fn reduce(
    pts: &[[f64; 3]],
    tol: f64,
    up: usize,
    metric: impl Fn(&[f64; 3]) -> [f64; 3],
) -> Vec<[f64; 3]> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let m: Vec<[f64; 3]> = pts.iter().map(&metric).collect();
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (mut worst, mut at) = (0.0f64, a);
        for k in a + 1..b {
            let d = seg_dist(m[k], m[a], m[b]).max(along_tool(m[k], m[a], m[b], up, tol));
            if d > worst {
                worst = d;
                at = k;
            }
        }
        if worst > tol {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    pts.iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// How far `p` is from the segment `a`-`b` along coordinate `up`, at `p`'s place across
/// (the other two coordinates); a segment along `up` itself: infinite unless `p` is on it.
fn along_tool(p: [f64; 3], a: [f64; 3], b: [f64; 3], up: usize, tol: f64) -> f64 {
    let h: Vec<usize> = (0..3).filter(|&k| k != up).collect();
    let (dx, dy) = (b[h[0]] - a[h[0]], b[h[1]] - a[h[1]]);
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-18 {
        let off = (p[h[0]] - a[h[0]]).dhypot(p[h[1]] - a[h[1]]);
        return if off <= tol { 0.0 } else { f64::INFINITY };
    }
    let t = (((p[h[0]] - a[h[0]]) * dx + (p[h[1]] - a[h[1]]) * dy) / l2).clamp(0.0, 1.0);
    (p[up] - (a[up] + t * (b[up] - a[up]))).abs()
}

/// Distance from `p` to the segment `a`-`b`.
fn seg_dist(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let l2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if l2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let d = [ap[0] - t * ab[0], ap[1] - t * ab[1], ap[2] - t * ab[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Round the corners of a run at one Z: each corner turning more than 10 degrees becomes
/// points on an arc tangent to both moves, within `tol` of the corner, using at most a third
/// of either move; corners whose blend would be under 10 x `tol` in radius stay sharp.
fn blend_corners(pts: &[[f64; 3]], tol: f64) -> Vec<[f64; 3]> {
    if pts.len() < 3 || pts.iter().any(|p| (p[2] - pts[0][2]).abs() > 1e-9) {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    for k in 1..pts.len() - 1 {
        let (a, c, b) = (pts[k - 1], pts[k], pts[k + 1]);
        let u = [a[0] - c[0], a[1] - c[1]];
        let v = [b[0] - c[0], b[1] - c[1]];
        let (lu, lv) = (u[0].dhypot(u[1]), v[0].dhypot(v[1]));
        if lu < 1e-9 || lv < 1e-9 {
            out.push(c);
            continue;
        }
        let (u, v) = ([u[0] / lu, u[1] / lu], [v[0] / lv, v[1] / lv]);
        // the angle between the two moves at the corner (180: straight on)
        let theta = (u[0] * v[0] + u[1] * v[1]).clamp(-1.0, 1.0).dacos();
        let turn = std::f64::consts::PI - theta;
        if turn < 10f64.to_radians() || theta < 1e-6 {
            out.push(c);
            continue;
        }
        // an arc of radius rr tangent to both moves sits (rr / sin(theta/2) - rr) inside the
        // corner; its tangent points are rr / tan(theta/2) from it
        let half = theta / 2.0;
        let rr_tol = tol / (1.0 / half.dsin() - 1.0);
        let rr_len = (lu.min(lv) / 3.0) * half.dtan();
        let rr = rr_tol.min(rr_len);
        let t = rr / half.dtan();
        // a blend too small to matter (a sharp corner at a fine tolerance) is not worth it
        if rr < 10.0 * tol {
            out.push(c);
            continue;
        }
        let p0 = [c[0] + u[0] * t, c[1] + u[1] * t];
        let p1 = [c[0] + v[0] * t, c[1] + v[1] * t];
        // the arc's centre on the bisector
        let bis = [u[0] + v[0], u[1] + v[1]];
        let lb = bis[0].dhypot(bis[1]);
        let d = rr / half.dsin();
        let o = [c[0] + bis[0] / lb * d, c[1] + bis[1] / lb * d];
        let (s0, s1) = (
            (p0[1] - o[1]).datan2(p0[0] - o[0]),
            (p1[1] - o[1]).datan2(p1[0] - o[0]),
        );
        let mut sweep = s1 - s0;
        if sweep > std::f64::consts::PI {
            sweep -= std::f64::consts::TAU;
        } else if sweep < -std::f64::consts::PI {
            sweep += std::f64::consts::TAU;
        }
        let n = ((sweep.abs() / 10f64.to_radians()).ceil() as usize).max(2);
        for i in 0..=n {
            let s = s0 + sweep * i as f64 / n as f64;
            out.push([o[0] + rr * s.dcos(), o[1] + rr * s.dsin(), c[2]]);
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    fn line(x: f64, y: f64, z: f64) -> Move {
        Move::Line3 {
            x,
            y,
            z,
            axis: [0.0, 0.0, 1.0],
            feed: 300.0,
        }
    }

    #[test]
    fn collinear_points_go_and_bends_stay() {
        let mut m = vec![Move::Rapid3 {
            x: Some(0.0),
            y: Some(0.0),
            z: Some(0.0),
        }];
        for k in 1..=10 {
            m.push(line(k as f64, 0.0, 0.0));
        }
        m.push(line(10.0, 5.0, 0.0));
        smooth(&mut m, 0.01, false);
        assert_eq!(m.len(), 3, "{m:?}");
        // off: unchanged
        let mut n = vec![
            Move::Rapid3 {
                x: Some(0.0),
                y: Some(0.0),
                z: Some(0.0),
            },
            line(1.0, 0.0, 0.0),
            line(2.0, 0.0, 0.0),
        ];
        smooth(&mut n, 0.0, true);
        assert_eq!(n.len(), 3);
    }

    #[test]
    fn a_helix_at_one_radius_needs_no_points_between() {
        let mut m = vec![Move::Rapid {
            x: Some(0.0),
            r: Some(10.0),
            a: Some(0.0),
        }];
        for k in 1..=60 {
            m.push(Move::Cut {
                x: -k as f64 * 0.05,
                r: 10.0,
                a: k as f64 * 6.0,
                feed: 300.0,
                plunge_cap: 60.0,
            });
        }
        smooth(&mut m, 0.005, false);
        // one turn, in steps of at most 45 degrees, on the same helix
        assert_eq!(m.len(), 1 + 8, "{m:?}");
        assert!(matches!(m[8], Move::Cut { a, .. } if (a - 360.0).abs() < 1e-9));
        assert!(
            matches!(m[1], Move::Cut { x, a, .. } if (a - 45.0).abs() < 1e-9 && (x + 0.375).abs() < 1e-9)
        );
    }

    #[test]
    fn a_corner_becomes_an_arc_within_the_tolerance() {
        // a 30 degree bend: an arc of radius 0.28 for 0.01 mm
        let t = 30f64.to_radians();
        let pts = vec![
            [0.0, 0.0, -1.0],
            [10.0, 0.0, -1.0],
            [10.0 + 10.0 * t.dcos(), 10.0 * t.dsin(), -1.0],
        ];
        let out = blend_corners(&pts, 0.01);
        assert!(out.len() > 3);
        // every point within 0.01 of the corner's two moves
        for p in &out {
            let d = seg_dist(*p, pts[0], pts[1]).min(seg_dist(*p, pts[1], pts[2]));
            assert!(d <= 0.01 + 1e-9, "{p:?} {d}");
        }
        // a square corner at that tolerance would be 0.024 in radius: left sharp
        let sq = vec![[0.0, 0.0, -1.0], [10.0, 0.0, -1.0], [10.0, 10.0, -1.0]];
        assert_eq!(blend_corners(&sq, 0.01), sq);
    }

    /// Smoothed from where the operations before left the machine: a move from A 20862 to
    /// 20868 stays a sixth of a turn, not 58 turns from A 0 (Mar's impeller-6 parting, after
    /// a flowline: 18 minutes of the tool turning in the air).
    #[test]
    fn smoothed_from_where_the_machine_is() {
        let mut moves = vec![
            Move::Cut {
                x: -23.6,
                r: 24.4,
                a: 20868.0,
                feed: 500.0,
                plunge_cap: 500.0,
            },
            Move::Cut {
                x: -23.6,
                r: 24.3,
                a: 20874.0,
                feed: 500.0,
                plunge_cap: 500.0,
            },
        ];
        smooth_from(&mut moves, 0.005, false, (Some(-23.6), Some(24.5), 20862.0));
        assert!(
            moves.len() <= 2,
            "{} moves: {:?}",
            moves.len(),
            &moves[..moves.len().min(3)]
        );
        assert!(moves
            .iter()
            .all(|m| matches!(m, Move::Cut { a, .. } if *a > 20862.0)));
    }
}
