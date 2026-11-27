//! The playback's stock drawn exact (Mar: "there cant be visual artifacts as that defeats the
//! point of the sim"): not a mesh off the tri-dexel's grid, which shows the grid at some zoom
//! however it is smoothed, but the bar minus the tool's swept volumes, met by each pixel's ray.
//!
//! Every cutting move becomes one or more *pieces*: the tool (flat, ball or V, as the
//! tri-dexel takes it: [`ToolSolid`]) moved in a straight line with its axis held, in the part's
//! frame. A straight move with A held is one piece, exact. A move that turns (a rotary move, a
//! pose that turns) is cut into pieces short enough that no point of the tool strays more than
//! [`TOL`] from where the real motion takes it.
//!
//! A piece's swept volume is the Minkowski sum of a convex tool and a segment: convex, so a ray
//! meets it in one interval. With the ray `o + t d` and the piece's parameter `s` (0 to 1
//! along its move), the tool's own coordinates of the ray's point are affine in (t, s): each
//! part of the tool (a cylinder, the ball, the V's cone) is one quadratic and a few linear
//! constraints there, and the interval's ends are the region's extremes in t, found in closed
//! form (the corners, the edges' crossings with the quadric, the quadric's own extremes).
//!
//! A ray's visible point: from where it enters the bar, while it is inside a piece (played
//! before the cutoff), on to where that piece's interval ends (the furthest of those it is
//! in). Where it stops is the surface: the bar's own, or the exit of the piece that cut it
//! there, whose tool gives its normal and whose operation its colour. A bounding volume
//! hierarchy over the pieces (each node with the first segment under it, so a playback's
//! place is a cutoff) finds the pieces a point is in.
//!
//! The same is done per pixel on the GPU (web/gpu/exact.js); this is its reference.

use crate::dexel::{seg_at, ToolSolid};
use crate::sim::Segment;

/// How far (mm) any point of the tool may stray from its real path in a turning move's pieces.
pub const TOL: f64 = 0.0005;

/// A straight piece is cut at most this many tool diameters long (the hierarchy's boxes tight).
const MAX_LEN_DIAMETERS: f64 = 2.0;

/// A tool moved in a straight line, its axis held, in the part's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    /// The tip where it starts.
    pub p: [f64; 3],
    /// Its move.
    pub v: [f64; 3],
    /// The tool's axis (tip to shank, unit).
    pub a: [f64; 3],
    /// How much of the tool (from its tip) can reach the bar.
    pub len: f64,
    /// Its tool, an index into [`Scene::tools`].
    pub tool: u16,
    /// The operation cutting (its tag: id + 1).
    pub op: u16,
    /// The playback's segment it is part of.
    pub seg: u32,
    /// A piece that turns: its motion exactly (then `p`, `v` and `a` are its chord, for show).
    pub turn: Option<Turn>,
}

/// A turning piece's motion: the machine's tip from `m0` by `dm` and A from `a0` by `da`
/// (radians), both linear in the piece's parameter, as the machine moves; looked at in
/// `samples` poses, as many as keep the tool within [`TOL`] between them (see [`span_at`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub m0: [f64; 3],
    pub dm: [f64; 3],
    pub a0: f64,
    pub da: f64,
    pub samples: u32,
    /// How fast (mm per the piece's parameter) any point of its tool within the bar moves at
    /// most.
    pub speed: f64,
}

impl Turn {
    /// The tool's tip and axis (part frame) at `f` (0 to 1) along the piece.
    pub fn at(&self, f: f64) -> ([f64; 3], [f64; 3]) {
        let m = add(self.m0, mul(self.dm, f));
        let (s, c) = (-(self.a0 + self.da * f)).sin_cos();
        (
            [m[0], c * m[1] - s * m[2], s * m[1] + c * m[2]],
            [0.0, -s, c],
        )
    }
}

/// A node of the hierarchy: its box, and its children (`count` 0: at `first` and `first + 1`)
/// or its pieces (`count` of them from `first`), and the first segment under it.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    pub first: u32,
    pub count: u32,
    pub min_seg: u32,
}

/// The bar and the pieces that cut it.
pub struct Scene {
    pub radius: f64,
    pub length: f64,
    pub tools: Vec<ToolSolid>,
    pub pieces: Vec<Piece>,
    pub nodes: Vec<Node>,
    /// The hierarchy's leaves' boxes: each a stretch of a piece's tool ([`piece_boxes`]), the
    /// piece's index.
    pub prims: Vec<u32>,
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    let l = norm(a);
    if l > 0.0 {
        mul(a, 1.0 / l)
    } else {
        a
    }
}

/// Where the line `tip + s dir` leaves the cylinder of radius `rc` round X (the largest `s`
/// inside it), or None when it never is inside.
fn exit_along(tip: [f64; 3], dir: [f64; 3], rc: f64) -> Option<f64> {
    let (py, pz, dy, dz) = (tip[1], tip[2], dir[1], dir[2]);
    let a = dy * dy + dz * dz;
    let c = py * py + pz * pz - rc * rc;
    if a < 1e-12 {
        return (c <= 0.0).then_some(f64::INFINITY);
    }
    let b = py * dy + pz * dz;
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    Some((-b + disc.sqrt()) / a)
}

/// How much of a tool moved from `tip` by `v` along `dir` can reach the bar (radius `rb`):
/// the furthest its axis is inside the cylinder the tool's radius wider, anywhere along the
/// move (None: nowhere).
fn reach(tip: [f64; 3], v: [f64; 3], dir: [f64; 3], rc: f64) -> Option<f64> {
    let at = |f: f64| exit_along(add(tip, mul(v, f)), dir, rc);
    // (the set of (f, s) inside is convex: the furthest s concave in f, its maximum found by a
    // golden search where the ends do not hold it)
    let mut best = [0.0, 0.5, 1.0]
        .iter()
        .filter_map(|&f| at(f))
        .fold(f64::NEG_INFINITY, f64::max);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let g = 0.5 * (5f64.sqrt() - 1.0);
    let val = |f: f64| at(f).unwrap_or(f64::NEG_INFINITY);
    let (mut x1, mut x2) = (hi - g * (hi - lo), lo + g * (hi - lo));
    let (mut f1, mut f2) = (val(x1), val(x2));
    for _ in 0..48 {
        if f1 < f2 {
            lo = x1;
            x1 = x2;
            f1 = f2;
            x2 = lo + g * (hi - lo);
            f2 = val(x2);
        } else {
            hi = x2;
            x2 = x1;
            f2 = f1;
            x1 = hi - g * (hi - lo);
            f1 = val(x1);
        }
    }
    best = best.max(f1).max(f2);
    (best > f64::NEG_INFINITY).then_some(best)
}

/// The machine's tip and A (radians) a fraction `f` along a move from `from` (as
/// [`seg_at`] poses it).
fn machine_at(seg: &Segment, from: [f64; 3], f: f64) -> ([f64; 3], f64) {
    let p = [0, 1, 2].map(|k| from[k] + (seg.to[k] - from[k]) * f);
    match seg.index_a {
        Some(a) => (p, seg.a_from.map_or(a, |a0| a0 + (a - a0) * f).to_radians()),
        None => ([p[0], 0.0, p[1]], p[2].to_radians()),
    }
}

/// How far (mm) the tool moves between a turning piece's poses at most: what keeps a ray that
/// meets the tool between two of them from passing both by more than `tol` (a scallop of
/// `tol` on the tool's radius).
fn pose_step(t: &ToolSolid, tol: f64) -> f64 {
    (8.0 * t.radius.max(0.05) * tol).sqrt()
}

