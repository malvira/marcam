//! Faces the kernel leaves out, meshed here instead.
//!
//! SolidCraft's kernel trims a face on its surface's parameters with an iso-line seam where the
//! face winds round a closed surface. A face no iso line crosses cleanly (an impeller's hub
//! floor: six twisted blade roots between them cover every angle) gets no pcurves, so the kernel
//! leaves it out and the part has a hole where the face should be. For CAM a mesh is all that is
//! needed, so such a face is meshed from the file directly: its edges are sampled in 3D within
//! the chord, put on the surface's parameters, laid out flat (a closed direction round a ring,
//! so a face that winds round it is a plain polygon with holes), triangulated with constraints
//! and refined until every facet is within the chord of the surface.
//!
//! Only what can be done exactly is done: a surface or curve kind not handled here, a face on a
//! surface closed both ways, or a face whose boundary does not meet the rest of the body keeps
//! the kernel's warning and stays out.

use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;
use std::sync::Arc;

use solidcore_step::geom::{self, Curve, Surface};
use solidcore_step::p21::{self, Exchange, Param};
use solidcore_step::read::Reader;
use spade::{ConstrainedDelaunayTriangulation, Point2, RefinementParameters, Triangulation};

type P3 = [f64; 3];
type Uv = [f64; 2];

/// Hard caps, so a bad face cannot run away.
const MAX_EDGE_DEPTH: u32 = 14;
const MAX_FACE_VERTICES: usize = 400_000;
const REFINE_ROUNDS: usize = 24;

/// A face meshed here: its STEP entity and its triangles (outward, in millimetres, placed).
pub(crate) struct FaceMesh {
    pub face: u64,
    pub triangles: Vec<[P3; 3]>,
}

fn add(a: P3, b: P3) -> P3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: P3, s: f64) -> P3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn len(a: P3) -> f64 {
    dot(a, a).sqrt()
}
fn dist(a: P3, b: P3) -> f64 {
    len(sub(a, b))
}

// ------------------------------------------------------------------------------- NURBS

/// Full knot vector from distinct knots and multiplicities.
fn expand(knots: &[f64], mults: &[usize]) -> Vec<f64> {
    knots
        .iter()
        .zip(mults)
        .flat_map(|(k, m)| std::iter::repeat_n(*k, *m))
        .collect()
}

/// The span `i` with `U[i] <= t < U[i+1]` for `n` control points of degree `p`.
fn span(n: usize, p: usize, u: &[f64], t: f64) -> usize {
    if t >= u[n] {
        // the last non-empty span
        let mut i = n - 1;
        while i > p && u[i] >= u[n] {
            i -= 1;
        }
        return i;
    }
    if t <= u[p] {
        let mut i = p;
        while i + 1 < n && u[i + 1] <= u[p] {
            i += 1;
        }
        return i;
    }
    let (mut lo, mut hi) = (p, n);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if t < u[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The `p + 1` non-zero basis functions at `t` in span `i` (The NURBS Book, A2.2).
fn basis(i: usize, p: usize, u: &[f64], t: f64) -> Vec<f64> {
    let mut n = vec![0.0; p + 1];
    let mut left = vec![0.0; p + 1];
    let mut right = vec![0.0; p + 1];
    n[0] = 1.0;
    for j in 1..=p {
        left[j] = t - u[i + 1 - j];
        right[j] = u[i + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let d = right[r + 1] + left[j - r];
            let tmp = if d == 0.0 { 0.0 } else { n[r] / d };
            n[r] = saved + right[r + 1] * tmp;
            saved = left[j - r] * tmp;
        }
        n[j] = saved;
    }
    n
}

#[derive(Clone)]
struct Nurbs1 {
    p: usize,
    u: Vec<f64>,
    /// Homogeneous (w·x, w·y, w·z, w).
    pts: Vec<[f64; 4]>,
}

impl Nurbs1 {
    fn new(b: &geom::BSplineCurve) -> Option<Self> {
        let u = expand(&b.knots, &b.multiplicities);
        let n = b.points.len();
        if n == 0 || u.len() != n + b.degree + 1 {
            return None;
        }
        let pts = b
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let w = b.weights.as_ref().map_or(1.0, |w| w[i]);
                [p[0] * w, p[1] * w, p[2] * w, w]
            })
            .collect();
        Some(Nurbs1 {
            p: b.degree,
            u,
            pts,
        })
    }
    fn domain(&self) -> [f64; 2] {
        [self.u[self.p], self.u[self.pts.len()]]
    }
    fn at(&self, t: f64) -> P3 {
        let n = self.pts.len();
        let i = span(n, self.p, &self.u, t);
        let b = basis(i, self.p, &self.u, t);
        let mut h = [0.0; 4];
        for (k, bk) in b.iter().enumerate() {
            let c = self.pts[i - self.p + k];
            for d in 0..4 {
                h[d] += bk * c[d];
            }
        }
        [h[0] / h[3], h[1] / h[3], h[2] / h[3]]
    }
}

#[derive(Clone)]
struct Nurbs2 {
    pu: usize,
    pv: usize,
    ku: Vec<f64>,
    kv: Vec<f64>,
    pts: Vec<Vec<[f64; 4]>>,
}

