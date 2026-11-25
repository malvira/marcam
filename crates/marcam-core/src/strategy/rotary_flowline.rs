//! Rotary flowline (morph): passes that follow the channels between the part's walls, A and X
//! together. At each X, a channel is a stretch round the axis where the part (the tool's
//! drop-cutter surface) stays within `wall_height` of its lowest at that X; channels are
//! followed from X to X by their overlap. A pass keeps a fixed fraction of the way across
//! its channel (morphing from one wall's line to the other's), its tip on the surface; the
//! passes a stepover apart across the channel's floor, one after the other without leaving
//! the channel (Mar's impeller: the blade channels finished with far fewer retracts than
//! rasters along X).

use super::rotary_finish::refine;
use crate::dropcutter::DropCutter;
use crate::job::{FlowSpan, RotaryFlowline};
use crate::toolpath::{Builder, Move};

/// Angular resolution of the channel search (degrees).
const SEARCH_DEG: f64 = 0.5;
/// The most a channel's middle moves round from one X sample to the next (degrees).
const MAX_SHIFT: f64 = 15.0;

/// One channel: at each X sample it is in, its angular extent `[lo, hi]` (degrees,
/// unwrapped so that hi > lo and successive samples join up).
#[derive(Debug, Clone)]
pub struct Channel {
    pub xs: Vec<f64>,
    pub lo: Vec<f64>,
    pub hi: Vec<f64>,
}

/// The stretches round the axis (degrees) where `row` (the surface at angles `SEARCH_DEG`
/// apart) is within `wall` of its lowest: circular, as `[start, end]` with end > start.
fn low_runs(row: &[f64], wall: f64) -> Vec<[f64; 2]> {
    let n = row.len();
    let floor = row.iter().copied().fold(f64::INFINITY, f64::min);
    let low: Vec<bool> = row.iter().map(|&r| r <= floor + wall).collect();
    if low.iter().all(|&l| l) {
        return vec![[0.0, 360.0]];
    }
    // start from a high sample so no run wraps over the start
    let s = low.iter().position(|&l| !l).expect("one is high");
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for k in 1..=n {
        let i = (s + k) % n;
        match (low[i], start) {
            (true, None) => start = Some(s + k),
            (false, Some(b)) => {
                // (each end where the surface crosses the wall's foot, between the samples:
                // a pass following a twisting channel turns smoothly)
                let thr = floor + wall;
                let cross = |lo_i: usize, hi_i: usize| {
                    let (p, q) = (row[lo_i % n], row[hi_i % n]);
                    if (q - p).abs() < 1e-12 {
                        0.5
                    } else {
                        ((thr - p) / (q - p)).clamp(0.0, 1.0)
                    }
                };
                let e = s + k - 1;
                let start_at = (b - 1) as f64 + cross(b - 1, b);
                let end_at = e as f64 + cross(e, e + 1);
                runs.push([start_at * SEARCH_DEG, end_at * SEARCH_DEG]);
                start = None;
            }
            _ => {}
        }
    }
    runs
}

/// Each low stretch widened to the middle of the walls on either side of it: from one
/// wall's top to the next's (circular; `runs` in order round the axis).
fn sectors(runs: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let k = runs.len();
    (0..k)
        .map(|i| {
            let prev_end = if i == 0 {
                runs[k - 1][1] - 360.0
            } else {
                runs[i - 1][1]
            };
            let next_start = if i + 1 == k {
                runs[0][0] + 360.0
            } else {
                runs[i + 1][0]
            };
            [
                0.5 * (prev_end + runs[i][0]),
                0.5 * (runs[i][1] + next_start),
            ]
        })
        .collect()
}

/// The channels over `xs`: each X's low stretches (with `sector`, each widened to the walls'
/// tops either side), joined to the last X's by overlap.
pub fn channels(cl: &dyn Fn(f64, f64) -> f64, xs: &[f64], wall: f64, sector: bool) -> Vec<Channel> {
    let n = (360.0 / SEARCH_DEG).round() as usize;
    let mut done: Vec<Channel> = Vec::new();
    let mut open: Vec<Channel> = Vec::new();
    for &x in xs {
        let row: Vec<f64> = (0..n).map(|k| cl(x, k as f64 * SEARCH_DEG)).collect();
        let mut runs = low_runs(&row, wall);
        if sector && !(runs.len() == 1 && runs[0][1] - runs[0][0] >= 360.0 - 1e-9) {
            runs.sort_by(|p, q| p[0].total_cmp(&q[0]));
            runs = sectors(&runs);
        }
        // (no walls at this X, low all round: no channel; the impeller's hub in front of its
        // blades)
        if runs.len() == 1 && runs[0][1] - runs[0][0] >= 360.0 - 1e-9 {
            done.append(&mut open);
            continue;
        }
        let mut next: Vec<Channel> = Vec::new();
        let mut taken = vec![false; open.len()];
        for run in runs {
            // the open channel it overlaps (its angle shifted by whole turns to meet it)
            let found = open.iter().enumerate().find_map(|(k, c)| {
                if taken[k] {
                    return None;
                }
                let (plo, phi) = (*c.lo.last()?, *c.hi.last()?);
                let mid = 0.5 * (plo + phi);
                let shift = ((mid - 0.5 * (run[0] + run[1])) / 360.0).round() * 360.0;
                let (lo, hi) = (run[0] + shift, run[1] + shift);
                // (the same channel: overlapping, moved round and grown or shrunk only a
                // little; else a wall ends or begins here and the pass would jump across it)
                let moved = (0.5 * (lo + hi) - mid).abs();
                let ratio = (hi - lo) / (phi - plo).max(1e-9);
                (lo <= phi && hi >= plo && moved <= MAX_SHIFT && (0.5..=2.0).contains(&ratio))
                    .then_some((k, lo, hi))
            });
            match found {
                Some((k, lo, hi)) => {
                    taken[k] = true;
                    let mut c = open[k].clone();
                    c.xs.push(x);
                    c.lo.push(lo);
                    c.hi.push(hi);
                    next.push(c);
                }
                None => next.push(Channel {
                    xs: vec![x],
                    lo: vec![run[0]],
                    hi: vec![run[1]],
                }),
            }
        }
        for (k, c) in open.into_iter().enumerate() {
            if !taken[k] {
                done.push(c);
            }
        }
        open = next;
    }
    done.extend(open);
    done.retain(|c| c.xs.len() >= 2);
    done
}