/// The pieces of the cutting moves of `segs` (the playback's, in order) on a bar of `radius`,
/// X from `-length` to 0. `tool_of`: a tool number's solid, as the tri-dexel takes it.
pub fn pieces(
    radius: f64,
    length: f64,
    segs: &[Segment],
    tool_of: &dyn Fn(u32) -> Option<ToolSolid>,
    tol: f64,
) -> (Vec<ToolSolid>, Vec<Piece>) {
    let mut numbers: Vec<u32> = Vec::new();
    let mut tools: Vec<ToolSolid> = Vec::new();
    let mut out = Vec::new();
    for (k, seg) in segs.iter().enumerate() {
        if seg.rapid {
            continue;
        }
        let (Some(from), Some(n)) = (seg.from, seg.tool) else {
            continue;
        };
        let ti = match numbers.iter().position(|&m| m == n) {
            Some(i) => i,
            None => match tool_of(n) {
                Some(t) => {
                    numbers.push(n);
                    tools.push(t);
                    tools.len() - 1
                }
                None => continue,
            },
        };
        let t = tools[ti];
        let rc = radius + t.radius;
        // (how far it turns: a rotary move's A, a turning pose's)
        let turn = match seg.index_a {
            Some(a) => seg.a_from.map_or(0.0, |a0| (a - a0).to_radians().abs()),
            None => (seg.to[2] - from[2]).to_radians().abs(),
        };
        let at = |f: f64| seg_at(seg, from, f);
        if turn <= 1e-12 {
            // straight, its axis held: exact as it is (in parts, the hierarchy's boxes tight)
            let (p0, a) = at(0.0);
            let (p1, _) = at(1.0);
            let v = sub(p1, p0);
            let parts =
                ((norm(v) / (MAX_LEN_DIAMETERS * 2.0 * t.radius).max(0.5)).ceil() as usize).max(1);
            for j in 0..parts {
                let p = add(p0, mul(v, j as f64 / parts as f64));
                let w = mul(v, 1.0 / parts as f64);
                let Some(r) = reach(p, w, a, rc) else {
                    continue;
                };
                if r < 0.0 {
                    continue;
                }
                let len = r.min(t.height);
                let lo = p[0].min(p[0] + w[0]) - t.radius - len;
                let hi = p[0].max(p[0] + w[0]) + t.radius + len;
                if hi < -length || lo > 0.0 {
                    continue;
                }
                out.push(Piece {
                    p,
                    v: w,
                    a,
                    len,
                    tool: ti as u16,
                    op: seg.op,
                    seg: k as u32,
                    turn: None,
                });
            }
            continue;
        }
        // turning: pieces of at most 2 degrees and a dozen poses, each pose at most
        // `pose_step` from the next for every point of the tool within the bar
        let (m0, a0) = machine_at(seg, from, 0.0);
        let (m1, a1) = machine_at(seg, from, 1.0);
        let speed = norm(sub(m1, m0)) + rc * (a1 - a0).abs();
        let step = pose_step(&t, tol);
        let np = ((speed / (11.0 * step)).ceil() as usize)
            .max((turn.to_degrees() / 2.0).ceil() as usize)
            .clamp(1, 1 << 20);
        for i in 0..np {
            let (f0, f1) = (i as f64 / np as f64, (i + 1) as f64 / np as f64);
            let (pm0, pa0) = machine_at(seg, from, f0);
            let (pm1, pa1) = machine_at(seg, from, f1);
            let samples = ((speed / np as f64 / step).ceil() as u32 + 1).clamp(3, 16);
            let dm = sub(pm1, pm0);
            let tr = Turn {
                m0: pm0,
                dm,
                a0: pa0,
                da: pa1 - pa0,
                samples,
                speed: norm(dm) + (rc + norm(dm)) * (pa1 - pa0).abs(),
            };
            // the tool's reach into the bar at its poses (and a pose's step more)
            let mut len = f64::NEG_INFINITY;
            for j in 0..=8 {
                let (tip, dir) = tr.at(j as f64 / 8.0);
                if let Some(e) = exit_along(tip, dir, rc) {
                    len = len.max(e);
                }
            }
            if len == f64::NEG_INFINITY {
                // (a turn that never comes near the bar at its poses: a pose's step from it at
                // most, so the bar further than that: nothing cut)
                let near = (0..=8).any(|j| {
                    let (tip, dir) = tr.at(j as f64 / 8.0);
                    exit_along(tip, dir, rc + speed / np as f64 / 8.0).is_some()
                });
                if !near {
                    continue;
                }
                len = 0.0;
            }
            let len = (len + speed / np as f64 / 8.0).min(t.height);
            if len < 0.0 {
                continue;
            }
            let (p0, d0) = tr.at(0.0);
            let (p1, d1) = tr.at(1.0);
            out.push(Piece {
                p: p0,
                v: sub(p1, p0),
                a: unit(add(d0, d1)),
                len,
                tool: ti as u16,
                op: seg.op,
                seg: k as u32,
                turn: Some(tr),
            });
        }
    }
    (tools, out)
}

/// A static tool's box.
fn tool_box(t: &ToolSolid, tip: [f64; 3], a: [f64; 3], len: f64) -> ([f64; 3], [f64; 3]) {
    let len = if t.ball { len.max(2.0 * t.radius) } else { len };
    let top = add(tip, mul(a, len));
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for k in 0..3 {
        let e = t.radius * (1.0 - a[k] * a[k]).max(0.0).sqrt();
        lo[k] = tip[k].min(top[k]) - e;
        hi[k] = tip[k].max(top[k]) + e;
    }
    (lo, hi)
}

/// A piece's box.
pub fn piece_box(pc: &Piece, t: &ToolSolid) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut grow = |b: ([f64; 3], [f64; 3]), m: f64| {
        for k in 0..3 {
            lo[k] = lo[k].min(b.0[k] - m);
            hi[k] = hi[k].max(b.1[k] + m);
        }
    };
    match pc.turn {
        None => {
            grow(tool_box(t, pc.p, pc.a, pc.len), 0.0);
            grow(tool_box(t, add(pc.p, pc.v), pc.a, pc.len), 0.0);
        }
        Some(tr) => {
            // (its poses 1/32 apart, each grown by how far any of its points goes before the next)
            let n = 32;
            let (m0, _) = tr.at(0.0);
            let reach = norm(m0) + pc.len + 2.0 * t.radius;
            let m = (norm(tr.dm) + reach * tr.da.abs()) / n as f64;
            for j in 0..=n {
                let (tip, a) = tr.at(j as f64 / n as f64);
                grow(tool_box(t, tip, a, pc.len), m);
            }
        }
    }
    (lo, hi)
}

/// The box of a stretch of a tool from `z0` to `z1` along its axis `a` from `tip`.
fn chunk_box(tip: [f64; 3], a: [f64; 3], z0: f64, z1: f64, r: f64) -> ([f64; 3], [f64; 3]) {
    let (p, q) = (add(tip, mul(a, z0)), add(tip, mul(a, z1)));
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for k in 0..3 {
        let e = r * (1.0 - a[k] * a[k]).max(0.0).sqrt();
        lo[k] = p[k].min(q[k]) - e;
        hi[k] = p[k].max(q[k]) + e;
    }
    (lo, hi)
}

/// A piece's boxes: one for each stretch of its tool along its axis, at most this long (tool
/// radii) each. A point in the air under many passes is in few of them: the tall boxes of
/// whole tools (shanks leaning over a channel) held it in hundreds.
const CHUNK_RADII: f64 = 2.0;

pub fn piece_boxes(pc: &Piece, t: &ToolSolid) -> Vec<([f64; 3], [f64; 3])> {
    let top = if t.ball {
        pc.len.max(2.0 * t.radius)
    } else {
        pc.len
    };
    let step = (CHUNK_RADII * t.radius).max(0.5);
    let k = ((top / step).ceil() as usize).max(1);
    let zs: Vec<(f64, f64)> = (0..k)
        .map(|i| (top * i as f64 / k as f64, top * (i + 1) as f64 / k as f64))
        .collect();
    let mut out = Vec::with_capacity(k);
    for &(z0, z1) in &zs {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        let mut grow = |b: ([f64; 3], [f64; 3]), m: f64| {
            for k in 0..3 {
                lo[k] = lo[k].min(b.0[k] - m);
                hi[k] = hi[k].max(b.1[k] + m);
            }
        };
        match pc.turn {
            None => {
                grow(chunk_box(pc.p, pc.a, z0, z1, t.radius), 0.0);
                grow(chunk_box(add(pc.p, pc.v), pc.a, z0, z1, t.radius), 0.0);
            }
            Some(tr) => {
                let n = 32;
                let (m0, _) = tr.at(0.0);
                let reach = norm(m0) + top + 2.0 * t.radius;
                let m = (norm(tr.dm) + reach * tr.da.abs()) / n as f64;
                for j in 0..=n {
                    let (tip, a) = tr.at(j as f64 / n as f64);
                    grow(chunk_box(tip, a, z0, z1, t.radius), m);
                }
            }
        }
        out.push((lo, hi));
    }
    out
}

impl Scene {
    pub fn new(radius: f64, length: f64, tools: Vec<ToolSolid>, pieces: Vec<Piece>) -> Scene {
        let mut s = Scene {
            radius,
            length,
            tools,
            pieces,
            nodes: Vec::new(),
            prims: Vec::new(),
        };
        s.build();
        s
    }

    /// The job's bar and the cutting moves of `segs`.
    pub fn of(
        radius: f64,
        length: f64,
        segs: &[Segment],
        tool_of: &dyn Fn(u32) -> Option<ToolSolid>,
    ) -> Scene {
        let (tools, pieces) = pieces(radius, length, segs, tool_of, TOL);
        Scene::new(radius, length, tools, pieces)
    }

