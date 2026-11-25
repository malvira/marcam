//! A 3D in-process stock as a tri-dexel: along each of the three axes of the part frame a
//! grid of rays `h` apart, each holding the solid's intervals along it, sorted. Cutting is a
//! boolean subtract of the tool's solid on every ray it crosses, so (unlike the radial map of
//! [`crate::sim`]) it holds holes and overhangs: a cut in from the side, a pocket under a
//! blade leaning over the hub. (Mar's impeller: the radial map could not tell what the
//! indexed roughing had taken from under the blades, so rest work on it went round air.)
//!
//! The part frame: X along the A axis, the bar from X `-length` to 0; machine frame = Rx(A)
//! part frame, the tool along machine +Z (see [`crate::kinematics`]).

/// A tool's solid: a flat end (a cylinder from its tip up) or a ball end (a capsule), `height`
/// long from the tip (its flutes, or more: what may not pass through the stock).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolSolid {
    pub radius: f64,
    pub ball: bool,
    pub height: f64,
}

/// The tri-dexel. Family `d` holds the rays along axis `d`; a ray is indexed by its other two
/// coordinates (axes `(d + 1) % 3` and `(d + 2) % 3`), each on its own grid.
#[derive(Debug, Clone)]
pub struct Dexel {
    pub h: f64,
    pub lo: [f64; 3],
    pub n: [usize; 3],
    rays: [Vec<Vec<[f32; 2]>>; 3],
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn axis(d: usize) -> [f64; 3] {
    let mut e = [0.0; 3];
    e[d] = 1.0;
    e
}

/// The tool's pose in the part frame: its tip and its direction (tip to shank), from a tip at
/// machine (x, y, z) with the part turned to A `a_deg` (the tool along machine +Z).
pub fn pose(x: f64, y: f64, z: f64, a_deg: f64) -> ([f64; 3], [f64; 3]) {
    // part = Rx(-A) machine
    let (s, c) = (-a_deg).to_radians().sin_cos();
    let rot = |v: [f64; 3]| [v[0], c * v[1] - s * v[2], s * v[1] + c * v[2]];
    (rot([x, y, z]), rot([0.0, 0.0, 1.0]))
}

/// The parameters `s` (along `e` from `o`) where the line meets the solid round the segment
/// `c0`..`c0 + len * t` (`t` unit) at `r` from it: a finite cylinder (`caps`: cut square at
/// both ends) or a capsule (rounded ends). Convex: one interval, or none.
fn line_vs_segment(
    o: [f64; 3],
    e: [f64; 3],
    c0: [f64; 3],
    t: [f64; 3],
    len: f64,
    r: f64,
    caps: bool,
) -> Option<(f64, f64)> {
    let w = sub(o, c0);
    let (wt, et) = (dot(w, t), dot(e, t));
    let wp = sub(w, [t[0] * wt, t[1] * wt, t[2] * wt]);
    let ep = sub(e, [t[0] * et, t[1] * et, t[2] * et]);
    let a = dot(ep, ep);
    // the infinite cylinder
    let (mut s0, mut s1) = if a < 1e-12 {
        if dot(wp, wp) > r * r {
            return None;
        }
        (f64::NEG_INFINITY, f64::INFINITY)
    } else {
        let b = 2.0 * dot(wp, ep);
        let c = dot(wp, wp) - r * r;
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let q = disc.sqrt();
        ((-b - q) / (2.0 * a), (-b + q) / (2.0 * a))
    };
    // the slab of the segment: 0 <= u <= len
    let (u0, u1) = if et.abs() < 1e-12 {
        if wt < 0.0 || wt > len {
            (f64::INFINITY, f64::NEG_INFINITY)
        } else {
            (f64::NEG_INFINITY, f64::INFINITY)
        }
    } else {
        let (p, q) = ((0.0 - wt) / et, (len - wt) / et);
        (p.min(q), p.max(q))
    };
    let mut lo = s0.max(u0);
    let mut hi = s1.min(u1);
    if !caps {
        // the round ends: the spheres at both ends of the segment
        for c in [
            c0,
            [c0[0] + t[0] * len, c0[1] + t[1] * len, c0[2] + t[2] * len],
        ] {
            let w = sub(o, c);
            let b = dot(w, e);
            let d = b * b - (dot(w, w) - r * r);
            if d >= 0.0 {
                let q = d.sqrt();
                lo = lo.min(-b - q);
                hi = hi.max(-b + q);
            }
        }
    }
    s0 = lo;
    s1 = hi;
    (s1 > s0).then_some((s0, s1))
}

impl ToolSolid {
    /// Where the line `o + s e` passes through the tool at tip `tip`, direction `dir` (unit).
    pub fn along(
        &self,
        tip: [f64; 3],
        dir: [f64; 3],
        o: [f64; 3],
        e: [f64; 3],
    ) -> Option<(f64, f64)> {
        if self.ball {
            let c0 = [
                tip[0] + dir[0] * self.radius,
                tip[1] + dir[1] * self.radius,
                tip[2] + dir[2] * self.radius,
            ];
            line_vs_segment(
                o,
                e,
                c0,
                dir,
                (self.height - self.radius).max(0.0),
                self.radius,
                false,
            )
        } else {
            line_vs_segment(o, e, tip, dir, self.height, self.radius, true)
        }
    }

