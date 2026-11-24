//! Pickables: the features of a part an operation can be pointed at, recognised from its
//! mesh. One list serves every operation (holes for drilling, pockets for pocketing, edge
//! loops for contours, faces and surfaces for finishing, bands for rotary work); each
//! operation type accepts some kinds and fills its parameters from them ([`accepts`],
//! [`apply`]).
//!
//! Recognition works on the face regions of [`crate::geometry::face_regions`] (the same ids
//! the `faces` selections use), in the setup's frame:
//! - vise and indexed 3+1 (Z up, the part turned to the operation's A index): flat faces,
//!   holes (concave vertical cylinders, through or blind), pockets (up-facing floors walled
//!   all round, with islands), the edge loops of flat faces, and every other region as a
//!   surface;
//! - rotary (the part's axis is X): cylindrical bands (round all the way, constant radius),
//!   revolved bands (cones, shoulders, other surfaces of revolution) and surfaces (the rest).

use crate::det::Det;
use crate::geometry::{face_regions, FaceRegion};
use crate::mesh::{cross, sub, Mesh, Triangle};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// The frame the part is recognised in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Frame {
    /// Z up (vise; indexed 3+1 with the part turned to `index_a` degrees about X). `sides`:
    /// also the planar faces facing other ways, with the A index that turns each up
    /// (indexed setups).
    Xyz { index_a: f64, sides: bool },
    /// The rotary axis along X.
    Rotary,
}

/// A feature to pick.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Pickable {
    /// Stable within a part: the kind and the face region(s) it comes from.
    pub id: String,
    /// "hole", "pocket", "flat_face", "edge_loop", "surface", "cylindrical_band",
    /// "revolved_band".
    pub kind: String,
    /// What a person reads: "Hole 5.00 mm through, 12.00 deep".
    pub label: String,
    /// The face regions it covers (what lights up).
    pub faces: Vec<String>,
    /// Equal for features alike enough to pick together ("select all similar").
    pub similar: String,
    /// Kind-specific numbers (mm, degrees), see the recognisers.
    pub data: Value,
    /// What it is, independent of the mesh's triangles (kind, position, size), for finding it
    /// again on a changed model: what an operation keeps of its picks (see [`crate::replace`]).
    #[serde(skip_serializing_if = "Value::is_null")]
    pub signature: Value,
}

const FLAT: f64 = 0.999; // |nz| of a flat face
const WALL: f64 = 0.02; // |nz| of a vertical wall
const Q: f64 = 1e4; // vertex welding (0.1 micron)

type Key = (i64, i64, i64);
fn key(p: &[f64; 3]) -> Key {
    (
        (p[0] * Q).round() as i64,
        (p[1] * Q).round() as i64,
        (p[2] * Q).round() as i64,
    )
}

fn unit_normal(t: &Triangle) -> [f64; 3] {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-300);
    [n[0] / l, n[1] / l, n[2] / l]
}

