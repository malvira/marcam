//! Toolpath strategies. Each one appends moves to a [`Builder`].

pub mod adaptive;
pub mod bore;
pub mod custom;
pub mod edges;
pub mod engrave;
mod flank_chamfer;
mod helical_knurl;
pub mod parting;
mod rotary_finish;
mod rotary_rough;
mod rotary_turning;
pub mod three_axis;

pub use flank_chamfer::flank_chamfer;
pub use helical_knurl::helical_knurl;
pub use parting::{parting, PartingTool};
pub use rotary_finish::{rotary_finish, rotary_finish_on};
pub use rotary_rough::{rotary_rough, StockNear};
pub use rotary_turning::rotary_turning;

use crate::toolpath::Builder;

/// Feed parameters shared by helical moves.
#[derive(Debug, Clone, Copy)]
pub struct HelixFeeds {
    pub feed: f64,
    pub plunge_cap: f64,
    pub segment_deg: f64,
}

/// Rotary helix from `x0` to `x1` with radius `rfun(x)`, advancing `step` along X per revolution.
/// A turns in `direction` (+1/-1). If `r_start` is given the tool ramps down to the profile over
/// one revolution at `x0`; then one full revolution at `x0`, the helix, and one at `x1`.
#[allow(clippy::too_many_arguments)]
pub fn helix(
    b: &mut Builder,
    x0: f64,
    x1: f64,
    rfun: &dyn Fn(f64) -> f64,
    step: f64,
    f: HelixFeeds,
    r_start: Option<f64>,
    direction: i32,
) {
    let dir = direction as f64;
    let seg = f.segment_deg;
    let n_rev = (360.0 / seg) as usize;
    if let Some(zs) = r_start {
        for i in 1..=n_rev {
            if crate::budget::over() {
                break;
            }
            let frac = i as f64 / n_rev as f64;
            let a = b.a + dir * seg;
            b.cut(x0, zs + (rfun(x0) - zs) * frac, a, f.feed, f.plunge_cap);
        }
    }
    for _ in 0..n_rev {
        let a = b.a + dir * seg;
        b.cut(x0, rfun(x0), a, f.feed, f.plunge_cap);
    }
    let len = (x1 - x0).abs();
    if len > 1e-9 {
        let n = ((len / step * 360.0 / seg).ceil() as usize).max(1);
        for i in 1..=n {
            if crate::budget::over() {
                break;
            }
            let x = x0 + (x1 - x0) * i as f64 / n as f64;
            let a = b.a + dir * 360.0 * len / step / n as f64;
            b.cut(x, rfun(x), a, f.feed, f.plunge_cap);
        }
    }
    for _ in 0..n_rev {
        let a = b.a + dir * seg;
        b.cut(x1, rfun(x1), a, f.feed, f.plunge_cap);
    }
}
