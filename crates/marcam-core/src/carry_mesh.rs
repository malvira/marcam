//! The cut result of a setup as a closed mesh: the hand-off of a part to any next setup, in
//! any orientation (Mar: "take the result of the simulation at that point, make a new mesh
//! with it and then have it be the new model in the next setup"; "I can clamp round stock in
//! any orientation").
//!
//! A block's simulated cells (each one's top and bottom) or a bar's (its radius at each X and
//! angle) are made a closed mesh, turned and moved into the next setup, and sampled again in
//! that setup's own kind: a block's columns cast straight down (each column's highest and
//! lowest point: where a column crosses a gap, a pocket now facing sideways, the gap counts
//! as material, which errs toward cutting air, never toward a crash), or a bar's rays cast
//! out from the axis (each the farthest point: a gap the same).

use crate::det::Det;
use crate::mesh::Triangle;
use crate::sim::{Grid, RadialMap};
use crate::Mesh;

type V3 = [f64; 3];

/// A rotation matrix (rows) from degrees about X, then Y, then Z; quarter turns exact.
pub fn rotation(deg: V3) -> [[f64; 3]; 3] {
    let sc = |d: f64| -> (f64, f64) {
        let q = d / 90.0;
        if q.fract() == 0.0 {
            match (q as i64).rem_euclid(4) {
                0 => (0.0, 1.0),
                1 => (1.0, 0.0),
                2 => (0.0, -1.0),
                _ => (-1.0, 0.0),
            }
        } else {
            d.to_radians().dsin_cos()
        }
    };
    let (sx, cx) = sc(deg[0]);
    let (sy, cy) = sc(deg[1]);
    let (sz, cz) = sc(deg[2]);
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mul(rz, mul(ry, rx))
}

pub fn mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut c = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    c
}

pub fn apply(m: [[f64; 3]; 3], p: V3) -> V3 {
    [0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2])
}

/// Degrees about X, then Y, then Z for a rotation matrix (the inverse of [`rotation`]); values
/// within a ten-thousandth of a degree of a quarter turn made exact.
pub fn euler(m: [[f64; 3]; 3]) -> V3 {
    // m = Rz Ry Rx: m[2][0] = -sin y
    let sy = (-m[2][0]).clamp(-1.0, 1.0);
    let (x, y, z) = if sy.abs() < 1.0 - 1e-12 {
        let y = sy.dasin();
        let x = m[2][1].datan2(m[2][2]);
        let z = m[1][0].datan2(m[0][0]);
        (x, y, z)
    } else {
        // gimbal: Y a quarter turn; Z taken as none
        let y = if sy > 0.0 {
            std::f64::consts::FRAC_PI_2
        } else {
            -std::f64::consts::FRAC_PI_2
        };
        let x = if sy > 0.0 {
            m[0][1].datan2(m[1][1])
        } else {
            (-m[0][1]).datan2(m[1][1])
        };
        (x, y, 0.0)
    };
    [x, y, z].map(|r| {
        let d = r.to_degrees();
        let q = (d / 90.0).round() * 90.0;
        let v = if (d - q).abs() < 1e-4 { q } else { d };
        if v == -180.0 {
            180.0
        } else if v == 0.0 {
            0.0
        } else {
            v
        }
    })
}

fn quad(out: &mut Vec<Triangle>, a: V3, b: V3, c: V3, d: V3) {
    out.push([a, b, c]);
    out.push([a, c, d]);
}

