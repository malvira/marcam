//! Material-removal simulation on a radial heightmap: the stock's outer radius over a grid of
//! (x, part angle). Also builds the same map for the finished part (from a mesh by radial ray
//! casting, or from a profile) so the two can be compared.
//!
//! Angles here are part angles φ (the direction of a part point in the part's own Y/Z frame).
//! A tool at rotary angle `a` sits over part angle `φ = 90° - a` (see [`crate::dropcutter`]).

use crate::det::Det;
use crate::dropcutter::Cutter;
use crate::geom::RevolvedProfile;
use crate::mesh::Mesh;
use crate::render::{ramp, Canvas, Frame};
use crate::toolpath::{Move, Toolpath};
use std::f64::consts::{PI, TAU};

/// Grid layout shared by stock and part maps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub x0: f64,
    pub dx: f64,
    pub nx: usize,
    /// Cells around the axis (cell `j` is centred on φ = (j + 0.5) * 360 / na degrees).
    pub na: usize,
}

impl Grid {
    pub fn new(x_min: f64, x_max: f64, dx: f64, na: usize) -> Grid {
        let nx = (((x_max - x_min) / dx).ceil() as usize).max(1);
        Grid {
            x0: x_min,
            dx,
            nx,
            na: na.max(8),
        }
    }

    pub fn x(&self, i: usize) -> f64 {
        self.x0 + (i as f64 + 0.5) * self.dx
    }

    /// Part angle of column `j`, radians.
    pub fn phi(&self, j: usize) -> f64 {
        (j as f64 + 0.5) * TAU / self.na as f64
    }

    pub fn len(&self) -> usize {
        self.nx * self.na
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Radius per cell (`i * na + j`).
#[derive(Debug, Clone)]
pub struct RadialMap {
    pub grid: Grid,
    pub r: Vec<f32>,
    /// Which operation last cut each cell: operation id + 1 (0 = never cut).
    pub op: Vec<u16>,
    /// No cell is above this radius (the stock's starting radius: cutting only lowers it):
    /// moves wholly above it are not sampled.
    pub top: f64,
}

impl RadialMap {
    /// The bar as it comes: each cell at the stock's radius at its angle (a square, hex or
    /// rectangular bar is not round).
    /// The job's bar as it comes: [`RadialMap::of_stock`], or what the setup before left when
    /// it carries its stock over.
    pub fn of_job(grid: Grid, job: &crate::Job) -> RadialMap {
        job.carried
            .as_ref()
            .and_then(|c| c.radial(grid, &job.stock))
            .unwrap_or_else(|| RadialMap::of_stock(grid, &job.stock))
    }

    pub fn of_stock(grid: Grid, stock: &crate::job::Stock) -> RadialMap {
        let mut m = RadialMap::filled(grid, stock.radius);
        if stock.section.is_some() {
            let ring: Vec<f32> = (0..grid.na)
                .map(|j| {
                    let phi = (j as f64 + 0.5) * std::f64::consts::TAU / grid.na as f64;
                    stock.radius_at(phi) as f32
                })
                .collect();
            for i in 0..grid.nx {
                m.r[i * grid.na..(i + 1) * grid.na].copy_from_slice(&ring);
            }
        }
        m
    }

    pub fn filled(grid: Grid, r: f64) -> RadialMap {
        RadialMap {
            grid,
            r: vec![r as f32; grid.len()],
            op: vec![0; grid.len()],
            top: r,
        }
    }

    /// [`RadialMap::top`] measured (after filling a map in).
    fn measured(mut self) -> RadialMap {
        self.top = self.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64;
        self
    }

    pub fn at(&self, i: usize, j: usize) -> f64 {
        self.r[i * self.grid.na + j] as f64
    }

    /// Finished part from a revolved profile (0 outside the profile's X range).
    pub fn from_profile(grid: Grid, geom: &RevolvedProfile) -> RadialMap {
        RadialMap::from_profile_at(grid, geom, 0.0)
    }

    /// Like [`RadialMap::from_profile`], sampled `offset` mm from each cell centre.
    pub fn from_profile_at(grid: Grid, geom: &RevolvedProfile, offset: f64) -> RadialMap {
        let mut m = RadialMap::filled(grid, 0.0);
        for i in 0..grid.nx {
            let x = grid.x(i) + offset;
            let r = if x < geom.x_min() || x > geom.x_max() {
                0.0
            } else {
                geom.radius(x)
            };
            for j in 0..grid.na {
                m.r[i * grid.na + j] = r as f32;
            }
        }
        m.measured()
    }

    /// Finished part from a mesh: outermost hit of each cell's radial ray (0 where none).
    pub fn from_mesh(grid: Grid, mesh: &Mesh) -> RadialMap {
        RadialMap::from_mesh_at(grid, mesh, 0.0)
    }

    /// Like [`RadialMap::from_mesh`], with rays `offset` mm from each cell centre.
    pub fn from_mesh_at(grid: Grid, mesh: &Mesh, offset: f64) -> RadialMap {
        Self::from_mesh_layers_at(grid, mesh, offset).0
    }

    /// [`RadialMap::from_mesh_at`], and where each ray from the axis crosses the part more
    /// than twice on its way out (a gap under an overhang): the top of the part below the
    /// outermost gap (elsewhere the outer radius).
    pub fn from_mesh_layers_at(grid: Grid, mesh: &Mesh, offset: f64) -> (RadialMap, RadialMap) {
        let mut m = RadialMap::filled(grid, 0.0);
        let mut hits: Vec<Vec<f32>> = vec![Vec::new(); grid.nx * grid.na];
        let dphi = TAU / grid.na as f64;
        for t in &mesh.triangles {
            let xs = [t[0][0], t[1][0], t[2][0]];
            let x_lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let x_hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let i0 = ((x_lo - grid.x0) / grid.dx - 1.5).ceil().max(0.0) as usize;
            let i1 = ((x_hi - grid.x0) / grid.dx + 0.5).floor();
            if i1 < 0.0 {
                continue;
            }
            let i1 = (i1 as usize).min(grid.nx.saturating_sub(1));
            // angular columns: the projected triangle's span (all if it covers the axis)
            let p2 = [[t[0][1], t[0][2]], [t[1][1], t[1][2]], [t[2][1], t[2][2]]];
            let cols: Vec<usize> = match angular_cover(p2) {
                None => (0..grid.na).collect(),
                Some((start, len)) => {
                    // one spare column each side: rays exactly on a seam must reach both
                    // neighbours (the inside test decides)
                    let j0 = ((start / dphi) - 0.5).ceil() as i64 - 1;
                    let j1 = (((start + len) / dphi) - 0.5).floor() as i64 + 1;
                    (j0..=j1)
                        .map(|j| j.rem_euclid(grid.na as i64) as usize)
                        .collect()
                }
            };
            let n = crate::mesh::cross(crate::mesh::sub(t[1], t[0]), crate::mesh::sub(t[2], t[0]));
            for i in i0..=i1 {
                let x = grid.x(i) + offset;
                for &j in &cols {
                    let (s, c) = grid.phi(j).dsin_cos();
                    // ray p = (x, ρc, ρs), ρ >= 0: n·(p - t0) = 0
                    let den = n[1] * c + n[2] * s;
                    if den.abs() < 1e-15 {
                        continue;
                    }
                    let rho = (n[0] * (t[0][0] - x) + n[1] * t[0][1] + n[2] * t[0][2]) / den;
                    if rho < 0.0 {
                        continue;
                    }
                    let p = [x, rho * c, rho * s];
                    if inside_3d(t, &n, p) {
                        let k = i * grid.na + j;
                        m.r[k] = m.r[k].max(rho as f32);
                        hits[k].push(rho as f32);
                    }
                }
            }
        }
        // the crossings out from the axis, in order (one counted once where two triangles
        // share an edge): the last is the outer surface, the one before it where the outer
        // solid starts; a third from the end, the top of the solid under the gap
        let mut under = m.clone();
        for (k, h) in hits.iter_mut().enumerate() {
            if h.len() < 3 {
                continue;
            }
            h.sort_by(|a, b| a.total_cmp(b));
            h.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
            let n = h.len();
            if n >= 3 && h[n - 2] - h[n - 3] > 1e-3 {
                under.r[k] = h[n - 3];
            }
        }
        (m.measured(), under)
    }
}

/// The parts of a straight move from `p` to `q` worth sampling, by its height `h(p) = p[k]`:
/// where the tip is above `top` nothing is touched (the tool hangs above the stock) and that
/// part is left out; where it is below `low` the tool already goes through everything it
/// can reach, so that part is moved up to `low` (it cuts the same). None: the move is
/// wholly between them (sample it as it is). Some: the pieces, in order (maybe none).
pub fn clip_height(
    p: [f64; 3],
    q: [f64; 3],
    k: usize,
    low: f64,
    top: f64,
) -> Option<Vec<([f64; 3], [f64; 3])>> {
    let (hp, hq) = (p[k], q[k]);
    if hp.min(hq) >= low && hp.max(hq) <= top {
        return None;
    }
    let at = |t: f64| [0, 1, 2].map(|i| p[i] + (q[i] - p[i]) * t);
    // the move's parameter where its height crosses `top` and `low` (ascending, in (0, 1))
    let mut cuts = vec![0.0, 1.0];
    for h in [low, top] {
        if (hp - h) * (hq - h) < 0.0 {
            cuts.push((h - hp) / (hq - hp));
        }
    }
    cuts.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (at(w[0]), at(w[1]));
        let mid = 0.5 * (a[k] + b[k]);
        if mid > top {
            continue;
        }
        if mid < low {
            let (mut a, mut b) = (a, b);
            a[k] = low;
            b[k] = low;
            out.push((a, b));
        } else {
            out.push((a, b));
        }
    }
    Some(out)
}

/// Cell visits the simulation may take for one playback or batch run: about 30 s natively,
/// a minute in the browser. A finer grid than that for the job is refused before it starts
/// (see [`work_estimate`]).
pub const WORK_LIMIT: f64 = 1.5e9;

/// About how many cell visits simulating `segs` on a rotary grid `g` (stock no higher than
/// `top`) takes: the samples each move is stamped at, as the simulation takes them, times
/// the cells each stamp looks at (and each stamp's own cost). Cheap (no stock is touched), to refuse a grid too fine for
/// a job before starting.
pub fn work_estimate(
    g: Grid,
    top: f64,
    segs: &[Segment],
    cutter_for: &dyn Fn(u32) -> Option<Cutter>,
    step: f64,
) -> f64 {
    let dphi = TAU / g.na as f64;
    let mut total = 0.0;
    for seg in segs {
        let (Some(c), Some(from)) = (seg.tool.and_then(cutter_for), seg.from) else {
            continue;
        };
        let rad = c.radius();
        let rows = (2.0 * rad / g.dx).floor() + 1.0;
        let low = -(top + c.height(rad) + 0.01);
        let k = if seg.index_a.is_some() { 2 } else { 1 };
        let pieces = clip_height(from, seg.to, k, low, top).unwrap_or_else(|| vec![(from, seg.to)]);
        for (p, q) in pieces {
            let len = if seg.index_a.is_some() {
                ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)).sqrt()
            } else {
                let arc = (0.5 * (p[1] + q[1])).abs() * (q[2] - p[2]).to_radians().abs();
                (q[0] - p[0]).abs().max((q[1] - p[1]).abs()).max(arc)
            };
            let n = (len / step).ceil().max(1.0) + 1.0;
            let h = 0.5 * (p[k] + q[k]);
            let angles = if seg.index_a.is_some() {
                if h > 0.0 {
                    (2.0 * rad.datan2(h).min(PI / 2.0) / dphi + 2.0).min(g.na as f64)
                } else {
                    g.na as f64
                }
            } else {
                let half = if h <= rad {
                    PI / 2.0
                } else {
                    (rad / h).dasin()
                };
                2.0 * half / dphi + 1.0
            };
            // (and a stamp's own cost, whatever its size: about 16 cells' worth)
            total += n * (rows * angles + 16.0);
        }
    }
    total
}

