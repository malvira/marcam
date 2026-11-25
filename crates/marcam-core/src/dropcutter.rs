//! Radial drop-cutter: the lowest tool-tip radius at `(x, a)` for a tool whose axis is radial
//! (perpendicular to the rotary X axis, passing through it) so that it touches but does not cut
//! into the mesh.
//!
//! Frame: a part point `p` appears at `Rx(a) * p` on the machine (A rotates the part
//! right-handed about +X); the tool points down the machine's -Z, above the axis. After rotating
//! the mesh by `a` the problem is the classic 3-axis drop-cutter at `(x, y = 0)`.
//!
//! Spatial index: triangles are binned on a grid over (x, part angle φ). Each triangle goes into
//! every bin whose tool position could reach it: its x range grown by the tool radius, and its
//! angular span grown by `asin(R / ρmin)` (ρmin = closest approach of the triangle to the axis).
//! A query reads exactly one bin.

use crate::det::Det;
use crate::mesh::{Mesh, Triangle};
use std::f64::consts::{PI, TAU};

/// Axisymmetric cutter, tip at the origin, axis up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cutter {
    Flat {
        radius: f64,
    },
    Ball {
        radius: f64,
    },
    /// Cone with the given half angle (radians), up to `radius`.
    Cone {
        radius: f64,
        half_angle: f64,
    },
    /// Tapered ball: a ball of `tip_radius` blending (tangent) into a cone of `half_angle`
    /// (radians) up to `radius`.
    TaperedBall {
        tip_radius: f64,
        half_angle: f64,
        radius: f64,
    },
}

impl Cutter {
    /// The cutter `d` wider all round (a flat or ball tool: walls left `d` thick); None for
    /// other shapes when `d` is not 0.
    pub fn grown(self, d: f64) -> Option<Cutter> {
        if d == 0.0 {
            return Some(self);
        }
        match self {
            Cutter::Flat { radius } if radius + d > 0.0 => {
                Some(Cutter::Flat { radius: radius + d })
            }
            Cutter::Ball { radius } if radius + d > 0.0 => {
                Some(Cutter::Ball { radius: radius + d })
            }
            _ => None,
        }
    }

    pub fn radius(&self) -> f64 {
        match *self {
            Cutter::Flat { radius }
            | Cutter::Ball { radius }
            | Cutter::Cone { radius, .. }
            | Cutter::TaperedBall { radius, .. } => radius,
        }
    }

    /// Height of the cutting surface above the tip at distance `d` from the axis.
    pub fn height(&self, d: f64) -> f64 {
        match *self {
            Cutter::Flat { .. } => 0.0,
            Cutter::Ball { radius } => radius - (radius * radius - d * d).max(0.0).sqrt(),
            Cutter::Cone { half_angle, .. } => d / half_angle.dtan(),
            Cutter::TaperedBall {
                tip_radius: rb,
                half_angle,
                ..
            } => {
                // the ball meets the cone where its slope is the cone's
                let (s, c) = half_angle.dsin_cos();
                let dt = rb * c;
                if d <= dt {
                    rb - (rb * rb - d * d).max(0.0).sqrt()
                } else {
                    rb * (1.0 - s) + (d - dt) * c / s
                }
            }
        }
    }

    /// Stepover that leaves cusps `h` high between parallel passes on a flat floor: ball
    /// 2 sqrt(h (2R - h)), cone 2 h tan(half angle), a tapered ball as its ball while the
    /// cusp is on it and then up its cone. None for a flat end mill or `h <= 0`.
    pub fn scallop_width(&self, h: f64) -> Option<f64> {
        if h <= 0.0 {
            return None;
        }
        let ball = |r: f64| 2.0 * (h * (2.0 * r - h.min(r))).sqrt();
        match *self {
            Cutter::Flat { .. } => None,
            Cutter::Ball { radius } => Some(ball(radius)),
            Cutter::Cone { half_angle, .. } => Some(2.0 * h * half_angle.dtan()),
            Cutter::TaperedBall {
                tip_radius: rb,
                half_angle,
                ..
            } => {
                let (s, c) = half_angle.dsin_cos();
                let zt = rb * (1.0 - s);
                Some(if h <= zt {
                    ball(rb)
                } else {
                    2.0 * (rb * c + (h - zt) * s / c)
                })
            }
        }
    }