/// A block's cells as a closed mesh: each cell with material (`top > bot`) a column from its
/// bottom to its top (cell `(i, j)` from `x0 + i cell` to `x0 + (i + 1) cell`); the faces
/// between columns only where their heights differ; runs of cells along X with the same top
/// (or bottom) as one face.
pub fn block_mesh(
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    top: &[f64],
    bot: &[f64],
) -> Mesh {
    let at = |i: usize, j: usize| -> Option<(f64, f64)> {
        let k = j * nx + i;
        (top[k] > bot[k] + 1e-9).then_some((bot[k], top[k]))
    };
    let xe = |i: usize| x0 + i as f64 * cell;
    let ye = |j: usize| y0 + j as f64 * cell;
    let mut t = Vec::new();
    // the tops and the bottoms, in runs along X
    for j in 0..ny {
        let mut i = 0;
        while i < nx {
            let Some((b0, t0)) = at(i, j) else {
                i += 1;
                continue;
            };
            let mut e = i + 1;
            while e < nx && at(e, j).is_some_and(|(_, tt)| tt == t0) {
                e += 1;
            }
            let (xa, xb, ya, yb) = (xe(i), xe(e), ye(j), ye(j + 1));
            quad(
                &mut t,
                [xa, ya, t0],
                [xb, ya, t0],
                [xb, yb, t0],
                [xa, yb, t0],
            );
            let _ = b0;
            i = e;
        }
        let mut i = 0;
        while i < nx {
            let Some((b0, _)) = at(i, j) else {
                i += 1;
                continue;
            };
            let mut e = i + 1;
            while e < nx && at(e, j).is_some_and(|(bb, _)| bb == b0) {
                e += 1;
            }
            let (xa, xb, ya, yb) = (xe(i), xe(e), ye(j), ye(j + 1));
            quad(
                &mut t,
                [xa, ya, b0],
                [xa, yb, b0],
                [xb, yb, b0],
                [xb, ya, b0],
            );
            i = e;
        }
    }
    // between neighbours (and at the edges): where one has material the other has not
    let span = |c: Option<(f64, f64)>| c.unwrap_or((0.0, 0.0));
    let walls = |a: Option<(f64, f64)>, b: Option<(f64, f64)>| -> Vec<(f64, f64, bool)> {
        // the parts of a's interval not in b's (facing +), and of b's not in a's (facing -)
        let mut out = Vec::new();
        let (ab, at_) = span(a);
        let (bb, bt) = span(b);
        let ha = a.is_some();
        let hb = b.is_some();
        let sub = |lo: f64, hi: f64, olo: f64, ohi: f64, other: bool| -> Vec<(f64, f64)> {
            if !other || ohi <= lo || olo >= hi {
                return vec![(lo, hi)];
            }
            let mut v = Vec::new();
            if olo > lo {
                v.push((lo, olo.min(hi)));
            }
            if ohi < hi {
                v.push((ohi.max(lo), hi));
            }
            v
        };
        if ha {
            for (lo, hi) in sub(ab, at_, bb, bt, hb) {
                if hi > lo + 1e-9 {
                    out.push((lo, hi, true));
                }
            }
        }
        if hb {
            for (lo, hi) in sub(bb, bt, ab, at_, ha) {
                if hi > lo + 1e-9 {
                    out.push((lo, hi, false));
                }
            }
        }
        out
    };
    for j in 0..ny {
        for i in 0..=nx {
            let a = if i > 0 { at(i - 1, j) } else { None };
            let b = if i < nx { at(i, j) } else { None };
            let x = xe(i);
            let (ya, yb) = (ye(j), ye(j + 1));
            for (lo, hi, plus) in walls(a, b) {
                // a's wall faces +X, b's -X
                if plus {
                    quad(&mut t, [x, ya, lo], [x, yb, lo], [x, yb, hi], [x, ya, hi]);
                } else {
                    quad(&mut t, [x, ya, lo], [x, ya, hi], [x, yb, hi], [x, yb, lo]);
                }
            }
        }
    }
    for i in 0..nx {
        for j in 0..=ny {
            let a = if j > 0 { at(i, j - 1) } else { None };
            let b = if j < ny { at(i, j) } else { None };
            let y = ye(j);
            let (xa, xb) = (xe(i), xe(i + 1));
            for (lo, hi, plus) in walls(a, b) {
                if plus {
                    quad(&mut t, [xa, y, lo], [xa, y, hi], [xb, y, hi], [xb, y, lo]);
                } else {
                    quad(&mut t, [xa, y, lo], [xb, y, lo], [xb, y, hi], [xa, y, hi]);
                }
            }
        }
    }
    Mesh {
        triangles: t,
        faces: Vec::new(),
    }
}

