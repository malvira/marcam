//! Parting (rotary): see [`crate::job::Parting`]. The tool stands radially over the turning
//! bar and feeds in as A turns (a helix in radius), each turn a little less deep and a little
//! slower than the last as the core shrinks, so the cut ends gently at the axis.

use crate::det::Det;
use crate::job::Parting;
use crate::numeric::rad;
use crate::toolpath::Builder;

/// The tool a parting cut is made with.
#[derive(Debug, Clone, Copy)]
pub enum PartingTool {
    /// A flat end mill of this radius: a square groove, a flat face on the part.
    Flat { radius: f64 },
    /// A V-bit (half angle, degrees): its tip is kept back from the part so the flank never
    /// crosses `x`, leaving a cone on the remainder's side.
    Vbit { half_angle: f64 },
}

/// The parting passes; returns the lowest tip radius.
pub fn parting(
    b: &mut Builder,
    tool: PartingTool,
    stock_r: f64,
    safe_r: f64,
    op: &Parting,
) -> Result<f64, String> {
    if !(op.depth_per_rev > 0.0 && op.segment_deg > 0.0) {
        return Err("depth_per_rev and segment_deg must be > 0".into());
    }
    if !(op.taper > 0.0 && op.taper <= 1.0) {
        return Err("taper: a fraction above 0, at most 1".into());
    }
    if !(op.step_x > 0.0 && op.step_x <= 1.0) {
        return Err("step_x: a fraction of the tool width above 0, at most 1".into());
    }
    if op.stop_radius < 0.0 || op.sliver < 0.0 || op.chamfer < 0.0 || op.width < 0.0 {
        return Err("width, stop_radius, sliver and chamfer must not be negative".into());
    }
    let top = op.top.unwrap_or(stock_r).min(stock_r);
    let end = if op.stop_radius > 0.0 {
        op.stop_radius
    } else {
        op.sliver
    };
    if top.partial_cmp(&end) != Some(std::cmp::Ordering::Greater) {
        return Err(format!(
            "it would stop at a radius of {end} mm, not below the material's {top:.3} mm"
        ));
    }
    // the tool's half width where it meets the material's top, at the deepest
    let half = match tool {
        PartingTool::Flat { radius } => radius,
        PartingTool::Vbit { half_angle } => (top - end) * rad(half_angle).dtan(),
    };
    // pass centres from the remainder's side to the part's (the part-side wall cut last)
    let width = op.width.max(2.0 * half);
    let (first, last) = (op.x - width + half, op.x - half);
    // (a groove one tool wide: one pass; impeller-6's 4e-16 mm of rounding gave it two)
    let n = (((last - first) / (op.step_x * 2.0 * half).max(1e-3)) - 1e-6)
        .ceil()
        .max(0.0) as usize;
    let centres: Vec<f64> = (0..=n)
        .map(|i| {
            if n == 0 {
                last
            } else {
                first + (last - first) * i as f64 / n as f64
            }
        })
        .collect();
    let seg = op.segment_deg;
    // how deep and how fast at a radius: falling to `taper` of the outside's at the axis
    let scale = |r: f64| op.taper + (1.0 - op.taper) * (r / top).clamp(0.0, 1.0);
    let turn = |b: &mut Builder, x: f64, r: f64, feed: f64| {
        let steps = (360.0 / seg).ceil() as usize;
        for _ in 0..steps {
            let a = b.a + seg;
            b.cut(x, r, a, feed, op.plunge_feed);
        }
    };

    // the chamfer on the part's edge, first
    if op.chamfer > 0.0 {
        b.comment("--- parting: chamfer the part's edge ---");
        match tool {
            PartingTool::Vbit { half_angle } => {
                // the tip at the edge, as deep as the flank needs to reach `chamfer` along X
                let r = top - op.chamfer / rad(half_angle).dtan();
                b.rapid_r(safe_r);
                b.rapid_x(op.x);
                b.rapid_r(top + 0.5);
                b.plunge(r.max(end), op.plunge_feed);
                turn(b, op.x, r.max(end), op.feed);
            }
            PartingTool::Flat { radius } => {
                // in steps: the tool's part-side edge at x + s, down to top - chamfer + s
                let steps = ((op.chamfer / 0.05).ceil() as usize).clamp(2, 40);
                for k in (1..steps).rev() {
                    if crate::budget::over() {
                        break;
                    }
                    let s = op.chamfer * k as f64 / steps as f64;
                    let (x, r) = (op.x + s - radius, top - op.chamfer + s);
                    b.rapid_r(safe_r);
                    b.rapid_x(x);
                    b.rapid_r(top + 0.5);
                    b.plunge(r, op.plunge_feed);
                    turn(b, x, r, op.feed);
                }
            }
        }
        b.rapid_r(safe_r);
    }

    b.comment(format!(
        "--- parting at X{:.3}: {} ---",
        op.x,
        if op.stop_radius > 0.0 {
            format!("down to a radius of {:.3}", op.stop_radius)
        } else if op.sliver > 0.0 {
            format!("cut off, leaving a {:.3} mm core", op.sliver)
        } else {
            "cut off".to_string()
        }
    ));
    for &x in &centres {
        if crate::budget::over() {
            break;
        }
        b.rapid_r(safe_r);
        b.rapid_x(x);
        let mut r = top + 0.5;
        b.rapid_r(r);
        // the helical infeed: each step a segment of A and its share of the depth per turn
        while r > end + 1e-9 {
            if crate::budget::over() {
                break;
            }
            let k = scale(r);
            let next = (r - op.depth_per_rev * k * seg / 360.0).max(end);
            let a = b.a + seg;
            b.cut(x, next, a, op.feed * k, op.plunge_feed);
            r = next;
        }
        // a turn at the bottom (a stop or a sliver: a clean floor); a cut-off is through
        if end > 0.0 {
            turn(b, x, end, op.feed * scale(end));
        }
        b.rapid_r(safe_r);
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;
    use crate::toolpath::Move;

    fn op() -> Parting {
        serde_json::from_value(serde_json::json!({
            "tool": 1, "x": -20.0, "depth_per_rev": 0.5, "feed": 300.0, "plunge_feed": 60.0
        }))
        .unwrap()
    }

    fn cuts(b: &Builder) -> Vec<(f64, f64, f64, f64)> {
        b.moves
            .iter()
            .filter_map(|m| match m {
                Move::Cut { x, r, a, feed, .. } => Some((*x, *r, *a, *feed)),
                _ => None,
            })
            .collect()
    }

    fn run(tool: PartingTool, o: &Parting) -> Builder {
        let mut b = Builder::new();
        b.x = Some(0.0);
        b.r = Some(20.0);
        parting(&mut b, tool, 10.0, 15.0, o).unwrap();
        b
    }

    #[test]
    fn a_flat_tool_feeds_in_helically_to_the_axis_slowing_down() {
        let b = run(PartingTool::Flat { radius: 1.5 }, &op());
        let c = cuts(&b);
        // the tool's part-side edge on x: its centre a radius behind
        assert!(
            c.iter().all(|m| (m.0 - (-21.5)).abs() < 1e-9),
            "{:?}",
            &c[..3]
        );
        // the radius falls with A to 0 (a cut-off), never rising
        assert!(c
            .windows(2)
            .all(|w| w[1].1 <= w[0].1 + 1e-12 && w[1].2 > w[0].2));
        assert!(c.last().unwrap().1.abs() < 1e-12);
        // the first turn 0.5 mm deep at full feed, the last 0.4 of that
        let per_seg = |i: usize| c[i].1 - c[i + 1].1;
        assert!(
            (per_seg(0) - 0.5 * 6.0 / 360.0).abs() < 2e-4,
            "{}",
            per_seg(0)
        );
        let l = c.len();
        assert!(
            (per_seg(l - 3) - 0.4 * 0.5 * 6.0 / 360.0).abs() < 1e-4,
            "{}",
            per_seg(l - 3)
        );
        assert!(
            (c[0].3 - 300.0).abs() < 1.0 && (c[l - 1].3 - 120.0).abs() < 2.0,
            "{} {}",
            c[0].3,
            c[l - 1].3
        );
    }

    #[test]
    fn stop_sliver_width_and_chamfer() {
        // stop at a radius: a turn at the bottom
        let mut o = op();
        o.stop_radius = 4.0;
        let c = cuts(&run(PartingTool::Flat { radius: 1.5 }, &o));
        assert!(c.iter().all(|m| m.1 >= 4.0 - 1e-9));
        let at_bottom = c.iter().filter(|m| (m.1 - 4.0).abs() < 1e-9).count();
        assert!(at_bottom >= 60, "{at_bottom}");
        // a sliver: down to it
        let mut o = op();
        o.sliver = 0.3;
        let c = cuts(&run(PartingTool::Flat { radius: 1.5 }, &o));
        assert!((c.iter().map(|m| m.1).fold(f64::INFINITY, f64::min) - 0.3).abs() < 1e-9);
        // wider than the tool: passes stepped along X, the part side last
        let mut o = op();
        o.width = 6.0;
        let c = cuts(&run(PartingTool::Flat { radius: 1.5 }, &o));
        let mut xs: Vec<f64> = c.iter().map(|m| m.0).collect();
        xs.dedup();
        assert!(
            xs.len() >= 3
                && (xs[0] - (-24.5)).abs() < 1e-9
                && (xs.last().unwrap() - (-21.5)).abs() < 1e-9,
            "{xs:?}"
        );
        assert!(xs.windows(2).all(|w| w[1] - w[0] <= 0.8 * 3.0 + 1e-9));
        // a chamfer with the flat tool: steps reaching x + chamfer, first
        let mut o = op();
        o.chamfer = 1.0;
        let c = cuts(&run(PartingTool::Flat { radius: 1.5 }, &o));
        let edge = c
            .iter()
            .map(|m| m.0 + 1.5)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(edge > -20.0 + 0.8 && edge <= -19.0 + 1e-9, "{edge}");
        // a V-bit: its tip far enough back that the flank meets x only at the top
        let c = cuts(&run(PartingTool::Vbit { half_angle: 30.0 }, &op()));
        let tip = c[0].0;
        assert!(
            (tip - (-20.0 - 10.0 * 30f64.to_radians().dtan())).abs() < 1e-9,
            "{tip}"
        );
    }

    #[test]
    fn nonsense_is_refused() {
        let mut o = op();
        o.stop_radius = 12.0;
        let mut b = Builder::new();
        assert!(parting(&mut b, PartingTool::Flat { radius: 1.0 }, 10.0, 15.0, &o).is_err());
        let mut o = op();
        o.taper = 0.0;
        assert!(parting(&mut b, PartingTool::Flat { radius: 1.0 }, 10.0, 15.0, &o).is_err());
    }

    /// A groove one tool wide is one pass, whatever the rounding of its X (Mar's impeller-6,
    /// at X -22 with a 3.175 tool: the same pass twice).
    #[test]
    fn a_groove_one_tool_wide_is_one_pass() {
        let mut o = op();
        o.x = -22.0;
        o.stop_radius = 6.0;
        let b = run(
            PartingTool::Flat {
                radius: 3.175 / 2.0,
            },
            &o,
        );
        let passes = b
            .moves
            .windows(2)
            .filter(|w| {
                matches!(w[0], Move::Rapid { x: Some(_), .. })
                    && matches!(w[1], Move::Rapid { r: Some(_), .. })
            })
            .count();
        assert_eq!(passes, 1);
    }
}
