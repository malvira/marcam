//! Rotary turning: rough the stock down to the revolved profile in radial levels with a flat
//! endmill, then finish it in one helical pass.

use super::{helix, HelixFeeds};
use crate::geom::RevolvedProfile;
use crate::job::RotaryTurning;
use crate::numeric::{arange, interp, py_max, round_half_even};
use crate::toolpath::Builder;

/// Contiguous runs of `true` in `mask`, as `(xs[start], xs[end])`.
fn intervals(mask: &[bool], xs: &[f64]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &m) in mask.iter().enumerate() {
        if m && start.is_none() {
            start = Some(i);
        }
        if !m {
            if let Some(s) = start.take() {
                out.push((xs[s], xs[i - 1]));
            }
        }
    }
    if let Some(s) = start {
        out.push((xs[s], xs[xs.len() - 1]));
    }
    out
}

/// Tool-centre radius table for a flat endmill of radius `rt` over the profile.
struct DropTable {
    xg: Vec<f64>,
    zt: Vec<f64>,
}

impl DropTable {
    fn new(geom: &RevolvedProfile, rt: f64, op: &RotaryTurning, lo: f64, hi: f64) -> DropTable {
        let footprint = arange(-rt, rt + 1e-9, op.footprint_step);
        let k0 = ((lo - 1.0) / op.grid).floor() as i64;
        let k1 = ((hi + 1.0) / op.grid).ceil() as i64;
        let xg: Vec<f64> = (k0..=k1)
            .map(|k| round_half_even(k as f64 * op.grid, 6))
            .collect();
        let zt = xg.iter().map(|&x| geom.flat_drop(x, &footprint)).collect();
        DropTable { xg, zt }
    }

    fn at(&self, x: f64) -> f64 {
        interp(x, &self.xg, &self.zt)
    }
}

pub fn rotary_turning(
    b: &mut Builder,
    geom: &RevolvedProfile,
    tool_diameter: f64,
    stock_r: f64,
    safe_r: f64,
    op: &RotaryTurning,
) {
    let rt = tool_diameter / 2.0;
    // a picked span: only that part of the profile
    let (x_min, x_max) = match op.x_range {
        Some([a, b]) => (geom.x_min().max(a.min(b)), geom.x_max().min(a.max(b))),
        None => (geom.x_min(), geom.x_max()),
    };
    let lo = x_min - rt - 1.0;
    let hi = x_max + rt + 1.0;
    // (Mar) its stock to leave: the faces across the bar as if the tool were wider by the
    // axial leave, the radius up by the radial
    let table = DropTable::new(geom, rt + op.leave_axial.max(-rt * 0.9), op, lo, hi);
    let zt = |x: f64| table.at(x) + op.leave_radial;

    let xs: Vec<f64> = table
        .xg
        .iter()
        .copied()
        .filter(|&x| x >= lo && x <= hi)
        .collect();
    let z_need: Vec<f64> = xs.iter().map(|&x| zt(x) + op.finish_allowance).collect();
    let z_min = z_need.iter().copied().fold(f64::INFINITY, f64::min);

    let mut levels = Vec::new();
    let mut level = stock_r - op.rough_depth;
    while level > z_min - op.rough_depth + 1e-6 {
        levels.push(py_max(level, z_min));
        level -= op.rough_depth;
    }

    let rough = HelixFeeds {
        feed: op.rough_feed,
        plunge_cap: op.plunge_feed,
        segment_deg: op.segment_deg,
    };
    let mut prev = stock_r;
    let mut direction = 1;
    b.comment("--- roughing ---");
    for &level in &levels {
        if crate::budget::over() {
            break;
        }
        let mask: Vec<bool> = z_need.iter().map(|&z| z < level - 1e-6).collect();
        for (mut xa, mut xb) in intervals(&mask, &xs) {
            // The drop table already accounts for the tool radius, so the interval is exactly
            // where the tool centre can sit at this level: no wall drops inside a pass.
            let zf = |x: f64| py_max(level, zt(x) + op.finish_allowance);
            // alternate direction per level so A stays bounded
            if direction < 0 {
                std::mem::swap(&mut xa, &mut xb);
            }
            let zs = py_max(prev + 0.5, zf(xa)); // never plunge below the profile at entry
            b.rapid_r(safe_r);
            b.rapid_x(xa);
            b.plunge(zs, op.plunge_feed);
            helix(b, xa, xb, &zf, op.rough_step, rough, Some(zs), direction);
            b.rapid_r(safe_r);
        }
        prev = level;
        direction = -direction;
    }

    b.comment("--- finishing ---");
    let (xa, xb) = (x_max + rt + 0.5, x_min + rt + 0.02);
    b.rapid_r(safe_r);
    b.rapid_x(xa);
    let z0 = zt(xa);
    b.plunge(z0 + 0.5, op.plunge_feed);
    let finish = HelixFeeds {
        feed: op.finish_feed,
        ..rough
    };
    helix(b, xa, xb, &zt, op.finish_step, finish, Some(z0 + 0.5), 1);
    b.rapid_r(safe_r);
}