/// A bar's cells as a closed mesh: each cell (X from `x0 + i dx`, angle round from `j` /
/// `na` of a turn) a wedge from the axis out to its radius; the faces between wedges only
/// where their radii differ; the ends closed. Angles as the simulation has them: from +Y
/// towards +Z.
pub fn bar_mesh(map: &RadialMap) -> Mesh {
    let g = map.grid;
    let (nx, na) = (g.nx, g.na);
    // (each cell's face a flat chord between its edges' angles: its corners that far out, so
    // the ray at the cell's middle angle meets it at the cell's radius exactly)
    let k = 1.0 / (std::f64::consts::PI / na as f64).dcos();
    let r = |i: usize, j: usize| map.r[i * na + j] as f64 * k;
    let ang = |j: usize| std::f64::consts::TAU * j as f64 / na as f64;
    let pt = |x: f64, rr: f64, a: f64| -> V3 {
        let (s, c) = a.dsin_cos();
        [x, rr * c, rr * s]
    };
    let xe = |i: usize| g.x0 + i as f64 * g.dx;
    let mut t = Vec::new();
    for j in 0..na {
        let (a0, a1) = (ang(j), ang(j + 1));
        // the outer surface, in runs along X
        let mut i = 0;
        while i < nx {
            let rr = r(i, j);
            let mut e = i + 1;
            while e < nx && r(e, j) == rr {
                e += 1;
            }
            if rr > 0.0 {
                let (xa, xb) = (xe(i), xe(e));
                quad(
                    &mut t,
                    pt(xa, rr, a0),
                    pt(xb, rr, a0),
                    pt(xb, rr, a1),
                    pt(xa, rr, a1),
                );
            }
            i = e;
        }
        // between X neighbours (and the ends): the ring between their radii
        for i in 0..=nx {
            let ra = if i > 0 { r(i - 1, j) } else { 0.0 };
            let rb = if i < nx { r(i, j) } else { 0.0 };
            if (ra - rb).abs() < 1e-9 {
                continue;
            }
            let x = xe(i);
            let (lo, hi) = (ra.min(rb), ra.max(rb));
            // facing +X where the one before is bigger
            if ra > rb {
                quad(
                    &mut t,
                    pt(x, lo, a0),
                    pt(x, hi, a0),
                    pt(x, hi, a1),
                    pt(x, lo, a1),
                );
            } else {
                quad(
                    &mut t,
                    pt(x, lo, a0),
                    pt(x, lo, a1),
                    pt(x, hi, a1),
                    pt(x, hi, a0),
                );
            }
        }
    }
    // between angular neighbours: the radial wall between their radii
    for i in 0..nx {
        let (xa, xb) = (xe(i), xe(i + 1));
        for j in 0..na {
            let (ra, rb) = (r(i, j), r(i, (j + 1) % na));
            if (ra - rb).abs() < 1e-9 {
                continue;
            }
            let a = ang(j + 1);
            let (lo, hi) = (ra.min(rb), ra.max(rb));
            if ra > rb {
                quad(
                    &mut t,
                    pt(xa, lo, a),
                    pt(xb, lo, a),
                    pt(xb, hi, a),
                    pt(xa, hi, a),
                );
            } else {
                quad(
                    &mut t,
                    pt(xa, lo, a),
                    pt(xa, hi, a),
                    pt(xb, hi, a),
                    pt(xb, lo, a),
                );
            }
        }
    }
    // (a wedge's inner edge is the axis: zero-area there, nothing to close)
    Mesh {
        triangles: t,
        faces: Vec::new(),
    }
}