    /// The box round the tool at a pose.
    fn bounds(&self, tip: [f64; 3], dir: [f64; 3]) -> ([f64; 3], [f64; 3]) {
        let top = [
            tip[0] + dir[0] * self.height,
            tip[1] + dir[1] * self.height,
            tip[2] + dir[2] * self.height,
        ];
        let mut lo = [0.0; 3];
        let mut hi = [0.0; 3];
        for k in 0..3 {
            lo[k] = tip[k].min(top[k]) - self.radius;
            hi[k] = tip[k].max(top[k]) + self.radius;
        }
        (lo, hi)
    }
}

impl Dexel {
    /// A round bar of `radius` on the A axis, X from `-length` to 0, rays `h` apart.
    pub fn bar(radius: f64, length: f64, h: f64) -> Dexel {
        Dexel::bar_within(
            radius,
            length,
            h,
            [f64::NEG_INFINITY; 3],
            [f64::INFINITY; 3],
        )
    }

    /// [`Dexel::bar`] with only the rays that pass through the box `lo`..`hi` (on the same
    /// grid): what is in the box, for less.
    pub fn bar_within(radius: f64, length: f64, h: f64, lo: [f64; 3], hi: [f64; 3]) -> Dexel {
        let full = [-length - h, -radius - h, -radius - h];
        let full_n = [
            ((length + 2.0 * h) / h).ceil() as usize + 1,
            ((2.0 * radius + 2.0 * h) / h).ceil() as usize + 1,
            ((2.0 * radius + 2.0 * h) / h).ceil() as usize + 1,
        ];
        let skip = [0, 1, 2].map(|k| {
            let s = ((lo[k] - full[k]) / h).floor().clamp(0.0, 1e9) as usize;
            s.min(full_n[k] - 1)
        });
        let lo = [0, 1, 2].map(|k| full[k] + skip[k] as f64 * h);
        let n = [0, 1, 2].map(|k| {
            let want = (((hi[k] - lo[k]) / h).ceil().max(0.0) + 1.0).min(1e9) as usize;
            want.clamp(1, full_n[k] - skip[k])
        });
        let mut dx = Dexel {
            h,
            lo,
            n,
            rays: [Vec::new(), Vec::new(), Vec::new()],
        };
        let inside_yz = |y: f64, z: f64| y * y + z * z <= radius * radius;
        for d in 0..3 {
            let (a, b) = ((d + 1) % 3, (d + 2) % 3);
            let mut rays = vec![Vec::new(); n[a] * n[b]];
            for ia in 0..n[a] {
                for ib in 0..n[b] {
                    let (ca, cb) = (dx.coord(a, ia), dx.coord(b, ib));
                    let mut p = [0.0; 3];
                    p[a] = ca;
                    p[b] = cb;
                    let iv = match d {
                        // along X: the bar's length where (y, z) is inside its circle
                        0 => inside_yz(p[1], p[2]).then_some([-length, 0.0]),
                        // along Y or Z: the chord through the circle, X inside the bar
                        _ => {
                            let other = if d == 1 { p[2] } else { p[1] };
                            let x = p[0];
                            let half = radius * radius - other * other;
                            ((-length..=0.0).contains(&x) && half > 0.0)
                                .then(|| [-half.sqrt(), half.sqrt()])
                        }
                    };
                    if let Some([s, t]) = iv {
                        rays[ia * n[b] + ib].push([s as f32, t as f32]);
                    }
                }
            }
            dx.rays[d] = rays;
        }
        dx
    }