impl Nurbs2 {
    fn new(b: &geom::BSplineSurface) -> Option<Self> {
        let ku = expand(&b.u_knots, &b.u_multiplicities);
        let kv = expand(&b.v_knots, &b.v_multiplicities);
        let nu = b.points.len();
        let nv = b.points.first()?.len();
        if nu == 0 || nv == 0 || ku.len() != nu + b.u_degree + 1 || kv.len() != nv + b.v_degree + 1
        {
            return None;
        }
        let pts = b
            .points
            .iter()
            .enumerate()
            .map(|(i, row)| {
                row.iter()
                    .enumerate()
                    .map(|(j, p)| {
                        let w = b.weights.as_ref().map_or(1.0, |w| w[i][j]);
                        [p[0] * w, p[1] * w, p[2] * w, w]
                    })
                    .collect()
            })
            .collect();
        Some(Nurbs2 {
            pu: b.u_degree,
            pv: b.v_degree,
            ku,
            kv,
            pts,
        })
    }
    fn domain(&self) -> [[f64; 2]; 2] {
        [
            [self.ku[self.pu], self.ku[self.pts.len()]],
            [self.kv[self.pv], self.kv[self.pts[0].len()]],
        ]
    }
    fn at(&self, u: f64, v: f64) -> P3 {
        let (nu, nv) = (self.pts.len(), self.pts[0].len());
        let i = span(nu, self.pu, &self.ku, u);
        let j = span(nv, self.pv, &self.kv, v);
        let bu = basis(i, self.pu, &self.ku, u);
        let bv = basis(j, self.pv, &self.kv, v);
        let mut h = [0.0; 4];
        for (a, ba) in bu.iter().enumerate() {
            let row = &self.pts[i - self.pu + a];
            for (b, bb) in bv.iter().enumerate() {
                let c = row[j - self.pv + b];
                let w = ba * bb;
                for d in 0..4 {
                    h[d] += w * c[d];
                }
            }
        }
        [h[0] / h[3], h[1] / h[3], h[2] / h[3]]
    }
}

// ------------------------------------------------------------------------------ curves

/// A curve that can be evaluated here, on its STEP parameter.
enum Crv {
    Line {
        origin: P3,
        dir: P3,
    },
    Conic {
        pos: geom::Placement,
        a: f64,
        b: f64,
    },
    Spline(Nurbs1),
    Polyline(Vec<P3>),
    Moved(Box<Crv>, geom::Transform),
}

impl Crv {
    fn new(c: &Curve) -> Option<Crv> {
        Some(match c {
            Curve::Line {
                origin,
                direction,
                magnitude,
            } => Crv::Line {
                origin: *origin,
                dir: mul(*direction, *magnitude),
            },
            Curve::Circle { position, radius } => Crv::Conic {
                pos: *position,
                a: *radius,
                b: *radius,
            },
            Curve::Ellipse {
                position,
                semi_axis_1,
                semi_axis_2,
            } => Crv::Conic {
                pos: *position,
                a: *semi_axis_1,
                b: *semi_axis_2,
            },
            Curve::BSpline(b) => Crv::Spline(Nurbs1::new(b)?),
            Curve::Polyline(p) if p.len() >= 2 => Crv::Polyline(p.clone()),
            Curve::Trimmed { basis, .. } => Crv::new(basis)?,
            Curve::SurfaceCurve { curve_3d, .. } => Crv::new(curve_3d)?,
            Curve::Replica { parent, transform } => {
                Crv::Moved(Box::new(Crv::new(parent)?), *transform)
            }
            _ => return None,
        })
    }
    fn at(&self, t: f64) -> P3 {
        match self {
            Crv::Line { origin, dir } => add(*origin, mul(*dir, t)),
            Crv::Conic { pos, a, b } => pos.at(a * t.cos(), b * t.sin(), 0.0),
            Crv::Spline(n) => n.at(t),
            Crv::Polyline(p) => {
                let i = (t.floor().max(0.0) as usize).min(p.len() - 2);
                let f = t - i as f64;
                add(p[i], mul(sub(p[i + 1], p[i]), f))
            }
            Crv::Moved(c, m) => m.point(c.at(t)),
        }
    }
    fn period(&self) -> Option<f64> {
        match self {
            Crv::Conic { .. } => Some(TAU),
            Crv::Moved(c, _) => c.period(),
            _ => None,
        }
    }
    fn domain(&self) -> Option<[f64; 2]> {
        match self {
            Crv::Line { .. } => None,
            Crv::Conic { .. } => Some([0.0, TAU]),
            Crv::Spline(n) => Some(n.domain()),
            Crv::Polyline(p) => Some([0.0, (p.len() - 1) as f64]),
            Crv::Moved(c, _) => c.domain(),
        }
    }
    /// The parameter of the point of the curve nearest `p`.
    fn param(&self, p: P3) -> Option<f64> {
        match self {
            Crv::Line { origin, dir } => {
                let d2 = dot(*dir, *dir);
                (d2 > 0.0).then(|| dot(sub(p, *origin), *dir) / d2)
            }
            Crv::Conic { pos, a, b } => {
                let q = sub(p, pos.origin);
                let (x, y) = (dot(q, pos.ref_direction) / a, dot(q, pos.y()) / b);
                Some(y.atan2(x).rem_euclid(TAU))
            }
            _ => {
                let [t0, t1] = self.domain()?;
                let n = 400;
                let mut best = (f64::INFINITY, t0);
                for k in 0..=n {
                    let t = t0 + (t1 - t0) * k as f64 / n as f64;
                    let d = dist(self.at(t), p);
                    if d < best.0 {
                        best = (d, t);
                    }
                }
                // golden-section search around the best sample
                let h = (t1 - t0) / n as f64;
                let (mut a, mut b) = ((best.1 - h).max(t0), (best.1 + h).min(t1));
                let g = 0.618_033_988_749_895;
                for _ in 0..60 {
                    let c = b - g * (b - a);
                    let d = a + g * (b - a);
                    if dist(self.at(c), p) < dist(self.at(d), p) {
                        b = d;
                    } else {
                        a = c;
                    }
                }
                Some(0.5 * (a + b))
            }
        }
    }
}

// ---------------------------------------------------------------------------- surfaces

enum Srf {
    Plane(geom::Placement),
    Cylinder(geom::Placement, f64),
    Cone(geom::Placement, f64, f64),
    Sphere(geom::Placement, f64),
    Torus(geom::Placement, f64, f64),
    Spline(Nurbs2),
    /// The profile at `v` turned by `u` about the axis.
    Revolution(Crv, geom::Axis1),
    /// `profile(u) + v·dir`.
    Extrusion(Crv, P3),
}