/// `mesh` turned by `m` then moved by `shift`.
pub fn transform(mesh: &Mesh, m: [[f64; 3]; 3], shift: V3) -> Mesh {
    Mesh {
        triangles: mesh
            .triangles
            .iter()
            .map(|tr| {
                tr.map(|p| {
                    let q = apply(m, p);
                    [q[0] + shift[0], q[1] + shift[1], q[2] + shift[2]]
                })
            })
            .collect(),
        faces: mesh.faces.clone(),
    }
}

/// Where the line through `o` along `d` meets triangle `tr` (its parameter), if it does.
fn hit(o: V3, d: V3, tr: &Triangle) -> Option<f64> {
    let sub = |a: V3, b: V3| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: V3, b: V3| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let (e1, e2) = (sub(tr[1], tr[0]), sub(tr[2], tr[0]));
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, tr[0]);
    let u = dot(s, p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// The mesh as a block of `cell` columns over its own extent (X and Y): each column's
/// highest and lowest point (a gap between counted as material). Returns (x0, y0, nx, ny,
/// tops, bottoms); a column the mesh misses has its top at its bottom.
pub fn sample_block(mesh: &Mesh, cell: f64) -> (f64, f64, usize, usize, Vec<f64>, Vec<f64>) {
    let (lo, hi) = mesh.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    let nx = (((hi[0] - lo[0]) / cell).ceil() as usize).max(1);
    let ny = (((hi[1] - lo[1]) / cell).ceil() as usize).max(1);
    let (x0, y0) = (lo[0], lo[1]);
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); nx * ny];
    for (k, tr) in mesh.triangles.iter().enumerate() {
        let (mut a, mut b) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in tr {
            for q in 0..2 {
                a[q] = a[q].min(p[q]);
                b[q] = b[q].max(p[q]);
            }
        }
        // the columns whose centres the triangle's box covers
        let i0 = ((a[0] - x0) / cell - 0.5).ceil().max(0.0) as usize;
        let i1 = (((b[0] - x0) / cell - 0.5).floor() as i64).min(nx as i64 - 1);
        let j0 = ((a[1] - y0) / cell - 0.5).ceil().max(0.0) as usize;
        let j1 = (((b[1] - y0) / cell - 0.5).floor() as i64).min(ny as i64 - 1);
        if i1 < i0 as i64 || j1 < j0 as i64 {
            continue;
        }
        for j in j0..=j1 as usize {
            for i in i0..=i1 as usize {
                bins[j * nx + i].push(k as u32);
            }
        }
    }
    let z0 = lo[2] - 1.0;
    let mut top = vec![lo[2]; nx * ny];
    let mut bot = vec![lo[2]; nx * ny];
    for j in 0..ny {
        for i in 0..nx {
            let o = [
                x0 + (i as f64 + 0.5) * cell,
                y0 + (j as f64 + 0.5) * cell,
                z0,
            ];
            let (mut a, mut b) = (f64::INFINITY, f64::NEG_INFINITY);
            for &k in &bins[j * nx + i] {
                if let Some(s) = hit(o, [0.0, 0.0, 1.0], &mesh.triangles[k as usize]) {
                    a = a.min(z0 + s);
                    b = b.max(z0 + s);
                }
            }
            if a <= b {
                top[j * nx + i] = b;
                bot[j * nx + i] = a;
            }
        }
    }
    (x0, y0, nx, ny, top, bot)
}