    /// Distance from the axis at which the surface's slope equals `slope` (best contact
    /// against a plane of that slope), clamped to the radius.
    fn contact_distance(&self, slope: f64) -> f64 {
        match *self {
            Cutter::Flat { radius } => radius,
            Cutter::Ball { radius } => radius * slope / (1.0 + slope * slope).sqrt(),
            Cutter::Cone { radius, half_angle } => {
                if slope > 1.0 / half_angle.dtan() {
                    radius
                } else {
                    0.0
                }
            }
            Cutter::TaperedBall {
                tip_radius,
                half_angle,
                radius,
            } => {
                if slope > 1.0 / half_angle.dtan() {
                    radius
                } else {
                    tip_radius * slope / (1.0 + slope * slope).sqrt()
                }
            }
        }
    }
}

/// Precomputed per-triangle data.
#[derive(Debug, Clone)]
struct Tri {
    t: Triangle,
    x_min: f64,
    x_max: f64,
    /// Largest distance of a vertex from the axis: no contact on this triangle puts the
    /// tool tip further out.
    r_max: f64,
}

#[derive(Debug, Clone)]
pub struct DropCutter {
    cutter: Cutter,
    tris: Vec<Tri>,
    x0: f64,
    bx: f64,
    nx: usize,
    nphi: usize,
    bins: Vec<Vec<u32>>,
}

/// `sqrt(x² + y²)`. Not `f64::hypot`: on wasm32 that is a software routine built on emulated
/// fused multiply-add and was most of the drop-cutter's run time.
#[inline]
fn len2(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}

/// Distance from the origin to a 2D triangle (0 if inside).
fn origin_distance_2d(p: [[f64; 2]; 3]) -> f64 {
    let cross = |a: [f64; 2], b: [f64; 2]| a[0] * b[1] - a[1] * b[0];
    let s: Vec<f64> = (0..3).map(|i| cross(p[i], p[(i + 1) % 3])).collect();
    if (s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)) && s.iter().any(|&v| v != 0.0) {
        return 0.0;
    }
    (0..3)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % 3]);
            let ab = [b[0] - a[0], b[1] - a[1]];
            let l2 = ab[0] * ab[0] + ab[1] * ab[1];
            let t = if l2 > 0.0 {
                (-(a[0] * ab[0] + a[1] * ab[1]) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            len2(a[0] + t * ab[0], a[1] + t * ab[1])
        })
        .fold(f64::INFINITY, f64::min)
}

/// Smallest arc `(start, length)` covering the three angles (radians, start in [0, 2π)).
fn angular_span(angles: [f64; 3]) -> (f64, f64) {
    let mut a: Vec<f64> = angles.iter().map(|v| v.rem_euclid(TAU)).collect();
    a.sort_by(|p, q| p.total_cmp(q));
    // the arc is the complement of the largest gap
    let gaps = [a[1] - a[0], a[2] - a[1], a[0] + TAU - a[2]];
    let (i, g) = gaps.iter().enumerate().fold(
        (0, -1.0),
        |best, (i, &g)| if g > best.1 { (i, g) } else { best },
    );
    let start = a[(i + 1) % 3];
    (start, TAU - g)
}

impl DropCutter {
    pub fn new(mesh: &Mesh, cutter: Cutter) -> DropCutter {
        DropCutter::reaching(mesh, cutter, 0.0)
    }