    fn coord(&self, k: usize, i: usize) -> f64 {
        self.lo[k] + i as f64 * self.h
    }

    fn range(&self, k: usize, lo: f64, hi: f64) -> std::ops::Range<usize> {
        let i0 = ((lo - self.lo[k]) / self.h).ceil().max(0.0) as usize;
        let i1 = ((hi - self.lo[k]) / self.h).floor();
        if i1 < 0.0 {
            return 0..0;
        }
        i0..(i1 as usize).min(self.n[k] - 1) + 1
    }

    /// Each ray the tool crosses, and where along it: `f(family, ray, from, to)`.
    fn crossing(
        &self,
        tool: &ToolSolid,
        tip: [f64; 3],
        dir: [f64; 3],
        mut f: impl FnMut(usize, usize, f64, f64),
    ) {
        let (lo, hi) = tool.bounds(tip, dir);
        for d in 0..3 {
            let (a, b) = ((d + 1) % 3, (d + 2) % 3);
            for ia in self.range(a, lo[a], hi[a]) {
                for ib in self.range(b, lo[b], hi[b]) {
                    let mut o = [0.0; 3];
                    o[a] = self.coord(a, ia);
                    o[b] = self.coord(b, ib);
                    if let Some((s0, s1)) = tool.along(tip, dir, o, axis(d)) {
                        f(d, ia * self.n[b] + ib, s0, s1);
                    }
                }
            }
        }
    }

    /// Takes the tool at a pose out of the stock; returns the volume taken (mm3, the mean of
    /// the three families' estimates).
    pub fn subtract(&mut self, tool: &ToolSolid, tip: [f64; 3], dir: [f64; 3]) -> f64 {
        let mut cuts: Vec<(usize, usize, f64, f64)> = Vec::new();
        self.crossing(tool, tip, dir, |d, k, s0, s1| cuts.push((d, k, s0, s1)));
        let mut taken = 0.0;
        for (d, k, s0, s1) in cuts {
            let ray = &mut self.rays[d][k];
            if ray.is_empty() {
                continue;
            }
            let (s0, s1) = (s0 as f32, s1 as f32);
            let mut out: Vec<[f32; 2]> = Vec::with_capacity(ray.len() + 1);
            for &[a, b] in ray.iter() {
                if b <= s0 || a >= s1 {
                    out.push([a, b]);
                    continue;
                }
                taken += (b.min(s1) - a.max(s0)) as f64;
                if a < s0 {
                    out.push([a, s0]);
                }
                if b > s1 {
                    out.push([s1, b]);
                }
            }
            *ray = out;
        }
        taken * self.h * self.h / 3.0
    }

    /// As [`Dexel::subtract`], keeping each ray it changed as it was in `log`, for
    /// [`Dexel::restore`].
    pub fn subtract_logged(
        &mut self,
        tool: &ToolSolid,
        tip: [f64; 3],
        dir: [f64; 3],
        log: &mut Vec<(usize, usize, Vec<[f32; 2]>)>,
    ) -> f64 {
        let mut cuts: Vec<(usize, usize, f64, f64)> = Vec::new();
        self.crossing(tool, tip, dir, |d, k, s0, s1| cuts.push((d, k, s0, s1)));
        let mut taken = 0.0;
        for (d, k, s0, s1) in cuts {
            let ray = &self.rays[d][k];
            let (s0, s1) = (s0 as f32, s1 as f32);
            if !ray.iter().any(|&[a, b]| b > s0 && a < s1) {
                continue;
            }
            let mut out: Vec<[f32; 2]> = Vec::with_capacity(ray.len() + 1);
            for &[a, b] in ray.iter() {
                if b <= s0 || a >= s1 {
                    out.push([a, b]);
                    continue;
                }
                taken += (b.min(s1) - a.max(s0)) as f64;
                if a < s0 {
                    out.push([a, s0]);
                }
                if b > s1 {
                    out.push([s1, b]);
                }
            }
            log.push((d, k, std::mem::replace(&mut self.rays[d][k], out)));
        }
        taken * self.h * self.h / 3.0
    }