/// Plan the flowline passes. Returns the lowest tip radius used.
pub fn rotary_flowline(
    b: &mut Builder,
    dc: &DropCutter,
    safe_r: f64,
    op: &RotaryFlowline,
) -> Result<f64, String> {
    if op.stepover_mm <= 0.0 || op.sample_mm <= 0.0 {
        return Err("stepover_mm and sample_mm must be > 0".into());
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
    let sector = op.span == FlowSpan::Sector;
    let chans = channels(&cl, &xs, op.wall_height.max(0.0), sector);
    if chans.is_empty() {
        b.warnings.push(
            "rotary flowline: no channel between walls in its X range, nothing to follow".into(),
        );
        return Ok(f64::INFINITY);
    }
    let first = b.moves.len();
    let tol = op.tolerance.max(1e-4);
    let feeds = (op.feed, op.plunge_feed);
    let go = |b: &mut Builder, x: f64, a: f64| {
        let p0 = [b.x.unwrap_or(x), b.r.unwrap_or(cl(x, a)), b.a];
        refine(b, &cl, p0, [x, cl(x, a), a], tol, 0, feeds);
    };
    for c in &chans {
        if crate::budget::over() {
            break;
        }
        // passes across the channel: a stepover apart at its widest floor (over the walls'
        // tops, a sector: at its highest)
        let widest =
            c.xs.iter()
                .zip(c.lo.iter().zip(&c.hi))
                .map(|(&x, (&lo, &hi))| {
                    let r = if sector {
                        cl(x, lo).max(cl(x, hi)).max(cl(x, 0.5 * (lo + hi)))
                    } else {
                        cl(x, 0.5 * (lo + hi))
                    }
                    .max(0.5);
                    (hi - lo).to_radians() * r
                })
                .fold(0.0, f64::max);
        let n = ((widest / op.stepover_mm).ceil() as usize).max(1);
        // (a sector's last pass is the next one's first: on the wall's top between them)
        let last = if sector { n - 1 } else { n };
        let vs: Vec<f64> = (0..=last.max(1)).map(|k| k as f64 / n as f64).collect();
        let at = |i: usize, v: f64| c.lo[i] + v * (c.hi[i] - c.lo[i]);
        // down onto the first pass's start, from the safe radius
        b.rapid_r(safe_r);
        let a0 = at(0, vs[0]);
        // (A as near the last as the turn allows)
        let a0 = a0 + ((b.a - a0) / 360.0).round() * 360.0;
        let turn = a0 - at(0, vs[0]);
        b.rapid(Some(c.xs[0]), None, Some(a0));
        b.plunge(cl(c.xs[0], a0), op.plunge_feed);
        for (k, &v) in vs.iter().enumerate() {
            if crate::budget::over() {
                break;
            }
            let idx: Vec<usize> = if k % 2 == 0 {
                (0..c.xs.len()).collect()
            } else {
                (0..c.xs.len()).rev().collect()
            };
            for (j, &i) in idx.iter().enumerate() {
                let (x, a) = (c.xs[i], at(i, v) + turn);
                if j == 0 && k > 0 {
                    // across to this pass at the channel's end, on the floor
                    let pa = b.a;
                    let steps = (((a - pa).abs() / 1.0).ceil() as usize).max(1);
                    for s in 1..=steps {
                        go(b, x, pa + (a - pa) * s as f64 / steps as f64);
                    }
                    continue;
                }
                go(b, x, a);
            }
        }
        b.rapid_r(safe_r);
    }
    Ok(b.moves[first..]
        .iter()
        .filter_map(|m| match m {
            Move::Cut { r, .. } | Move::Plunge { r, .. } => Some(*r),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min))
}
