//! Rotary deburring (4-axis simultaneous): the part's sharp convex edges a tool over the axis
//! can reach (a blade's or a fin's top edges, a groove's lips) chamfered with a V-bit that
//! follows each edge, X and A together, its tip `depth` below it.
//!
//! The V-bit stands over the axis with its axis through the edge. Where one face of the edge
//! looks out from the axis (a top) and the other runs down from it (a wall), the cone takes
//! `depth` off the wall and `depth · tan(half angle)` off the top: a 90° bit, a 45° chamfer
//! `depth` on each leg. An edge whose top no face looks out from the axis it cannot reach is
//! left; so is a stretch where the cone over it would meet the part elsewhere first.

use crate::dropcutter::DropCutter;
use crate::job::RotaryDeburr;
use crate::mesh::Mesh;
use crate::toolpath::Builder;
use std::collections::HashMap;

type VKey = (i64, i64, i64);

fn key(p: &[f64; 3]) -> VKey {
    let q = |v: f64| (v * 1e4).round() as i64;
    (q(p[0]), q(p[1]), q(p[2]))
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn unit_normal(t: &[[f64; 3]; 3]) -> Option<[f64; 3]> {
    let (u, v) = (sub(t[1], t[0]), sub(t[2], t[0]));
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let l = dot(n, n).sqrt();
    (l > 1e-12).then(|| [n[0] / l, n[1] / l, n[2] / l])
}

/// The part's sharp convex edges a tool over the axis reaches, within X `x0..x1`, chained into
/// polylines (part-frame points): where the faces turn by `min_angle` degrees or more and one
/// of them looks out from the axis (within 35°).
pub fn deburr_edges(mesh: &Mesh, x0: f64, x1: f64, min_angle: f64) -> Vec<Vec<[f64; 3]>> {
    let tris = &mesh.triangles;
    let normals: Vec<Option<[f64; 3]>> = tris.iter().map(unit_normal).collect();
    let mut at: HashMap<VKey, [f64; 3]> = HashMap::new();
    let mut edges: HashMap<(VKey, VKey), Vec<usize>> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for e in 0..3 {
            let (p, q) = (t[e], t[(e + 1) % 3]);
            let (a, b) = (key(&p), key(&q));
            at.insert(a, p);
            at.insert(b, q);
            let k = if a < b { (a, b) } else { (b, a) };
            edges.entry(k).or_default().push(i);
        }
    }
    let cos_turn = min_angle.to_radians().cos();
    let cos_out = 35f64.to_radians().cos();
    let centroid = |t: &[[f64; 3]; 3]| [0, 1, 2].map(|k| (t[0][k] + t[1][k] + t[2][k]) / 3.0);
    let mut sharp: Vec<(VKey, VKey)> = Vec::new();
    for (&(a, b), list) in &edges {
        if list.len() != 2 {
            continue;
        }
        let (ta, tb) = (list[0], list[1]);
        let (Some(na), Some(nb)) = (normals[ta], normals[tb]) else {
            continue;
        };
        if dot(na, nb) > cos_turn {
            continue;
        }
        // convex: each face's other side below the other's plane
        if dot(sub(centroid(&tris[tb]), centroid(&tris[ta])), na) > -1e-9 {
            continue;
        }
        let (pa, pb) = (at[&a], at[&b]);
        let mid = [0, 1, 2].map(|k| 0.5 * (pa[k] + pb[k]));
        if mid[0] < x0 - 1e-9 || mid[0] > x1 + 1e-9 {
            continue;
        }
        let rho = mid[1].hypot(mid[2]);
        if rho < 1e-6 {
            continue;
        }
        let out = [0.0, mid[1] / rho, mid[2] / rho];
        // (a top: one face looking out from the axis)
        if dot(na, out).max(dot(nb, out)) < cos_out {
            continue;
        }
        sharp.push((a, b));
    }
    // chained: through vertices two sharp edges share
    let mut by: HashMap<VKey, Vec<usize>> = HashMap::new();
    for (i, &(a, b)) in sharp.iter().enumerate() {
        by.entry(a).or_default().push(i);
        by.entry(b).or_default().push(i);
    }
    let mut used = vec![false; sharp.len()];
    let mut chains = Vec::new();
    // (from an end where there is one, else anywhere: a loop)
    let mut order: Vec<usize> = (0..sharp.len()).collect();
    order.sort_by_key(|&i| {
        let (a, b) = sharp[i];
        let ends = (by[&a].len() != 2) as u8 + (by[&b].len() != 2) as u8;
        std::cmp::Reverse(ends)
    });
    for start in order {
        if used[start] {
            continue;
        }
        used[start] = true;
        let (a, b) = sharp[start];
        let (first, mut cur) = if by[&a].len() != 2 { (a, b) } else { (b, a) };
        let mut chain = vec![at[&first], at[&cur]];
        loop {
            let next = by[&cur]
                .iter()
                .copied()
                .filter(|&e| !used[e])
                .find(|_| by[&cur].len() == 2);
            let Some(e) = next else { break };
            used[e] = true;
            let (p, q) = sharp[e];
            cur = if p == cur { q } else { p };
            chain.push(at[&cur]);
        }
        chains.push(chain);
    }
    chains
}