    /// Puts back what [`Dexel::subtract_logged`] took, latest first.
    pub fn restore(&mut self, log: Vec<(usize, usize, Vec<[f32; 2]>)>) {
        for (d, k, ray) in log.into_iter().rev() {
            self.rays[d][k] = ray;
        }
    }

    /// The stock inside the tool at a pose (mm3, as [`Dexel::subtract`] would take), without
    /// taking it.
    pub fn inside(&self, tool: &ToolSolid, tip: [f64; 3], dir: [f64; 3]) -> f64 {
        let mut v = 0.0;
        self.crossing(tool, tip, dir, |d, k, s0, s1| {
            for &[a, b] in &self.rays[d][k] {
                let (a, b) = (a as f64, b as f64);
                if b > s0 && a < s1 {
                    v += b.min(s1) - a.max(s0);
                }
            }
        });
        v * self.h * self.h / 3.0
    }

    /// The stock's volume (mm3, the mean of the three families').
    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for fam in &self.rays {
            for ray in fam {
                for &[a, b] in ray {
                    v += (b - a) as f64;
                }
            }
        }
        v * self.h * self.h / 3.0
    }

    /// Where along the ray from the axis at X `x`, part angle `phi_deg` (0: +Y, 90: +Z), the
    /// stock ends furthest out (0: none), up to `r_max`: sampled every `step` mm on the
    /// nearest rays.
    pub fn radial_top(&self, x: f64, phi_deg: f64, r_max: f64, step: f64) -> f64 {
        let (s, c) = phi_deg.to_radians().sin_cos();
        let mut r = r_max;
        while r > 0.0 {
            if self.solid_at([x, r * c, r * s]) {
                return r;
            }
            r -= step;
        }
        0.0
    }

    /// Seen from above with the part turned to A `a_deg` (the tool along machine +Z): the
    /// stock's top and bottom on the vertical line through machine (x, y), between `z_lo`
    /// and `z_hi`, sampled every `step` mm; None where the line misses it.
    pub fn column(
        &self,
        x: f64,
        y: f64,
        a_deg: f64,
        z_lo: f64,
        z_hi: f64,
        step: f64,
    ) -> Option<(f64, f64)> {
        let at = |z: f64| {
            let (p, _) = pose(x, y, z, a_deg);
            self.solid_at(p)
        };
        let mut top = None;
        let mut z = z_hi;
        while z >= z_lo {
            if at(z) {
                top = Some(z);
                break;
            }
            z -= step;
        }
        let top = top?;
        let mut z = z_lo;
        while z <= top {
            if at(z) {
                return Some((top, z));
            }
            z += step;
        }
        Some((top, top))
    }