    /// The hierarchy: binned surface-area splits, at most 4 pieces a leaf.
    fn build(&mut self) {
        let mut boxes: Vec<([f64; 3], [f64; 3])> = Vec::new();
        let mut owner: Vec<u32> = Vec::new();
        for (i, p) in self.pieces.iter().enumerate() {
            for b in piece_boxes(p, &self.tools[p.tool as usize]) {
                boxes.push(b);
                owner.push(i as u32);
            }
        }
        let n = boxes.len();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        let mut nodes: Vec<Node> = Vec::with_capacity(2 * n / 2 + 1);
        nodes.push(Node {
            lo: [0.0; 3],
            hi: [0.0; 3],
            first: 0,
            count: 0,
            min_seg: 0,
        });
        // (start, end, node)
        let mut stack = vec![(0usize, n, 0usize)];
        while let Some((s, e, ni)) = stack.pop() {
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            let mut clo = [f64::INFINITY; 3];
            let mut chi = [f64::NEG_INFINITY; 3];
            let mut min_seg = u32::MAX;
            for &i in &idx[s..e] {
                let (l, h) = boxes[i as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(l[k]);
                    hi[k] = hi[k].max(h[k]);
                    let c = 0.5 * (l[k] + h[k]);
                    clo[k] = clo[k].min(c);
                    chi[k] = chi[k].max(c);
                }
                min_seg = min_seg.min(self.pieces[owner[i as usize] as usize].seg);
            }
            let f = |x: f64, down: bool| {
                let y = x as f32;
                // (rounded outward: a box never smaller than its pieces')
                if down && (y as f64) > x {
                    f32::from_bits(if y > 0.0 {
                        y.to_bits() - 1
                    } else {
                        y.to_bits() + 1
                    })
                } else if !down && (y as f64) < x {
                    f32::from_bits(if y >= 0.0 {
                        y.to_bits() + 1
                    } else {
                        y.to_bits() - 1
                    })
                } else {
                    y
                }
            };
            nodes[ni].lo = [f(lo[0], true), f(lo[1], true), f(lo[2], true)];
            nodes[ni].hi = [f(hi[0], false), f(hi[1], false), f(hi[2], false)];
            nodes[ni].min_seg = if e > s { min_seg } else { 0 };
            let count = e - s;
            if count <= 4 {
                nodes[ni].first = s as u32;
                nodes[ni].count = count as u32;
                continue;
            }
            // binned SAH on the centroids
            const B: usize = 16;
            let mut best = (f64::INFINITY, 0usize, 0usize);
            let area = |l: [f64; 3], h: [f64; 3]| {
                let d = [h[0] - l[0], h[1] - l[1], h[2] - l[2]];
                if d[0] < 0.0 {
                    0.0
                } else {
                    d[0] * d[1] + d[1] * d[2] + d[2] * d[0]
                }
            };
            for ax in 0..3 {
                let ext = chi[ax] - clo[ax];
                if ext <= 1e-12 {
                    continue;
                }
                let mut bl = [([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]); B];
                let mut bc = [0usize; B];
                for &i in &idx[s..e] {
                    let (l, h) = boxes[i as usize];
                    let c = 0.5 * (l[ax] + h[ax]);
                    let b = (((c - clo[ax]) / ext) * B as f64).min(B as f64 - 1.0) as usize;
                    bc[b] += 1;
                    for k in 0..3 {
                        bl[b].0[k] = bl[b].0[k].min(l[k]);
                        bl[b].1[k] = bl[b].1[k].max(h[k]);
                    }
                }
                let mut right = [(0.0f64, 0usize); B];
                let (mut rl, mut rh, mut rc) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3], 0usize);
                for b in (1..B).rev() {
                    rc += bc[b];
                    for k in 0..3 {
                        rl[k] = rl[k].min(bl[b].0[k]);
                        rh[k] = rh[k].max(bl[b].1[k]);
                    }
                    right[b] = (area(rl, rh), rc);
                }
                let (mut ll, mut lh, mut lc) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3], 0usize);
                for b in 0..B - 1 {
                    lc += bc[b];
                    for k in 0..3 {
                        ll[k] = ll[k].min(bl[b].0[k]);
                        lh[k] = lh[k].max(bl[b].1[k]);
                    }
                    let (ra, rcn) = right[b + 1];
                    if lc == 0 || rcn == 0 {
                        continue;
                    }
                    let cost = area(ll, lh) * lc as f64 + ra * rcn as f64;
                    if cost < best.0 {
                        best = (cost, ax, b + 1);
                    }
                }
            }
            let mid = if best.0.is_finite() {
                let (ax, b) = (best.1, best.2);
                let ext = chi[ax] - clo[ax];
                let split = clo[ax] + ext * b as f64 / B as f64;
                let slice = &mut idx[s..e];
                let mut i = 0;
                let mut j = slice.len();
                while i < j {
                    let (l, h) = boxes[slice[i] as usize];
                    if 0.5 * (l[ax] + h[ax]) < split {
                        i += 1;
                    } else {
                        j -= 1;
                        slice.swap(i, j);
                    }
                }
                s + i
            } else {
                s + count / 2
            };
            let mid = if mid == s || mid == e {
                s + count / 2
            } else {
                mid
            };
            let l = nodes.len();
            nodes[ni].first = l as u32;
            nodes[ni].count = 0;
            for _ in 0..2 {
                nodes.push(Node {
                    lo: [0.0; 3],
                    hi: [0.0; 3],
                    first: 0,
                    count: 0,
                    min_seg: 0,
                });
            }
            stack.push((s, mid, l));
            stack.push((mid, e, l + 1));
        }
        // the pieces in the order the leaves first name them (near ones near in memory)
        let mut new_of = vec![u32::MAX; self.pieces.len()];
        let mut order: Vec<u32> = Vec::with_capacity(self.pieces.len());
        for &i in &idx {
            let o = owner[i as usize] as usize;
            if new_of[o] == u32::MAX {
                new_of[o] = order.len() as u32;
                order.push(o as u32);
            }
        }
        self.pieces = order.iter().map(|&o| self.pieces[o as usize]).collect();
        self.prims = idx
            .iter()
            .map(|&i| new_of[owner[i as usize] as usize])
            .collect();
        self.nodes = nodes;
    }

    /// The scene as the GPU takes it (web/gpu/exact.js): the pieces, 16 floats each (tip, or a
    /// turning one's machine tip, and the tool's reach; its move, or the machine's, and its
    /// segment; its axis, or a turning one's A from (within one turn), A by and speed, and
    /// tool * 65536 + operation; its poses and 1 if it turns), the nodes, 12 each (box low
    /// corner and first child or prim; high corner and prims' count, 0 for two children; the
    /// first segment under it) and the prims (a piece's index each).
    pub fn packed(&self) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let mut pieces: Vec<f32> = Vec::with_capacity(self.pieces.len() * 16);
        for pc in &self.pieces {
            let toolop = (pc.tool as u32 * 65536 + pc.op as u32) as f32;
            match pc.turn {
                None => pieces.extend_from_slice(&[
                    pc.p[0] as f32,
                    pc.p[1] as f32,
                    pc.p[2] as f32,
                    pc.len as f32,
                    pc.v[0] as f32,
                    pc.v[1] as f32,
                    pc.v[2] as f32,
                    pc.seg as f32,
                    pc.a[0] as f32,
                    pc.a[1] as f32,
                    pc.a[2] as f32,
                    toolop,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ]),
                Some(tr) => pieces.extend_from_slice(&[
                    tr.m0[0] as f32,
                    tr.m0[1] as f32,
                    tr.m0[2] as f32,
                    pc.len as f32,
                    tr.dm[0] as f32,
                    tr.dm[1] as f32,
                    tr.dm[2] as f32,
                    pc.seg as f32,
                    tr.a0.rem_euclid(std::f64::consts::TAU) as f32,
                    tr.da as f32,
                    tr.speed as f32,
                    toolop,
                    tr.samples as f32,
                    1.0,
                    0.0,
                    0.0,
                ]),
            }
        }
        let mut nodes: Vec<f32> = Vec::with_capacity(self.nodes.len() * 12);
        for nd in &self.nodes {
            nodes.extend_from_slice(&[
                nd.lo[0],
                nd.lo[1],
                nd.lo[2],
                nd.first as f32,
                nd.hi[0],
                nd.hi[1],
                nd.hi[2],
                nd.count as f32,
                nd.min_seg as f32,
                0.0,
                0.0,
                0.0,
            ]);
        }
        let prims = self.prims.iter().map(|&i| i as f32).collect();
        (pieces, nodes, prims)
    }

    /// Every piece (before segment `cutoff`) whose box holds `p`.
    pub fn query(&self, p: [f64; 3], cutoff: u32, f: &mut dyn FnMut(usize)) {
        if self.pieces.is_empty() {
            return;
        }
        let mut stack = vec![0usize];
        while let Some(ni) = stack.pop() {
            let nd = &self.nodes[ni];
            if nd.min_seg >= cutoff {
                continue;
            }
            if (0..3).any(|k| p[k] < nd.lo[k] as f64 || p[k] > nd.hi[k] as f64) {
                continue;
            }
            if nd.count == 0 {
                stack.push(nd.first as usize);
                stack.push(nd.first as usize + 1);
            } else {
                for k in nd.first..nd.first + nd.count {
                    let i = self.prims[k as usize] as usize;
                    if self.pieces[i].seg < cutoff {
                        f(i);
                    }
                }
            }
        }
    }

    /// Where the ray `o + t d` (`d` unit) enters the bar and leaves it, and the bar's outward
    /// normal where it enters.
    pub fn bar_span(&self, o: [f64; 3], d: [f64; 3]) -> Option<(f64, f64, [f64; 3])> {
        let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
        let mut n0 = [0.0; 3];
        // the cylinder
        let a = d[1] * d[1] + d[2] * d[2];
        let b = o[1] * d[1] + o[2] * d[2];
        let c = o[1] * o[1] + o[2] * o[2] - self.radius * self.radius;
        if a < 1e-14 {
            if c > 0.0 {
                return None;
            }
        } else {
            let disc = b * b - a * c;
            if disc < 0.0 {
                return None;
            }
            let q = disc.sqrt();
            t0 = (-b - q) / a;
            t1 = (-b + q) / a;
            let p = add(o, mul(d, t0));
            n0 = unit([0.0, p[1], p[2]]);
        }
        // the ends
        if d[0].abs() < 1e-14 {
            if o[0] < -self.length || o[0] > 0.0 {
                return None;
            }
        } else {
            let (mut e0, mut e1) = ((-self.length - o[0]) / d[0], (0.0 - o[0]) / d[0]);
            let (mut n_e0, mut n_e1) = ([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
            if e0 > e1 {
                std::mem::swap(&mut e0, &mut e1);
                std::mem::swap(&mut n_e0, &mut n_e1);
            }
            let _ = n_e1;
            if e0 > t0 {
                t0 = e0;
                n0 = n_e0;
            }
            t1 = t1.min(e1);
        }
        (t0 < t1).then_some((t0, t1, n0))
    }

    /// The ray's visible point on the stock cut up to segment `cutoff` (`d` unit): its `t`, the
    /// piece that cut it there (None: the bar's own surface) and its normal (towards the ray).
    pub fn cast(&self, o: [f64; 3], d: [f64; 3], cutoff: u32) -> Option<Hit> {
        let (t0, t1, n0) = self.bar_span(o, d)?;
        let t0 = t0.max(0.0);
        if t0 >= t1 {
            return None;
        }
        let mut t = t0;
        let mut hit = Hit {
            t,
            piece: None,
            normal: n0,
            steps: 0,
            tests: [0; 2],
            colour: None,
        };
        let eps = 1e-9 * (1.0 + t0.abs());
        loop {
            let p = add(o, mul(d, t));
            let mut best = t + eps;
            let mut found: Option<(usize, [f64; 3])> = None;
            let tests = &mut hit.tests;
            let x = p;
            self.query(p, cutoff, &mut |i| {
                let pc = &self.pieces[i];
                tests[pc.turn.is_some() as usize] += 1;
                let tool = &self.tools[pc.tool as usize];
                if let Some(f) = holds(pc, tool, x, eps) {
                    if let Some((exit, n)) = exit_from(pc, tool, o, d, f) {
                        if exit > best {
                            best = exit;
                            found = Some((i, n));
                        }
                    }
                }
            });
            hit.steps += 1;
            match found {
                None => break,
                Some((i, n)) => {
                    t = best;
                    if t >= t1 {
                        return None;
                    }
                    hit.t = t;
                    hit.piece = Some(i);
                    hit.normal = mul(n, -1.0);
                }
            }
            if hit.steps > 100_000 {
                break;
            }
        }
        // its colour: the first operation to have cut within COLOUR_DEPTH of the surface there
        // (a later one taking a few µm more off the same surface leaves it the earlier's: two
        // finishes on one face were specks of both)
        hit.colour = hit.piece;
        if let Some(i0) = hit.piece {
            let x = add(add(o, mul(d, hit.t)), mul(hit.normal, COLOUR_DEPTH));
            let mut first = self.pieces[i0].seg;
            self.query(x, cutoff, &mut |i| {
                let pc = &self.pieces[i];
                if pc.seg < first && holds(pc, &self.tools[pc.tool as usize], x, eps).is_some() {
                    first = pc.seg;
                    hit.colour = Some(i);
                }
            });
        }
        Some(hit)
    }
}

/// How much more (mm) a later operation must take off a surface to colour it (see
/// [`Scene::cast`]): far below anything seen or machined.
pub const COLOUR_DEPTH: f64 = 0.003;

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f64,
    pub piece: Option<usize>,
    pub normal: [f64; 3],
    /// The point queries it took.
    pub steps: u32,
    /// The pieces it looked at (straight, turning).
    pub tests: [u32; 2],
    /// The piece whose operation colours it (see [`COLOUR_DEPTH`]).
    pub colour: Option<usize>,
}