fn tri_area(t: &Triangle) -> f64 {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// One region's triangles and numbers.
struct Region {
    info: FaceRegion,
    tris: Vec<usize>,
    area: f64,
    nz: [f64; 2],
    z: [f64; 2],
}

/// Recognise the pickable features of `mesh` (in the setup's frame).
pub fn pickables(mesh: &Mesh, frame: Frame) -> Vec<Pickable> {
    let mut out = recognise(mesh, frame);
    for p in &mut out {
        p.signature = crate::replace::signature(p);
    }
    out
}

fn recognise(mesh: &Mesh, frame: Frame) -> Vec<Pickable> {
    let (infos, of) = face_regions(mesh, 30.0);
    match frame {
        Frame::Rotary => {
            let mut out = rotary(&infos);
            out.extend(round_edges(mesh, &infos, &of));
            add_band_ends(&mut out, mesh, &infos, &of);
            out
        }
        Frame::Xyz { index_a, sides } => {
            let turned;
            let m = if index_a != 0.0 {
                turned = mesh.rotated_x(index_a);
                &turned
            } else {
                mesh
            };
            let mut out = xyz(m, infos.clone(), &of);
            if sides {
                out.extend(side_faces(mesh, &infos, &of, index_a));
            }
            out
        }
    }
}

/// The radius at each end of every band (X min, X max): a cone's two radii.
fn add_band_ends(out: &mut [Pickable], mesh: &Mesh, infos: &[FaceRegion], of: &[usize]) {
    for p in out.iter_mut() {
        if p.kind != "cylindrical_band" && p.kind != "revolved_band" {
            continue;
        }
        let Some(k) = infos.iter().position(|r| p.faces.first() == Some(&r.id)) else {
            continue;
        };
        let [x0, x1] = infos[k].x;
        let (mut lo, mut hi) = ((0.0, 0usize), (0.0, 0usize));
        for (i, t) in mesh.triangles.iter().enumerate() {
            if of[i] != k {
                continue;
            }
            for q in t {
                let r = q[1].dhypot(q[2]);
                if (q[0] - x0).abs() < 1e-6 {
                    lo = (lo.0 + r, lo.1 + 1);
                }
                if (q[0] - x1).abs() < 1e-6 {
                    hi = (hi.0 + r, hi.1 + 1);
                }
            }
        }
        if lo.1 > 0 && hi.1 > 0 {
            p.data["r_ends"] = json!([round_um(lo.0 / lo.1 as f64), round_um(hi.0 / hi.1 as f64)]);
        }
    }
}

/// Round edges where two full-round faces meet (a shoulder's corner, a chamfer's place):
/// their X and radius.
fn round_edges(mesh: &Mesh, infos: &[FaceRegion], of: &[usize]) -> Vec<Pickable> {
    let mut by_pair: HashMap<(usize, usize), Vec<[f64; 3]>> = HashMap::new();
    let mut edge_tri: HashMap<(Key, Key), usize> = HashMap::new();
    for (i, t) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            edge_tri.insert((key(&t[e]), key(&t[(e + 1) % 3])), i);
        }
    }
    for (i, t) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            let (a, b) = (&t[e], &t[(e + 1) % 3]);
            let Some(&j) = edge_tri.get(&(key(b), key(a))) else {
                continue;
            };
            let (ri, rj) = (of[i], of[j]);
            if ri < rj && infos[ri].a.is_none() && infos[rj].a.is_none() {
                by_pair.entry((ri, rj)).or_default().extend([*a, *b]);
            }
        }
    }
    let mut out = Vec::new();
    for ((ri, rj), pts) in by_pair {
        let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
        let rs: Vec<f64> = pts.iter().map(|p| p[1].dhypot(p[2])).collect();
        let span = |v: &[f64]| {
            v.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                - v.iter().cloned().fold(f64::INFINITY, f64::min)
        };
        // a circle about the axis: one X, one radius
        if span(&xs) > 1e-3 || span(&rs) > 1e-2 {
            continue;
        }
        let x = xs.iter().sum::<f64>() / xs.len() as f64;
        let r = rs.iter().sum::<f64>() / rs.len() as f64;
        let (ia, ib) = (&infos[ri].id, &infos[rj].id);
        out.push(Pickable {
            id: format!("circular_edge:{ia}:{ib}"),
            kind: "circular_edge".into(),
            label: format!("Round edge at X{x:.2}, {:.2} mm", 2.0 * r),
            faces: vec![ia.clone(), ib.clone()],
            similar: format!("circular_edge:{}", r2(r)),
            data: json!({"x": round_um(x), "r": round_um(r)}),
            signature: Value::Null,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Planar faces whose normal is square to the rotary axis but not up at `index_a`: the A
/// index that turns each up, and its distance from the axis.
fn side_faces(mesh: &Mesh, infos: &[FaceRegion], of: &[usize], index_a: f64) -> Vec<Pickable> {
    let mut acc: Vec<([f64; 3], f64, f64, bool)> = vec![([0.0; 3], 0.0, 0.0, true); infos.len()];
    let mut first: Vec<Option<[f64; 3]>> = vec![None; infos.len()];
    for (i, t) in mesh.triangles.iter().enumerate() {
        let a = tri_area(t);
        if a < 1e-12 {
            continue;
        }
        let n = unit_normal(t);
        let e = &mut acc[of[i]];
        match first[of[i]] {
            None => first[of[i]] = Some(n),
            Some(f) => {
                if f[0] * n[0] + f[1] * n[1] + f[2] * n[2] < 0.9999 {
                    e.3 = false; // not planar
                }
            }
        }
        for (acc_k, n_k) in e.0.iter_mut().zip(n) {
            *acc_k += a * n_k;
        }
        e.1 += a;
        e.2 += a * (n[0] * t[0][0] + n[1] * t[0][1] + n[2] * t[0][2]);
    }
    let mut out = Vec::new();
    for (k, (n, area, d, planar)) in acc.into_iter().enumerate() {
        if !planar || area < 1e-6 {
            continue;
        }
        let n = [n[0] / area, n[1] / area, n[2] / area];
        if n[0].abs() > 0.01 {
            continue;
        }
        // the part turned by A about X brings (ny, nz) up: A = atan2(ny, nz)
        let a = n[1].datan2(n[2]).to_degrees();
        let a = (a * 1000.0).round() / 1000.0;
        if ((a - index_a + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.01 {
            continue; // already up: a flat face at this index
        }
        let dist = d / area;
        let id = &infos[k].id;
        out.push(Pickable {
            id: format!("side_face:{id}"),
            kind: "side_face".into(),
            label: format!("Side face at A{a:.1}, {dist:.2} from the axis, {area:.1} mm2"),
            faces: vec![id.clone()],
            similar: format!("side_face:{}", r2(dist)),
            data: json!({"index_a": a, "distance": round_um(dist), "area": area, "x": infos[k].x}),
            signature: Value::Null,
        });
    }
    out
}

fn rotary(infos: &[FaceRegion]) -> Vec<Pickable> {
    infos
        .iter()
        .map(|r| {
            let full = r.a.is_none();
            let (kind, label) = if full && r.shape == "cylinder" {
                (
                    "cylindrical_band",
                    format!(
                        "Cylinder {:.2} mm, X{:.2}..{:.2}",
                        2.0 * r.r[1],
                        r.x[0],
                        r.x[1]
                    ),
                )
            } else if full {
                (
                    "revolved_band",
                    format!(
                        "{} X{:.2}..{:.2}, radius {:.2}..{:.2}",
                        match r.shape.as_str() {
                            "cone" => "Cone",
                            "plane" => "Shoulder",
                            _ => "Revolved surface",
                        },
                        r.x[0],
                        r.x[1],
                        r.r[0],
                        r.r[1]
                    ),
                )
            } else {
                (
                    "surface",
                    format!("Surface X{:.2}..{:.2}, {:.1} mm2", r.x[0], r.x[1], r.area),
                )
            };
            let similar = match kind {
                "cylindrical_band" => format!("cylindrical_band:{}", r2(r.r[1])),
                "revolved_band" => {
                    format!("revolved_band:{}:{}:{}", r.shape, r2(r.r[0]), r2(r.r[1]))
                }
                _ => format!("surface:{}", r.id),
            };
            Pickable {
                id: format!("{kind}:{}", r.id),
                kind: kind.into(),
                label,
                faces: vec![r.id.clone()],
                similar,
                data: json!({
                    "x": [round_um(r.x[0]), round_um(r.x[1])],
                    "r": [round_um(r.r[0]), round_um(r.r[1])],
                    "a": r.a, "shape": r.shape, "area": r.area
                }),
                signature: Value::Null,
            }
        })
        .collect()
}

/// Closed loops of the edges in `edges` (directed, as the triangles run): their vertices.
fn loops(edges: &[(Key, Key)]) -> Vec<Vec<Key>> {
    let mut next: HashMap<Key, Vec<Key>> = HashMap::new();
    for &(a, b) in edges {
        next.entry(a).or_default().push(b);
    }
    let mut used: HashMap<(Key, Key), bool> = HashMap::new();
    let mut out = Vec::new();
    for &(a0, b0) in edges {
        if used.contains_key(&(a0, b0)) {
            continue;
        }
        let mut lp = vec![a0];
        let (mut a, mut b) = (a0, b0);
        used.insert((a, b), true);
        let mut guard = 0;
        while b != a0 && guard < 1_000_000 {
            guard += 1;
            lp.push(b);
            let Some(c) = next
                .get(&b)
                .and_then(|v| v.iter().find(|c| !used.contains_key(&(b, **c))))
            else {
                break;
            };
            a = b;
            b = *c;
            used.insert((a, b), true);
        }
        if b == a0 && lp.len() >= 3 {
            out.push(lp);
        }
    }
    out
}

fn signed_area(p: &[[f64; 2]]) -> f64 {
    let n = p.len();
    (0..n)
        .map(|i| p[i][0] * p[(i + 1) % n][1] - p[(i + 1) % n][0] * p[i][1])
        .sum::<f64>()
        / 2.0
}

fn length(p: &[[f64; 2]]) -> f64 {
    let n = p.len();
    (0..n)
        .map(|i| (p[(i + 1) % n][0] - p[i][0]).dhypot(p[(i + 1) % n][1] - p[i][1]))
        .sum()
}

/// Least-squares circle through XY points: (centre, radius, largest misfit).
fn fit_circle(pts: &[[f64; 2]]) -> Option<([f64; 2], f64, f64)> {
    if pts.len() < 6 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = (
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    );
    let (mut suu, mut svv, mut suv, mut suuu, mut svvv, mut suvv, mut svuu) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in pts {
        let (u, v) = (p[0] - mx, p[1] - my);
        suu += u * u;
        svv += v * v;
        suv += u * v;
        suuu += u * u * u;
        svvv += v * v * v;
        suvv += u * v * v;
        svuu += v * u * u;
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-12 {
        return None;
    }
    let (b1, b2) = (0.5 * (suuu + suvv), 0.5 * (svvv + svuu));
    let uc = (b1 * svv - b2 * suv) / det;
    let vc = (suu * b2 - suv * b1) / det;
    let c = [mx + uc, my + vc];
    let r = (uc * uc + vc * vc + (suu + svv) / n).sqrt();
    let misfit = pts
        .iter()
        .map(|p| ((p[0] - c[0]).dhypot(p[1] - c[1]) - r).abs())
        .fold(0.0, f64::max);
    Some((c, r, misfit))
}

/// Largest gap between the points' angles about `c` (degrees).
fn largest_gap(pts: &[[f64; 2]], c: [f64; 2]) -> f64 {
    let mut a: Vec<f64> = pts
        .iter()
        .map(|p| {
            (p[1] - c[1])
                .datan2(p[0] - c[0])
                .to_degrees()
                .rem_euclid(360.0)
        })
        .collect();
    a.sort_by(|x, y| x.total_cmp(y));
    a.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    if a.len() < 2 {
        return 360.0;
    }
    let mut g = a[0] + 360.0 - a[a.len() - 1];
    for k in 1..a.len() {
        g = g.max(a[k] - a[k - 1]);
    }
    g
}

fn xyz(mesh: &Mesh, infos: Vec<FaceRegion>, of: &[usize]) -> Vec<Pickable> {
    let tris = &mesh.triangles;
    let mut regions: Vec<Region> = infos
        .into_iter()
        .map(|info| Region {
            info,
            tris: Vec::new(),
            area: 0.0,
            nz: [f64::INFINITY, f64::NEG_INFINITY],
            z: [f64::INFINITY, f64::NEG_INFINITY],
        })
        .collect();
    let mut pos: HashMap<Key, [f64; 3]> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        let r = &mut regions[of[i]];
        r.tris.push(i);
        let a = tri_area(t);
        r.area += a;
        if a > 1e-12 {
            let n = unit_normal(t);
            r.nz = [r.nz[0].min(n[2]), r.nz[1].max(n[2])];
        }
        for p in t {
            r.z = [r.z[0].min(p[2]), r.z[1].max(p[2])];
            pos.insert(key(p), *p);
        }
    }
    let zmin = mesh.bounds().map_or(0.0, |(lo, _)| lo[2]);
    // directed edges -> triangle, to find each boundary edge's neighbour across it
    let mut edge_tri: HashMap<(Key, Key), usize> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            edge_tri.insert((key(&t[k]), key(&t[(k + 1) % 3])), i);
        }
    }
    let is_up = |r: &Region| r.nz[0] > FLAT;
    let is_down = |r: &Region| r.nz[1] < -FLAT;
    let is_wall = |r: &Region| r.nz[0] > -WALL && r.nz[1] < WALL;
    let mut out = Vec::new();

    // holes: concave vertical cylinders
    let mut hole_regions: Vec<(usize, [f64; 2], f64)> = Vec::new();
    for (k, r) in regions.iter().enumerate() {
        if !is_wall(r) || r.area < 1e-6 {
            continue;
        }
        let pts: Vec<[f64; 2]> = r
            .tris
            .iter()
            .flat_map(|&i| tris[i].iter().map(|p| [p[0], p[1]]))
            .collect();
        let Some((c, rad, misfit)) = fit_circle(&pts) else {
            continue;
        };
        if misfit > (0.01 * rad).max(0.02) || largest_gap(&pts, c) > 30.0 {
            continue;
        }
        // concave: the walls face the centre
        let inward = r
            .tris
            .iter()
            .map(|&i| {
                let t = &tris[i];
                let n = unit_normal(t);
                let m = [
                    (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                    (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                ];
                let d = [c[0] - m[0], c[1] - m[1]];
                let l = d[0].dhypot(d[1]).max(1e-12);
                tri_area(t) * (n[0] * d[0] + n[1] * d[1]) / l
            })
            .sum::<f64>()
            / r.area;
        if inward < 0.9 {
            continue;
        }
        hole_regions.push((k, c, rad));
    }
    let mut floor_of_hole: HashMap<usize, usize> = HashMap::new();
    for &(k, c, rad) in &hole_regions {
        let r = &regions[k];
        let (top, bottom) = (r.z[1], r.z[0]);
        // a flat floor at the bottom inside the circle: blind
        let floor = regions.iter().position(|f| {
            is_up(f)
                && (f.z[0] - bottom).abs() < 1e-3
                && f.tris.iter().all(|&i| {
                    tris[i]
                        .iter()
                        .all(|p| (p[0] - c[0]).dhypot(p[1] - c[1]) <= rad * 1.01 + 1e-3)
                })
        });
        // closed at the top by a down-facing face: it opens away from the spindle
        let capped = regions.iter().any(|f| {
            is_down(f)
                && (f.z[1] - top).abs() < 1e-3
                && f.tris.iter().all(|&i| {
                    tris[i]
                        .iter()
                        .all(|p| (p[0] - c[0]).dhypot(p[1] - c[1]) <= rad * 1.01 + 1e-3)
                })
        });
        if capped {
            continue;
        }
        let through = floor.is_none() && (bottom - zmin).abs() < 1e-3;
        if let Some(f) = floor {
            floor_of_hole.insert(f, k);
        }
        let depth = top - bottom;
        let d = 2.0 * rad;
        let mut faces = vec![r.info.id.clone()];
        if let Some(f) = floor {
            faces.push(regions[f].info.id.clone());
        }
        out.push(Pickable {
            id: format!("hole:{}", r.info.id),
            kind: "hole".into(),
            label: format!(
                "Hole {d:.2} mm {}, {depth:.2} deep",
                if through { "through" } else { "blind" }
            ),
            faces,
            similar: format!("hole:{}:{}:{}", r2(d), through, r2(depth)),
            data: json!({
                "center": [round_um(c[0]), round_um(c[1])], "diameter": round_um(d), "top": top, "bottom": bottom,
                "depth": depth, "through": through
            }),
            signature: Value::Null,
        });
    }
    let hole_set: std::collections::HashSet<usize> = hole_regions.iter().map(|h| h.0).collect();

    // the part's bottom (the largest down face at the lowest Z): its outline is the part's
    // outline to cut it out along, whatever its top is like
    // (its area, outline, the outline's vertices, its region)
    type Bottom = (f64, Vec<[f64; 2]>, Vec<Key>, usize);
    let mut bottom: Option<Bottom> = None;
    let zmax = mesh.bounds().map_or(0.0, |(_, hi)| hi[2]);
    // flat faces, their edge loops, and pockets
    for (k, r) in regions.iter().enumerate() {
        let up = is_up(r);
        if !(up || is_down(r)) {
            continue;
        }
        let z = if up { r.z[1] } else { r.z[0] };
        let mine: std::collections::HashSet<usize> = r.tris.iter().copied().collect();
        // boundary edges: the triangle across belongs to another region
        let mut bedges = Vec::new();
        let mut walls_up = true;
        let mut wall_regions: Vec<usize> = Vec::new();
        // the top of the wall across each boundary edge, by the edge's start vertex
        let mut wall_top: HashMap<Key, f64> = HashMap::new();
        // ... and, whatever its kind (a wall, a slope, a textured surface), whether the
        // material across rises above the face, and its region
        let mut rises: HashMap<Key, (bool, Option<usize>)> = HashMap::new();
        // (an up face) the bottom of the material falling away across each boundary edge: a
        // contour picked on its outline cuts down the wall to there, not to the face
        let mut falls_to: HashMap<Key, f64> = HashMap::new();
        for &i in &r.tris {
            let t = &tris[i];
            for e in 0..3 {
                let (a, b) = (key(&t[e]), key(&t[(e + 1) % 3]));
                let across = edge_tri.get(&(b, a)).copied();
                if across.is_some_and(|j| mine.contains(&j)) {
                    continue;
                }
                bedges.push((a, b));
                match across {
                    Some(j) => {
                        let wr = of[j];
                        let tz = tris[j]
                            .iter()
                            .map(|p| p[2])
                            .fold(f64::NEG_INFINITY, f64::max);
                        rises.insert(a, (tz > z + 1e-3, Some(wr)));
                        if up && tz <= z + 1e-3 && regions[wr].z[0] < z - 1e-3 {
                            falls_to.insert(a, regions[wr].z[0]);
                        }
                        if !(is_wall(&regions[wr]) && tz > z + 1e-3) {
                            walls_up = false;
                        } else {
                            wall_top.insert(a, regions[wr].z[1]);
                            if !wall_regions.contains(&wr) {
                                wall_regions.push(wr);
                            }
                        }
                    }
                    None => {
                        rises.insert(a, (false, None));
                        walls_up = false;
                    }
                }
            }
        }
        let key_loops = loops(&bedges);
        let xy = |l: &Vec<Key>| -> Vec<[f64; 2]> {
            l.iter()
                .map(|k| {
                    let p = pos[k];
                    [p[0], p[1]]
                })
                .collect()
        };
        let mut order: Vec<usize> = (0..key_loops.len()).collect();
        order.sort_by(|&a, &b| {
            signed_area(&xy(&key_loops[b]))
                .abs()
                .total_cmp(&signed_area(&xy(&key_loops[a])).abs())
        });
        // a pocket's rim: the lowest top of the walls round its outline (not its islands')
        let rim = order.first().map_or(f64::INFINITY, |&o| {
            key_loops[o]
                .iter()
                .filter_map(|k| wall_top.get(k).copied())
                .fold(f64::INFINITY, f64::min)
        });
        // a recessed flat: material rises all round its outline, whether walls, slopes or a
        // textured surface; the openings inside (holes, bosses) are its islands
        let mut outer_regions: Vec<usize> = Vec::new();
        let recessed = order.first().is_some_and(|&o| {
            key_loops[o].iter().all(|k| match rises.get(k) {
                Some(&(true, Some(wr))) => {
                    if !outer_regions.contains(&wr) {
                        outer_regions.push(wr);
                    }
                    true
                }
                _ => false,
            })
        });
        // round a recessed flat: the highest material about it (what is above is cut as air)
        let rim = if walls_up {
            rim
        } else {
            outer_regions
                .iter()
                .map(|&w| regions[w].z[1])
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let mut lps: Vec<Vec<[f64; 2]>> = order.iter().map(|&o| xy(&key_loops[o])).collect();
        // for each outline point: whether the edge from it is open (no higher material across)
        let open: Vec<bool> = order.first().map_or(Vec::new(), |&o| {
            key_loops[o]
                .iter()
                .map(|k| !matches!(rises.get(k), Some(&(true, _))))
                .collect()
        });
        // outer loop first (largest area); for an up face, outer loops run counter-clockwise
        if !up {
            for l in &mut lps {
                l.reverse();
            }
        }
        let id = &r.info.id;
        if !up
            && (z - zmin).abs() < 1e-3
            && !lps.is_empty()
            && bottom.as_ref().is_none_or(|b| r.area > b.0)
        {
            bottom = Some((r.area, lps[0].clone(), key_loops[order[0]].clone(), k));
        }
        if up && !floor_of_hole.contains_key(&k) {
            out.push(Pickable {
                id: format!("flat_face:{id}"),
                kind: "flat_face".into(),
                label: format!("Flat face at Z{z:.2}, {:.1} mm2", r.area),
                faces: vec![id.clone()],
                similar: format!("flat_face:{}", r2(z)),
                data: json!({
                    "z": z, "area": r.area, "loops": lps.len(),
                    "outline": lps.first().map(|l| close(l)), "islands": lps.iter().skip(1).map(|l| close(l)).collect::<Vec<_>>(),
                    "open": open,
                    "cx": round_um(lps.first().map_or(0.0, |l| { let (lo, hi) = bbox(l); (lo[0] + hi[0]) / 2.0 })),
                    "cy": round_um(lps.first().map_or(0.0, |l| { let (lo, hi) = bbox(l); (lo[1] + hi[1]) / 2.0 }))
                }),
                signature: Value::Null,
            });
            let pocket_like = (walls_up && !wall_regions.iter().any(|w| hole_set.contains(w)))
                || (recessed && !outer_regions.iter().any(|w| hole_set.contains(w)));
            if pocket_like && !lps.is_empty() && rim.is_finite() {
                if !walls_up {
                    wall_regions = outer_regions.clone();
                }
                let outline = lps[0].clone();
                let islands: Vec<Vec<[f64; 2]>> = lps[1..].to_vec();
                let depth = rim - z;
                let (lo, hi) = bbox(&outline);
                let mut faces = vec![id.clone()];
                faces.extend(wall_regions.iter().map(|&w| regions[w].info.id.clone()));
                out.push(Pickable {
                    id: format!("pocket:{id}"),
                    kind: "pocket".into(),
                    label: format!(
                        "Pocket {:.2} x {:.2}, {depth:.2} deep{}",
                        hi[0] - lo[0],
                        hi[1] - lo[1],
                        match islands.len() {
                            0 => String::new(),
                            1 => ", 1 island".into(),
                            n => format!(", {n} islands"),
                        }
                    ),
                    faces,
                    similar: format!(
                        "pocket:{}:{}:{}:{}",
                        r2(hi[0] - lo[0]),
                        r2(hi[1] - lo[1]),
                        r2(depth),
                        islands.len()
                    ),
                    data: json!({
                        "floor": z, "top": rim, "depth": depth, "outline": close(&outline),
                        "islands": islands.iter().map(|l| close(l)).collect::<Vec<_>>()
                    }),
                    signature: Value::Null,
                });
            }
        }
        if floor_of_hole.contains_key(&k) {
            continue;
        }
        // edge loops: the outline of each flat face, and the openings in up faces
        for (n, l) in lps.iter().enumerate() {
            if !up && n > 0 {
                continue; // the bottom's holes are the holes' loops
            }
            let outer = n == 0;
            let side = if outer { "outside" } else { "inside" };
            let len = length(l);
            // (an up face) where the wall falling from this loop ends: all the way round it
            // (the highest of its bottoms: a contour cuts down to it and no further)
            let wall_bottom = up
                .then(|| {
                    let ks = &key_loops[order[n]];
                    let b: Vec<f64> = ks.iter().filter_map(|k| falls_to.get(k).copied()).collect();
                    (b.len() == ks.len() && !b.is_empty())
                        .then(|| b.iter().copied().fold(f64::NEG_INFINITY, f64::max))
                })
                .flatten();
            out.push(Pickable {
                id: format!("edge_loop:{id}:{n}"),
                kind: "edge_loop".into(),
                label: format!(
                    "Edge loop at Z{z:.2} ({}, {len:.1} mm)",
                    if outer { "outline" } else { "opening" }
                ),
                faces: vec![id.clone()],
                similar: format!("edge_loop:{}:{}:{}", r2(z), side, r2(len)),
                data: json!({"z": z, "path": close(l), "side": side, "length": len, "up": up, "wall_bottom": wall_bottom}),
                signature: Value::Null,
            });
        }
    }
    // every other region: a surface
    for r in &regions {
        let used = out.iter().any(|p| p.faces.contains(&r.info.id));
        if used || r.area < 1e-6 {
            continue;
        }
        out.push(Pickable {
            id: format!("surface:{}", r.info.id),
            kind: "surface".into(),
            label: format!("Surface Z{:.2}..{:.2}, {:.1} mm2", r.z[0], r.z[1], r.area),
            faces: vec![r.info.id.clone()],
            similar: format!("surface:{}", r.info.id),
            data: json!({"z": r.z, "area": r.area}),
            signature: Value::Null,
        });
    }
    // the part's outline: picked on its bottom or on any outer side face (however many facets,
    // drafted or not), the faces that stand on the bottom's outline
    if let Some((_, lp, keys, k)) = bottom {
        let on: std::collections::HashSet<Key> = keys.into_iter().collect();
        let mut faces = vec![regions[k].info.id.clone()];
        for (w, r) in regions.iter().enumerate() {
            if w == k || is_up(r) || is_down(r) || hole_set.contains(&w) {
                continue;
            }
            if r.tris
                .iter()
                .any(|&i| tris[i].iter().any(|p| on.contains(&key(p))))
            {
                faces.push(r.info.id.clone());
            }
        }
        let path = drop_collinear(&lp);
        let (lo, hi) = bbox(&path);
        out.push(Pickable {
            id: format!("outline:{}", regions[k].info.id),
            kind: "outline".into(),
            label: format!(
                "Part outline {:.2} x {:.2} (cut it out)",
                hi[0] - lo[0],
                hi[1] - lo[1]
            ),
            faces,
            similar: "outline".into(),
            data: json!({"path": close(&path), "bottom": zmin, "top": zmax}),
            signature: Value::Null,
        });
    }
    out.sort_by(|a, b| (kind_order(&a.kind), &a.id).cmp(&(kind_order(&b.kind), &b.id)));
    out
}

/// A loop without its points in a straight line (a faceted outline's many collinear points).
fn drop_collinear(l: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let n = l.len();
    if n < 4 {
        return l.to_vec();
    }
    (0..n)
        .filter(|&i| {
            let (a, b, c) = (l[(i + n - 1) % n], l[i], l[(i + 1) % n]);
            let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
            let len = (c[0] - a[0]).dhypot(c[1] - a[1]).max(1e-9);
            cross.abs() / len > 1e-6
        })
        .map(|i| l[i])
        .collect()
}

fn kind_order(k: &str) -> usize {
    [
        "hole",
        "pocket",
        "flat_face",
        "edge_loop",
        "outline",
        "surface",
    ]
    .iter()
    .position(|x| *x == k)
    .unwrap_or(9)
}

fn bbox(p: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for q in p {
        for k in 0..2 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    (lo, hi)
}

/// The loop with its first point repeated at the end (as the operations take paths).
fn close(l: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut v: Vec<[f64; 2]> = l.iter().map(|p| [round_um(p[0]), round_um(p[1])]).collect();
    if let Some(&f) = v.first() {
        v.push(f);
    }
    v
}

fn round_um(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// A setup's part, as a mesh in the setup's frame, and the frame to recognise it in: the
/// model asked for, else the setup's mesh model, else its first model (a profile is
/// revolved). Vise and indexed setups are Z up (indexed: turned to `index_a`), rotary ones
/// along X. `mesh_of` reads a mesh model's file. Returns the setup id too.
pub fn setup_part(
    p: &crate::project::Project,
    setup: Option<&str>,
    model: Option<&str>,
    index_a: f64,
    mesh_of: &dyn Fn(&crate::job::MeshSource) -> Result<Mesh, String>,
) -> Result<(String, Mesh, Frame), String> {
    use crate::project::{ModelKind, StockDef};
    let s = match setup {
        Some(id) => p
            .setups
            .iter()
            .find(|s| s.id == id)
            .ok_or(format!("no setup {id:?}"))?,
        None => p.setups.first().ok_or("the project has no setups")?,
    };
    let is_mesh = |id: &String| {
        p.models
            .iter()
            .any(|m| &m.id == id && matches!(m.kind, ModelKind::Mesh(_)))
    };
    let model_id = match model {
        Some(m) => m.to_string(),
        None => s
            .part
            .iter()
            .find(|id| is_mesh(id))
            .or(s.part.first())
            .cloned()
            .ok_or("the setup has no part model")?,
    };
    let m = p
        .models
        .iter()
        .find(|m| m.id == model_id)
        .ok_or(format!("no model {model_id:?}"))?;
    let part = match &m.kind {
        ModelKind::Profile(prof) => Mesh::revolve(prof, 360),
        // (a carried part where this setup holds it)
        ModelKind::Mesh(src) => mesh_of(
            &p.model_source_in(s, &model_id)
                .unwrap_or_else(|| src.clone()),
        )?,
    };
    let block = matches!(s.stock, StockDef::Box { .. });
    let kind = s.kind.clone().unwrap_or_else(|| {
        if block {
            "vise".into()
        } else {
            "rotary".into()
        }
    });
    let frame = if kind == "rotary" {
        Frame::Rotary
    } else {
        Frame::Xyz {
            index_a,
            sides: kind == "indexed",
        }
    };
    Ok((s.id.clone(), part, frame))
}

/// The top of a setup's stock in the recognition frame (block: its top; bar: its radius).
pub fn stock_top(p: &crate::project::Project, setup: &str) -> Option<f64> {
    use crate::project::StockDef;
    let s = p.setups.iter().find(|s| s.id == setup)?;
    // a bar lying in a vise: its top is Z0, as a block's
    if !matches!(s.stock, StockDef::Box { .. })
        && (s.kind.as_deref() == Some("vise") || s.holding.kind == "vise")
    {
        return Some(0.0);
    }
    Some(match s.stock {
        StockDef::Box { max, .. } => max[2],
        StockDef::Cylinder { radius, .. } | StockDef::FromPrevious { radius, .. } => radius,
        // (a square, hex or rectangular bar: its corners)
        ref bar => bar.cylinder().radius,
    })
}

// ------------------------------------------------------------------ operations

/// The kinds an operation type takes, in order of preference (empty: picking does not apply).
pub fn accepts(op_type: &str) -> &'static [&'static str] {
    match op_type {
        "drill" | "bore" | "thread_mill" => &["hole"],
        "chamfer" => &["hole", "pocket", "outline", "edge_loop"],
        "pocket" => &["pocket", "flat_face"],
        "contour" => &["edge_loop", "outline"],
        "face" => &["flat_face", "side_face"],
        "raster3d" | "rough3d" | "waterline" => {
            &["surface", "flat_face", "pocket", "hole", "side_face"]
        }
        "rotary_finish" => &["cylindrical_band", "revolved_band", "surface"],
        "rotary_rough" | "rotary_turning" => &["cylindrical_band", "revolved_band", "surface"],
        "helical_knurl" => &["revolved_band", "cylindrical_band"],
        "flank_chamfer" => &["circular_edge"],
        "parting" => &["revolved_band", "cylindrical_band", "circular_edge"],
        "engrave" | "vcarve" => &["flat_face", "side_face", "cylindrical_band"],
        _ => &[],
    }
}

/// The outline (and islands) a picked flat face bounds a facing or a pocket by, for a tool of
/// radius `r`: where higher material borders the face the tool stays a radius inside (the
/// offsets do that); where the face is open (the part's edge, a step down) the outline is moved
/// out by the radius and a little, so the tool runs past the edge and leaves no ridge.
fn face_paths(p: &Pickable, r: Option<f64>) -> Vec<Value> {
    let pts: Vec<[f64; 2]> = serde_json::from_value(p.data["outline"].clone()).unwrap_or_default();
    let open: Vec<bool> = serde_json::from_value(p.data["open"].clone()).unwrap_or_default();
    let mut out = pts.clone();
    // `close` repeats the first point at the end
    let n = if pts.len() > 1 && pts.first() == pts.last() {
        pts.len() - 1
    } else {
        pts.len()
    };
    if let Some(r) = r.filter(|_| open.len() == n && n >= 3 && open.iter().any(|&o| o)) {
        let push = r + 0.1;
        // an up face's outline runs counter-clockwise: outward is to the right of each edge
        let normal = |i: usize| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = dx.dhypot(dy).max(1e-12);
            [dy / l, -dx / l]
        };
        for i in 0..n {
            let prev = (i + n - 1) % n;
            // only between two open edges: a corner where a wall starts stays, so the wall
            // keeps the tool a radius off
            if !(open[prev] && open[i]) {
                continue;
            }
            let mut m = [0.0, 0.0];
            for e in [prev, i] {
                if open[e] {
                    let nn = normal(e);
                    m = [m[0] + nn[0], m[1] + nn[1]];
                }
            }
            let l = m[0].dhypot(m[1]);
            if l > 1e-9 {
                out[i] = [pts[i][0] + push * m[0] / l, pts[i][1] + push * m[1] / l];
            }
        }
        if pts.len() > n {
            out[n] = out[0];
        }
    }
    let mut paths = vec![json!(out)];
    if let Some(is) = p.data["islands"].as_array() {
        paths.extend(is.iter().cloned());
    }
    paths
}

/// How far below the part's bottom a cut-out goes (mm).
pub const BREAKTHROUGH: f64 = 0.2;

/// What a pick needs to know about the setup.
#[derive(Debug, Clone)]
pub struct ApplyContext {
    /// Top of the stock (Z, 3-axis).
    pub stock_top: Option<f64>,
    /// Diameter of the operation's tool (mm).
    pub tool_diameter: Option<f64>,
    /// The part model the face ids belong to.
    pub model: String,
    /// The operation as it is (its text and height place an engraving).
    pub op: Option<Value>,
}

/// The parameters `picks` set on an operation of type `op_type`, and notes for the person
/// (a suggestion, a mismatch). Errors when a pick is of a kind the type does not take.
pub fn apply(
    op_type: &str,
    picks: &[&Pickable],
    cx: &ApplyContext,
) -> Result<(Map<String, Value>, Vec<String>), String> {
    let ok = accepts(op_type);
    if picks.is_empty() {
        return Err("nothing picked".into());
    }
    for p in picks {
        if !ok.contains(&p.kind.as_str()) {
            return Err(format!(
                "{op_type} does not take a {} (it takes {})",
                p.kind.replace('_', " "),
                if ok.is_empty() {
                    "no picks".to_string()
                } else {
                    ok.join(", ").replace('_', " ")
                }
            ));
        }
    }
    // several faces / pockets / outlines in one operation: each as one alone, as regions done
    // in turn (each with its own outline and depth); the first also fills the fields
    if picks.len() > 1
        && matches!(op_type, "face" | "pocket" | "contour")
        && picks.iter().all(|p| p.kind != "side_face")
    {
        let mut regions = Vec::new();
        let mut first = None;
        let mut notes: Vec<String> = Vec::new();
        for p in picks {
            let (v, n) = apply(op_type, &[*p], cx)?;
            let keep = |k: &str| v.get(k).cloned();
            let mut r = Map::new();
            for k in ["paths", "path", "side", "top", "depth"] {
                if let Some(x) = keep(k) {
                    r.insert(k.into(), x);
                }
            }
            regions.push(Value::Object(r));
            for x in n {
                if !notes.contains(&x) {
                    notes.push(x);
                }
            }
            first.get_or_insert(v);
        }
        let mut v = first.unwrap_or_default();
        v.insert("regions".into(), Value::Array(regions));
        notes.push(format!(
            "{} picked: each is worked in turn (nearest next), to its own depth",
            picks.len()
        ));
        return Ok((v, notes));
    }
    let f = |p: &Pickable, k: &str| p.data[k].as_f64().unwrap_or(0.0);
    let mut v = Map::new();
    let mut notes = Vec::new();
    if matches!(op_type, "face" | "pocket" | "contour") {
        // (one feature: the plain fields, no regions)
        v.insert("regions".into(), json!([]));
    }
    match op_type {
        "drill" => {
            let points: Vec<Value> = picks.iter().map(|p| p.data["center"].clone()).collect();
            let top = picks
                .iter()
                .map(|p| f(p, "top"))
                .fold(f64::NEG_INFINITY, f64::max);
            // through holes: 0.5 mm past the bottom
            let depth = picks
                .iter()
                .map(|p| top - f(p, "bottom") + if p.data["through"] == true { 0.5 } else { 0.0 })
                .fold(0.0, f64::max);
            let d = picks.iter().map(|p| f(p, "diameter")).fold(0.0, f64::max);
            v.insert("points".into(), Value::Array(points));
            v.insert("top".into(), json!(round_um(top)));
            v.insert("depth".into(), json!(round_um(depth)));
            // a peck of about one diameter, at least 0.5 mm
            v.insert("peck".into(), json!(round_um(d.max(0.5))));
            if let Some(td) = cx.tool_diameter {
                let wrong: Vec<f64> = picks
                    .iter()
                    .map(|p| f(p, "diameter"))
                    .filter(|hd| (hd - td).abs() > 0.05)
                    .collect();
                if !wrong.is_empty() {
                    notes.push(format!(
                        "the {td:.2} mm tool drills {td:.2} mm holes; {} {} (use Bore instead)",
                        if wrong.len() == 1 {
                            "this hole is"
                        } else {
                            "these holes are"
                        },
                        wrong
                            .iter()
                            .map(|d| format!("{d:.2} mm"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            if picks.iter().any(|p| (f(p, "top") - top).abs() > 1e-3) {
                notes.push(
                    "the holes start at different heights: all are drilled from the highest".into(),
                );
            }
        }
        "bore" => {
            let points: Vec<Value> = picks.iter().map(|p| p.data["center"].clone()).collect();
            let top = picks
                .iter()
                .map(|p| f(p, "top"))
                .fold(f64::NEG_INFINITY, f64::max);
            let depth = picks
                .iter()
                .map(|p| top - f(p, "bottom") + if p.data["through"] == true { 0.5 } else { 0.0 })
                .fold(0.0, f64::max);
            let d = f(picks[0], "diameter");
            if picks.iter().any(|p| (f(p, "diameter") - d).abs() > 1e-3) {
                notes.push(format!(
                    "the holes are of different sizes: all are bored at {d:.2} mm (the first)"
                ));
            }
            if let Some(td) = cx.tool_diameter {
                if td >= d - 0.1 {
                    notes.push(format!("the {td:.2} mm tool is too big to bore a {d:.2} mm hole: use a smaller one (or drill it)"));
                }
            }
            v.insert("points".into(), Value::Array(points));
            v.insert("diameter".into(), json!(round_um(d)));
            v.insert("top".into(), json!(round_um(top)));
            v.insert("depth".into(), json!(round_um(depth)));
        }
        "pocket" => {
            let p = picks[0];
            if picks.len() > 1 {
                notes.push("a pocket operation clears one pocket: the first one picked".into());
            }
            let paths = if p.kind == "flat_face" {
                face_paths(p, cx.tool_diameter.map(|d| d / 2.0))
            } else {
                let mut paths = vec![p.data["outline"].clone()];
                if let Some(is) = p.data["islands"].as_array() {
                    paths.extend(is.iter().cloned());
                }
                paths
            };
            v.insert("paths".into(), Value::Array(paths));
            if p.kind == "flat_face" {
                // a flat face as the floor: from the stock top (or the face) down to it
                let top = cx.stock_top.unwrap_or(f(p, "z"));
                v.insert("top".into(), json!(round_um(top)));
                v.insert("depth".into(), json!(round_um((top - f(p, "z")).max(0.01))));
                if f(p, "loops") > 1.0 {
                    notes.push("the openings in the face (holes, bosses) are left as islands: drill the holes with their own operation".into());
                }
            } else {
                v.insert("top".into(), json!(round_um(f(p, "top"))));
                v.insert("depth".into(), json!(round_um(f(p, "depth"))));
            }
        }
        "contour" if picks[0].kind == "outline" => {
            // cut the part out: outside its outline, from the stock top to just below the part
            let p = picks[0];
            let top = cx.stock_top.unwrap_or(f(p, "top"));
            v.insert("path".into(), p.data["path"].clone());
            v.insert("side".into(), json!("outside"));
            v.insert("top".into(), json!(round_um(top)));
            v.insert(
                "depth".into(),
                json!(round_um(top - f(p, "bottom") + BREAKTHROUGH)),
            );
            notes.push(format!("cut out {BREAKTHROUGH} mm below the part's bottom to break through: hold the part (tabs, or the stock below it) so it does not come loose"));
        }
        "contour" => {
            let p = picks[0];
            if picks.len() > 1 {
                notes.push("a contour follows one loop: the first one picked".into());
            }
            let top = cx.stock_top.unwrap_or(f(p, "z"));
            // (Mar's boss: its outline picked, the contour cut 0.5 deep, the stock's margin)
            // down the wall that falls from the loop, to its foot; else to the loop
            let floor = p.data["wall_bottom"].as_f64().unwrap_or(f(p, "z"));
            v.insert("path".into(), p.data["path"].clone());
            v.insert("side".into(), p.data["side"].clone());
            v.insert("top".into(), json!(round_um(top)));
            v.insert("depth".into(), json!(round_um((top - floor).max(0.01))));
        }
        "face" => {
            let p = picks[0];
            let top = cx.stock_top.unwrap_or(f(p, "z"));
            if p.kind == "side_face" {
                // indexed: turn the face up, take the bar down to it
                v.insert("index_a".into(), p.data["index_a"].clone());
                v.insert(
                    "depth".into(),
                    json!(round_um((top - f(p, "distance")).max(0.01))),
                );
            } else {
                v.insert("depth".into(), json!(round_um((top - f(p, "z")).max(0.01))));
                // bounded by the face: never outside its outline (the tool stays a radius in
                // from it), its openings left as islands
                v.insert(
                    "paths".into(),
                    Value::Array(face_paths(p, cx.tool_diameter.map(|d| d / 2.0))),
                );
                if f(p, "loops") > 1.0 {
                    notes.push("the openings in the face (holes, bosses) are left as islands: drill the holes with their own operation".into());
                }
                if picks.len() > 1 {
                    notes.push("facing is bounded by one face: the first one picked".into());
                }
            }
        }
        "chamfer" => {
            // holes by their size; one outline (a pocket's edge, the part's outline, a loop)
            let holes: Vec<Value> = picks
                .iter()
                .filter(|p| p.kind == "hole")
                .map(|p| {
                    let c = &p.data["center"];
                    json!([c[0], c[1], round_um(f(p, "diameter"))])
                })
                .collect();
            let loops: Vec<&&Pickable> = picks.iter().filter(|p| p.kind != "hole").collect();
            if loops.len() > 1 {
                notes.push(
                    "a chamfer follows one outline (and any holes): the first one picked".into(),
                );
            }
            let (paths, side) = match loops.first() {
                Some(p) if p.kind == "pocket" => (json!([p.data["outline"]]), json!("inside")),
                Some(p) if p.kind == "outline" => (json!([p.data["path"]]), json!("outside")),
                Some(p) => (json!([p.data["path"]]), p.data["side"].clone()),
                None => (json!([]), json!("outside")),
            };
            let top = picks
                .iter()
                .map(|p| {
                    if p.kind == "edge_loop" {
                        f(p, "z")
                    } else {
                        f(p, "top")
                    }
                })
                .fold(f64::NEG_INFINITY, f64::max);
            v.insert("paths".into(), paths);
            v.insert("side".into(), side);
            v.insert("holes".into(), Value::Array(holes));
            v.insert("top".into(), json!(round_um(top)));
        }
        "thread_mill" => {
            // a metric thread whose tap drill is the hole (its size less the pitch)
            const METRIC: [(f64, f64); 10] = [
                (3.0, 0.5),
                (4.0, 0.7),
                (5.0, 0.8),
                (6.0, 1.0),
                (8.0, 1.25),
                (10.0, 1.5),
                (12.0, 1.75),
                (16.0, 2.0),
                (20.0, 2.5),
                (24.0, 3.0),
            ];
            let d = f(picks[0], "diameter");
            let (m, pitch) = METRIC
                .iter()
                .copied()
                .min_by(|a, b| ((a.0 - a.1) - d).abs().total_cmp(&((b.0 - b.1) - d).abs()))
                .expect("a list");
            let points: Vec<Value> = picks.iter().map(|p| p.data["center"].clone()).collect();
            let top = picks
                .iter()
                .map(|p| f(p, "top"))
                .fold(f64::NEG_INFINITY, f64::max);
            // the whole hole if through, else a pitch short of its bottom
            let depth = picks
                .iter()
                .map(|p| {
                    top - f(p, "bottom")
                        - if p.data["through"] == true {
                            0.0
                        } else {
                            pitch
                        }
                })
                .fold(f64::INFINITY, f64::min)
                .max(pitch);
            v.insert("points".into(), Value::Array(points));
            v.insert("diameter".into(), json!(m));
            v.insert("pitch".into(), json!(pitch));
            v.insert("internal".into(), json!(true));
            v.insert("top".into(), json!(round_um(top)));
            v.insert("depth".into(), json!(round_um(depth)));
            notes.push(format!(
                "M{m} x {pitch} taken for the {d:.2} mm hole (its tap drill): change the size and pitch if it is another thread"
            ));
        }
        "rotary_turning" => {
            let lo = picks
                .iter()
                .map(|p| p.data["x"][0].as_f64().unwrap_or(0.0))
                .fold(f64::INFINITY, f64::min);
            let hi = picks
                .iter()
                .map(|p| p.data["x"][1].as_f64().unwrap_or(0.0))
                .fold(f64::NEG_INFINITY, f64::max);
            v.insert("x_range".into(), json!([round_um(lo), round_um(hi)]));
        }
        "helical_knurl" => {
            // the knurled body: back (X min) and front (X max), each with its radius
            let p = picks[0];
            if picks.len() > 1 {
                notes.push("a knurl covers one body: the first one picked".into());
            }
            let x = [
                p.data["x"][0].as_f64().unwrap_or(0.0),
                p.data["x"][1].as_f64().unwrap_or(0.0),
            ];
            let r = match p.data["r_ends"].as_array() {
                Some(e) => [e[0].as_f64().unwrap_or(0.0), e[1].as_f64().unwrap_or(0.0)],
                None => [p.data["r"][1].as_f64().unwrap_or(0.0); 2],
            };
            v.insert("body_back".into(), json!([round_um(x[0]), r[0]]));
            v.insert("body_front".into(), json!([round_um(x[1]), r[1]]));
        }
        "parting" => {
            // snapped to the picked edge, or to the back (X min) of a picked band, where the
            // material is as high as the band's radius there
            let p = picks[0];
            if picks.len() > 1 {
                notes.push("one parting cut: at the first one picked".into());
            }
            let (x, r) = if p.kind == "circular_edge" {
                (f(p, "x"), f(p, "r"))
            } else {
                let r = match p.data["r_ends"].as_array() {
                    Some(e) => e[0].as_f64().unwrap_or(0.0),
                    None => p.data["r"][1].as_f64().unwrap_or(0.0),
                };
                (p.data["x"][0].as_f64().unwrap_or(0.0), r)
            };
            v.insert("x".into(), json!(round_um(x)));
            if r > 0.0 {
                v.insert("top".into(), json!(round_um(r)));
            }
        }
        "flank_chamfer" => {
            let p = picks[0];
            v.insert("edge".into(), json!([f(p, "x"), f(p, "r")]));
        }
        "engrave" | "vcarve" => {
            // the surface the text is cut into, and the text centred on the picked face
            let p = picks[0];
            let op = cx.op.clone().unwrap_or(Value::Null);
            let h = op["text_height"].as_f64().unwrap_or(5.0);
            let text = op["text"].as_str().unwrap_or("").to_string();
            // the text's width at its height (the same font the operation cuts)
            let width = if text.is_empty() {
                0.0
            } else {
                crate::engrave::text_paths(&text, h, [0.0, 0.0], 0.0)
                    .ok()
                    .map(|paths| {
                        let xs = paths.iter().flatten().map(|q| q[0]);
                        let (lo, hi) = xs.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                            (a.min(x), b.max(x))
                        });
                        if hi > lo {
                            hi - lo
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0)
            };
            let (top, centre) = match p.kind.as_str() {
                // wrapped round the bar: Y is arc length from the top at A0
                "cylindrical_band" => {
                    let x = &p.data["x"];
                    (
                        p.data["r"][1].as_f64().unwrap_or(0.0),
                        [
                            (x[0].as_f64().unwrap_or(0.0) + x[1].as_f64().unwrap_or(0.0)) / 2.0,
                            0.0,
                        ],
                    )
                }
                "side_face" => {
                    v.insert("index_a".into(), p.data["index_a"].clone());
                    let x = &p.data["x"];
                    (
                        f(p, "distance"),
                        [
                            (x[0].as_f64().unwrap_or(0.0) + x[1].as_f64().unwrap_or(0.0)) / 2.0,
                            0.0,
                        ],
                    )
                }
                _ => (f(p, "z"), [f(p, "cx"), f(p, "cy")]),
            };
            v.insert("top".into(), json!(round_um(top)));
            v.insert(
                "text_at".into(),
                json!([
                    round_um(centre[0] - width / 2.0),
                    round_um(centre[1] - h / 2.0)
                ]),
            );
            if text.is_empty() {
                notes.push(
                    "the placement is for text: set the text (paths keep their own places)".into(),
                );
            }
        }
        _ => {
            // finishing: the faces, and for rotary roughing the X span they cover
            let mut ids: Vec<String> = Vec::new();
            for p in picks {
                for id in &p.faces {
                    if !ids.contains(id) {
                        ids.push(id.clone());
                    }
                }
            }
            if op_type == "rotary_rough" {
                let lo = picks
                    .iter()
                    .map(|p| p.data["x"][0].as_f64().unwrap_or(0.0))
                    .fold(f64::INFINITY, f64::min);
                let hi = picks
                    .iter()
                    .map(|p| p.data["x"][1].as_f64().unwrap_or(0.0))
                    .fold(f64::NEG_INFINITY, f64::max);
                v.insert("x_range".into(), json!([round_um(lo), round_um(hi)]));
            } else {
                v.insert("faces".into(), json!(ids));
                let _ = &cx.model;
            }
        }
    }
    Ok((v, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    #[test]
    fn a_circle_fits_exactly() {
        let pts: Vec<[f64; 2]> = (0..32)
            .map(|k| {
                let t = k as f64 * std::f64::consts::TAU / 32.0;
                [3.0 + 2.5 * t.dcos(), -1.0 + 2.5 * t.dsin()]
            })
            .collect();
        let (c, r, misfit) = fit_circle(&pts).unwrap();
        assert!(
            (c[0] - 3.0).abs() < 1e-9
                && (c[1] + 1.0).abs() < 1e-9
                && (r - 2.5).abs() < 1e-9
                && misfit < 1e-9
        );
        assert!(largest_gap(&pts, c) < 12.0);
    }
}