/// The part over each cell: at the centre, and the lowest and highest radius across the
/// cell's X extent (cells that straddle a wall see both sides).
#[derive(Debug, Clone)]
pub struct Part {
    pub center: RadialMap,
    pub lo: RadialMap,
    pub hi: RadialMap,
    /// (Mar's impeller) where a ray from the axis leaves the part and meets it again further
    /// out (under a blade leaning over the hub): the top of the part below that gap, lowest
    /// across the cell; elsewhere as `lo`. A radial map cannot hold the gap, so a stock cut
    /// down into it (the tool reached under the overhang) is not a gouge: only below this is.
    pub under: Option<RadialMap>,
}

impl Part {
    fn from_samples(maps: Vec<RadialMap>) -> Part {
        let mut lo = maps[0].clone();
        let mut hi = maps[0].clone();
        for m in &maps[1..] {
            for k in 0..lo.r.len() {
                lo.r[k] = lo.r[k].min(m.r[k]);
                hi.r[k] = hi.r[k].max(m.r[k]);
            }
        }
        Part {
            center: maps[0].clone(),
            lo,
            hi,
            under: None,
        }
    }

    pub fn from_profile(grid: Grid, geom: &RevolvedProfile, points: &[[f64; 2]]) -> Part {
        let e = 0.4999 * grid.dx;
        let mut maps: Vec<RadialMap> = [0.0, -e, e]
            .iter()
            .map(|&o| RadialMap::from_profile_at(grid, geom, o))
            .collect();
        // profile corners inside a cell count too
        let mut extra = maps[0].clone();
        let mut any = false;
        for i in 0..grid.nx {
            let xc = grid.x(i);
            for p in points {
                if (p[0] - xc).abs() <= e {
                    any = true;
                    for j in 0..grid.na {
                        let k = i * grid.na + j;
                        extra.r[k] = extra.r[k].min(p[1] as f32);
                    }
                }
            }
        }
        if any {
            maps.push(extra);
        }
        Part::from_samples(maps)
    }