/// Where a ray meets a piece's swept volume: from `t0` to `t1`, and the tool's outward normal
/// where it leaves (`n1`).
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub t0: f64,
    pub t1: f64,
    pub n1: [f64; 3],
}

/// F(t, s) = a t² + 2b ts + c s² + 2d t + 2e s + f.
#[derive(Clone, Copy, Debug)]
struct Quad([f64; 6]);

impl Quad {
    fn at(&self, t: f64, s: f64) -> f64 {
        let [a, b, c, d, e, f] = self.0;
        a * t * t + 2.0 * b * t * s + c * s * s + 2.0 * d * t + 2.0 * e * s + f
    }
}

/// The roots of `a x² + b x + c` (none, one or two).
fn roots(a: f64, b: f64, c: f64, out: &mut Vec<f64>) {
    let scale = a.abs().max(b.abs()).max(c.abs());
    if scale == 0.0 {
        return;
    }
    if a.abs() <= 1e-14 * scale {
        if b.abs() > 1e-14 * scale {
            out.push(-c / b);
        }
        return;
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        // (a graze: the double root, if within rounding)
        if disc > -1e-12 * b * b {
            out.push(-b / (2.0 * a));
        }
        return;
    }
    let q = -0.5 * (b + b.signum() * disc.sqrt());
    if q != 0.0 {
        out.push(q / a);
        out.push(c / q);
    } else {
        out.push(0.0);
    }
}

/// The extremes in t of { F ≤ 0, every line `α t + β s + γ ≥ 0` }: (t, s, which constraint
/// holds it there: 0 the quadric, i + 1 line i) for the least and the greatest. The first two
/// lines are those of s (0 to 1).
/// A region's extreme: (t, s, the constraint holding it there).
type Extreme = (f64, f64, usize);