    /// [`DropCutter::new`] for tools up to `reach` mm off the axis in Y too
    /// ([`DropCutter::radius_at_offset`]).
    pub fn reaching(mesh: &Mesh, cutter: Cutter, reach: f64) -> DropCutter {
        let r = cutter.radius() + reach.abs();
        let tris: Vec<Tri> = mesh
            .triangles
            .iter()
            .map(|t| Tri {
                t: *t,
                x_min: t[0][0].min(t[1][0]).min(t[2][0]),
                x_max: t[0][0].max(t[1][0]).max(t[2][0]),
                r_max: t.iter().map(|p| len2(p[1], p[2])).fold(0.0, f64::max),
            })
            .collect();
        let (lo, hi) = tris
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| {
                (lo.min(t.x_min), hi.max(t.x_max))
            });
        let (lo, hi) = if lo.is_finite() {
            (lo - r, hi + r)
        } else {
            (0.0, 1.0)
        };
        let bx = (r.max(0.25)).max((hi - lo) / 400.0);
        let nx = (((hi - lo) / bx).ceil() as usize).max(1);
        let nphi = 120; // 3 degree bins
        let bphi = TAU / nphi as f64;
        let mut bins = vec![Vec::new(); nx * nphi];
        for (k, tri) in tris.iter().enumerate() {
            let t = &tri.t;
            let p2 = [[t[0][1], t[0][2]], [t[1][1], t[1][2]], [t[2][1], t[2][2]]];
            let rho = origin_distance_2d(p2);
            let i0 = (((tri.x_min - r - lo) / bx).floor().max(0.0)) as usize;
            let i1 = ((((tri.x_max + r - lo) / bx).floor()) as usize).min(nx - 1);
            let phi_bins: Vec<usize> = if rho <= r * (1.0 + 1e-9) {
                (0..nphi).collect()
            } else {
                let w = (r / rho).min(1.0).dasin() + 1e-9;
                let (start, len) = angular_span([
                    p2[0][1].datan2(p2[0][0]),
                    p2[1][1].datan2(p2[1][0]),
                    p2[2][1].datan2(p2[2][0]),
                ]);
                let (s, e) = (start - w, start + len + w);
                if e - s >= TAU {
                    (0..nphi).collect()
                } else {
                    let j0 = (s / bphi).floor() as i64;
                    let j1 = (e / bphi).floor() as i64;
                    (j0..=j1)
                        .map(|j| j.rem_euclid(nphi as i64) as usize)
                        .collect()
                }
            };
            for i in i0..=i1 {
                for &j in &phi_bins {
                    bins[i * nphi + j].push(k as u32);
                }
            }
        }
        // highest triangles first: a query stops at the first one that cannot beat its best
        for b in &mut bins {
            b.sort_by(|&p, &q| tris[q as usize].r_max.total_cmp(&tris[p as usize].r_max));
        }
        DropCutter {
            cutter,
            tris,
            x0: lo,
            bx,
            nx,
            nphi,
            bins,
        }
    }

    pub fn cutter(&self) -> Cutter {
        self.cutter
    }

    /// Tool-tip radius at axial position `x` and rotary angle `a_deg`; never below `floor`
    /// (which must be >= 0: the far side of the part is ignored).
    pub fn radius_at(&self, x: f64, a_deg: f64, floor: f64) -> f64 {
        self.radius_at_offset(x, a_deg, 0.0, floor)
    }

    /// The tip's height (machine Z) of the tool upright `y` mm off the axis in Y, at X `x`
    /// with the part at A `a_deg`, dropped onto it from above (the cutter's whole length
    /// along its line, as [`DropCutter::radius_at`] along the radius). The cutter must have
    /// been made [`DropCutter::reaching`] that far.
    pub fn radius_at_offset(&self, x: f64, a_deg: f64, y: f64, floor: f64) -> f64 {
        let a = a_deg.to_radians();
        // the part direction under the tool: rotated angle φ + a == 90°
        let phi = (PI / 2.0 - a).rem_euclid(TAU);
        let ix = (x - self.x0) / self.bx;
        // (past the planning budget: the operation is failing, see crate::budget)
        if crate::budget::over() || ix < 0.0 || ix >= self.nx as f64 {
            return floor;
        }
        let j = ((phi / TAU * self.nphi as f64) as usize).min(self.nphi - 1);
        let (s, c) = a.dsin_cos();
        let r = self.cutter.radius();
        let mut best = floor;
        let mut looked = crate::budget::PER_QUERY;
        for &k in &self.bins[ix as usize * self.nphi + j] {
            looked += 1;
            let tri = &self.tris[k as usize];
            if tri.r_max <= best {
                break;
            }
            if tri.x_max < x - r || tri.x_min > x + r {
                continue;
            }
            // rotate into the tool frame: local (u, v, w) = (x - x_tool, y', z')
            let mut lt = [[0.0; 3]; 3];
            for (q, p) in lt.iter_mut().zip(&tri.t) {
                *q = [p[0] - x, p[1] * c - p[2] * s - y, p[1] * s + p[2] * c];
            }
            let zmax = lt[0][2].max(lt[1][2]).max(lt[2][2]);
            if zmax <= best {
                continue;
            }
            let ymin = lt[0][1].min(lt[1][1]).min(lt[2][1]);
            let ymax = lt[0][1].max(lt[1][1]).max(lt[2][1]);
            if ymin > r || ymax < -r {
                continue;
            }
            // bound: no contact can be higher than zmax - h(closest approach)
            let dmin = origin_distance_2d([
                [lt[0][0], lt[0][1]],
                [lt[1][0], lt[1][1]],
                [lt[2][0], lt[2][1]],
            ]);
            if dmin > r || zmax - self.cutter.height(dmin) <= best {
                continue;
            }
            best = best.max(drop_triangle_above(&self.cutter, &lt, best));
        }
        crate::budget::spend(looked);
        best
    }
}