    pub fn from_mesh(grid: Grid, mesh: &Mesh) -> Part {
        let e = 0.4999 * grid.dx;
        let (maps, unders): (Vec<RadialMap>, Vec<RadialMap>) = [0.0, -e, e]
            .iter()
            .map(|&o| RadialMap::from_mesh_layers_at(grid, mesh, o))
            .unzip();
        let mut part = Part::from_samples(maps);
        let mut under = unders[0].clone();
        for u in &unders[1..] {
            for k in 0..under.r.len() {
                under.r[k] = under.r[k].min(u.r[k]);
            }
        }
        // (no gap anywhere: as `lo`)
        if under.r.iter().zip(&part.lo.r).any(|(u, l)| u < l) {
            part.under = Some(under);
        }
        part
    }
}

/// Angular arc `(start, len)` (radians) covered by a 2D triangle seen from the origin, or `None`
/// if the triangle contains the origin.
fn angular_cover(p: [[f64; 2]; 3]) -> Option<(f64, f64)> {
    let cross = |a: [f64; 2], b: [f64; 2]| a[0] * b[1] - a[1] * b[0];
    let s = [cross(p[0], p[1]), cross(p[1], p[2]), cross(p[2], p[0])];
    if (s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)) && s.iter().any(|&v| v != 0.0) {
        return None;
    }
    let mut a: Vec<f64> = p
        .iter()
        .map(|q| q[1].datan2(q[0]).rem_euclid(TAU))
        .collect();
    a.sort_by(|x, y| x.total_cmp(y));
    let gaps = [a[1] - a[0], a[2] - a[1], a[0] + TAU - a[2]];
    let (i, g) = gaps
        .iter()
        .enumerate()
        .fold((0, -1.0), |b, (i, &g)| if g > b.1 { (i, g) } else { b });
    Some((a[(i + 1) % 3], TAU - g))
}

fn inside_3d(t: &crate::mesh::Triangle, n: &[f64; 3], p: [f64; 3]) -> bool {
    let tol = -1e-9 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]);
    (0..3).all(|k| {
        let (a, b) = (t[k], t[(k + 1) % 3]);
        let e = crate::mesh::sub(b, a);
        let w = crate::mesh::sub(p, a);
        let c = crate::mesh::cross(e, w);
        c[0] * n[0] + c[1] * n[1] + c[2] * n[2] >= tol
    })
}

/// Result of simulating one toolpath.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimStats {
    /// Tool positions evaluated.
    pub samples: usize,
    /// Cells lowered by feed moves.
    pub cells_cut: usize,
    /// Rapid moves that would have removed material (crashes), with the move index.
    pub rapid_hits: Vec<usize>,
}

/// One straight tool move in a job, in order: what playback steps through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Tip position before the move, if known: (x, r, a).
    pub from: Option<[f64; 3]>,
    /// Tip position after the move.
    pub to: [f64; 3],
    pub rapid: bool,
    /// Tool loaded at the time.
    pub tool: Option<u32>,
    /// Operation id + 1 (0 before the first operation marker).
    pub op: u16,
    /// Index of the toolpath (program) in the job.
    pub program: u16,
    /// Index of the move in its toolpath.
    pub move_index: usize,
    /// Some(A) for a 3-axis move made with the rotary axis held at A (indexed 3+1, or A = 0
    /// on a vise): `from`/`to` are then machine [x, y, z].
    pub index_a: Option<f64>,
}

/// The job's motion as a flat list of segments (each program starts from an unknown position
/// with A at 0, as on the machine after `G92 A0` or a fresh start).
pub fn segments(tps: &[Toolpath]) -> Vec<Segment> {
    let mut out = Vec::new();
    for (p, tp) in tps.iter().enumerate() {
        let (mut x, mut r, mut a) = (None::<f64>, None::<f64>, 0.0f64);
        let mut y = 0.0f64;
        let mut tool = None;
        let mut op = 0u16;
        for (idx, m) in tp.moves.iter().enumerate() {
            // indexed 3+1 moves: machine X/Y/Z with A held; general moves solve their A
            let xyz = match *m {
                Move::Pose { tip, axis, feed } => match crate::kinematics::solve_a(axis, a) {
                    Ok(na) => {
                        a = na;
                        let q = crate::kinematics::to_machine(tip, na);
                        Some((Some(q[0]), q[1], Some(q[2]), feed.is_none()))
                    }
                    Err(_) => None,
                },
                Move::Rapid3 {
                    x: mx,
                    y: my,
                    z: mz,
                } => Some((mx.or(x), my.unwrap_or(y), mz.or(r), true)),
                Move::Line3 {
                    x: mx,
                    y: my,
                    z: mz,
                    ..
                } => Some((Some(mx), my, Some(mz), false)),
                _ => None,
            };
            if let Some((nx, ny, nz, rapid)) = xyz {
                if let (Some(x1), Some(z1)) = (nx, nz) {
                    out.push(Segment {
                        from: x.zip(r).map(|(x0, z0)| [x0, y, z0]),
                        to: [x1, ny, z1],
                        rapid,
                        tool,
                        op,
                        program: p as u16,
                        move_index: idx,
                        index_a: Some(a),
                    });
                }
                x = nx;
                y = ny;
                r = nz;
                continue;
            }
            let (nx, nr, na, rapid) = match *m {
                Move::ToolChange { tool: t, .. } => {
                    tool = Some(t);
                    continue;
                }
                Move::Operation { id, .. } => {
                    op = (id + 1).min(u16::MAX as u32) as u16;
                    continue;
                }
                Move::Comment { .. }
                | Move::Rapid3 { .. }
                | Move::Line3 { .. }
                | Move::Pose { .. } => continue,
                Move::Rapid {
                    x: mx,
                    r: mr,
                    a: ma,
                } => (mx.or(x), mr.or(r), ma.unwrap_or(a), true),
                Move::Plunge { r: mr, .. } => (x, Some(mr), a, false),
                Move::Cut {
                    x: mx,
                    r: mr,
                    a: ma,
                    ..
                } => (Some(mx), Some(mr), ma, false),
            };
            if let (Some(x1), Some(r1)) = (nx, nr) {
                out.push(Segment {
                    from: x.zip(r).map(|(x0, r0)| [x0, r0, a]),
                    to: [x1, r1, na],
                    rapid,
                    tool,
                    op,
                    program: p as u16,
                    move_index: idx,
                    index_a: None,
                });
            }
            x = nx;
            r = nr;
            a = na;
            y = 0.0;
        }
    }
    out
}

/// Rows `row0..row1` of a radial map, with their radii and tags: what one thread stamps.
pub struct Band<'a> {
    pub g: Grid,
    /// See [`RadialMap::top`].
    pub top: f64,
    pub row0: usize,
    pub row1: usize,
    pub r: &'a mut [f32],
    pub op: &'a mut [u16],
}

impl RadialMap {
    /// The whole map as one band.
    pub fn band(&mut self) -> Band<'_> {
        Band {
            g: self.grid,
            top: self.top,
            row0: 0,
            row1: self.grid.nx,
            r: &mut self.r,
            op: &mut self.op,
        }
    }
}