/// `p` turned by `angle` about the axis through `o` along unit `d` (Rodrigues).
fn rotate(p: P3, o: P3, d: P3, angle: f64) -> P3 {
    let v = sub(p, o);
    let (s, c) = angle.sin_cos();
    add(
        o,
        add(
            add(mul(v, c), mul(cross(d, v), s)),
            mul(d, dot(d, v) * (1.0 - c)),
        ),
    )
}

impl Srf {
    fn new(s: &Surface) -> Option<Srf> {
        Some(match s {
            Surface::Plane { position } => Srf::Plane(*position),
            Surface::Cylinder { position, radius } => Srf::Cylinder(*position, *radius),
            Surface::Cone {
                position,
                radius,
                semi_angle,
            } => Srf::Cone(*position, *radius, semi_angle.tan()),
            Surface::Sphere { position, radius } => Srf::Sphere(*position, *radius),
            Surface::Torus {
                position,
                major_radius,
                minor_radius,
            } => Srf::Torus(*position, *major_radius, *minor_radius),
            Surface::BSpline(b) => Srf::Spline(Nurbs2::new(b)?),
            Surface::Revolution { profile, axis } => Srf::Revolution(Crv::new(profile)?, *axis),
            Surface::Extrusion {
                profile,
                direction,
                magnitude,
            } => Srf::Extrusion(Crv::new(profile)?, mul(*direction, *magnitude)),
            Surface::RectangularTrimmed { basis, .. } => Srf::new(basis)?,
            _ => return None,
        })
    }
    fn at(&self, uv: Uv) -> P3 {
        let [u, v] = uv;
        match self {
            Srf::Plane(p) => p.at(u, v, 0.0),
            Srf::Cylinder(p, r) => p.at(r * u.cos(), r * u.sin(), v),
            Srf::Cone(p, r, t) => {
                let rr = r + v * t;
                p.at(rr * u.cos(), rr * u.sin(), v)
            }
            Srf::Sphere(p, r) => p.at(r * v.cos() * u.cos(), r * v.cos() * u.sin(), r * v.sin()),
            Srf::Torus(p, big, r) => {
                let rr = big + r * v.cos();
                p.at(rr * u.cos(), rr * u.sin(), r * v.sin())
            }
            Srf::Spline(n) => {
                let [[u0, u1], [v0, v1]] = n.domain();
                n.at(u.clamp(u0, u1), v.clamp(v0, v1))
            }
            Srf::Revolution(c, a) => rotate(c.at(v), a.origin, a.direction, u),
            Srf::Extrusion(c, d) => add(c.at(u), mul(*d, v)),
        }
    }
    /// Each parameter's range where the surface has one.
    fn domain(&self) -> [Option<[f64; 2]>; 2] {
        let turn = Some([0.0, TAU]);
        match self {
            Srf::Plane(_) => [None, None],
            Srf::Cylinder(..) | Srf::Cone(..) => [turn, None],
            Srf::Sphere(..) => [turn, Some([-TAU / 4.0, TAU / 4.0])],
            Srf::Torus(..) => [turn, turn],
            Srf::Spline(n) => n.domain().map(Some),
            Srf::Revolution(c, _) => [turn, c.domain()],
            Srf::Extrusion(c, _) => [c.domain(), None],
        }
    }
    /// The parameters of a point on the surface, where they have a closed form.
    fn invert(&self, p: P3) -> Option<Uv> {
        let local = |pl: &geom::Placement| {
            let q = sub(p, pl.origin);
            [dot(q, pl.ref_direction), dot(q, pl.y()), dot(q, pl.axis)]
        };
        Some(match self {
            Srf::Plane(pl) => {
                let [x, y, _] = local(pl);
                [x, y]
            }
            Srf::Cylinder(pl, _) | Srf::Cone(pl, ..) => {
                let [x, y, z] = local(pl);
                [y.atan2(x), z]
            }
            Srf::Sphere(pl, _) => {
                let [x, y, z] = local(pl);
                [y.atan2(x), z.atan2(x.hypot(y))]
            }
            Srf::Torus(pl, big, _) => {
                let [x, y, z] = local(pl);
                [y.atan2(x), z.atan2(x.hypot(y) - big)]
            }
            _ => return None,
        })
    }
}

/// A surface with what the mesher needs to know of it.
struct Patch {
    s: Srf,
    /// The period of each parameter that closes on itself.
    period: [Option<f64>; 2],
    /// Each parameter's range (made from the boundary where the surface has none).
    range: [[f64; 2]; 2],
    /// Coarse samples for a first guess of a point's parameters.
    grid: Vec<(Uv, P3)>,
}