/// Tip height for a tool at the origin (axis vertical) against one triangle; `-inf` if the
/// triangle is outside the footprint.
pub fn drop_triangle(cutter: &Cutter, t: &Triangle) -> f64 {
    drop_triangle_above(cutter, t, f64::NEG_INFINITY)
}

/// Like [`drop_triangle`], but may skip work that cannot beat `floor` (the result is then
/// `<= floor`).
fn drop_triangle_above(cutter: &Cutter, t: &Triangle, floor: f64) -> f64 {
    let r = cutter.radius();
    let mut best = floor;
    // facet: plane z = z0 + g·(p - p0)
    let n = crate::mesh::cross(crate::mesh::sub(t[1], t[0]), crate::mesh::sub(t[2], t[0]));
    let nlen = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if nlen > 0.0 && n[2].abs() > 1e-12 * nlen {
        let g = [-n[0] / n[2], -n[1] / n[2]];
        let plane = |p: [f64; 2]| t[0][2] + g[0] * (p[0] - t[0][0]) + g[1] * (p[1] - t[0][1]);
        let slope = len2(g[0], g[1]);
        let dir = if slope > 0.0 {
            [g[0] / slope, g[1] / slope]
        } else {
            [0.0, 0.0]
        };
        let mut cand = |d: f64| {
            let p = [dir[0] * d, dir[1] * d];
            if inside_2d(t, p) {
                best = best.max(plane(p) - cutter.height(d));
            }
        };
        cand(cutter.contact_distance(slope));
        cand(r);
        cand(0.0);
    }
    // edges (vertices are their end points)
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        if a[2].max(b[2]) - cutter.height(segment_distance_2d(a, b).min(r)) <= best {
            continue;
        }
        best = best.max(drop_edge(cutter, a, b));
    }
    best
}