fn extent(q: &Quad, lines: &[[f64; 3]]) -> Option<(Extreme, Extreme)> {
    let mut lo: Option<(f64, f64, usize)> = None;
    let mut hi: Option<(f64, f64, usize)> = None;
    let scale = q.0.iter().fold(0.0f64, |m, x| m.max(x.abs())).max(1e-30);
    let ok = |t: f64, s: f64, skip: [usize; 2], quad_on: bool| {
        if !t.is_finite() || !s.is_finite() {
            return false;
        }
        for (i, l) in lines.iter().enumerate() {
            if skip.contains(&(i + 1)) {
                continue;
            }
            let v = l[0] * t + l[1] * s + l[2];
            let m = l[0].abs().max(l[1].abs()).max(l[2].abs()) * 1e-9 + 1e-12;
            if v < -m {
                return false;
            }
        }
        quad_on || q.at(t, s) <= 1e-9 * scale
    };
    let mut take = |t: f64, s: f64, tag: usize| {
        if lo.is_none_or(|l| t < l.0) {
            lo = Some((t, s, tag));
        }
        if hi.is_none_or(|h| t > h.0) {
            hi = Some((t, s, tag));
        }
    };
    let mut r = Vec::with_capacity(2);
    // a line (α t + β s + γ = 0) with the quadric
    let on_line = |l: [f64; 3],
                   tag: usize,
                   skip: usize,
                   r: &mut Vec<f64>,
                   take: &mut dyn FnMut(f64, f64, usize)| {
        let [a, b, c, d, e, f] = q.0;
        r.clear();
        if l[1].abs() >= l[0].abs() {
            if l[1] == 0.0 {
                return;
            }
            let (m, n) = (-l[0] / l[1], -l[2] / l[1]);
            roots(
                a + 2.0 * b * m + c * m * m,
                2.0 * (b * n + c * m * n + d + e * m),
                c * n * n + 2.0 * e * n + f,
                r,
            );
            for &t in r.iter() {
                let s = m * t + n;
                if ok(t, s, [skip, 0], true) {
                    take(t, s, tag);
                }
            }
        } else {
            let (m, n) = (-l[1] / l[0], -l[2] / l[0]);
            roots(
                a * m * m + 2.0 * b * m + c,
                2.0 * (a * m * n + b * n + d * m + e),
                a * n * n + 2.0 * d * n + f,
                r,
            );
            for &s in r.iter() {
                let t = m * s + n;
                if ok(t, s, [skip, 0], true) {
                    take(t, s, tag);
                }
            }
        }
    };
    // the quadric's own extremes in t (∂F/∂s = 0)
    let [_, b, c, _, e, _] = q.0;
    if b != 0.0 || c != 0.0 {
        on_line([b, c, e], 0, 0, &mut r, &mut take);
    }
    for (i, l) in lines.iter().enumerate() {
        // (a line of s (the first two) holds the quadric's normal there; a cap its own)
        let tag = if i < 2 { 0 } else { i + 1 };
        on_line(*l, tag, i + 1, &mut r, &mut take);
    }
    // the corners
    for i in 0..lines.len() {
        for j in i + 1..lines.len() {
            let (l1, l2) = (lines[i], lines[j]);
            let det = l1[0] * l2[1] - l1[1] * l2[0];
            if det.abs() < 1e-14 * (l1[0].abs() + l1[1].abs()) * (l2[0].abs() + l2[1].abs()) {
                continue;
            }
            let t = (-l1[2] * l2[1] + l1[1] * l2[2]) / det;
            let s = (-l1[0] * l2[2] + l1[2] * l2[0]) / det;
            if ok(t, s, [i + 1, j + 1], false) {
                // (a corner of s and a cap: the cap's)
                let tag = if i >= 2 { i + 1 } else { j + 1 };
                take(t, s, tag);
            }
        }
    }
    Some((lo?, hi?))
}

/// The parts of a tool: its cylinder (from `zlo`, whether its floor is the tool's own), its
/// ball or its cone.
#[derive(Clone, Copy)]
enum Part {
    Cyl { zlo: f64, zhi: f64, floor: bool },
    Ball,
    Cone { zhi: f64 },
}

fn parts(t: &ToolSolid, len: f64) -> ([Option<Part>; 2], usize) {
    if t.ball {
        let r = t.radius;
        (
            [
                Some(Part::Ball),
                (len > r).then_some(Part::Cyl {
                    zlo: r,
                    zhi: len,
                    floor: false,
                }),
            ],
            2,
        )
    } else if t.cone > 0.0 {
        let zc = t.radius / t.cone;
        (
            [
                Some(Part::Cone {
                    zhi: zc.min(len.max(0.0)),
                }),
                (len > zc).then_some(Part::Cyl {
                    zlo: zc,
                    zhi: len,
                    floor: false,
                }),
            ],
            2,
        )
    } else {
        (
            [
                Some(Part::Cyl {
                    zlo: 0.0,
                    zhi: len,
                    floor: true,
                }),
                None,
            ],
            1,
        )
    }
}

/// A point's signed distance to a 2D convex polygon (counter-clockwise).
fn sd_poly(p: [f64; 2], v: &[[f64; 2]]) -> f64 {
    let mut d = f64::INFINITY;
    let mut inside = true;
    for i in 0..v.len() {
        let (a, b) = (v[i], v[(i + 1) % v.len()]);
        let e = [b[0] - a[0], b[1] - a[1]];
        let w = [p[0] - a[0], p[1] - a[1]];
        let h = ((w[0] * e[0] + w[1] * e[1]) / (e[0] * e[0] + e[1] * e[1])).clamp(0.0, 1.0);
        let q = [w[0] - e[0] * h, w[1] - e[1] * h];
        d = d.min((q[0] * q[0] + q[1] * q[1]).sqrt());
        if e[0] * w[1] - e[1] * w[0] < 0.0 {
            inside = false;
        }
    }
    if inside {
        -d
    } else {
        d
    }
}

/// A point's signed distance to the tool at `tip` along `a`, `len` long: in the plane through
/// its axis, to its profile (exact outside; inside, its sign right and no further from the
/// surface than it says).
pub fn sd_tool(t: &ToolSolid, tip: [f64; 3], a: [f64; 3], len: f64, x: [f64; 3]) -> f64 {
    let q = sub(x, tip);
    let z = dot(q, a);
    let r = norm(sub(q, mul(a, z)));
    let rr = t.radius;
    if t.ball {
        let disc = (r * r + (z - rr).powi(2)).sqrt() - rr;
        if len <= rr {
            return disc;
        }
        disc.min(sd_poly(
            [r, z],
            &[[-rr, rr], [rr, rr], [rr, len], [-rr, len]],
        ))
    } else if t.cone > 0.0 {
        let zc = rr / t.cone;
        if len <= zc {
            let w = t.cone * len;
            sd_poly([r, z], &[[0.0, 0.0], [w, len], [-w, len]])
        } else {
            sd_poly(
                [r, z],
                &[[0.0, 0.0], [rr, zc], [rr, len], [-rr, len], [-rr, zc]],
            )
        }
    } else {
        sd_poly([r, z], &[[-rr, 0.0], [rr, 0.0], [rr, len], [-rr, len]])
    }
}

/// Golden search for the greatest of `g` over [a, b] from `c` within it (`gc` = g(c), the
/// best known); `None` is less than anything. Returns where and what.
fn golden<T: Copy>(
    mut a: f64,
    mut c: f64,
    mut b: f64,
    mut gc: T,
    g: &dyn Fn(f64) -> Option<T>,
    better: &dyn Fn(&T, &T) -> bool,
) -> (f64, T) {
    const G: f64 = 0.381_966_011_250_105_2;
    for _ in 0..40 {
        if b - a < 1e-9 {
            break;
        }
        let right = b - c > c - a;
        let x = if right {
            c + G * (b - c)
        } else {
            c - G * (c - a)
        };
        match g(x) {
            Some(gx) if better(&gx, &gc) => {
                if right {
                    a = c;
                } else {
                    b = c;
                }
                c = x;
                gc = gx;
            }
            _ => {
                if right {
                    b = x;
                } else {
                    a = x;
                }
            }
        }
    }
    (c, gc)
}

/// Whether `a s² + b s + c <= tol` somewhere in [lo, hi].
fn quad_le(a: f64, b: f64, c: f64, lo: f64, hi: f64, tol: f64) -> bool {
    if lo > hi {
        return false;
    }
    let q = |s: f64| (a * s + b) * s + c;
    if q(lo) <= tol || q(hi) <= tol {
        return true;
    }
    if a > 0.0 {
        let v = -b / (2.0 * a);
        return v > lo && v < hi && q(v) <= tol;
    }
    false
}

/// The parameters s where `lo_z <= z0 - s dz <= hi_z`, within [lo, hi].
fn z_range(z0: f64, dz: f64, lo_z: f64, hi_z: f64, lo: f64, hi: f64) -> (f64, f64) {
    if dz.abs() < 1e-15 {
        return if z0 >= lo_z && z0 <= hi_z {
            (lo, hi)
        } else {
            (1.0, 0.0)
        };
    }
    let (a, b) = ((z0 - lo_z) / dz, (z0 - hi_z) / dz);
    (lo.max(a.min(b)), hi.min(a.max(b)))
}

