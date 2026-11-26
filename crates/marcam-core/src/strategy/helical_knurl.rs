//! Knurls cut with a V-bit over a conical body: diamond (families of helical V grooves
//! crossing) or straight (V grooves along X, parallel to the axis).

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
    let straight = match op.pattern.as_str() {
        "diamond" => false,
        "straight" => true,
        p => return Err(format!("knurl pattern \"{p}\": diamond or straight")),
    };
    // (straight: along X, no helix, one family)
    let helix = if straight { 0.0 } else { op.helix_angle };
    let families: Vec<i32> = if straight {
        vec![1]
    } else {
        op.families.clone()
    };
    let n = op.count as f64;
    let depth = |x: f64| {
        let p_perp = 2.0 * std::f64::consts::PI * body_r(x) / n * rad(helix).dcos();
        op.fill * p_perp / (2.0 * rad(vbit_angle / 2.0).dtan())
    };

    // run out past both ends of the body
    let xs = linspace(xb - runout[0], xf + runout[1], op.samples);
    let xc: Vec<f64> = xs.iter().map(|&x| x.clamp(xb, xf)).collect();
    // angle swept along the groove: dθ/dx = tan(α)/R(x)
    let tan_a = rad(helix).dtan();
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
        for &fam in &families {
            let sign = if fam > 0 { '+' } else { '-' };
            if straight {
                b.comment(format!("--- straight knurl pass {:.0}% ---", frac * 100.0));
            } else {
                b.comment(format!(
                    "--- knurl family {sign} pass {:.0}% ---",
                    frac * 100.0
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolpath::Move;

    fn op(pattern: &str) -> HelicalKnurl {
        serde_json::from_value(serde_json::json!({
            "tool": 2, "body_back": [-30.0, 10.0], "body_front": [-10.0, 9.0], "count": 24,
            "helix_angle": 30.0, "passes": [1.0], "feed": 200.0, "plunge_feed": 60.0,
            "pattern": pattern
        }))
        .unwrap()
    }

    /// Each groove's cutting moves: (x, a) along it.
    fn grooves(o: &HelicalKnurl) -> Vec<Vec<(f64, f64)>> {
        let mut b = Builder::new();
        helical_knurl(&mut b, 90.0, 15.0, o, [0.6, 0.6]).unwrap();
        let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
        for m in &b.moves {
            match *m {
                Move::Plunge { .. } => out.push(Vec::new()),
                Move::Cut { x, a, .. } => out.last_mut().unwrap().push((x, a)),
                _ => {}
            }
        }
        out
    }

    /// (Mar: "a selection box as a parameter for diamond knurl or straight knurls") Diamond:
    /// two families of helices, A turning along each groove; straight: one family, each
    /// groove along X at its own A, 24 of them a fifteenth of a turn apart. Without a pattern
    /// (an older project), diamond.
    #[test]
    fn diamond_grooves_turn_straight_ones_run_along_x() {
        let d = grooves(&op("diamond"));
        assert_eq!(d.len(), 48);
        assert!(d
            .iter()
            .all(|g| (g.last().unwrap().1 - g[0].1).abs() > 10.0));
        let s = grooves(&op("straight"));
        assert_eq!(s.len(), 24);
        for (k, g) in s.iter().enumerate() {
            assert!(
                g.iter().all(|&(_, a)| (a - k as f64 * 15.0).abs() < 1e-9),
                "{k}: {:?}",
                &g[..2]
            );
            assert!(g.last().unwrap().0 - g[0].0 > 20.0);
        }
        let mut old: serde_json::Value = serde_json::to_value(op("diamond")).unwrap();
        assert!(
            old.get("pattern").is_none(),
            "diamond not written: old files unchanged"
        );
        old.as_object_mut().unwrap().remove("pattern");
        let o: HelicalKnurl = serde_json::from_value(old).unwrap();
        assert_eq!(grooves(&o), d);
        let bad = op("knobbly");
        assert!(helical_knurl(&mut Builder::new(), 90.0, 15.0, &bad, [0.6, 0.6]).is_err());
    }
}
