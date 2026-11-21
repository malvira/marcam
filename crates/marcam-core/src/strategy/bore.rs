//! Boring (3-axis): round holes larger than the end mill, interpolated.
//!
//! For each hole: roughing rings from the middle out to the finishing allowance (each a
//! helical descent from just above the top to the bottom, then a full circle there), the
//! finish at the hole's size (a helix and a circle), and the spring passes (the finish again,
//! for the tool's deflection). Climb milling, with the spindle turning clockwise, runs
//! counter-clockwise round the inside of a hole (G3); conventional, clockwise (G2). After
//! each ring the tool moves to the middle before it lifts, away from the wall.
//!
//! The moves are straight segments within `tolerance` of the circle; with `arcs` the post
//! writes them back as G2/G3 (the program's operation marker asks it to).

use crate::det::Det;
use crate::job::{Bore, StockBox};
use crate::toolpath::Builder;
use std::f64::consts::TAU;

const UP: [f64; 3] = [0.0, 0.0, 1.0];

/// Angle step so a chord stays within `tol` of a circle of radius `r`.
fn step_angle(r: f64, tol: f64) -> f64 {
    if r <= tol {
        return TAU / 8.0;
    }
    (2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).dacos()).clamp(TAU / 720.0, TAU / 8.0)
}

/// The tool-centre radii of the passes for a hole: roughing rings (spaced at most `step`,
/// the first close enough to the middle to clear the core), the finish, the spring passes.
pub fn ring_radii(
    hole_r: f64,
    tool_r: f64,
    step: f64,
    allowance: f64,
    springs: u32,
) -> Result<(Vec<f64>, f64), String> {
    let finish = hole_r - tool_r;
    if finish < 0.05 {
        return Err(format!(
            "a {:.2} mm hole needs a tool smaller than it (this one is {:.2} mm): drill it instead",
            2.0 * hole_r,
            2.0 * tool_r
        ));
    }
    let rough_to = (finish - allowance.max(0.0)).max(0.0);
    let mut rings = Vec::new();
    if rough_to > 1e-6 {
        // rings at rough_to, rough_to - step, ... the innermost at most one tool radius from
        // the middle (it clears the core)
        let first = rough_to.min(tool_r * 0.9).max(rough_to.min(0.05));
        let n = ((rough_to - first) / step.max(0.01)).ceil() as usize;
        for k in 0..=n {
            let r = if n == 0 {
                rough_to
            } else {
                first + (rough_to - first) * k as f64 / n as f64
            };
            if rings.last().is_none_or(|&l: &f64| r > l + 1e-9) {
                rings.push(r);
            }
        }
    }
    let mut passes = rings;
    for _ in 0..=springs {
        passes.push(finish);
    }
    Ok((passes, finish))
}

pub fn bore(
    b: &mut Builder,
    tool_d: f64,
    stock: &StockBox,
    safe: f64,
    op: &Bore,
) -> Result<(), String> {
    let tool_r = tool_d / 2.0;
    let hole_r = op.diameter / 2.0;
    let top = op.top.unwrap_or(stock.max[2]);
    let bottom = top - op.depth;
    let (passes, _) = ring_radii(
        hole_r,
        tool_r,
        op.stepover * tool_d,
        op.finish_allowance,
        op.spring_passes,
    )?;
    let ccw = match op.direction.as_str() {
        "climb" => true,
        "conventional" => false,
        d => {
            return Err(format!(
                "direction {d:?}: use \"climb\" or \"conventional\""
            ))
        }
    };
    let sgn = if ccw { 1.0 } else { -1.0 };
    let pitch = op.helix_pitch.max(0.01);
    // (the operations before it having cut the material down: the helix starts there)
    let start_z = super::three_axis::material_top(top) + 0.5;
    b.comment("--- boring ---");
    for &[cx, cy] in &op.points {
        for &r in &passes {
            if crate::budget::over() {
                break;
            }
            let da = step_angle(r, op.tolerance.max(1e-4));
            // into the start, above the top, then down the helix
            b.rapid3(None, None, Some(safe));
            b.rapid3(Some(cx + r), Some(cy), None);
            b.rapid3(None, None, Some(start_z));
            let turns = (start_z - bottom) / pitch;
            let n = ((turns * TAU / da).ceil() as usize).max(1);
            for k in 1..=n {
                let t = k as f64 / n as f64;
                let a = sgn * turns * TAU * t;
                b.line3(
                    cx + r * a.dcos(),
                    cy + r * a.dsin(),
                    start_z + (bottom - start_z) * t,
                    UP,
                    op.feed,
                );
            }
            // a full circle at the bottom (the helix leaves a ramp there)
            let a0 = sgn * turns * TAU;
            let m = ((TAU / da).ceil() as usize).max(4);
            for k in 1..=m {
                let a = a0 + sgn * TAU * k as f64 / m as f64;
                b.line3(cx + r * a.dcos(), cy + r * a.dsin(), bottom, UP, op.feed);
            }
            // away from the wall, then up
            b.line3(cx, cy, bottom, UP, op.feed);
            b.rapid3(None, None, Some(safe));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    #[test]
    fn rings_reach_the_size_and_clear_the_core() {
        // a 12 mm hole with a 4 mm tool: finish radius 4; roughing to 3.9 in steps of 1.6
        let (p, f) = ring_radii(6.0, 2.0, 1.6, 0.1, 1).unwrap();
        assert_eq!(f, 4.0);
        assert!(
            p[0] <= 1.8 + 1e-9,
            "the first ring clears the middle: {p:?}"
        );
        assert!(p.windows(2).all(|w| w[1] - w[0] <= 1.6 + 1e-9), "{p:?}");
        assert_eq!(&p[p.len() - 2..], &[4.0, 4.0], "finish and one spring pass");
        assert!((p[p.len() - 3] - 3.9).abs() < 1e-9);
        // a hole barely bigger than the tool: only the finish
        let (p, _) = ring_radii(2.1, 2.0, 1.6, 0.1, 0).unwrap();
        assert_eq!(p.len(), 1);
        assert!((p[0] - 0.1).abs() < 1e-12);
        assert!(ring_radii(2.0, 2.0, 1.6, 0.1, 0)
            .unwrap_err()
            .contains("drill it instead"));
    }
}