impl Patch {
    fn wrap(&self, uv: Uv) -> Uv {
        let mut q = uv;
        for ((x, per), r) in q.iter_mut().zip(self.period).zip(self.range) {
            if let Some(per) = per {
                *x = r[0] + (*x - r[0]).rem_euclid(per);
            }
        }
        q
    }
    fn at(&self, uv: Uv) -> P3 {
        self.s.at(self.wrap(uv))
    }
    fn normal(&self, uv: Uv) -> P3 {
        let h = [
            1e-6 * (self.range[0][1] - self.range[0][0]).max(1e-9),
            1e-6 * (self.range[1][1] - self.range[1][0]).max(1e-9),
        ];
        let du = sub(
            self.at([uv[0] + h[0], uv[1]]),
            self.at([uv[0] - h[0], uv[1]]),
        );
        let dv = sub(
            self.at([uv[0], uv[1] + h[1]]),
            self.at([uv[0], uv[1] - h[1]]),
        );
        cross(du, dv)
    }
    /// The parameters of the point of the surface nearest `p`, starting from `near`.
    fn project(&self, p: P3, near: Option<Uv>, tol: f64) -> Uv {
        if let Some(uv) = self.s.invert(p) {
            return self.unwrap(uv, near);
        }
        let from_grid = || {
            self.grid
                .iter()
                .min_by(|a, b| dist(a.1, p).total_cmp(&dist(b.1, p)))
                .map_or([self.range[0][0], self.range[1][0]], |g| g.0)
        };
        let mut uv = self.newton(p, near.unwrap_or_else(from_grid));
        if near.is_some() && dist(self.at(uv), p) > 10.0 * tol {
            let again = self.newton(p, from_grid());
            if dist(self.at(again), p) < dist(self.at(uv), p) {
                uv = again;
            }
        }
        self.unwrap(uv, near)
    }
    /// `uv` moved by whole periods to lie next to `near`.
    fn unwrap(&self, mut uv: Uv, near: Option<Uv>) -> Uv {
        if let Some(n) = near {
            for k in 0..2 {
                if let Some(per) = self.period[k] {
                    uv[k] += ((n[k] - uv[k]) / per).round() * per;
                }
            }
        }
        uv
    }
    fn newton(&self, p: P3, start: Uv) -> Uv {
        let mut uv = start;
        let span = [
            (self.range[0][1] - self.range[0][0]).max(1e-9),
            (self.range[1][1] - self.range[1][0]).max(1e-9),
        ];
        for _ in 0..40 {
            let s = self.at(uv);
            let r = sub(s, p);
            let h = [1e-7 * span[0], 1e-7 * span[1]];
            let su = mul(
                sub(
                    self.at([uv[0] + h[0], uv[1]]),
                    self.at([uv[0] - h[0], uv[1]]),
                ),
                0.5 / h[0],
            );
            let sv = mul(
                sub(
                    self.at([uv[0], uv[1] + h[1]]),
                    self.at([uv[0], uv[1] - h[1]]),
                ),
                0.5 / h[1],
            );
            let (a, b, c) = (dot(su, su), dot(su, sv), dot(sv, sv));
            let (g0, g1) = (dot(su, r), dot(sv, r));
            let det = a * c - b * b;
            if det.is_nan() || det.abs() <= 1e-30 {
                break;
            }
            let mut d = [-(c * g0 - b * g1) / det, -(a * g1 - b * g0) / det];
            // at most a tenth of the range per step
            for k in 0..2 {
                d[k] = d[k].clamp(-0.1 * span[k], 0.1 * span[k]);
            }
            let mut next = [uv[0] + d[0], uv[1] + d[1]];
            for ((x, per), r) in next.iter_mut().zip(self.period).zip(self.range) {
                if per.is_none() {
                    *x = x.clamp(r[0], r[1]);
                }
            }
            let moved = (next[0] - uv[0]).abs() / span[0] + (next[1] - uv[1]).abs() / span[1];
            uv = next;
            if moved < 1e-13 {
                break;
            }
        }
        uv
    }
}

/// Whether parameter `k` of the surface closes on itself over `range`.
fn closes(s: &Srf, k: usize, range: [[f64; 2]; 2], tol: f64) -> bool {
    let o = 1 - k;
    let [a, b] = range[k];
    (0..=8).all(|i| {
        let t = range[o][0] + (range[o][1] - range[o][0]) * i as f64 / 8.0;
        let (mut p, mut q) = ([0.0; 2], [0.0; 2]);
        p[k] = a;
        q[k] = b;
        p[o] = t;
        q[o] = t;
        dist(s.at(p), s.at(q)) <= tol
    })
}

// ------------------------------------------------------------------------------- reading

fn refs(params: &[Param]) -> Vec<u64> {
    let mut out = Vec::new();
    for p in params {
        match p {
            Param::Ref(r) => out.push(*r),
            Param::List(l) => out.extend(refs(l)),
            _ => {}
        }
    }
    out
}

fn ent(ex: &Exchange, id: u64) -> Option<&p21::Entity> {
    ex.get(id)
}

/// A face's bounds as loops of edge uses: (curve entity, start vertex, end vertex, same sense,
/// forward in the loop).
type EdgeUse = (u64, u64, u64, bool, bool);

fn face_loops(ex: &Exchange, face: u64) -> Option<(u64, bool, Vec<Vec<EdgeUse>>)> {
    let f = ent(ex, face)?;
    if !matches!(f.name(), "ADVANCED_FACE" | "FACE_SURFACE") {
        return None;
    }
    let p = f.params();
    let bounds = p.get(1)?.as_list()?;
    let surface = p.get(2)?.as_ref_id()?;
    let same = p.get(3)?.as_bool()?;
    let mut loops = Vec::new();
    for b in bounds {
        let be = ent(ex, b.as_ref_id()?)?;
        let lp = ent(ex, be.params().get(1)?.as_ref_id()?)?;
        match lp.name() {
            "EDGE_LOOP" => {
                let mut uses = Vec::new();
                for oe in lp.params().get(1)?.as_list()? {
                    let oe = ent(ex, oe.as_ref_id()?)?;
                    if oe.name() != "ORIENTED_EDGE" {
                        return None;
                    }
                    let op = oe.params();
                    let ec = ent(ex, op.get(3)?.as_ref_id()?)?;
                    if ec.name() != "EDGE_CURVE" {
                        return None;
                    }
                    let fwd = op.get(4)?.as_bool()?;
                    let ep = ec.params();
                    uses.push((
                        ep.get(3)?.as_ref_id()?,
                        ep.get(1)?.as_ref_id()?,
                        ep.get(2)?.as_ref_id()?,
                        ep.get(4)?.as_bool()?,
                        fwd,
                    ));
                }
                loops.push(uses);
            }
            // a loop of one vertex bounds nothing to mesh
            "VERTEX_LOOP" => {}
            _ => return None,
        }
    }
    Some((surface, same, loops))
}