/// The mesh as a bar on `grid` (its axis the X axis): each cell's ray out from the axis, at
/// its X and angle, to the mesh's farthest point (a gap between counted as material); none
/// where the ray misses it.
pub fn sample_bar(mesh: &Mesh, grid: Grid) -> RadialMap {
    let (nx, na) = (grid.nx, grid.na);
    let tau = std::f64::consts::TAU;
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); nx * na];
    for (k, tr) in mesh.triangles.iter().enumerate() {
        let (xa, xb) = (
            tr.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
            tr.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
        );
        let i0 = ((xa - grid.x0) / grid.dx - 0.5).ceil().max(0.0) as usize;
        let i1 = (((xb - grid.x0) / grid.dx - 0.5).floor() as i64).min(nx as i64 - 1);
        if i1 < i0 as i64 {
            continue;
        }
        // the angles it spans round the axis (all of them when it is near the axis or wraps
        // more than half way)
        let angs: Vec<f64> = tr
            .iter()
            .map(|p| p[2].datan2(p[1]).rem_euclid(tau))
            .collect();
        let near = tr.iter().any(|p| p[1].hypot(p[2]) < 1e-6);
        let mut s = angs.clone();
        s.sort_by(f64::total_cmp);
        // the smallest arc holding the three: the largest gap left out
        let gaps = [s[1] - s[0], s[2] - s[1], s[0] + tau - s[2]];
        let (g, at) = gaps
            .iter()
            .enumerate()
            .map(|(i, g)| (*g, i))
            .fold((-1.0, 0), |m, x| if x.0 > m.0 { x } else { m });
        let (from, len) = (s[(at + 1) % 3], tau - g);
        let js: Vec<usize> = if near || len > std::f64::consts::PI {
            (0..na).collect()
        } else {
            let j0 = (from / tau * na as f64 - 0.5).ceil() as i64;
            let j1 = ((from + len) / tau * na as f64 - 0.5).floor() as i64;
            (j0..=j1)
                .map(|j| j.rem_euclid(na as i64) as usize)
                .collect()
        };
        for i in i0..=i1 as usize {
            for &j in &js {
                bins[i * na + j].push(k as u32);
            }
        }
    }
    let mut map = RadialMap::filled(grid, 0.0);
    for i in 0..nx {
        let x = grid.x(i);
        for j in 0..na {
            let (s, c) = grid.phi(j).dsin_cos();
            let mut best = 0.0f64;
            for &k in &bins[i * na + j] {
                if let Some(t) = hit([x, 0.0, 0.0], [0.0, c, s], &mesh.triangles[k as usize]) {
                    best = best.max(t);
                }
            }
            map.r[i * na + j] = best as f32;
        }
    }
    map.top = map.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64;
    map
}