/// Whether the point `x` is in the straight piece's swept volume (`eps` inside, about): there
/// is a place `s` along its move where the tool holds `x`. Each part of the tool is a quadric
/// in s (and a range of s its height allows): the least of it over that range said.
fn holds_straight(pc: &Piece, t: &ToolSolid, x: [f64; 3], eps: f64) -> bool {
    let a = pc.a;
    let q0 = sub(x, pc.p);
    let (z0, vz) = (dot(q0, a), dot(pc.v, a));
    let q0p = sub(q0, mul(a, z0));
    let vp = sub(pc.v, mul(a, vz));
    let r = t.radius - eps;
    // |q0p - s vp|² - r² (the cylinder round the axis)
    let (ca, cb, cc) = (dot(vp, vp), -2.0 * dot(q0p, vp), dot(q0p, q0p) - r * r);
    let tol = 0.0;
    if t.ball {
        // the ball: |q0 - R a - s v|² <= r²
        let w = sub(q0, mul(a, t.radius));
        if quad_le(
            dot(pc.v, pc.v),
            -2.0 * dot(w, pc.v),
            dot(w, w) - r * r,
            0.0,
            1.0,
            tol,
        ) {
            return true;
        }
        if pc.len > t.radius {
            let (lo, hi) = z_range(z0, vz, t.radius, pc.len - eps, 0.0, 1.0);
            return quad_le(ca, cb, cc, lo, hi, tol);
        }
        return false;
    }
    if t.cone > 0.0 {
        let zc = t.radius / t.cone;
        // the cone: |q0p - s vp|² - k² (z0 - s vz)² <= 0, z from 0 to zc (eps in: its
        // surface moved in by eps, about)
        let k2 = t.cone * t.cone;
        let shift = eps * (1.0 + k2).sqrt() / t.cone;
        let zs = z0 - shift;
        let (lo, hi) = z_range(z0, vz, shift, zc.min(pc.len), 0.0, 1.0);
        if quad_le(
            ca - k2 * vz * vz,
            cb + 2.0 * k2 * zs * vz,
            dot(q0p, q0p) - k2 * zs * zs,
            lo,
            hi,
            tol,
        ) {
            return true;
        }
        if pc.len > zc {
            let (lo, hi) = z_range(z0, vz, zc, pc.len - eps, 0.0, 1.0);
            return quad_le(ca, cb, cc, lo, hi, tol);
        }
        return false;
    }
    let (lo, hi) = z_range(z0, vz, eps, pc.len - eps, 0.0, 1.0);
    quad_le(ca, cb, cc, lo, hi, tol)
}

/// Whether the point `x` is in the piece's swept volume, `eps` inside: the pose (the
/// parameter along the piece) to leave it from. A turning piece: its tool's distance to `x`
/// changes no faster along it than [`Turn::speed`], so its poses show `x` out of it (every
/// one further than half a pose's step), or in it: at the pose nearest (the deepest), between
/// poses found by a golden search.
pub fn holds(pc: &Piece, t: &ToolSolid, x: [f64; 3], eps: f64) -> Option<f64> {
    let Some(tr) = pc.turn else {
        return holds_straight(pc, t, x, eps).then_some(0.0);
    };
    let dist = |f: f64| {
        let (tip, a) = tr.at(f);
        sd_tool(t, tip, a, pc.len, x)
    };
    if dist(0.5) > 0.5 * tr.speed + eps {
        return None;
    }
    let n = tr.samples.max(2) as usize;
    let h = 1.0 / (n - 1) as f64;
    let reach = 0.5 * tr.speed * h + eps;
    let (mut kb, mut vb) = (0usize, f64::INFINITY);
    let mut ds = [f64::INFINITY; 16];
    for (k, dk) in ds.iter_mut().enumerate().take(n) {
        *dk = dist(k as f64 * h);
        if *dk < vb {
            kb = k;
            vb = *dk;
        }
    }
    if vb > reach {
        return None;
    }
    // the deepest place near the nearest pose (and near any other pose near enough)
    let mut tried = 0;
    let mut best: Option<(f64, f64)> = None;
    let mut order: Vec<usize> = (0..n).filter(|&k| ds[k] <= reach).collect();
    order.sort_by(|&i, &j| ds[i].total_cmp(&ds[j]));
    for &k in &order {
        let (fa, fb) = (
            (k.saturating_sub(1)) as f64 * h,
            (k + 1).min(n - 1) as f64 * h,
        );
        let (f, v) = golden(
            fa,
            k as f64 * h,
            fb,
            -ds[k],
            &|f| Some(-dist(f)),
            &|a, b| a > b,
        );
        if -v <= -eps && best.is_none_or(|b| -v < b.1) {
            best = Some((f, -v));
        }
        tried += 1;
        // (as the GPU: the nearest three at most)
        if best.is_some() || tried >= 3 {
            break;
        }
    }
    let _ = kb;
    best.map(|b| b.0)
}

/// Where the ray `o + t d` leaves the piece's volume from a point in it held at `f` (see
/// [`holds`]): a straight piece's whole stretch; a turning one's tool at that pose (the ray is
/// in the volume all that way, if not as far on as the volume takes it: the next look goes on).
pub fn exit_from(
    pc: &Piece,
    t: &ToolSolid,
    o: [f64; 3],
    d: [f64; 3],
    f: f64,
) -> Option<(f64, [f64; 3])> {
    let Some(tr) = pc.turn else {
        let sp = span(pc, t, o, d)?;
        return Some((sp.t1, sp.n1));
    };
    let at = |f: f64| {
        let (tip, a) = tr.at(f);
        span(
            &Piece {
                p: tip,
                v: [0.0; 3],
                a,
                turn: None,
                ..*pc
            },
            t,
            o,
            d,
        )
    };
    let here = at(f)?;
    let mut best = (here.t1, here.n1);
    // and on through the run of poses next to it that the ray meets (the volume's stretch of
    // the ray: each pose's tool meets it, the next's overlaps it)
    let n = tr.samples.max(2) as usize;
    let h = 1.0 / (n - 1) as f64;
    let k0 = ((f / h).floor() as usize).min(n - 1);
    let mut sp: Vec<Option<Option<Span>>> = vec![None; n];
    let get = |k: usize, sp: &mut Vec<Option<Option<Span>>>| {
        *sp[k].get_or_insert_with(|| at(k as f64 * h))
    };
    for start in [k0, (k0 + 1).min(n - 1)] {
        if get(start, &mut sp).is_none() {
            continue;
        }
        let mut k = start;
        while let Some(s) = get(k, &mut sp) {
            if s.t1 > best.0 {
                best = (s.t1, s.n1);
            }
            if k == 0 {
                break;
            }
            k -= 1;
        }
        let mut k = start + 1;
        while k < n {
            match get(k, &mut sp) {
                Some(s) => {
                    if s.t1 > best.0 {
                        best = (s.t1, s.n1);
                    }
                }
                None => break,
            }
            k += 1;
        }
    }
    Some(best)
}

/// Where the ray `o + t d` (`d` unit) meets the volume a straight piece's tool sweeps.
pub fn span(pc: &Piece, t: &ToolSolid, o: [f64; 3], d: [f64; 3]) -> Option<Span> {
    // (t counted from the ray's point nearest the piece's middle: small numbers)
    let mid = add(pc.p, mul(pc.v, 0.5));
    let tc = dot(sub(mid, o), d);
    let o2 = add(o, mul(d, tc));
    let a = pc.a;
    let v = pc.v;
    let w = sub(o2, pc.p);
    let (wz, dz, vz) = (dot(w, a), dot(d, a), dot(v, a));
    let wp = sub(w, mul(a, wz));
    let dp = sub(d, mul(a, dz));
    let vp = sub(v, mul(a, vz));
    let r = t.radius;
    let (ps, _) = parts(t, pc.len);
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut n1 = [0.0; 3];
    let mut hi_real = false;
    let s_lines = [[0.0, 1.0, 0.0], [0.0, -1.0, 1.0]];
    let z_ge = |z: f64| [dz, -vz, wz - z];
    let z_le = |z: f64| [-dz, vz, z - wz];
    for part in ps.iter().flatten() {
        let (q, lines): (Quad, Vec<[f64; 3]>) = match *part {
            Part::Cyl { zlo, zhi, .. } => (
                Quad([
                    dot(dp, dp),
                    -dot(dp, vp),
                    dot(vp, vp),
                    dot(dp, wp),
                    -dot(vp, wp),
                    dot(wp, wp) - r * r,
                ]),
                vec![s_lines[0], s_lines[1], z_ge(zlo), z_le(zhi)],
            ),
            Part::Ball => {
                let wc = sub(w, mul(a, r));
                (
                    Quad([
                        1.0,
                        -dot(d, v),
                        dot(v, v),
                        dot(d, wc),
                        -dot(v, wc),
                        dot(wc, wc) - r * r,
                    ]),
                    vec![s_lines[0], s_lines[1]],
                )
            }
            Part::Cone { zhi } => {
                let k2 = t.cone * t.cone;
                (
                    Quad([
                        dot(dp, dp) - k2 * dz * dz,
                        -dot(dp, vp) + k2 * dz * vz,
                        dot(vp, vp) - k2 * vz * vz,
                        dot(dp, wp) - k2 * dz * wz,
                        -dot(vp, wp) + k2 * vz * wz,
                        dot(wp, wp) - k2 * wz * wz,
                    ]),
                    vec![s_lines[0], s_lines[1], z_ge(0.0), z_le(zhi)],
                )
            }
        };
        let Some(((t0, _, _), (t1, s1, tag))) = extent(&q, &lines) else {
            continue;
        };
        lo = lo.min(t0);
        // the normal where it leaves
        let qp = sub(add(w, mul(d, t1)), mul(v, s1));
        let z = dot(qp, a);
        let (n, real) = match (*part, tag) {
            (Part::Cyl { .. }, 0) => (unit(sub(qp, mul(a, z))), true),
            (Part::Ball, _) => (unit(sub(qp, mul(a, r))), true),
            (Part::Cone { .. }, 0) => {
                let perp = unit(sub(qp, mul(a, z)));
                (unit(sub(perp, mul(a, t.cone))), true)
            }
            (Part::Cyl { floor, .. }, 3) => (mul(a, -1.0), floor),
            (Part::Cone { .. }, 3) => (mul(a, -1.0), false),
            (_, 4) => (a, true),
            _ => (unit(sub(qp, mul(a, z))), true),
        };
        let eps = 1e-9 * (1.0 + t1.abs());
        if t1 > hi + eps || (t1 >= hi - eps && real && !hi_real) {
            if t1 > hi {
                hi = t1;
            }
            n1 = n;
            hi_real = real;
        }
    }
    (lo <= hi).then_some(Span {
        t0: lo + tc,
        t1: hi + tc,
        n1,
    })
}