/// The point of a VERTEX_POINT.
fn vertex(ex: &Exchange, reader: &Reader, v: u64) -> Option<P3> {
    let e = ent(ex, v)?;
    reader.point(e.params().get(1)?.as_ref_id()?).ok()
}

/// One edge use sampled within `chord`, from its start in the loop to its end.
fn sample_edge(ex: &Exchange, reader: &mut Reader, u: EdgeUse, chord: f64) -> Option<Vec<P3>> {
    let (cid, v1, v2, same, fwd) = u;
    let curve: Arc<Curve> = reader.curve(cid).ok()?;
    let c = Crv::new(&curve)?;
    let (p1, p2) = (vertex(ex, reader, v1)?, vertex(ex, reader, v2)?);
    let t1 = c.param(p1)?;
    let mut t2 = c.param(p2)?;
    if let Some(per) = c.period() {
        let d = if same {
            (t2 - t1).rem_euclid(per)
        } else {
            (t1 - t2).rem_euclid(per)
        };
        let d = if d < 1e-9 * per { per } else { d };
        t2 = if same { t1 + d } else { t1 - d };
    } else if dist(p1, p2) <= chord.min(1e-3) {
        // a closed spline edge: all of it, its way
        let [a, b] = c.domain()?;
        let (s, e) = if same { (a, b) } else { (b, a) };
        return finish_edge(&c, s, e, p1, p2, fwd, chord);
    }
    finish_edge(&c, t1, t2, p1, p2, fwd, chord)
}

fn finish_edge(
    c: &Crv,
    t1: f64,
    t2: f64,
    p1: P3,
    p2: P3,
    fwd: bool,
    chord: f64,
) -> Option<Vec<P3>> {
    /// The piece `(a, pa)`..`(b, pb)` split until within `chord`, its points after `pa` added.
    fn split(
        c: &Crv,
        (a, pa): (f64, P3),
        (b, pb): (f64, P3),
        chord: f64,
        depth: u32,
        out: &mut Vec<P3>,
    ) {
        let m = 0.5 * (a + b);
        let pm = c.at(m);
        // the chord's distance from the curve at a quarter, half and three quarters
        let off = |t: f64, f: f64| dist(c.at(t), add(pa, mul(sub(pb, pa), f)));
        let worst = off(m, 0.5)
            .max(off(a + 0.25 * (b - a), 0.25))
            .max(off(a + 0.75 * (b - a), 0.75));
        if depth < MAX_EDGE_DEPTH && worst > chord {
            split(c, (a, pa), (m, pm), chord, depth + 1, out);
            split(c, (m, pm), (b, pb), chord, depth + 1, out);
        } else {
            out.push(pb);
        }
    }
    let n = 8;
    let mut out = vec![p1];
    let mut prev = (t1, c.at(t1));
    for k in 1..=n {
        let t = t1 + (t2 - t1) * k as f64 / n as f64;
        let p = c.at(t);
        split(c, prev, (t, p), chord, 0, &mut out);
        prev = (t, p);
    }
    // the ends exactly on their vertices, as the neighbouring faces have them
    *out.last_mut()? = p2;
    out[0] = p1;
    if dist(c.at(t1), p1) > 0.1_f64.max(100.0 * chord)
        || dist(c.at(t2), p2) > 0.1_f64.max(100.0 * chord)
    {
        return None;
    }
    if !fwd {
        out.reverse();
    }
    Some(out)
}

// ------------------------------------------------------------------------------- meshing

/// Even-odd inside test against closed polygons, bucketed by y.
struct Inside {
    segs: Vec<[[f64; 2]; 2]>,
    lo: f64,
    h: f64,
    bins: Vec<Vec<usize>>,
}