/// Distance in the xy plane from the origin to segment `a`-`b`.
fn segment_distance_2d(a: [f64; 3], b: [f64; 3]) -> f64 {
    let e = [b[0] - a[0], b[1] - a[1]];
    let l2 = e[0] * e[0] + e[1] * e[1];
    let t = if l2 > 0.0 {
        (-(a[0] * e[0] + a[1] * e[1]) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    len2(a[0] + t * e[0], a[1] + t * e[1])
}

fn inside_2d(t: &Triangle, p: [f64; 2]) -> bool {
    let c =
        |a: [f64; 3], b: [f64; 3]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let (d0, d1, d2) = (c(t[0], t[1]), c(t[1], t[2]), c(t[2], t[0]));
    let eps = -1e-12;
    (d0 >= eps && d1 >= eps && d2 >= eps) || (d0 <= -eps && d1 <= -eps && d2 <= -eps)
}

/// Best contact of the cutter with segment `a`-`b`: maximise z(t) - h(d(t)) over the part
/// of the segment inside the footprint. The objective is concave (z linear, h convex and
/// increasing, d convex), so a golden-section search finds the maximum.
fn drop_edge(cutter: &Cutter, a: [f64; 3], b: [f64; 3]) -> f64 {
    let r = cutter.radius();
    let e = [b[0] - a[0], b[1] - a[1]];
    // |a.xy + t e|^2 <= r^2
    let qa = e[0] * e[0] + e[1] * e[1];
    let qb = 2.0 * (a[0] * e[0] + a[1] * e[1]);
    let qc = a[0] * a[0] + a[1] * a[1] - r * r;
    let (t0, t1) = if qa < 1e-24 {
        if qc > 0.0 {
            return f64::NEG_INFINITY;
        }
        (0.0, 1.0)
    } else {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 {
            return f64::NEG_INFINITY;
        }
        let sq = disc.sqrt();
        ((-qb - sq) / (2.0 * qa), (-qb + sq) / (2.0 * qa))
    };
    let (t0, t1) = (t0.max(0.0), t1.min(1.0));
    if t0 > t1 {
        return f64::NEG_INFINITY;
    }
    let f = |t: f64| {
        let x = a[0] + t * e[0];
        let y = a[1] + t * e[1];
        let z = a[2] + t * (b[2] - a[2]);
        z - cutter.height(len2(x, y).min(r))
    };
    let mut best = f(t0).max(f(t1));
    if matches!(cutter, Cutter::Flat { .. }) {
        return best; // linear objective: an end point wins
    }
    let (mut lo, mut hi) = (t0, t1);
    let gr = 0.5 * (5f64.sqrt() - 1.0);
    let mut c = hi - gr * (hi - lo);
    let mut d = lo + gr * (hi - lo);
    let (mut fc, mut fd) = (f(c), f(d));
    let len = (e[0] * e[0] + e[1] * e[1] + (b[2] - a[2]).powi(2)).sqrt();
    for _ in 0..80 {
        if (hi - lo) * len < 1e-7 {
            break;
        }
        if fc > fd {
            hi = d;
            d = c;
            fd = fc;
            c = hi - gr * (hi - lo);
            fc = f(c);
        } else {
            lo = c;
            c = d;
            fc = fd;
            d = lo + gr * (hi - lo);
            fd = f(d);
        }
    }
    best = best.max(fc).max(fd);
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;
    use crate::job::Profile;

    fn cyl(r: f64, x0: f64, x1: f64) -> Mesh {
        Mesh::revolve(
            &Profile {
                points: vec![[x0, r], [x1, r]],
                beyond_front: None,
            },
            720,
        )
    }

    #[test]
    fn flat_on_plane_and_edge() {
        let c = Cutter::Flat { radius: 1.0 };
        let t = [[-5.0, -5.0, 2.0], [5.0, -5.0, 2.0], [0.0, 5.0, 2.0]];
        assert_eq!(drop_triangle(&c, &t), 2.0);
        // sloped plane z = x: flat tool touches at x = +1
        let t = [[-5.0, -5.0, -5.0], [5.0, -5.0, 5.0], [0.0, 5.0, 0.0]];
        assert!((drop_triangle(&c, &t) - 1.0).abs() < 1e-12);
        // edge outside footprint
        let t = [[3.0, 0.0, 9.0], [4.0, 0.0, 9.0], [3.0, 1.0, 9.0]];
        assert_eq!(drop_triangle(&c, &t), f64::NEG_INFINITY);
    }

    #[test]
    fn ball_and_cone_on_slope() {
        // plane z = x (45 deg)
        let t = [[-5.0, -5.0, -5.0], [5.0, -5.0, 5.0], [0.0, 5.0, 0.0]];
        let b = Cutter::Ball { radius: 1.0 };
        // ball centre at distance 1 from plane: zc = sqrt(2), tip = sqrt(2) - 1
        assert!((drop_triangle(&b, &t) - (2f64.sqrt() - 1.0)).abs() < 1e-12);
        // 90 deg V-bit on a 45 deg plane: flank lies on the plane, tip at 0
        let v = Cutter::Cone {
            radius: 3.0,
            half_angle: PI / 4.0,
        };
        assert!(drop_triangle(&v, &t).abs() < 1e-9);
        // ball against a horizontal edge at height 0 offset 0.6 sideways
        let e = [[-5.0, 0.6, 0.0], [5.0, 0.6, 0.0], [0.0, 0.6, -5.0]];
        let want = -(1.0 - (1.0f64 - 0.36).sqrt());
        assert!((drop_triangle(&b, &e) - want).abs() < 1e-9);
    }

    #[test]
    fn radial_drop_on_cylinder() {
        let dc = DropCutter::new(&cyl(8.0, -10.0, 0.0), Cutter::Flat { radius: 1.5 });
        for a in [0.0, 13.0, 90.0, 181.5, 359.0] {
            let r = dc.radius_at(-5.0, a, 0.0);
            assert!((r - 8.0).abs() < 1e-3, "a={a}: {r}");
        }
        // off the end: the front cap is reached while the footprint overlaps the part
        assert!((dc.radius_at(1.0, 0.0, 0.0) - 8.0).abs() < 1e-3);
        assert_eq!(dc.radius_at(2.0, 0.0, 0.0), 0.0);
        assert_eq!(dc.radius_at(2.0, 0.0, 4.0), 4.0);
    }

    #[test]
    fn index_matches_brute_force() {
        let m = Mesh::revolve(
            &Profile {
                points: vec![[-6.0, 2.0], [-3.0, 2.0], [-3.0, 6.0], [0.0, 4.0]],
                beyond_front: None,
            },
            90,
        );
        for cutter in [
            Cutter::Flat { radius: 1.2 },
            Cutter::Ball { radius: 1.0 },
            Cutter::Cone {
                radius: 2.0,
                half_angle: PI / 4.0,
            },
        ] {
            let dc = DropCutter::new(&m, cutter);
            for i in 0..40 {
                let x = -7.0 + 0.2 * i as f64;
                for a in [0.0f64, 7.0, 45.0, 100.0, 222.0, 300.5] {
                    let (s, c) = a.to_radians().dsin_cos();
                    let brute = m
                        .triangles
                        .iter()
                        .map(|t| {
                            let mut lt = [[0.0; 3]; 3];
                            for (q, p) in lt.iter_mut().zip(t) {
                                *q = [p[0] - x, p[1] * c - p[2] * s, p[1] * s + p[2] * c];
                            }
                            drop_triangle(&cutter, &lt)
                        })
                        .fold(0.0, f64::max);
                    let got = dc.radius_at(x, a, 0.0);
                    assert!(
                        (got - brute).abs() < 1e-9,
                        "{cutter:?} x={x} a={a}: {got} vs {brute}"
                    );
                }
            }
        }
    }
}