/// Plan the deburring: each chain followed with the V-bit over the axis through it, `depth`
/// below the edge. `dc` the V-bit's drop-cutter (where the cone over a point first meets the
/// part: a stretch where that is above the edge is left); `clear(x, a, tip)`: the shank and
/// holder over a tip there clear of the part (else the stretch is left too).
pub fn rotary_deburr(
    b: &mut Builder,
    mesh: &Mesh,
    dc: &DropCutter,
    clear: &dyn Fn(f64, f64, f64) -> bool,
    safe_r: f64,
    op: &RotaryDeburr,
) -> Result<usize, String> {
    if op.depth <= 0.0 {
        return Err("rotary deburr: the depth must be more than 0".into());
    }
    let (x0, x1) = (
        op.x_range[0].min(op.x_range[1]),
        op.x_range[0].max(op.x_range[1]),
    );
    // (not the part's back end: the bar goes on past it to the chuck, its edges are not edges
    // of the stock until it is parted off; the holder went into the bar there)
    let back = mesh.bounds().map_or(f64::NEG_INFINITY, |(lo, _)| lo[0]);
    let back_clear = back + 0.25;
    let chains = deburr_edges(mesh, x0.max(back_clear), x1, op.min_angle);
    if chains.is_empty() {
        b.warnings.push(format!(
            "rotary deburr: no sharp edge a tool over the axis reaches in X {x0:.1}..{x1:.1} (faces turning by {:.0}° or more, one of them looking out from the axis)",
            op.min_angle
        ));
        return Ok(0);
    }
    b.comment("--- rotary deburr (V-bit along the edges, 4-axis simultaneous) ---");
    let step = op.sample_mm.max(0.02);
    let mut done = 0;
    let mut skipped = 0usize;
    for chain in &chains {
        // resampled along the edge
        let mut pts: Vec<[f64; 3]> = vec![chain[0]];
        for w in chain.windows(2) {
            let l = dot(sub(w[1], w[0]), sub(w[1], w[0])).sqrt();
            let n = (l / step).ceil().max(1.0) as usize;
            for k in 1..=n {
                let f = k as f64 / n as f64;
                pts.push([0, 1, 2].map(|i| w[0][i] + (w[1][i] - w[0][i]) * f));
            }
        }
        // each point: X, its radius and the A that puts it under the tool (continuous)
        let mut a_prev: Option<f64> = None;
        let mut run: Vec<(f64, f64, f64)> = Vec::new();
        let flush = |b: &mut Builder, run: &mut Vec<(f64, f64, f64)>, done: &mut usize| {
            if run.len() >= 2 {
                let (x, r, a) = run[0];
                b.rapid_r(safe_r);
                b.rapid(Some(x), None, Some(a));
                b.rapid_r(r + op.depth + 0.5);
                b.plunge(r, op.plunge_feed);
                for &(x, r, a) in run.iter().skip(1) {
                    b.cut(x, r, a, op.feed, op.plunge_feed);
                }
                b.rapid_r(safe_r);
                *done += 1;
            }
            run.clear();
        };
        for p in &pts {
            let rho = p[1].hypot(p[2]);
            let theta = p[2].atan2(p[1]).to_degrees();
            let mut a = 90.0 - theta;
            if let Some(q) = a_prev {
                a += 360.0 * ((q - a) / 360.0).round();
            }
            a_prev = Some(a);
            // (the cone over this point meets the part above the edge: something else in the
            // way there; the run stops)
            let first = dc.radius_at(p[0], a, 0.0);
            // (nor where the shank or the holder over it would meet the part: a short V-bit
            // between a hub's fins)
            let blocked = !clear(p[0], a, rho - op.depth);
            if blocked {
                skipped += 1;
            }
            if first > rho + 0.05 || p[0] < back_clear || blocked {
                flush(b, &mut run, &mut done);
                continue;
            }
            run.push((p[0], rho - op.depth, a));
        }
        flush(b, &mut run, &mut done);
    }
    if skipped > 0 {
        b.warnings.push(format!(
            "rotary deburr: {:.0} mm of edges left where the tool's shank or holder would come within {:.1} mm of the part or the stock (rough or finish first, or a longer tool)",
            skipped as f64 * step,
            2.0 * crate::collision::NEAR_MISS
        ));
    }
    Ok(done)
}