impl Inside {
    fn new(loops: &[Vec<[f64; 2]>]) -> Self {
        let mut segs = Vec::new();
        for l in loops {
            for i in 0..l.len() {
                segs.push([l[i], l[(i + 1) % l.len()]]);
            }
        }
        let lo = segs
            .iter()
            .map(|s| s[0][1].min(s[1][1]))
            .fold(f64::INFINITY, f64::min);
        let hi = segs
            .iter()
            .map(|s| s[0][1].max(s[1][1]))
            .fold(f64::NEG_INFINITY, f64::max);
        let n = 512;
        let h = ((hi - lo) / n as f64).max(1e-12);
        let mut bins = vec![Vec::new(); n + 1];
        for (i, s) in segs.iter().enumerate() {
            let a = (((s[0][1].min(s[1][1]) - lo) / h) as usize).min(n);
            let b = (((s[0][1].max(s[1][1]) - lo) / h) as usize).min(n);
            for bin in &mut bins[a..=b] {
                bin.push(i);
            }
        }
        Inside { segs, lo, h, bins }
    }
    fn contains(&self, p: [f64; 2]) -> bool {
        let k = ((p[1] - self.lo) / self.h).floor();
        if k < 0.0 || k as usize >= self.bins.len() {
            return false;
        }
        let mut inside = false;
        for &i in &self.bins[k as usize] {
            let [a, b] = self.segs[i];
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if x > p[0] {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

/// The flat layout of a face's parameters: a closed direction round a ring (so a face winding
/// round it is a polygon with a hole), the other across it; lengths roughly as on the part.
struct Layout {
    ring: Option<usize>,
    period: f64,
    lo: [f64; 2],
    scale: [f64; 2],
    /// Round a ring: the parameter across it and the radius it is laid at (increasing), so
    /// each circle round the ring is laid at about its length on the part.
    table: Vec<(f64, f64)>,
}

impl Layout {
    fn radius(&self, x: f64) -> f64 {
        let t = &self.table;
        let i = t.partition_point(|e| e.0 < x).clamp(1, t.len() - 1);
        let (a, b) = (t[i - 1], t[i]);
        a.1 + (x - a.0) * (b.1 - a.1) / (b.0 - a.0)
    }
    fn across(&self, r: f64) -> f64 {
        let t = &self.table;
        let i = t.partition_point(|e| e.1 < r).clamp(1, t.len() - 1);
        let (a, b) = (t[i - 1], t[i]);
        a.0 + (r - a.1) * (b.0 - a.0) / (b.1 - a.1)
    }
    fn to2(&self, uv: Uv) -> [f64; 2] {
        match self.ring {
            Some(k) => {
                let r = self.radius(uv[1 - k]);
                let a = TAU * (uv[k] - self.lo[k]) / self.period;
                [r * a.cos(), r * a.sin()]
            }
            None => [
                (uv[0] - self.lo[0]) * self.scale[0],
                (uv[1] - self.lo[1]) * self.scale[1],
            ],
        }
    }
    fn to_uv(&self, p: [f64; 2]) -> Uv {
        match self.ring {
            Some(k) => {
                let mut uv = [0.0; 2];
                uv[1 - k] = self.across(p[0].hypot(p[1]));
                uv[k] = self.lo[k] + p[1].atan2(p[0]) / TAU * self.period;
                uv
            }
            None => [
                self.lo[0] + p[0] / self.scale[0],
                self.lo[1] + p[1] / self.scale[1],
            ],
        }
    }
}

/// Mesh one face of the file within `chord`, in the file's coordinates.
fn mesh_face(
    ex: &Exchange,
    reader: &mut Reader,
    face: u64,
    flip: bool,
    chord: f64,
) -> Result<Vec<[P3; 3]>, String> {
    let (sid, same, loops) = face_loops(ex, face).ok_or("a face this mesher does not read")?;
    let surface = reader.surface(sid).map_err(|e| e.to_string())?;
    let s = Srf::new(&surface).ok_or("a surface kind this mesher does not evaluate")?;
    // the boundary in 3D
    let mut loops3: Vec<Vec<P3>> = Vec::new();
    for l in &loops {
        let mut pts: Vec<P3> = Vec::new();
        for &u in l {
            let e = sample_edge(ex, reader, u, 0.25 * chord)
                .ok_or("an edge this mesher does not read")?;
            for p in e {
                if pts.last().is_none_or(|q| dist(*q, p) > 1e-9) {
                    pts.push(p);
                }
            }
        }
        while pts.len() > 1 && dist(pts[0], *pts.last().unwrap_or(&pts[0])) <= 1e-9 {
            pts.pop();
        }
        if pts.len() >= 3 {
            loops3.push(pts);
        }
    }
    if loops3.is_empty() {
        return Err("a face without a boundary".into());
    }
    // ranges: the surface's own, else the boundary's
    let dom = s.domain();
    let mut range = [[0.0; 2]; 2];
    let guess: Vec<Option<Uv>> = loops3.iter().flatten().map(|p| s.invert(*p)).collect();
    for k in 0..2 {
        range[k] = match dom[k] {
            Some(r) => r,
            None => {
                let vals: Vec<f64> = guess.iter().flatten().map(|uv| uv[k]).collect();
                if vals.len() != guess.len() {
                    return Err("an unbounded surface".into());
                }
                let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                [lo, hi.max(lo + 1e-9)]
            }
        };
    }
    let tol = (chord * 0.1).max(1e-6);
    let mut period = [None, None];
    for k in 0..2 {
        if dom[k].is_some() && closes(&s, k, range, tol) {
            period[k] = Some(range[k][1] - range[k][0]);
        }
    }
    if period[0].is_some() && period[1].is_some() {
        return Err("a surface closed both ways".into());
    }
    let n = 40;
    let mut grid = Vec::with_capacity((n + 1) * (n + 1));
    for i in 0..=n {
        for j in 0..=n {
            let uv = [
                range[0][0] + (range[0][1] - range[0][0]) * i as f64 / n as f64,
                range[1][0] + (range[1][1] - range[1][0]) * j as f64 / n as f64,
            ];
            grid.push((uv, s.at(uv)));
        }
    }
    let patch = Patch {
        s,
        period,
        range,
        grid,
    };
    // the boundary on the parameters, each loop followed round
    let mut loops_uv: Vec<Vec<Uv>> = Vec::new();
    let mut worst = 0.0f64;
    for l in &loops3 {
        let mut out: Vec<Uv> = Vec::with_capacity(l.len());
        for p in l {
            let uv = patch.project(*p, out.last().copied(), chord);
            worst = worst.max(dist(patch.at(uv), *p));
            out.push(uv);
        }
        loops_uv.push(out);
    }
    if worst > 0.05_f64.max(20.0 * chord) {
        return Err(format!("its edges lie {worst:.3} mm off its surface"));
    }
    // the flat layout
    let ring = (0..2).find(|k| period[*k].is_some());
    let all: Vec<Uv> = loops_uv.iter().flatten().copied().collect();
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for uv in &all {
        for k in 0..2 {
            lo[k] = lo[k].min(uv[k]);
            hi[k] = hi[k].max(uv[k]);
        }
    }
    // mm per unit of each parameter, at the boundary's middle
    let mid = [0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1])];
    let rate = |k: usize| {
        let mut q = mid;
        let h = 1e-4 * (hi[k] - lo[k]).max(1e-9);
        q[k] += h;
        (dist(patch.at(q), patch.at(mid)) / h).max(1e-9)
    };
    let rates = [rate(0), rate(1)];
    let layout = match ring {
        Some(k) => {
            let o = 1 - k;
            let per = period[k].unwrap_or(TAU);
            lo[k] = patch.range[k][0];
            // across the ring: each circle's length round it, and the length across between them
            let (m, around) = (64usize, 64usize);
            let pt = |x: f64, y: f64| {
                let mut uv = [0.0; 2];
                uv[o] = x;
                uv[k] = lo[k] + per * y;
                patch.at(uv)
            };
            let xs: Vec<f64> = (0..=m)
                .map(|i| lo[o] + (hi[o] - lo[o]) * i as f64 / m as f64)
                .collect();
            let circle = |x: f64| {
                (0..around)
                    .map(|j| {
                        dist(
                            pt(x, j as f64 / around as f64),
                            pt(x, (j + 1) as f64 / around as f64),
                        )
                    })
                    .sum::<f64>()
            };
            let first = circle(xs[0]) / TAU;
            let width: f64 = (1..=m)
                .map(|i| {
                    (0..16)
                        .map(|j| dist(pt(xs[i - 1], j as f64 / 16.0), pt(xs[i], j as f64 / 16.0)))
                        .sum::<f64>()
                        / 16.0
                })
                .sum();
            let mut r = first.max(1e-3 * width.max(1e-9));
            let mut table = vec![(xs[0], r)];
            for i in 1..=m {
                let step = (0..16)
                    .map(|j| dist(pt(xs[i - 1], j as f64 / 16.0), pt(xs[i], j as f64 / 16.0)))
                    .sum::<f64>()
                    / 16.0;
                r += step.max(1e-6 * width.max(1e-9));
                table.push((xs[i], r));
            }
            Layout {
                ring: Some(k),
                period: per,
                lo,
                scale: [1.0; 2],
                table,
            }
        }
        None => Layout {
            ring: None,
            period: 0.0,
            lo,
            scale: rates,
            table: Vec::new(),
        },
    };
    let flat: Vec<Vec<[f64; 2]>> = loops_uv
        .iter()
        .map(|l| l.iter().map(|uv| layout.to2(*uv)).collect())
        .collect();
    let inside = Inside::new(&flat);

    // constrained triangulation of the boundary
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
        ConstrainedDelaunayTriangulation::new();
    for l in &flat {
        let mut hs = Vec::with_capacity(l.len());
        for p in l {
            hs.push(
                cdt.insert(Point2::new(p[0], p[1]))
                    .map_err(|e| format!("{e:?}"))?,
            );
        }
        for i in 0..hs.len() {
            let (a, b) = (hs[i], hs[(i + 1) % hs.len()]);
            if a != b {
                cdt.add_constraint_and_split(a, b, |p| p);
            }
        }
    }
    let budget = MAX_FACE_VERTICES.saturating_sub(cdt.num_vertices());
    cdt.refine(
        RefinementParameters::<f64>::new()
            .exclude_outer_faces(true)
            .keep_constraint_edges()
            .with_angle_limit(spade::AngleLimit::from_deg(20.0))
            .with_max_additional_vertices(budget / 2),
    );
    // refined until every facet is within the chord of the surface
    let at2 = |p: [f64; 2]| patch.at(layout.to_uv(p));
    for _ in 0..REFINE_ROUNDS {
        let mut add_pts: Vec<[f64; 2]> = Vec::new();
        for f in cdt.inner_faces() {
            let v = f.positions().map(|p| [p.x, p.y]);
            let c = [
                (v[0][0] + v[1][0] + v[2][0]) / 3.0,
                (v[0][1] + v[1][1] + v[2][1]) / 3.0,
            ];
            if !inside.contains(c) {
                continue;
            }
            let p3 = v.map(at2);
            let mid3 = mul(add(add(p3[0], p3[1]), p3[2]), 1.0 / 3.0);
            if dist(at2(c), mid3) > chord {
                add_pts.push(c);
                continue;
            }
            // edge midpoints, but for the boundary's (sampled within the chord already)
            for e in f.adjacent_edges() {
                if e.is_constraint_edge() {
                    continue;
                }
                let [a, b] = e.positions().map(|p| [p.x, p.y]);
                let m = [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])];
                if dist(at2(m), mul(add(at2(a), at2(b)), 0.5)) > chord {
                    add_pts.push(m);
                }
            }
        }
        if add_pts.is_empty() || cdt.num_vertices() + add_pts.len() > MAX_FACE_VERTICES {
            break;
        }
        for p in add_pts {
            cdt.insert(Point2::new(p[0], p[1]))
                .map_err(|e| format!("{e:?}"))?;
        }
    }
    // the facets inside, each facing as the face does
    let sense = if same != flip { 1.0 } else { -1.0 };
    let mut tris = Vec::new();
    for f in cdt.inner_faces() {
        let v = f.positions().map(|p| [p.x, p.y]);
        let c = [
            (v[0][0] + v[1][0] + v[2][0]) / 3.0,
            (v[0][1] + v[1][1] + v[2][1]) / 3.0,
        ];
        if !inside.contains(c) {
            continue;
        }
        let [a, b, cc] = v.map(at2);
        let n = cross(sub(b, a), sub(cc, a));
        if len(n) <= 1e-18 {
            continue;
        }
        let want = mul(patch.normal(layout.to_uv(c)), sense);
        tris.push(if dot(n, want) >= 0.0 {
            [a, b, cc]
        } else {
            [a, cc, b]
        });
    }
    if tris.is_empty() {
        return Err("no facets inside its boundary".into());
    }
    Ok(tris)
}

