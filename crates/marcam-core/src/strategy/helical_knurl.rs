//! Diamond knurl: families of helical V grooves cut with a V-bit over a conical body.

use crate::det::Det;
use crate::job::HelicalKnurl;
use crate::numeric::{deg, linspace, rad};
use crate::toolpath::Builder;

pub fn helical_knurl(
    b: &mut Builder,
    vbit_angle: f64,
    safe_r: f64,
    op: &HelicalKnurl,
    // (how far the grooves run past the back and the front: none at a shoulder)
    runout: [f64; 2],
) -> Result<(), String> {
    let [xb, rb] = op.body_back;
    let [xf, rf] = op.body_front;
    // the grooves turn by tan(helix angle) / radius per mm: a body needs a radius all along
    let ok = rb.min(rf) >= 0.1 && xf > xb;
    if !ok {
        return Err(format!(
            "the knurl's body runs from [{xb}, {rb}] to [{xf}, {rf}]: it needs its back before its front and a radius of 0.1 mm or more at both ends"
        ));
    }
    let body_r = |x: f64| rf + (rb - rf) * (xf - x) / (xf - xb);
    let n = op.count as f64;
    let depth = |x: f64| {
        let p_perp = 2.0 * std::f64::consts::PI * body_r(x) / n * rad(op.helix_angle).dcos();
        op.fill * p_perp / (2.0 * rad(vbit_angle / 2.0).dtan())
    };

    // run out past both ends of the body
    let xs = linspace(xb - runout[0], xf + runout[1], op.samples);
    let xc: Vec<f64> = xs.iter().map(|&x| x.clamp(xb, xf)).collect();
    // angle swept along the groove: dθ/dx = tan(α)/R(x)
    let tan_a = rad(op.helix_angle).dtan();
    let dth: Vec<f64> = xc.iter().map(|&x| tan_a / body_r(x)).collect();
    let mut theta = Vec::with_capacity(xs.len());
    theta.push(0.0);
    let mut acc = 0.0;
    for i in 1..xs.len() {
        acc += 0.5 * (dth[i] + dth[i - 1]) * (xs[i] - xs[i - 1]);
        theta.push(acc);
    }
    let theta: Vec<f64> = theta.into_iter().map(deg).collect();
    let r_body: Vec<f64> = xc.iter().map(|&x| body_r(x)).collect();
    let r_bot: Vec<f64> = xc.iter().map(|&x| body_r(x) - depth(x)).collect();
    let approach = rb.max(rf) + op.approach_clearance;

    for &frac in &op.passes {
        let z: Vec<f64> = r_body
            .iter()
            .zip(&r_bot)
            .map(|(&r, &bot)| r - (r - bot) * frac)
            .collect();
        for &fam in &op.families {
            let sign = if fam > 0 { '+' } else { '-' };
            b.comment(format!(
                "--- knurl family {sign} pass {:.0}% ---",
                frac * 100.0
            ));
            for k in 0..op.count {
                if crate::budget::over() {
                    break;
                }
                let a0 = k as f64 * 360.0 / n;
                let a: Vec<f64> = theta.iter().map(|&t| a0 + fam as f64 * t).collect();
                b.rapid_r(safe_r);
                b.rapid(Some(xs[0]), None, Some(a[0]));
                b.rapid_r(approach);
                b.plunge(z[0], op.plunge_feed);
                for i in 1..xs.len() {
                    b.cut(xs[i], z[i], a[i], op.feed, op.plunge_feed);
                }
                b.rapid_r(safe_r);
            }
        }
    }
    Ok(())
}