/// A mesh written as binary STL (for checking it: the setup's details offer it).
pub fn to_stl(mesh: &Mesh, header: &str) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    for (k, b) in header.bytes().take(80).enumerate() {
        out[k] = b;
    }
    out.extend((mesh.triangles.len() as u32).to_le_bytes());
    for tr in &mesh.triangles {
        let e1 = [
            tr[1][0] - tr[0][0],
            tr[1][1] - tr[0][1],
            tr[1][2] - tr[0][2],
        ];
        let e2 = [
            tr[2][0] - tr[0][0],
            tr[2][1] - tr[0][1],
            tr[2][2] - tr[0][2],
        ];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-30);
        for v in n.iter().map(|v| v / l).chain(tr.iter().flatten().copied()) {
            out.extend((v as f32).to_le_bytes());
        }
        out.extend([0u8, 0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 x 6 x 4 block in 1 mm cells, its top at Z0, a slot across it (X 4 to 6) 2 deep.
    fn slotted() -> (f64, f64, f64, usize, usize, Vec<f64>, Vec<f64>) {
        let (nx, ny) = (10, 6);
        let mut top = vec![0.0; nx * ny];
        for j in 0..ny {
            for i in 4..6 {
                top[j * nx + i] = -2.0;
            }
        }
        (0.0, 0.0, 1.0, nx, ny, top, vec![-4.0; nx * ny])
    }

    #[test]
    fn a_block_meshed_and_sampled_again_is_the_same() {
        let (x0, y0, c, nx, ny, top, bot) = slotted();
        let m = block_mesh(x0, y0, c, nx, ny, &top, &bot);
        let (sx, sy, mx, my, t2, b2) = sample_block(&m, c);
        assert_eq!((sx, sy, mx, my), (x0, y0, nx, ny));
        for k in 0..nx * ny {
            assert!(
                (t2[k] - top[k]).abs() < 1e-9 && (b2[k] - bot[k]).abs() < 1e-9,
                "{k}: {} {}",
                t2[k],
                b2[k]
            );
        }
    }

    #[test]
    fn a_bar_meshed_and_sampled_again_is_the_same() {
        let g = Grid::new(-20.0, 0.0, 0.5, 72);
        let mut map = RadialMap::filled(g, 10.0);
        // a groove round it, and a flat on one side
        for i in 0..g.nx {
            for j in 0..g.na {
                let x = g.x(i);
                let mut r = 10.0f32;
                if (-12.0..-8.0).contains(&x) {
                    r = 7.0;
                }
                if j < 6 {
                    r = r.min(8.0);
                }
                map.r[i * g.na + j] = r;
            }
        }
        let m = bar_mesh(&map);
        let back = sample_bar(&m, g);
        for (a, b) in map.r.iter().zip(&back.r) {
            assert!((a - b).abs() < 1e-4, "{a} {b}");
        }
    }

    #[test]
    fn a_block_tipped_on_its_side_keeps_its_outline_and_fills_the_pocket() {
        // the slotted block turned a quarter about Y: its old top faces +X, the slot opens
        // sideways
        let (x0, y0, c, nx, ny, top, bot) = slotted();
        let m = transform(
            &block_mesh(x0, y0, c, nx, ny, &top, &bot),
            rotation([0.0, 90.0, 0.0]),
            [0.0; 3],
        );
        let (lo, hi) = m.bounds().unwrap();
        // X -4..0 (the old Z: its top at X0), Y 0..6, Z -10..0 (the old X)
        for (a, b) in lo
            .iter()
            .zip(&[-4.0, 0.0, -10.0])
            .chain(hi.iter().zip(&[0.0, 6.0, 0.0]))
        {
            assert!((a - b).abs() < 1e-9, "{lo:?} {hi:?}");
        }
        let (sx, _, mx, my, t2, b2) = sample_block(&m, 1.0);
        assert_eq!((mx, my), (4, 6));
        for j in 0..my {
            for i in 0..mx {
                let k = j * mx + i;
                // every column top to bottom: where the slot now opens sideways (X 2 to 4, the
                // old top 2 deep, at old X 4 to 6: new Z -6 to -4) counted as material
                assert!(
                    (t2[k] - 0.0).abs() < 1e-9 && (b2[k] + 10.0).abs() < 1e-9,
                    "{} {} {} {}",
                    sx,
                    i,
                    t2[k],
                    b2[k]
                );
            }
        }
    }

    #[test]
    fn euler_angles_come_back() {
        for d in [
            [0.0, 0.0, 90.0],
            [90.0, 0.0, 0.0],
            [0.0, 90.0, 0.0],
            [30.0, -20.0, 45.0],
            [180.0, 0.0, 0.0],
            [90.0, 0.0, 90.0],
        ] {
            let m = rotation(d);
            let back = rotation(euler(m));
            for i in 0..3 {
                for j in 0..3 {
                    assert!(
                        (m[i][j] - back[i][j]).abs() < 1e-9,
                        "{d:?} -> {:?}",
                        euler(m)
                    );
                }
            }
        }
    }

    #[test]
    fn a_mesh_written_as_stl_reads_back() {
        let (x0, y0, c, nx, ny, top, bot) = slotted();
        let m = block_mesh(x0, y0, c, nx, ny, &top, &bot);
        let back = Mesh::from_bytes(&to_stl(&m, "test")).unwrap();
        assert_eq!(back.triangles.len(), m.triangles.len());
    }
}