// --------------------------------------------------------------------------- the bodies

/// The faces of a solid item's shells, each with whether its shell turns it over.
fn item_faces(ex: &Exchange, item: u64) -> Vec<(u64, bool)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(item, false)];
    while let Some((id, flip)) = stack.pop() {
        if !seen.insert((id, flip)) {
            continue;
        }
        let Some(e) = ent(ex, id) else { continue };
        match e.name() {
            "ADVANCED_FACE" | "FACE_SURFACE" => out.push((id, flip)),
            "ORIENTED_FACE" | "ORIENTED_CLOSED_SHELL" | "ORIENTED_OPEN_SHELL" => {
                let p = e.params();
                let turn = p.get(3).and_then(Param::as_bool) == Some(false);
                if let Some(r) = p.get(2).and_then(Param::as_ref_id) {
                    stack.push((r, flip != turn));
                }
            }
            "MANIFOLD_SOLID_BREP"
            | "BREP_WITH_VOIDS"
            | "CLOSED_SHELL"
            | "OPEN_SHELL"
            | "SHELL_BASED_SURFACE_MODEL" => {
                for r in refs(e.params()) {
                    stack.push((r, flip));
                }
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The length unit (mm per file unit) of the representation that holds `item`.
fn item_units(ex: &Exchange, item: u64) -> solidcore_step::read::Units {
    for e in ex.entities.values() {
        let params = if e.is_complex() {
            e.record("REPRESENTATION")
        } else if e.name().ends_with("REPRESENTATION") {
            Some(e.params())
        } else {
            None
        };
        let Some(p) = params else { continue };
        let holds = p
            .get(1)
            .and_then(Param::as_list)
            .is_some_and(|l| l.iter().any(|x| x.as_ref_id() == Some(item)));
        if holds {
            if let Some(ctx) = p.get(2).and_then(Param::as_ref_id) {
                return solidcore_step::units::context(ex, ctx).units;
            }
        }
    }
    solidcore_step::read::Units::default()
}

/// Each body's placement (column-major 4×4) from the import's product tree.
fn placements(tree: &[solidcraft_kernel::StepNode]) -> HashMap<usize, [[f64; 4]; 4]> {
    fn mat(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
        // column-major: m[col][row]
        let mut m = [[0.0; 4]; 4];
        for c in 0..4 {
            for r in 0..4 {
                m[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
            }
        }
        m
    }
    fn walk(
        n: &solidcraft_kernel::StepNode,
        parent: &[[f64; 4]; 4],
        out: &mut HashMap<usize, [[f64; 4]; 4]>,
    ) {
        let m = mat(parent, &n.transform);
        for b in &n.bodies {
            out.entry(*b).or_insert(m);
        }
        for c in &n.children {
            walk(c, &m, out);
        }
    }
    let id = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut out = HashMap::new();
    for n in tree {
        walk(n, &id, &mut out);
    }
    out
}

fn place(m: &[[f64; 4]; 4], p: P3) -> P3 {
    let mut q = [0.0; 3];
    for (r, qr) in q.iter_mut().enumerate() {
        *qr = m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r];
    }
    q
}

/// The faces of each body the kernel left out, meshed here, and what could not be.
pub(crate) fn left_out(
    text: &str,
    imp: &solidcraft_kernel::StepImport,
    kernel_meshes: &[Vec<[P3; 3]>],
    whole: &[bool],
    chord: f64,
) -> (Vec<Vec<FaceMesh>>, Vec<String>) {
    let mut out: Vec<Vec<FaceMesh>> = imp.bodies.iter().map(|_| Vec::new()).collect();
    let mut notes = Vec::new();
    let Ok(ex) = p21::parse(text) else {
        return (out, notes);
    };
    let missing: Vec<Vec<(u64, bool)>> = imp
        .bodies
        .iter()
        .zip(whole)
        .map(|(b, all)| {
            let have: HashSet<u64> = if *all {
                HashSet::new()
            } else {
                b.face_origin.iter().copied().collect()
            };
            item_faces(&ex, b.source_item)
                .into_iter()
                .filter(|(f, _)| !have.contains(f))
                .collect()
        })
        .collect();
    if missing.iter().all(Vec::is_empty) {
        return (out, notes);
    }
    let places = placements(&imp.tree);
    for (bi, (b, faces)) in imp.bodies.iter().zip(&missing).enumerate() {
        if faces.is_empty() {
            continue;
        }
        let mut reader = Reader::new(&ex, item_units(&ex, b.source_item));
        let m = places.get(&bi).copied();
        // the body's kernel mesh vertices, to check each face meets them where it should
        let mut verts: HashMap<[i64; 3], Vec<P3>> = HashMap::new();
        let cell = 0.5;
        let key = |p: P3| p.map(|x| (x / cell).floor() as i64);
        for t in &kernel_meshes[bi] {
            for p in t {
                verts.entry(key(*p)).or_default().push(*p);
            }
        }
        let near = |p: P3| {
            let k = key(p);
            let mut best = f64::INFINITY;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(v) = verts.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                            for q in v {
                                best = best.min(dist(*q, p));
                            }
                        }
                    }
                }
            }
            best
        };
        for &(f, flip) in faces {
            match mesh_face(&ex, &mut reader, f, flip, chord) {
                Ok(mut tris) => {
                    if let Some(m) = &m {
                        for t in &mut tris {
                            *t = t.map(|p| place(m, p));
                        }
                    }
                    // the face's corners are the body's: a face elsewhere is a placement this
                    // mesher got wrong, and is left out rather than put there
                    let corners: Vec<P3> = face_loops(&ex, f)
                        .map(|(_, _, ls)| {
                            ls.iter()
                                .flatten()
                                .filter_map(|u| vertex(&ex, &reader, u.1))
                                .collect()
                        })
                        .unwrap_or_default();
                    let corners: Vec<P3> = corners
                        .into_iter()
                        .map(|p| m.as_ref().map_or(p, |m| place(m, p)))
                        .collect();
                    let off = corners.iter().map(|p| near(*p)).fold(0.0f64, f64::max);
                    if !verts.is_empty() && off > 0.05_f64.max(10.0 * chord) {
                        notes.push(format!("face #{f}: left out (meshed, but {off:.3} mm from the rest of its body)"));
                        continue;
                    }
                    out[bi].push(FaceMesh {
                        face: f,
                        triangles: tris,
                    });
                }
                Err(why) => notes.push(format!("face #{f}: left out ({why})")),
            }
        }
    }
    (out, notes)
}