/// The part's triangles for the exact view's material left (web/gpu/exact.js: each pixel's
/// distance to the part, as [`crate::left3d`] measures it at its vertices): a bounding volume
/// hierarchy over them, its nodes 8 floats each (box low corner and first child or triangle;
/// high corner and triangle count, 0 for two children at `first` and `first + 1`) and the
/// triangles in its leaves' order, 12 floats each (its corners, each with a 0).
pub fn part_bvh(tris: &[[[f64; 3]; 3]]) -> (Vec<f32>, Vec<f32>) {
    let n = tris.len();
    let mut idx: Vec<u32> = (0..n as u32).collect();
    let cen: Vec<[f64; 3]> = tris
        .iter()
        .map(|t| [0, 1, 2].map(|k| (t[0][k] + t[1][k] + t[2][k]) / 3.0))
        .collect();
    let mut nodes: Vec<[f32; 8]> = vec![[0.0; 8]];
    let mut stack = vec![(0usize, n, 0usize)];
    while let Some((s, e, ni)) = stack.pop() {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &i in &idx[s..e] {
            for p in &tris[i as usize] {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        let nd = &mut nodes[ni];
        for k in 0..3 {
            nd[k] = lo[k] as f32 - 1e-4;
            nd[4 + k] = hi[k] as f32 + 1e-4;
        }
        if e - s <= 4 {
            nd[3] = s as f32;
            nd[7] = (e - s) as f32;
            continue;
        }
        // split at the median of the widest spread of centroids
        let mut clo = [f64::INFINITY; 3];
        let mut chi = [f64::NEG_INFINITY; 3];
        for &i in &idx[s..e] {
            for k in 0..3 {
                clo[k] = clo[k].min(cen[i as usize][k]);
                chi[k] = chi[k].max(cen[i as usize][k]);
            }
        }
        let ax = (0..3)
            .max_by(|&a, &b| (chi[a] - clo[a]).total_cmp(&(chi[b] - clo[b])))
            .unwrap();
        let mid = s + (e - s) / 2;
        idx[s..e].select_nth_unstable_by(mid - s, |&a, &b| {
            cen[a as usize][ax].total_cmp(&cen[b as usize][ax])
        });
        let l = nodes.len();
        nodes[ni][3] = l as f32;
        nodes[ni][7] = 0.0;
        nodes.push([0.0; 8]);
        nodes.push([0.0; 8]);
        stack.push((s, mid, l));
        stack.push((mid, e, l + 1));
    }
    let flat: Vec<f32> = nodes.iter().flatten().copied().collect();
    let mut t = Vec::with_capacity(n * 12);
    for &i in &idx {
        for p in &tris[i as usize] {
            t.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, 0.0]);
        }
    }
    (flat, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(r: f64) -> ToolSolid {
        ToolSolid {
            radius: r,
            ball: false,
            height: 20.0,
            cone: 0.0,
        }
    }
    fn ball(r: f64) -> ToolSolid {
        ToolSolid {
            radius: r,
            ball: true,
            height: 20.0,
            cone: 0.0,
        }
    }

    /// Whether `x` is in the tool at `tip` along `a` (its length `len`).
    fn in_tool(t: &ToolSolid, tip: [f64; 3], a: [f64; 3], len: f64, x: [f64; 3]) -> bool {
        let q = sub(x, tip);
        let z = dot(q, a);
        let rr = norm(sub(q, mul(a, z)));
        if z < 0.0 || z > len.max(if t.ball { 2.0 * t.radius } else { 0.0 }) {
            return false;
        }
        if t.ball {
            if z < t.radius {
                return norm(sub(q, mul(a, t.radius))) <= t.radius;
            }
            return (rr <= t.radius && z <= len) || norm(sub(q, mul(a, t.radius))) <= t.radius;
        }
        if t.cone > 0.0 && z < t.radius / t.cone {
            return rr <= t.cone * z;
        }
        rr <= t.radius
    }

    /// The span by brute force: the ray sampled finely, each point tested against the tool at
    /// many places along the piece.
    fn brute(
        pc: &Piece,
        t: &ToolSolid,
        o: [f64; 3],
        d: [f64; 3],
        t_lo: f64,
        t_hi: f64,
    ) -> Option<(f64, f64)> {
        let n = 4000;
        let mut first = None;
        let mut last = None;
        for i in 0..=n {
            let tt = t_lo + (t_hi - t_lo) * i as f64 / n as f64;
            let x = add(o, mul(d, tt));
            let inside = (0..=400).any(|k| {
                let s = k as f64 / 400.0;
                in_tool(t, add(pc.p, mul(pc.v, s)), pc.a, pc.len, x)
            });
            if inside {
                first.get_or_insert(tt);
                last = Some(tt);
            }
        }
        Some((first?, last?))
    }

    #[test]
    fn spans_match_brute_force() {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 1_000_000) as f64 / 1_000_000.0
        };
        let tools = [
            flat(1.5),
            ball(1.5),
            ToolSolid {
                radius: 1.5,
                ball: false,
                height: 20.0,
                cone: 1.0,
            },
        ];
        let mut checked = 0;
        for _ in 0..300 {
            let t = tools[(rnd() * 3.0) as usize % 3];
            let a = unit([rnd() - 0.5, rnd() - 0.5, rnd() - 0.5]);
            let pc = Piece {
                p: [rnd() * 2.0 - 1.0, rnd() * 2.0 - 1.0, rnd() * 2.0 - 1.0],
                v: mul([rnd() - 0.5, rnd() - 0.5, rnd() - 0.5], 6.0 * rnd()),
                a,
                len: 2.0 + 4.0 * rnd(),
                tool: 0,
                op: 1,
                seg: 0,
                turn: None,
            };
            let o = [rnd() * 20.0 - 10.0, rnd() * 20.0 - 10.0, 15.0];
            let target = [rnd() * 4.0 - 2.0, rnd() * 4.0 - 2.0, rnd() * 4.0 - 2.0];
            let d = unit(sub(target, o));
            let got = span(&pc, &t, o, d);
            let want = brute(&pc, &t, o, d, 0.0, 40.0);
            match (got, want) {
                (Some(g), Some(w)) => {
                    let step = 40.0 / 4000.0;
                    assert!(
                        (g.t0 - w.0).abs() < 2.0 * step && (g.t1 - w.1).abs() < 2.0 * step,
                        "{:?} vs {:?} ({:?} {:?})",
                        (g.t0, g.t1),
                        w,
                        pc,
                        t
                    );
                    checked += 1;
                }
                (None, None) => {}
                (Some(g), None) => assert!(
                    g.t1 - g.t0 < 0.05,
                    "span {:?} where brute none",
                    (g.t0, g.t1)
                ),
                (None, Some(w)) => assert!(w.1 - w.0 < 0.05, "none where brute {:?}", w),
            }
        }
        assert!(checked > 40, "{checked}");
    }

    #[test]
    fn a_slot_in_the_bar_seen_from_above() {
        // a flat Ø4 down 2 mm into a Ø20 bar, across it along Y at X = -10
        let r = 10.0;
        let pc = Piece {
            p: [-10.0, -15.0, 8.0],
            v: [0.0, 30.0, 0.0],
            a: [0.0, 0.0, 1.0],
            len: 20.0,
            tool: 0,
            op: 7,
            seg: 0,
            turn: None,
        };
        let sc = Scene::new(r, 30.0, vec![flat(2.0)], vec![pc]);
        let down = [0.0, 0.0, -1.0];
        // in the slot: its floor at z = 8, op 7, normal up
        let h = sc.cast([-10.5, 0.0, 50.0], down, 1).unwrap();
        assert!((h.t - 42.0).abs() < 1e-9, "{}", h.t);
        assert_eq!(h.piece.map(|i| sc.pieces[i].op), Some(7));
        assert!((h.normal[2] - 1.0).abs() < 1e-9, "{:?}", h.normal);
        // beside it: the bar's own top
        let h = sc.cast([-13.0, 0.0, 50.0], down, 1).unwrap();
        assert!((h.t - 40.0).abs() < 1e-9 && h.piece.is_none());
        // before the cut is played: the bar's top in the slot too
        let h = sc.cast([-10.5, 0.0, 50.0], down, 0).unwrap();
        assert!((h.t - 40.0).abs() < 1e-9 && h.piece.is_none());
        // the slot's wall seen at a slant: the cylinder's side, its normal along -X
        let d = unit([1.0, 0.0, -1.0]);
        let o = sub([-8.0, 0.0, 9.0], mul(d, 30.0));
        let h = sc.cast(o, d, 1).unwrap();
        let p = add(o, mul(d, h.t));
        assert!(
            (p[0] + 8.0).abs() < 1e-9 && (p[2] - 9.0).abs() < 1e-9,
            "{p:?}"
        );
        assert!((h.normal[0] + 1.0).abs() < 1e-9, "{:?}", h.normal);
    }

    #[test]
    fn turning_spans_match_dense_poses() {
        // (asked: random rays against turning moves, against the tool's poses 0.01 degrees
        // apart; none may miss the volume, invent it, or leave it anywhere but its boundary)
        let mut seed = 987654321u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 1_000_000) as f64 / 1_000_000.0
        };
        let tools = [
            flat(1.5),
            ball(1.5),
            ToolSolid {
                radius: 1.5875,
                ball: false,
                height: 20.0,
                cone: 1.0,
            },
            flat(0.4),
            ball(0.5),
        ];
        let tol = 1.5 * TOL;
        let (mut checked, mut inside_checked, mut cases) = (0, 0, 0);
        let mut most_looks = 0;
        let mut all_looks: Vec<usize> = Vec::new();
        for case in 0..std::env::var("EXACT_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600)
        {
            let t = tools[case % tools.len()];
            // a turning move: tip at machine (x, y, z) near the bar's surface (radius 10)
            let m0 = [rnd() * 4.0 - 2.0, rnd() * 4.0 - 2.0, 8.0 + rnd() * 3.0];
            let dm = mul([rnd() - 0.5, rnd() - 0.5, rnd() - 0.5], 2.0 * rnd());
            let da = (rnd() - 0.5) * 4f64.to_radians();
            let a0 = rnd() * std::f64::consts::TAU;
            let len = 4.0 + 4.0 * rnd();
            let speed = norm(dm) + (10.0 + t.radius) * da.abs();
            let want = (speed / pose_step(&t, TOL)).ceil() as u32 + 1;
            if want > 16 {
                continue;
            }
            cases += 1;
            let tr = Turn {
                m0,
                dm,
                a0,
                da,
                samples: want.max(3),
                // (the test's points anywhere the tool reaches, not only in a bar of radius 10)
                speed: norm(dm) + (norm(m0) + len + t.radius + norm(dm)) * da.abs(),
            };
            let (p0, d0) = tr.at(0.0);
            let (p1, d1) = tr.at(1.0);
            let pc = Piece {
                p: p0,
                v: sub(p1, p0),
                a: unit(add(d0, d1)),
                len,
                tool: 0,
                op: 1,
                seg: 0,
                turn: Some(tr),
            };
            // the reference: the tool every 0.01 degrees (and 2 µm of the tip's travel)
            let nref =
                ((da.abs().to_degrees() / 0.01).max(norm(dm) / 0.002).ceil() as usize).max(200);
            let poses: Vec<([f64; 3], [f64; 3])> =
                (0..=nref).map(|i| tr.at(i as f64 / nref as f64)).collect();
            // (the nearest pose then refined between its neighbours: the poses' own scallops out)
            let sd = |x: [f64; 3]| {
                let (mut bi, mut bv) = (0usize, f64::INFINITY);
                for (i, &(tip, a)) in poses.iter().enumerate() {
                    let v = sd_tool(&t, tip, a, len, x);
                    if v < bv {
                        bi = i;
                        bv = v;
                    }
                }
                let g = |f: f64| {
                    let (tip, a) = tr.at(f);
                    sd_tool(&t, tip, a, len, x)
                };
                let (mut lo, mut hi) = (
                    bi.saturating_sub(1) as f64 / nref as f64,
                    (bi + 1).min(nref) as f64 / nref as f64,
                );
                for _ in 0..60 {
                    let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
                    if g(m1) < g(m2) {
                        hi = m2;
                    } else {
                        lo = m1;
                    }
                }
                bv.min(g(0.5 * (lo + hi)))
            };
            // a ray at a point near the tool's path, from anywhere around
            let (pm, am) = tr.at(rnd());
            let aim = add(
                add(pm, mul(am, rnd() * 3.0)),
                mul(
                    [rnd() - 0.5, rnd() - 0.5, rnd() - 0.5],
                    2.0 * t.radius + 1.0,
                ),
            );
            let o = add(
                aim,
                mul(unit([rnd() - 0.5, rnd() - 0.5, rnd() - 0.5]), 25.0),
            );
            let d = unit(sub(aim, o));
            // along the ray: where the reference is inside
            let n = 2000;
            let ts: Vec<f64> = (0..=n).map(|i| 15.0 + 20.0 * i as f64 / n as f64).collect();
            let inside: Vec<f64> = ts.iter().map(|&tt| sd(add(o, mul(d, tt)))).collect();
            let ins: Vec<usize> = (0..=n).filter(|&i| inside[i] < -tol).collect();
            for q in 0..10 {
                // asked at a point inside (if any) or anywhere
                let tq = if q % 2 == 0 && !ins.is_empty() {
                    ts[ins[(rnd() * ins.len() as f64) as usize % ins.len()]]
                } else {
                    ts[(rnd() * n as f64) as usize]
                };
                let here = sd(add(o, mul(d, tq)));
                let eps = 1e-9;
                let got = holds(&pc, &t, add(o, mul(d, tq)), eps);
                checked += 1;
                match got {
                    None => assert!(
                        here > -tol,
                        "case {case}: missed: inside by {here} at t {tq} ({pc:?} {t:?} o {o:?} d {d:?})"
                    ),
                    Some(_) => {
                        inside_checked += 1;
                        assert!(here < tol, "case {case}: invented: outside by {here} at t {tq}");
                        // on as the cast goes (a pose held, out of it, again): no material passed
                        // over, and out at the volume's boundary in a few looks
                        let mut tt = tq;
                        let mut looks = 0;
                        while let Some(f) = holds(&pc, &t, add(o, mul(d, tt)), eps) {
                            let (exit, _) = exit_from(&pc, &t, o, d, f).expect("held, so met");
                            if exit <= tt + eps {
                                break;
                            }
                            let steps = 100;
                            for i in 0..=steps {
                                let u = tt + (exit - tt) * i as f64 / steps as f64;
                                let s = sd(add(o, mul(d, u)));
                                assert!(s < tol, "case {case}: passed over material ({s} at t {u}, from {tt} to {exit})");
                            }
                            tt = exit;
                            looks += 1;
                            assert!(looks < 64, "case {case}: still in after 64 looks");
                        }
                        most_looks = most_looks.max(looks);
                        all_looks.push(looks);
                        let s = sd(add(o, mul(d, tt)));
                        assert!(
                            s > -tol,
                            "case {case}: stopped inside by {s} at {tt} (asked at {tq}; {pc:?} {t:?} o {o:?} d {d:?})"
                        );
                    }
                }
            }
        }
        all_looks.sort();
        println!(
            "looks through one turning piece: most {most_looks}, median {}, 95% {}",
            all_looks[all_looks.len() / 2],
            all_looks[all_looks.len() * 95 / 100]
        );
        assert!(
            cases > 200 && checked > 2000 && inside_checked > 500,
            "{cases} {checked} {inside_checked}"
        );
    }
}