/// Threads for a simulation: all cores natively (`MARCAM_THREADS` overrides), one on wasm,
/// and no more than one per 8 rows.
pub fn sim_threads(rows: usize) -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = rows;
        1
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = std::env::var("MARCAM_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        n.min(rows / 8).clamp(1, 64)
    }
}

/// Apply `segs` in order with `threads` bands of rows, one per thread. Each band sees every
/// segment in the same order, so the stock, the tags and the effects equal a single pass.
pub fn apply_banded(
    stock: &mut RadialMap,
    segs: &[Segment],
    cutters: &[Option<Cutter>],
    step: f64,
    threads: usize,
) -> Vec<SegmentEffect> {
    let g = stock.grid;
    let top = stock.top;
    if threads <= 1 || g.nx < 2 {
        let mut b = stock.band();
        return segs
            .iter()
            .zip(cutters)
            .map(|(s, c)| apply_segment_band(&mut b, s, c.as_ref(), step))
            .collect();
    }
    let per = g.nx.div_ceil(threads);
    let parts: Vec<Vec<SegmentEffect>> = std::thread::scope(|sc| {
        let handles: Vec<_> = stock
            .r
            .chunks_mut(per * g.na)
            .zip(stock.op.chunks_mut(per * g.na))
            .enumerate()
            .map(|(t, (r, op))| {
                let row0 = t * per;
                let row1 = (row0 + per).min(g.nx);
                sc.spawn(move || {
                    let mut b = Band {
                        g,
                        top,
                        row0,
                        row1,
                        r,
                        op,
                    };
                    segs.iter()
                        .zip(cutters)
                        .map(|(s, c)| apply_segment_band(&mut b, s, c.as_ref(), step))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("simulation thread"))
            .collect()
    });
    // every band sampled the same positions; cuts add up, hits and rows combine
    (0..segs.len())
        .map(|i| {
            let mut e = SegmentEffect {
                samples: parts[0][i].samples,
                ..SegmentEffect::default()
            };
            for p in &parts {
                let q = p[i];
                e.cells_cut += q.cells_cut;
                e.rapid_hit |= q.rapid_hit;
                if let Some((lo, hi)) = q.rows {
                    e.rows = Some(e.rows.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                }
            }
            e
        })
        .collect()
}

/// What one segment did to the stock.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SegmentEffect {
    pub samples: usize,
    pub cells_cut: usize,
    /// A rapid that removed material.
    pub rapid_hit: bool,
    /// Rows (x cells) touched: `(first, last)`.
    pub rows: Option<(usize, usize)>,
}

/// Remove material along one segment, sampling at most `step` mm apart.
pub fn apply_segment(
    stock: &mut RadialMap,
    seg: &Segment,
    cutter: Option<&Cutter>,
    step: f64,
) -> SegmentEffect {
    apply_segment_band(&mut stock.band(), seg, cutter, step)
}

/// [`apply_segment`] on the rows of one band.
pub fn apply_segment_band(
    stock: &mut Band,
    seg: &Segment,
    cutter: Option<&Cutter>,
    step: f64,
) -> SegmentEffect {
    let mut e = SegmentEffect::default();
    if let Some(a) = seg.index_a {
        return apply_indexed(stock, seg, cutter, step, a);
    }
    let (Some(c), Some(from)) = (cutter, seg.from) else {
        return e;
    };
    // only where the tip is below the stock's top; far past the axis it cuts what it would
    // just past it
    let low = -(stock.top + c.height(c.radius()) + 0.01);
    match clip_height(from, seg.to, 1, low, stock.top) {
        None => sample_rotary(stock, seg, c, from, seg.to, false, step, &mut e),
        Some(pieces) => {
            for (p, q) in pieces {
                sample_rotary(stock, seg, c, p, q, true, step, &mut e);
            }
        }
    }
    e
}

/// The tool's positions along a rotary move from `p` to `q` (x, tip radius, A), at most
/// `step` apart, stamped (`first`: from its start, else from just after it).
#[allow(clippy::too_many_arguments)]
fn sample_rotary(
    stock: &mut Band,
    seg: &Segment,
    c: &Cutter,
    p: [f64; 3],
    q: [f64; 3],
    first: bool,
    step: f64,
    e: &mut SegmentEffect,
) {
    let ([x0, r0, a0], [x1, r1, a1]) = (p, q);
    // a machine moves all axes linearly together
    let arc = (0.5 * (r0 + r1)).abs() * (a1 - a0).to_radians().abs();
    let len = (x1 - x0).abs().max((r1 - r0).abs()).max(arc);
    let n = ((len / step).ceil() as usize).max(1);
    for k in (if first { 0 } else { 1 })..=n {
        let t = k as f64 / n as f64;
        let (cut, rows) = stamp(
            stock,
            c,
            x0 + (x1 - x0) * t,
            r0 + (r1 - r0) * t,
            a0 + (a1 - a0) * t,
            seg.op,
        );
        e.samples += 1;
        if let Some((lo, hi)) = rows {
            e.rows = Some(e.rows.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
        if seg.rapid && cut > 0 {
            e.rapid_hit = true;
        } else {
            e.cells_cut += cut;
        }
    }
}

/// An indexed 3-axis segment: the tool (axis +Z) moves in machine X/Y/Z with A held at `a`.
fn apply_indexed(
    stock: &mut Band,
    seg: &Segment,
    cutter: Option<&Cutter>,
    step: f64,
    a: f64,
) -> SegmentEffect {
    let mut e = SegmentEffect::default();
    let (Some(c), Some(from)) = (cutter, seg.from) else {
        return e;
    };
    // only where the tip is below the bar's top; far below the axis it cuts what it would
    // just under the bar
    let low = -(stock.top + c.height(c.radius()) + 0.01);
    let pieces =
        clip_height(from, seg.to, 2, low, stock.top).unwrap_or_else(|| vec![(from, seg.to)]);
    for (p, q) in pieces {
        let len = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)).sqrt();
        let n = ((len / step).ceil() as usize).max(1);
        for k in 0..=n {
            let t = k as f64 / n as f64;
            let at = [0, 1, 2].map(|i| p[i] + (q[i] - p[i]) * t);
            let (cut, rows) = stamp_indexed(stock, c, at, a, seg.op);
            e.samples += 1;
            if let Some((lo, hi)) = rows {
                e.rows = Some(e.rows.map_or((lo, hi), |(u, v)| (u.min(lo), v.max(hi))));
            }
            if seg.rapid && cut > 0 {
                e.rapid_hit = true;
            } else {
                e.cells_cut += cut;
            }
        }
    }
    e
}

/// Lower the cells whose surface point lies inside a tool standing along machine +Z with its
/// tip at `tip` (machine X/Y/Z), the part turned to A = `a_deg`: each cell's ray from the axis
/// is cut back to where it enters the tool. Rays that pass through the tool below the surface
/// (a hole the heightmap cannot hold) are left alone.
fn stamp_indexed(
    stock: &mut Band,
    c: &Cutter,
    tip: [f64; 3],
    a_deg: f64,
    tag: u16,
) -> (usize, Option<(usize, usize)>) {
    let g = stock.g;
    let rad = c.radius();
    let lim = rad * (1.0 - 1e-9);
    let i0 = (((tip[0] - rad - g.x0) / g.dx - 0.5).ceil()).max(0.0) as usize;
    let i1 = ((tip[0] + rad - g.x0) / g.dx - 0.5).floor();
    if i1 < 0.0 || i0 >= g.nx {
        return (0, None);
    }
    // only this band's rows
    let (i0, i1) = (
        i0.max(stock.row0),
        (i1 as usize)
            .min(g.nx - 1)
            .min(stock.row1.saturating_sub(1)),
    );
    if stock.row1 == 0 || i0 > i1 {
        return (0, None);
    }
    let dphi = TAU / g.na as f64;
    let a = a_deg.to_radians();
    // Only rays that meet the tool's half-strip (|y - ty| < lim, z >= tip z) can be cut.
    // With the tip above the axis those lie between the strip's two lower corners as seen
    // from the axis: an exact bound (padded a cell), so the stock is the same.
    let (j_lo, j_hi) = if tip[2] > 0.0 {
        let t0 = tip[2].datan2(tip[1] + lim);
        let t1 = tip[2].datan2(tip[1] - lim);
        (
            ((t0 - a) / dphi - 1.5).floor() as i64,
            ((t1 - a) / dphi + 0.5).ceil() as i64,
        )
    } else {
        (0, g.na as i64 - 1)
    };
    let na = g.na as i64;
    let mut changed = 0;
    let mut rows: Option<(usize, usize)> = None;
    for i in i0..=i1 {
        let u = g.x(i) - tip[0];
        if u.abs() >= lim {
            continue;
        }
        for jj in j_lo..=j_hi.min(j_lo + na - 1) {
            let j = jj.rem_euclid(na) as usize;
            // the cell's ray in the machine frame: part angle phi turned by A
            let (s, cs) = ((j as f64 + 0.5) * dphi + a).dsin_cos();
            let inside = |rho: f64| {
                let d = (u * u + (rho * cs - tip[1]).powi(2)).sqrt(); // not hypot: slow on wasm32
                d < lim && rho * s >= tip[2] + c.height(d)
            };
            let k = (i - stock.row0) * g.na + j;
            let cur = stock.r[k] as f64;
            if !inside(cur) {
                continue;
            }
            // the tool is convex: along the ray it is one interval ending above `cur`
            let rho = if inside(0.0) {
                0.0
            } else {
                let (mut lo, mut hi) = (0.0, cur);
                while hi - lo > 1e-5 {
                    let mid = 0.5 * (lo + hi);
                    if inside(mid) {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                hi
            };
            if rho < cur {
                if cur - rho > 1e-3 {
                    changed += 1;
                }
                if cur - rho > 1e-6 {
                    stock.op[k] = tag;
                    rows = Some(rows.map_or((i, i), |(p, q)| (p.min(i), q.max(i))));
                }
                stock.r[k] = rho as f32;
            }
        }
    }
    (changed, rows)
}

/// Remove material along a toolpath. `cutter_for(tool)` gives each tool's shape; moves made
/// with a tool that has no cutter (e.g. a V-bit without a diameter) are skipped.
/// `step` is the largest spacing between evaluated tool positions (mm).
pub fn simulate(
    stock: &mut RadialMap,
    tp: &Toolpath,
    cutter_for: &dyn Fn(u32) -> Option<Cutter>,
    step: f64,
) -> SimStats {
    simulate_threads(stock, tp, cutter_for, step, sim_threads(stock.grid.nx))
}

/// [`simulate`] on `threads` threads (bands of rows); the result does not depend on it.
pub fn simulate_threads(
    stock: &mut RadialMap,
    tp: &Toolpath,
    cutter_for: &dyn Fn(u32) -> Option<Cutter>,
    step: f64,
    threads: usize,
) -> SimStats {
    let segs = segments(std::slice::from_ref(tp));
    let cutters: Vec<Option<Cutter>> = segs.iter().map(|s| s.tool.and_then(cutter_for)).collect();
    let mut st = SimStats::default();
    for (seg, e) in segs
        .iter()
        .zip(apply_banded(stock, &segs, &cutters, step, threads))
    {
        st.samples += e.samples;
        st.cells_cut += e.cells_cut;
        if e.rapid_hit {
            st.rapid_hits.push(seg.move_index);
        }
    }
    st
}

/// Simulation grid resolution: rotary cells `dx` mm along X by `na` around; 3-axis cells of
/// the block's longer side / `xyz_cells`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Resolution {
    pub dx: f64,
    pub na: usize,
    pub xyz_cells: usize,
}

impl Resolution {
    /// "coarse" (0.2 mm x 2 deg, 120 across), "normal" (0.1 mm x 1.5 deg, 240), "fine"
    /// (0.05 mm x 0.5 deg, 480), "ultra" (0.025 mm x 0.25 deg, 960).
    pub fn preset(name: &str) -> Option<Resolution> {
        let (dx, na, xyz_cells) = match name {
            "coarse" => (0.2, 180, 120),
            "normal" => (0.1, 240, 240),
            "fine" => (0.05, 720, 480),
            "ultra" => (0.025, 1440, 960),
            _ => return None,
        };
        Some(Resolution { dx, na, xyz_cells })
    }

    /// 3-axis cell size (mm) for a block.
    pub fn cell_for(&self, stock: &crate::job::StockBox) -> f64 {
        let span = (stock.max[0] - stock.min[0]).max(stock.max[1] - stock.min[1]);
        (span / self.xyz_cells.max(1) as f64).max(0.01)
    }
}

/// Largest grid the simulations accept (cells).
pub const MAX_CELLS: usize = 40_000_000;

/// Snapshot spacing for playback: every 1000 segments, wider when the grid is large so the
/// snapshots stay under about 256 MB.
pub fn checkpoint_spacing(segments: usize, cell_bytes: usize) -> usize {
    let budget = 256usize << 20;
    let fit = (budget / cell_bytes.max(1)).max(1);
    1000usize.max(segments.div_ceil(fit))
}

/// Step through a whole job's material removal, forwards or backwards. Stock snapshots every
/// `every` segments make jumping back cheap.
pub struct Playback {
    pub segments: Vec<Segment>,
    cutters: std::collections::HashMap<u32, Cutter>,
    step: f64,
    pub stock: RadialMap,
    initial: RadialMap,
    pos: usize,
    every: usize,
    checkpoints: Vec<(usize, RadialMap)>,
}

impl Playback {
    pub fn new(
        initial: RadialMap,
        tps: &[Toolpath],
        cutter_for: &dyn Fn(u32) -> Option<Cutter>,
        step: f64,
    ) -> Playback {
        let segments = segments(tps);
        let cutters = segments
            .iter()
            .filter_map(|s| s.tool)
            .filter_map(|t| cutter_for(t).map(|c| (t, c)))
            .collect();
        let every = checkpoint_spacing(segments.len(), initial.r.len() * 6);
        Playback {
            segments,
            cutters,
            step,
            stock: initial.clone(),
            initial,
            pos: 0,
            every,
            checkpoints: Vec::new(),
        }
    }

    /// Segments between stock snapshots.
    pub fn checkpoint_every(&self) -> usize {
        self.every
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Segments applied so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Show the stock after the first `n` segments. Returns the rows that may have changed.
    pub fn seek(&mut self, n: usize) -> Option<(usize, usize)> {
        let n = n.min(self.segments.len());
        let mut dirty: Option<(usize, usize)> = None;
        if n < self.pos {
            let (at, snap) = self
                .checkpoints
                .iter()
                .rev()
                .find(|(at, _)| *at <= n)
                .map(|(at, s)| (*at, s.clone()))
                .unwrap_or((0, self.initial.clone()));
            self.stock = snap;
            self.pos = at;
            dirty = Some((0, self.stock.grid.nx.saturating_sub(1)));
        }
        while self.pos < n {
            // up to the next snapshot; long runs split the rows across threads
            let stop = n.min((self.pos / self.every + 1) * self.every);
            let segs = &self.segments[self.pos..stop];
            let cutters: Vec<Option<Cutter>> = segs
                .iter()
                .map(|s| s.tool.and_then(|t| self.cutters.get(&t).copied()))
                .collect();
            let threads = if segs.len() >= 500 {
                sim_threads(self.stock.grid.nx)
            } else {
                1
            };
            for e in apply_banded(&mut self.stock, segs, &cutters, self.step, threads) {
                if let Some((lo, hi)) = e.rows {
                    dirty = Some(dirty.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                }
            }
            self.pos = stop;
            if self.pos.is_multiple_of(self.every)
                && !self.checkpoints.iter().any(|(at, _)| *at == self.pos)
            {
                self.checkpoints.push((self.pos, self.stock.clone()));
                self.checkpoints.sort_by_key(|(at, _)| *at);
            }
        }
        dirty
    }
}

/// Lower every cell inside the tool at tip position (x, r, a). Returns cells changed by more
/// than 1 µm.
fn stamp(
    stock: &mut Band,
    c: &Cutter,
    x: f64,
    r: f64,
    a_deg: f64,
    tag: u16,
) -> (usize, Option<(usize, usize)>) {
    let g = stock.g;
    let rad = c.radius();
    let phi_t = (PI / 2.0 - a_deg.to_radians()).rem_euclid(TAU);
    let i0 = (((x - rad - g.x0) / g.dx - 0.5).ceil()).max(0.0) as usize;
    let i1 = ((x + rad - g.x0) / g.dx - 0.5).floor();
    if i1 < 0.0 || i0 >= g.nx {
        return (0, None);
    }
    // only this band's rows
    let (i0, i1) = (
        i0.max(stock.row0),
        (i1 as usize)
            .min(g.nx - 1)
            .min(stock.row1.saturating_sub(1)),
    );
    if stock.row1 == 0 || i0 > i1 {
        return (0, None);
    }
    let mut rows: Option<(usize, usize)> = None;
    let half = if r <= rad {
        PI / 2.0
    } else {
        (rad / r).dasin()
    };
    let dphi = TAU / g.na as f64;
    let j0 = ((phi_t - half) / dphi - 0.5).ceil() as i64;
    let j1 = ((phi_t + half) / dphi - 0.5).floor() as i64;
    let mut changed = 0;
    for i in i0..=i1 {
        let u = g.x(i) - x;
        // touching the tool's edge is not cutting (cells exactly at a wall)
        let lim = rad * (1.0 - 1e-9);
        if u.abs() >= lim {
            continue;
        }
        for jj in j0..=j1 {
            let j = jj.rem_euclid(g.na as i64) as usize;
            let delta = jj as f64 * dphi + 0.5 * dphi - phi_t;
            let (s, cs) = delta.dsin_cos();
            if cs <= 1e-9 {
                continue;
            }
            let s = s.abs();
            // lowest radius on this cell's ray that is inside the tool
            let mut rho = r / cs;
            let mut inside = false;
            for _ in 0..8 {
                let d = (u * u + (rho * s) * (rho * s)).sqrt();
                if d >= lim {
                    inside = false;
                    break;
                }
                inside = true;
                let next = (r + c.height(d)) / cs;
                if (next - rho).abs() < 1e-7 {
                    rho = next;
                    break;
                }
                rho = next;
            }
            if inside {
                let k = (i - stock.row0) * g.na + j;
                let cur = stock.r[k] as f64;
                if rho < cur {
                    if cur - rho > 1e-3 {
                        changed += 1;
                    }
                    if cur - rho > 1e-6 {
                        stock.op[k] = tag;
                        rows = Some(rows.map_or((i, i), |(a, b)| (a.min(i), b.max(i))));
                    }
                    stock.r[k] = rho as f32;
                }
            }
        }
    }
    (changed, rows)
}

/// Comparison of a simulated stock with the finished part.
#[derive(Debug, Clone, PartialEq)]
pub struct Deviation {
    /// Cells compared (part present).
    pub cells: usize,
    /// Most material left above the part (mm).
    pub max_left: f64,
    /// Mean material left (mm).
    pub mean_left: f64,
    /// Deepest cut into the part (mm, >= 0).
    pub max_gouge: f64,
    /// Where the deepest gouge is: (x, part angle in degrees).
    pub gouge_at: Option<(f64, f64)>,
    /// Cells more than `tol` below the part.
    pub gouge_cells: usize,
    /// Cells more than `tol` below the part that were last cut by an operation meant to
    /// cut into it (engraving, knurls, chamfers, indexed features): not counted above.
    pub feature_cells: usize,
}

/// Compare `stock` with `part` over cells whose x lies in `[x_lo, x_hi]` and where the part
/// exists. Material left is measured from the highest part radius in each cell, gouges from
/// the lowest, so a cell that straddles a wall counts as neither.
pub fn deviation(stock: &RadialMap, part: &Part, x_lo: f64, x_hi: f64, tol: f64) -> Deviation {
    deviation_with(stock, part, x_lo, x_hi, tol, &|_| false)
}

/// [`deviation`], leaving out cells below the part whose last cut came from an operation
/// tag for which `intended` is true (counted in `feature_cells` instead).
pub fn deviation_with(
    stock: &RadialMap,
    part: &Part,
    x_lo: f64,
    x_hi: f64,
    tol: f64,
    intended: &dyn Fn(u16) -> bool,
) -> Deviation {
    assert_eq!(stock.grid, part.center.grid);
    let g = stock.grid;
    let mut d = Deviation {
        cells: 0,
        max_left: 0.0,
        mean_left: 0.0,
        max_gouge: 0.0,
        gouge_at: None,
        gouge_cells: 0,
        feature_cells: 0,
    };
    let mut sum = 0.0;
    for i in 0..g.nx {
        let x = g.x(i);
        if x < x_lo || x > x_hi {
            continue;
        }
        for j in 0..g.na {
            let (hi, lo) = (part.hi.at(i, j), part.lo.at(i, j));
            // (under an overhang: only a cut below the part under the gap is a gouge)
            let lo = part.under.as_ref().map_or(lo, |u| lo.min(u.at(i, j)));
            if hi <= 0.0 {
                continue;
            }
            let s = stock.at(i, j);
            if lo - s > tol && intended(stock.op[i * g.na + j]) {
                d.feature_cells += 1;
                continue;
            }
            let left = s - hi;
            d.cells += 1;
            sum += left.max(0.0);
            d.max_left = d.max_left.max(left);
            // (Mar's carry-through: 3340 "gouged" cells on the turned OD the knurl never
            // touched) a cell below the part that nothing here cut is where the stock started
            // (a piece carried in, at the previous setup's resolution), not a gouge of these
            // programs: theirs only
            let cut = stock.op[i * g.na + j] != 0;
            let gouge = if cut { lo - s } else { (lo - s).min(0.0) };
            if gouge > d.max_gouge {
                d.max_gouge = gouge;
                d.gouge_at = Some((x, g.phi(j).to_degrees()));
            }
            if gouge > tol {
                d.gouge_cells += 1;
            }
        }
    }
    d.mean_left = if d.cells > 0 {
        sum / d.cells as f64
    } else {
        0.0
    };
    d
}

/// PNG of a simulated stock: side view (stock and part radius along the column at part
/// angle 90°, i.e. under the tool at A0) and an unrolled map of material left (stock - part).
pub fn stock_png(title: &str, stock: &RadialMap, part: &RadialMap, stock_r: f64) -> Vec<u8> {
    let g = stock.grid;
    let (w, h) = (1400usize, 1000usize);
    let mut cv = Canvas::new(w, h, [255, 255, 255]);
    cv.text(20, 12, 2, title, [40, 40, 40]);
    let (xa, xb) = (g.x0, g.x0 + g.nx as f64 * g.dx);
    let gx = nice(xb - xa);
    let side = Frame {
        px: [80.0, 50.0, w as f64 - 30.0, 470.0],
        data: [xa, 0.0, xb, stock_r + 1.0],
    };
    side.axes(&mut cv, gx, nice(stock_r + 1.0), "X", "R");
    let j90 = g.na / 4;
    for i in 0..g.nx {
        let x = g.x(i);
        let (px, py) = side.map(x, part.at(i, j90));
        let (_, p0) = side.map(x, 0.0);
        cv.line(px, py, px, p0, [205, 205, 205], 1.0);
    }
    for i in 0..g.nx {
        // envelope over all angles (light) and the A0 column (dark)
        let x = g.x(i);
        let max_r = (0..g.na).map(|j| stock.at(i, j)).fold(0.0, f64::max);
        let min_r = (0..g.na)
            .map(|j| stock.at(i, j))
            .fold(f64::INFINITY, f64::min);
        let (px, y0) = side.map(x, min_r);
        let (_, y1) = side.map(x, max_r);
        cv.line(px, y0, px, y1, [120, 160, 230], 1.0);
        let (_, y) = side.map(x, stock.at(i, j90));
        cv.rect(px - 1.0, y - 1.0, px + 1.0, y + 1.0, [20, 60, 170]);
    }
    let un = Frame {
        px: [80.0, 540.0, w as f64 - 30.0, 950.0],
        data: [xa, 0.0, xb, 360.0],
    };
    // material left: 0 (blue) .. 0.5 mm (red); light grey where the part is absent;
    // below the part (knurls, chamfers, gouges): grey-violet, darker = deeper (0..1 mm)
    let (l, t, r, b) = (un.px[0], un.px[1], un.px[2], un.px[3]);
    for py in t as i64..=b as i64 {
        for px in l as i64..=r as i64 {
            let x = xa + (px as f64 - l) / (r - l) * (xb - xa);
            let phi = (b - py as f64) / (b - t) * 360.0;
            let i = (((x - g.x0) / g.dx) as usize).min(g.nx - 1);
            let j = ((phi / 360.0 * g.na as f64) as usize).min(g.na - 1);
            let p = part.at(i, j);
            let c = if p <= 0.0 {
                [235, 235, 235]
            } else {
                let left = stock.at(i, j) - p;
                if left < -0.01 {
                    below((-left).min(1.0))
                } else {
                    ramp(left / 0.5)
                }
            };
            cv.set(px, py, c);
        }
    }
    un.axes(&mut cv, gx, 60.0, "X", "A");
    for i in 0..200 {
        let x = w as f64 - 260.0 + i as f64;
        cv.line(x, 975.0, x, 990.0, ramp(i as f64 / 199.0), 1.0);
    }
    for i in 0..120 {
        let x = 120.0 + i as f64;
        cv.line(x, 975.0, x, 990.0, below(i as f64 / 119.0), 1.0);
    }
    cv.text(20, 978, 1, "BELOW 0", [60, 60, 60]);
    cv.text(246, 978, 1, "1MM", [60, 60, 60]);
    cv.text(w as i64 - 390, 978, 1, "LEFT 0", [60, 60, 60]);
    cv.text(w as i64 - 54, 978, 1, "0.5MM", [60, 60, 60]);
    cv.to_png()
}

/// Colour for material removed below the part, `t` = depth / 1 mm.
fn below(t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (200.0 - 170.0 * t) as u8,
        (190.0 - 170.0 * t) as u8,
        (225.0 - 150.0 * t) as u8,
    ]
}

fn nice(span: f64) -> f64 {
    let raw = span / 10.0;
    let p = 10f64.dpowf(raw.dlog10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if raw <= m * p {
            return m * p;
        }
    }
    10.0 * p
}

/// Leave the operations `hidden` (their planning numbers, as in [`crate::Move::Operation`]) out
/// of `tps`: their moves go (their tool changes stay, so the operations after them have their
/// tool), so a simulation shows what the others do. The material they would remove stays.
pub fn without_ops(tps: &mut [crate::Toolpath], hidden: &[u32]) {
    use crate::Move;
    if hidden.is_empty() {
        return;
    }
    for tp in tps {
        let mut skip = false;
        tp.moves.retain(|m| {
            if let Move::Operation { id, .. } = m {
                skip = hidden.contains(id);
                return !skip;
            }
            !skip || matches!(m, Move::ToolChange { .. })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;
    use crate::job::Profile;

    fn cyl_profile() -> Profile {
        Profile {
            points: vec![[-10.0, 4.0], [-5.0, 4.0], [-5.0, 6.0], [0.0, 6.0]],
            beyond_front: None,
        }
    }

    #[test]
    fn mesh_map_matches_profile_map() {
        let p = cyl_profile();
        let g = Grid::new(-12.0, 2.0, 0.1, 360);
        let a = RadialMap::from_mesh(g, &Mesh::revolve(&p, 720));
        let b = RadialMap::from_profile(g, &RevolvedProfile::new(&p, 9.0));
        for i in 0..g.nx {
            for j in 0..g.na {
                assert!(
                    (a.at(i, j) - b.at(i, j)).abs() < 2e-3,
                    "x={} j={j}: {} vs {}",
                    g.x(i),
                    a.at(i, j),
                    b.at(i, j)
                );
            }
        }
    }

    #[test]
    fn one_stamp_cuts_a_flat_footprint() {
        let g = Grid::new(-5.0, 5.0, 0.05, 720);
        let mut s = RadialMap::filled(g, 10.0);
        let c = Cutter::Flat { radius: 1.0 };
        // tool at A0 sits over part angle 90 deg
        let (n, rows) = stamp(&mut s.band(), &c, 0.0, 8.0, 0.0, 7);
        assert!(rows.is_some());
        assert!(s.op.contains(&7));
        assert!(n > 0);
        let j90 = g.na / 4;
        let i0 = ((0.0 - g.x0) / g.dx) as usize;
        assert!((s.at(i0, j90) - 8.0).abs() < 1e-3);
        // outside the footprint along X
        let i_far = ((1.5 - g.x0) / g.dx) as usize;
        assert_eq!(s.at(i_far, j90), 10.0);
        // opposite side untouched
        assert_eq!(s.at(i0, 3 * g.na / 4), 10.0);
    }

    #[test]
    fn full_turn_turns_a_cylinder() {
        let g = Grid::new(-3.0, 3.0, 0.05, 360);
        let mut s = RadialMap::filled(g, 10.0);
        let mut b = crate::toolpath::Builder::new();
        b.tool_change(1, 1000);
        b.rapid_r(12.0);
        b.rapid_x(0.0);
        b.plunge(9.0, 60.0);
        for k in 1..=60 {
            b.cut(0.0, 9.0, k as f64 * 6.0, 100.0, 60.0);
        }
        b.rapid_r(12.0);
        let tp = Toolpath {
            name: "t".into(),
            title: "t".into(),
            tool: None,
            notes: vec![],
            reset_a: false,
            safe_r: 12.0,
            moves: b.moves,
            origin_comment: None,
            plunge_limit: None,
            ramp_angle: None,
            post: None,
        };
        let st = simulate(&mut s, &tp, &|_| Some(Cutter::Flat { radius: 1.0 }), 0.1);
        assert!(st.rapid_hits.is_empty());
        let i0 = ((0.0 - g.x0) / g.dx) as usize;
        for j in 0..g.na {
            assert!((s.at(i0, j) - 9.0).abs() < 1e-3, "j={j}: {}", s.at(i0, j));
        }
    }

    #[test]
    fn deviation_finds_a_real_gouge_but_not_a_wall_sliver() {
        let p = cyl_profile(); // r 4 for x in [-10, -5), r 6 for [-5, 0]
        let geom = RevolvedProfile::new(&p, 9.0);
        let g = Grid::new(-12.0, 2.0, 0.2, 90);
        let part = Part::from_profile(g, &geom, &p.points);
        let mut s = RadialMap::filled(g, 9.0);
        // cut to the part everywhere, then dig 1 mm into the r=6 section at A0
        for k in 0..s.r.len() {
            s.r[k] = part.center.r[k];
        }
        let c = Cutter::Flat { radius: 0.5 };
        stamp(&mut s.band(), &c, -2.0, 5.0, 0.0, 1);
        let d = deviation(&s, &part, -10.0, 0.0, 0.02);
        assert!((d.max_gouge - 1.0).abs() < 1e-3, "{d:?}");
        assert!(d.gouge_cells > 0);
        let (x, phi) = d.gouge_at.unwrap();
        assert!(
            (x + 2.0).abs() < 0.6 && (phi - 90.0).abs() < 10.0,
            "{x} {phi}"
        );
        // a cell straddling the step at x = -5 holds both radii: no gouge, no leftover
        let mut s2 = RadialMap::filled(g, 9.0);
        for k in 0..s2.r.len() {
            s2.r[k] = part.lo.r[k];
        }
        let d2 = deviation(&s2, &part, -10.0, 0.0, 0.02);
        assert_eq!(d2.gouge_cells, 0, "{d2:?}");
    }

    #[test]
    fn rapid_through_stock_is_reported() {
        let g = Grid::new(-3.0, 3.0, 0.1, 180);
        let mut s = RadialMap::filled(g, 10.0);
        let tp = Toolpath {
            name: "t".into(),
            title: "t".into(),
            tool: None,
            notes: vec![],
            reset_a: false,
            safe_r: 12.0,
            moves: vec![
                Move::ToolChange { tool: 1, rpm: 1 },
                Move::Rapid {
                    x: Some(0.0),
                    r: Some(12.0),
                    a: None,
                },
                Move::Rapid {
                    x: None,
                    r: Some(9.0),
                    a: None,
                },
            ],
            origin_comment: None,
            plunge_limit: None,
            ramp_angle: None,
            post: None,
        };
        let st = simulate(&mut s, &tp, &|_| Some(Cutter::Flat { radius: 1.0 }), 0.1);
        assert_eq!(st.rapid_hits, vec![2]);
    }
}

#[cfg(test)]
mod overhang_tests {
    use super::*;

    /// A box's 12 triangles (outward).
    fn cuboid(lo: [f64; 3], hi: [f64; 3]) -> Vec<crate::mesh::Triangle> {
        let c = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let quads = [
            [0, 2, 6, 4],
            [1, 5, 7, 3],
            [0, 4, 5, 1],
            [2, 3, 7, 6],
            [0, 1, 3, 2],
            [4, 6, 7, 5],
        ];
        quads
            .iter()
            .flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]])
            .collect()
    }

    /// (Mar's impeller: 1700 "gouged" cells, 10.7 mm deep, where the tool had cut under a
    /// blade leaning over the hub) a hub round the axis and a ledge out past a gap: a stock cut
    /// down into the gap is no gouge; cut into the hub, it is.
    #[test]
    fn a_cut_under_an_overhang_is_no_gouge_a_cut_into_what_is_below_is() {
        let mut tris = cuboid([-10.0, -5.0, -5.0], [0.0, 5.0, 5.0]);
        tris.extend(cuboid([-10.0, 8.0, -1.0], [0.0, 10.0, 1.0]));
        let mesh = Mesh {
            triangles: tris,
            faces: vec![],
        };
        let g = Grid::new(-10.0, 0.0, 0.1, 360);
        let part = Part::from_mesh(g, &mesh);
        assert!(part.under.is_some());
        // the ray along +Y (phi 0) halfway along: hub to 5, gap to 8, ledge to 10
        let (i, j) = (50, 0);
        assert!(
            (part.hi.at(i, j) - 10.0).abs() < 1e-3,
            "{}",
            part.hi.at(i, j)
        );
        assert!((part.under.as_ref().unwrap().at(i, j) - 5.0).abs() < 1e-3);
        let mut stock = part.hi.clone();
        // (cut there by an operation: only cuts count)
        stock.op[i * g.na + j] = 1;
        stock.r[i * g.na + j] = 6.0;
        let d = deviation(&stock, &part, -10.0, 0.0, 0.02);
        assert_eq!(d.gouge_cells, 0, "{d:?}");
        stock.r[i * g.na + j] = 4.0;
        let d = deviation(&stock, &part, -10.0, 0.0, 0.02);
        assert_eq!(d.gouge_cells, 1, "{d:?}");
        assert!((d.max_gouge - 1.0).abs() < 1e-3, "{d:?}");
    }
}