    /// The stock's surface as the rays see it: each interval's two ends, in the part frame.
    /// (Intervals less than a ray's spacing apart count as one: a tool grazing a ray leaves a
    /// hairline gap in it, not a surface; a check of what is left read those inside the part
    /// as stock left on it.)
    pub fn surface_points(&self) -> Vec<[f64; 3]> {
        let mut out = Vec::new();
        for d in 0..3 {
            let (a, b) = ((d + 1) % 3, (d + 2) % 3);
            for (k, ray) in self.rays[d].iter().enumerate() {
                let (ia, ib) = (k / self.n[b], k % self.n[b]);
                let mut merged: Vec<[f32; 2]> = Vec::with_capacity(ray.len());
                for &[s0, s1] in ray {
                    match merged.last_mut() {
                        Some(m) if (s0 - m[1]) < self.h as f32 => m[1] = m[1].max(s1),
                        _ => merged.push([s0, s1]),
                    }
                }
                for &[s0, s1] in &merged {
                    for s in [s0, s1] {
                        let mut p = [0.0; 3];
                        p[a] = self.coord(a, ia);
                        p[b] = self.coord(b, ib);
                        p[d] = s as f64;
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    /// Whether a point is in the stock (the X-family ray nearest it).
    pub fn solid_at(&self, p: [f64; 3]) -> bool {
        let iy = ((p[1] - self.lo[1]) / self.h).round();
        let iz = ((p[2] - self.lo[2]) / self.h).round();
        if iy < 0.0 || iz < 0.0 || iy as usize >= self.n[1] || iz as usize >= self.n[2] {
            return false;
        }
        let k = iy as usize * self.n[2] + iz as usize;
        let x = p[0] as f32;
        self.rays[0][k].iter().any(|&[a, b]| a <= x && x <= b)
    }
}

/// A rapid into the stock by less than this (mm) is clear of it: the tool along a wall it cut,
/// the stock's rays and the sampling of the cut leaving thousandths of it.
pub const GRAZE: f64 = 0.01;

/// What a replay of moves through the stock found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replay {
    /// Rapid moves that would pass through the stock (more than a trace): their move index
    /// and program.
    pub rapid_hits: Vec<(u16, usize)>,
    /// Rapid moves not through it by the slack but into it by more than [`GRAZE`]: a graze.
    pub rapid_grazes: Vec<(u16, usize)>,
    /// Per operation (its tag, as [`crate::sim::Segment::op`]): the biggest bite, stock taken
    /// per mm of travel (mm3/mm), and the move index where.
    pub worst_bite: Vec<(u16, f64, usize)>,
}

/// The tool's tip and direction a fraction `f` along a move from `from`.
fn seg_at(seg: &crate::sim::Segment, from: [f64; 3], f: f64) -> ([f64; 3], [f64; 3]) {
    let p = [0, 1, 2].map(|k| from[k] + (seg.to[k] - from[k]) * f);
    match seg.index_a {
        // (a pose that turns as it goes: A from where it started)
        Some(a) => pose(
            p[0],
            p[1],
            p[2],
            seg.a_from.map_or(a, |a0| a0 + (a - a0) * f),
        ),
        None => pose(p[0], 0.0, p[1], p[2]),
    }
}

/// The box round the tool `t` all along a move (sampled at most 0.25 mm apart), grown by
/// `margin`.
pub fn sweep_box(
    seg: &crate::sim::Segment,
    t: &ToolSolid,
    margin: f64,
) -> Option<([f64; 3], [f64; 3])> {
    let from = seg.from?;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let (a, _) = seg_at(seg, from, 0.0);
    let (b, _) = seg_at(seg, from, 1.0);
    let chord = (0..3).map(|k| (b[k] - a[k]).powi(2)).sum::<f64>().sqrt();
    let turn = match seg.index_a {
        Some(a) => seg.a_from.map_or(0.0, |a0| (a - a0).abs()),
        None => (seg.to[2] - from[2]).abs(),
    };
    let reach = from[1].abs().max(seg.to[1].abs()) + t.height + t.radius;
    let n = ((chord.max(turn.to_radians() * reach) / 0.25).ceil() as usize).clamp(1, 100_000);
    for i in 0..=n {
        let (tip, dir) = seg_at(seg, from, i as f64 / n as f64);
        let (l, h) = t.bounds(tip, dir);
        for k in 0..3 {
            lo[k] = lo[k].min(l[k] - margin);
            hi[k] = hi[k].max(h[k] + margin);
        }
    }
    Some((lo, hi))
}

/// Replays `segs` (in order) through the stock: feed moves take the tool out of it, sampled
/// at most `step` mm apart; rapids must pass clear of it (`slack` mm narrower and higher).
pub fn replay(
    dx: &mut Dexel,
    segs: &[crate::sim::Segment],
    tool_for: &dyn Fn(u32) -> Option<ToolSolid>,
    step: f64,
    slack: f64,
) -> Replay {
    let mut out = Replay::default();
    // (a bite is what the tool takes per mm its tip moved since the last place it cut from)
    let mut last: Option<[f64; 3]> = None;
    for seg in segs {
        let (Some(from), Some(t)) = (seg.from, seg.tool.and_then(tool_for)) else {
            continue;
        };
        // tip and direction at a parameter along the move
        let at = |f: f64| seg_at(seg, from, f);
        // its length as the tool's tip travels (the arc at its radius for turns)
        let len = match seg.index_a {
            Some(_) => (0..3)
                .map(|k| (seg.to[k] - from[k]).powi(2))
                .sum::<f64>()
                .sqrt(),
            None => {
                let arc =
                    0.5 * (from[1] + seg.to[1]).abs() * (seg.to[2] - from[2]).to_radians().abs();
                ((seg.to[0] - from[0]).powi(2) + (seg.to[1] - from[1]).powi(2))
                    .sqrt()
                    .max(arc)
            }
        };
        let n = ((len / step).ceil() as usize).max(1);
        let d = len / n as f64;
        if seg.rapid {
            // (the tool thinned by `by` and lifted as much: in the stock deeper than that)
            let into = |by: f64| {
                let thin = ToolSolid {
                    radius: (t.radius - by).max(0.01),
                    ..t
                };
                (0..=n).any(|i| {
                    let (tip, dir) = at(i as f64 / n as f64);
                    let tip = [
                        tip[0] + dir[0] * by,
                        tip[1] + dir[1] * by,
                        tip[2] + dir[2] * by,
                    ];
                    dx.inside(&thin, tip, dir) > 1e-3
                })
            };
            if into(slack) {
                out.rapid_hits.push((seg.program, seg.move_index));
            } else if slack > GRAZE && into(GRAZE) {
                out.rapid_grazes.push((seg.program, seg.move_index));
            }
            last = Some(at(1.0).0);
            continue;
        }
        let _ = d;
        for i in 1..=n {
            let (tip, dir) = at(i as f64 / n as f64);
            let moved = last.map_or(step, |q: [f64; 3]| {
                ((tip[0] - q[0]).powi(2) + (tip[1] - q[1]).powi(2) + (tip[2] - q[2]).powi(2)).sqrt()
            });
            last = Some(tip);
            let bite = dx.subtract(&t, tip, dir) / moved.max(0.05);
            match out.worst_bite.iter_mut().find(|w| w.0 == seg.op) {
                Some(w) if bite > w.1 => *w = (seg.op, bite, seg.move_index),
                Some(_) => {}
                None => out.worst_bite.push((seg.op, bite, seg.move_index)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_holds_its_volume_and_a_plunge_takes_the_tool_s() {
        let (r, l) = (10.0, 30.0);
        let mut d = Dexel::bar(r, l, 0.1);
        let v = d.volume();
        let exact = std::f64::consts::PI * r * r * l;
        assert!((v - exact).abs() < 0.01 * exact, "{v} vs {exact}");
        // a flat Ø4 tool down from above the bar to 2 mm under its top at X -15 (A 0)
        let t = ToolSolid {
            radius: 2.0,
            ball: false,
            height: 20.0,
        };
        let (tip, dir) = pose(-15.0, 0.0, 8.0, 0.0);
        let inside = d.inside(&t, tip, dir);
        let taken = d.subtract(&t, tip, dir);
        assert!((inside - taken).abs() < 1e-3 * taken, "{inside} {taken}");
        // the cylinder's volume above z 8 in the bar: about pi 2^2 2 (the top is curved)
        assert!(taken > 20.0 && taken < 26.0, "{taken}");
        assert!(
            (d.volume() - (v - taken)).abs() < 0.02 * taken,
            "{} {} {}",
            d.volume(),
            v,
            taken
        );
        // nothing left where it was
        assert!(d.inside(&t, tip, dir) < 1e-4);
        // turned to A 90 the same tool comes down from +Y (part frame): a second hole
        let (tip2, dir2) = pose(-15.0, 0.0, 8.0, 90.0);
        assert!(
            (dir2[1] - 1.0).abs() < 1e-9 || (dir2[1] + 1.0).abs() < 1e-9,
            "{dir2:?}"
        );
        assert!(d.subtract(&t, tip2, dir2) > 20.0);
    }

    #[test]
    fn a_cut_from_the_side_leaves_the_stock_above_it() {
        // (what the radial map could not hold) a ball across the bar at X -10 at 4 mm from
        // the axis, coming in from the side (along Y): under it the stock stays, above it too
        let mut d = Dexel::bar(10.0, 20.0, 0.1);
        let t = ToolSolid {
            radius: 1.5,
            ball: true,
            height: 30.0,
        };
        // the tool along -Y (part frame), its tip at y -2, z 4: a horizontal hole at z 4
        let tip = [-10.0, -2.0, 4.0];
        let dir = [0.0, -1.0, 0.0];
        d.subtract(&t, tip, dir);
        assert!(!d.solid_at([-10.0, -5.0, 4.0]));
        assert!(d.solid_at([-10.0, -5.0, 7.0]));
        assert!(d.solid_at([-10.0, -5.0, 1.0]));
        // along the ray from the axis at phi -90 (towards -Z), and at 90 (+Z): stock to 10
        assert!((d.radial_top(-10.0, 90.0, 12.0, 0.05) - 10.0).abs() < 0.2);
    }
}
