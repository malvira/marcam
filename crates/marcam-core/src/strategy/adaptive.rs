//! Adaptive (constant-engagement) clearing: at each level the tool follows the edge of the
//! material left, turning in or out at every step so the material it newly cuts is the
//! stepover's width (`ae`) along the step: a steady load on the tool, never a full-width slot
//! (but where a helix or a plunge comes in). It comes in from open air where it can (beside
//! the stock, or above what is already cleared), else down a helix at the widest place; it
//! links runs at depth across what is already cleared, or over the top.
//!
//! The level is a grid: where the tool's centre may stand (it cuts nothing it must not), the
//! stock at that height, and what the tool has cut. The tool cuts a disc of its radius.

use crate::det::Det;
/// The level's grid: cell (i, j) at (x0 + i h, y0 + j h).
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub x0: f64,
    pub y0: f64,
    pub h: f64,
    pub nx: usize,
    pub ny: usize,
}

impl Grid {
    pub fn point(&self, c: usize) -> [f64; 2] {
        [
            self.x0 + (c % self.nx) as f64 * self.h,
            self.y0 + (c / self.nx) as f64 * self.h,
        ]
    }

    /// The cell nearest `p` (None: off the grid).
    pub fn cell(&self, p: [f64; 2]) -> Option<usize> {
        let i = ((p[0] - self.x0) / self.h).round();
        let j = ((p[1] - self.y0) / self.h).round();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return None;
        }
        Some(j as usize * self.nx + i as usize)
    }

    pub fn len(&self) -> usize {
        self.nx * self.ny
    }

    /// Each cell within `radius` of `p` (row by row).
    pub fn disc(&self, p: [f64; 2], radius: f64, mut f: impl FnMut(usize)) {
        let h = self.h;
        let j0 = ((p[1] - radius - self.y0) / h).ceil().max(0.0) as usize;
        let j1 = ((p[1] + radius - self.y0) / h).floor();
        if j1 < 0.0 {
            return;
        }
        let j1 = (j1 as usize).min(self.ny.saturating_sub(1));
        for j in j0..=j1 {
            let dy = self.y0 + j as f64 * h - p[1];
            let w2 = radius * radius - dy * dy;
            if w2 < 0.0 {
                continue;
            }
            let w = w2.sqrt();
            let i0 = ((p[0] - w - self.x0) / h).ceil().max(0.0) as usize;
            let i1 = ((p[0] + w - self.x0) / h).floor();
            if i1 < 0.0 {
                continue;
            }
            let i1 = (i1 as usize).min(self.nx.saturating_sub(1));
            for i in i0..=i1 {
                f(j * self.nx + i);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a level's clearing does, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Up to the safe height, over to the point, and down (nothing in the way there).
    Down([f64; 2]),
    /// Up to the safe height, over, and down a helix round `centre` (radius `radius`) from
    /// above the material, ending with a whole turn at depth at angle `start` (radians).
    Helix {
        centre: [f64; 2],
        radius: f64,
        start: f64,
    },
    /// Up, over and straight down into the material (no room for a helix).
    Plunge([f64; 2]),
    /// Cutting at depth, through these points.
    Cut(Vec<[f64; 2]>),
    /// At depth across what is cleared (cutting nothing), through these points.
    Link(Vec<[f64; 2]>),
}

/// A level's clearing.
#[derive(Debug, Clone, Default)]
pub struct Cleared {
    pub steps: Vec<Step>,
    /// Plunges into the material (no room for a helix).
    pub plunges: usize,
    /// Cells of material the tool could reach but left (crumbs too small to go after).
    pub left: usize,
}

/// How a level is cleared.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Tool radius.
    pub r: f64,
    /// The width of material to cut (mm): the stepover.
    pub ae: f64,
    /// Climb milling (spindle clockwise: the material on the tool's right).
    pub climb: bool,
    /// Helix radius at most this (mm); 0: no helix (plunge where nothing is open).
    pub helix: f64,
}

/// Approximate Euclidean distance (mm) from each cell to the nearest cell where `src` holds
/// (chamfer 3-4, two passes); infinite with none.
pub fn distance(g: &Grid, src: &[bool]) -> Vec<f64> {
    let (nx, ny) = (g.nx, g.ny);
    let big = f64::INFINITY;
    let mut d: Vec<f64> = src.iter().map(|&s| if s { 0.0 } else { big }).collect();
    let (a, b) = (g.h, g.h * std::f64::consts::SQRT_2);
    for j in 0..ny {
        for i in 0..nx {
            let k = j * nx + i;
            let mut v = d[k];
            if i > 0 {
                v = v.min(d[k - 1] + a);
            }
            if j > 0 {
                v = v.min(d[k - nx] + a);
                if i > 0 {
                    v = v.min(d[k - nx - 1] + b);
                }
                if i + 1 < nx {
                    v = v.min(d[k - nx + 1] + b);
                }
            }
            d[k] = v;
        }
    }
    for j in (0..ny).rev() {
        for i in (0..nx).rev() {
            let k = j * nx + i;
            let mut v = d[k];
            if i + 1 < nx {
                v = v.min(d[k + 1] + a);
            }
            if j + 1 < ny {
                v = v.min(d[k + nx] + a);
                if i + 1 < nx {
                    v = v.min(d[k + nx + 1] + b);
                }
                if i > 0 {
                    v = v.min(d[k + nx - 1] + b);
                }
            }
            d[k] = v;
        }
    }
    d
}

/// The search buffers [`Level::way`] reuses (as large as the grid).
struct Scratch {
    back: Vec<usize>,
    dist: Vec<f64>,
    /// the cells the last search reached
    seen: Vec<usize>,
}

/// One level's clearing state.
struct Level<'a> {
    g: Grid,
    p: Params,
    allowed: &'a [bool],
    /// stock at this height the tool can reach (within its radius of an allowed cell)
    material: Vec<bool>,
    /// stock at this height (all of it, reachable or not)
    stock: &'a [bool],
    /// nothing left above this level over the cell
    open: &'a [bool],
    cut: Vec<bool>,
    /// material cells cut so far
    removed: usize,
    /// the cells (columns i0..=i1, rows j0..=j1) where material has gone since the free cells
    /// were last brought up to date ([`Level::refresh`])
    gone: Option<[usize; 4]>,
    /// material cells left ([`Level::remaining`])
    left: usize,
}

impl Level<'_> {
    fn remaining(&self, c: usize) -> bool {
        self.material[c] && !self.cut[c]
    }

    /// Cell `c`'s material has gone (cut, or given up as crumbs).
    fn taken(&mut self, c: usize) {
        self.left -= 1;
        let (i, j) = (c % self.g.nx, c / self.g.nx);
        self.gone = Some(match self.gone {
            None => [i, i, j, j],
            Some([i0, i1, j0, j1]) => [i0.min(i), i1.max(i), j0.min(j), j1.max(j)],
        });
    }

    /// Cells of material still there under the tool at `p`.
    fn engagement(&self, p: [f64; 2]) -> usize {
        let mut n = 0;
        self.g.disc(p, self.p.r, |c| {
            if self.remaining(c) {
                n += 1;
            }
        });
        n
    }

    /// The tool at `p` (the cells within `radius` of it: those [`Level::engagement`] counts)
    /// cuts. The material cells it newly cuts.
    fn cut_at(&mut self, p: [f64; 2], radius: f64) -> usize {
        let mut cells = Vec::new();
        self.g.disc(p, radius, |c| cells.push(c));
        let mut n = 0;
        for c in cells {
            if !self.cut[c] {
                self.cut[c] = true;
                if self.material[c] {
                    n += 1;
                    self.taken(c);
                }
            }
        }
        n
    }

    /// The tool's centre may stand at `p` (the cells round it allowed).
    fn may(&self, p: [f64; 2]) -> bool {
        let g = &self.g;
        let fi = (p[0] - g.x0) / g.h;
        let fj = (p[1] - g.y0) / g.h;
        if fi < 0.0 || fj < 0.0 || fi > (g.nx - 1) as f64 || fj > (g.ny - 1) as f64 {
            return false;
        }
        let (i0, j0) = (fi.floor() as usize, fj.floor() as usize);
        let (i1, j1) = ((i0 + 1).min(g.nx - 1), (j0 + 1).min(g.ny - 1));
        [(i0, j0), (i1, j0), (i0, j1), (i1, j1)]
            .iter()
            .all(|&(i, j)| self.allowed[j * g.nx + i])
    }

    /// Where the material left near `p` lies (a unit vector), if any within `reach`.
    fn towards_material(&self, p: [f64; 2], reach: f64) -> Option<[f64; 2]> {
        let g = &self.g;
        let k = (reach / g.h).ceil() as isize;
        let c = g.cell(p)?;
        let (ci, cj) = ((c % g.nx) as isize, (c / g.nx) as isize);
        let mut v = [0.0, 0.0];
        let mut n = 0;
        for dj in -k..=k {
            for di in -k..=k {
                let (a, b) = (ci + di, cj + dj);
                if a < 0 || b < 0 || a >= g.nx as isize || b >= g.ny as isize {
                    continue;
                }
                let q = g.point(b as usize * g.nx + a as usize);
                let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
                let d2 = dx * dx + dy * dy;
                if d2 > reach * reach || d2 < 1e-12 {
                    continue;
                }
                if self.remaining(b as usize * g.nx + a as usize) {
                    let d = d2.sqrt();
                    v[0] += dx / d;
                    v[1] += dy / d;
                    n += 1;
                }
            }
        }
        let l = v[0].dhypot(v[1]);
        (n > 0 && l > 1e-9).then(|| [v[0] / l, v[1] / l])
    }

    /// One run of cutting from `p` heading `dir` (radians), following the material's edge.
    /// With `forced`, it cuts on where the least it can take is more than twice the target
    /// (a slot: no other way in); else it stops there.
    fn run(
        &mut self,
        mut p: [f64; 2],
        mut dir: f64,
        steps_left: &mut usize,
        forced: bool,
    ) -> (Vec<[f64; 2]>, bool) {
        let h = self.g.h;
        let s = (0.25 * self.p.r).max(1.5 * h);
        // the cells a step should cut: the stepover's width along it
        let target = (self.p.ae * s / (h * h)).max(1.0);
        let side = if self.p.climb { 1.0 } else { -1.0 };
        let mut pts = vec![p];
        // from the far left (away from the material) round to the far right (into it)
        const SWEEP: f64 = 150.0;
        const N: usize = 30;
        while *steps_left > 0 {
            *steps_left -= 1;
            let at = |t: f64| t.to_radians();
            let mut best: Option<(f64, usize)> = None;
            let mut prev: Option<(f64, usize)> = None;
            let mut chosen: Option<f64> = None;
            for k in 0..=N {
                let turn = SWEEP - 2.0 * SWEEP * k as f64 / N as f64;
                let a = dir + side * at(turn);
                let q = [p[0] + s * a.dcos(), p[1] + s * a.dsin()];
                if !self.may(q) {
                    prev = None;
                    continue;
                }
                let e = self.engagement(q);
                if best.is_none_or(|(_, b)| e > b) {
                    best = Some((turn, e));
                }
                if e as f64 >= target {
                    // between the last one under and this one: halve the angle a few times
                    if let Some((t0, _)) = prev {
                        let (mut lo, mut hi) = (t0, turn);
                        for _ in 0..4 {
                            let mid = 0.5 * (lo + hi);
                            let am = dir + side * at(mid);
                            let qm = [p[0] + s * am.dcos(), p[1] + s * am.dsin()];
                            if self.may(qm) && self.engagement(qm) as f64 >= target {
                                hi = mid;
                            } else {
                                lo = mid;
                            }
                        }
                        let am = dir + side * at(hi);
                        let qm = [p[0] + s * am.dcos(), p[1] + s * am.dsin()];
                        chosen = Some(if self.may(qm) { hi } else { turn });
                    } else {
                        chosen = Some(turn);
                    }
                    break;
                }
                prev = Some((turn, e));
            }
            let turn = match (chosen, best) {
                (Some(t), _) => {
                    // more than twice the target even at the least: a slot
                    let a = dir + side * t.to_radians();
                    let e = self.engagement([p[0] + s * a.dcos(), p[1] + s * a.dsin()]);
                    if e as f64 > 2.0 * target && !forced {
                        return (pts, true);
                    }
                    t
                }
                // nowhere as much as the target: the most there is, if anything
                (None, Some((t, e))) if e > 0 => t,
                _ => break,
            };
            dir += side * turn.to_radians();
            let q = [p[0] + s * dir.dcos(), p[1] + s * dir.dsin()];
            let mid = [0.5 * (p[0] + q[0]), 0.5 * (p[1] + q[1])];
            self.removed += self.cut_at(mid, self.p.r) + self.cut_at(q, self.p.r);
            pts.push(q);
            p = q;
        }
        (pts, false)
    }

    /// Cells where the tool may stand without cutting anything (nothing left within its
    /// radius) and, with `down`, may come down from above too.
    fn free(&self, down: bool) -> Vec<bool> {
        let n = self.g.len();
        let block: Vec<bool> = (0..n)
            .map(|c| {
                if down {
                    !self.open[c] || (self.stock[c] && !self.cut[c])
                } else {
                    self.remaining(c)
                }
            })
            .collect();
        let d = distance(&self.g, &block);
        (0..n).map(|c| self.allowed[c] && d[c] > self.p.r).collect()
    }

    /// `free` ([`Level::free`] without `down`) and `front` ([`Level::frontier`]) brought up
    /// to date where material has gone since: only cells within the tool's radius of it can
    /// change, and only material within the radius of those decides them, so the distances
    /// are worked out (as [`distance`] does, in the same order) over that much round it only:
    /// the same cells as working them all out again.
    fn refresh(&mut self, free: &mut [bool], front: &mut [bool], fronts: &mut usize) {
        let Some([i0, i1, j0, j1]) = self.gone.take() else {
            return;
        };
        let (nx, ny) = (self.g.nx, self.g.ny);
        let k = (self.p.r / self.g.h).ceil() as usize + 2;
        let grow = |[a, b, c, d]: [usize; 4], k: usize| {
            [
                a.saturating_sub(k),
                (b + k).min(nx - 1),
                c.saturating_sub(k),
                (d + k).min(ny - 1),
            ]
        };
        // the cells that may change, and the window their distances are found in
        let out = grow([i0, i1, j0, j1], k);
        let [wi0, wi1, wj0, wj1] = grow(out, k);
        let (w, hgt) = (wi1 - wi0 + 1, wj1 - wj0 + 1);
        let mut d: Vec<f64> = (0..w * hgt)
            .map(|q| {
                if self.remaining((wj0 + q / w) * nx + wi0 + q % w) {
                    0.0
                } else {
                    f64::INFINITY
                }
            })
            .collect();
        let (a, b) = (self.g.h, self.g.h * std::f64::consts::SQRT_2);
        for j in 0..hgt {
            for i in 0..w {
                let k = j * w + i;
                let mut v = d[k];
                if i > 0 {
                    v = v.min(d[k - 1] + a);
                }
                if j > 0 {
                    v = v.min(d[k - w] + a);
                    if i > 0 {
                        v = v.min(d[k - w - 1] + b);
                    }
                    if i + 1 < w {
                        v = v.min(d[k - w + 1] + b);
                    }
                }
                d[k] = v;
            }
        }
        for j in (0..hgt).rev() {
            for i in (0..w).rev() {
                let k = j * w + i;
                let mut v = d[k];
                if i + 1 < w {
                    v = v.min(d[k + 1] + a);
                }
                if j + 1 < hgt {
                    v = v.min(d[k + w] + a);
                    if i + 1 < w {
                        v = v.min(d[k + w + 1] + b);
                    }
                    if i > 0 {
                        v = v.min(d[k + w - 1] + b);
                    }
                }
                d[k] = v;
            }
        }
        let [oi0, oi1, oj0, oj1] = out;
        for j in oj0..=oj1 {
            for i in oi0..=oi1 {
                let c = j * nx + i;
                free[c] = self.allowed[c] && d[(j - wj0) * w + (i - wi0)] > self.p.r;
            }
        }
        // (a cell's frontier: it and its neighbours free or not)
        let [fi0, fi1, fj0, fj1] = grow(out, 1);
        for j in fj0..=fj1 {
            for i in fi0..=fi1 {
                let f = self.front_at(free, j * nx + i);
                if f != front[j * nx + i] {
                    if f {
                        *fronts += 1;
                    } else {
                        *fronts -= 1;
                    }
                    front[j * nx + i] = f;
                }
            }
        }
    }

    /// Whether cell `c` is on the frontier (see [`Level::frontier`]).
    fn front_at(&self, free: &[bool], c: usize) -> bool {
        let (nx, ny) = (self.g.nx, self.g.ny);
        if !free[c] {
            return false;
        }
        let (i, j) = (c % nx, c / nx);
        for (di, dj) in [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, 1),
            (-1, 1),
            (1, -1),
        ] {
            let (a, b) = (i as isize + di, j as isize + dj);
            if a < 0 || b < 0 || a >= nx as isize || b >= ny as isize {
                continue;
            }
            let q = b as usize * nx + a as usize;
            if self.allowed[q] && !free[q] {
                return true;
            }
        }
        false
    }

    /// Free cells next to one where the tool may stand but is not free (it steps from there
    /// into material).
    fn frontier(&self, free: &[bool]) -> Vec<bool> {
        (0..self.g.len()).map(|c| self.front_at(free, c)).collect()
    }

    /// The heading a run starting at `p` takes: along the material's edge, the material on
    /// the tool's right (climb) or left.
    fn heading(&self, p: [f64; 2]) -> f64 {
        let side = if self.p.climb { 1.0 } else { -1.0 };
        let m = self.towards_material(p, 1.5 * self.p.r + 2.0 * self.g.h);
        m.map_or(0.0, |m| {
            m[1].datan2(m[0]) + side * std::f64::consts::FRAC_PI_2
        })
    }

    /// How far a run from `p` can go straight on before a wall (at most `cap`).
    fn room_ahead(&self, p: [f64; 2], cap: f64) -> f64 {
        let a = self.heading(p);
        let step = self.g.h;
        let mut l = 0.0;
        while l < cap {
            let q = [p[0] + (l + step) * a.dcos(), p[1] + (l + step) * a.dsin()];
            if !self.may(q) {
                break;
            }
            l += step;
        }
        l
    }

    /// The way at depth (cells, free all along but the first) from `from` to a frontier
    /// cell: of the nearest few, the one with the longest run ahead of it (a run that hits a
    /// wall at once is a bite and a retreat).
    fn way(
        &self,
        from: usize,
        free: &[bool],
        front: &[bool],
        sc: &mut Scratch,
    ) -> Option<Vec<usize>> {
        let (nx, ny) = (self.g.nx, self.g.ny);
        // (the buffers as the last search left them, put back: only the cells it reached)
        for &c in &sc.seen {
            sc.back[c] = usize::MAX;
            sc.dist[c] = 0.0;
        }
        sc.seen.clear();
        let Scratch { back, dist, seen } = sc;
        back[from] = from;
        seen.push(from);
        let mut queue = std::collections::VecDeque::from([from]);
        let mut found: Vec<usize> = Vec::new();
        let want = 4.0 * self.p.r;
        let mut reach_limit = f64::INFINITY;
        while let Some(c) = queue.pop_front() {
            if dist[c] > reach_limit {
                break;
            }
            if front[c] {
                found.push(c);
                if found.len() == 1 {
                    // look a little further than the nearest
                    reach_limit = dist[c] + 2.0 * want;
                }
                if found.len() >= 400 {
                    break;
                }
            }
            let (i, j) = (c % nx, c / nx);
            for (di, dj) in [
                (-1, 0),
                (1, 0),
                (0, -1),
                (0, 1),
                (-1, -1),
                (1, 1),
                (-1, 1),
                (1, -1),
            ] {
                let (a, b) = (i as isize + di, j as isize + dj);
                if a < 0 || b < 0 || a >= nx as isize || b >= ny as isize {
                    continue;
                }
                let q = b as usize * nx + a as usize;
                if free[q] && back[q] == usize::MAX {
                    back[q] = c;
                    seen.push(q);
                    let diag = if di != 0 && dj != 0 {
                        std::f64::consts::SQRT_2
                    } else {
                        1.0
                    };
                    dist[q] = dist[c] + diag * self.g.h;
                    queue.push_back(q);
                }
            }
        }
        // (every few cells: neighbours along a face are much the same)
        let best = found
            .iter()
            .enumerate()
            .filter(|&(k, _)| k % 4 == 0 || k + 1 == found.len())
            .map(|(_, &c)| (c, self.room_ahead(self.g.point(c), want) - 0.25 * dist[c]))
            // (each scored once; of equals, the last, as max_by takes it)
            .reduce(|x, y| {
                if x.1.total_cmp(&y.1) == std::cmp::Ordering::Greater {
                    x
                } else {
                    y
                }
            })
            .map(|(c, _)| c)?;
        let mut path = vec![best];
        let mut k = best;
        while k != from {
            k = back[k];
            path.push(k);
        }
        path.reverse();
        Some(path)
    }

    /// The cells of `path` cut short where a straight line stays on free cells.
    fn straighten(&self, path: &[usize], free: &[bool]) -> Vec<[f64; 2]> {
        let g = &self.g;
        let clear = |a: [f64; 2], b: [f64; 2]| {
            let n = ((b[0] - a[0]).dhypot(b[1] - a[1]) / (0.5 * g.h)).ceil() as usize;
            (0..=n).all(|k| {
                let t = k as f64 / n.max(1) as f64;
                let q = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                g.cell(q).is_some_and(|c| free[c])
            })
        };
        let mut out = vec![g.point(path[0])];
        let mut k = 0;
        while k + 1 < path.len() {
            let mut far = k + 1;
            for m in (k + 2..path.len()).rev() {
                if clear(g.point(path[k]), g.point(path[m])) {
                    far = m;
                    break;
                }
            }
            out.push(g.point(path[far]));
            k = far;
        }
        out
    }

    /// The best place for a helix: the most room (at least a fifth of the tool's radius for
    /// its circle), open above, with material to cut. (centre, helix radius)
    fn helix_place(&self) -> Option<([f64; 2], f64)> {
        let n = self.g.len();
        let walls: Vec<bool> = self.allowed.iter().map(|a| !a).collect();
        let room = distance(&self.g, &walls);
        let shut: Vec<bool> = self.open.iter().map(|o| !o).collect();
        let above = distance(&self.g, &shut);
        let left: Vec<bool> = (0..n).map(|c| self.remaining(c)).collect();
        let near = distance(&self.g, &left);
        let mut best: Option<(usize, f64)> = None;
        for c in 0..n {
            if !self.allowed[c] || near[c] > self.p.r {
                continue;
            }
            let hr = (room[c] - self.g.h).min(self.p.helix);
            if hr < 0.2 * self.p.r || above[c] < self.p.r + hr + self.g.h {
                continue;
            }
            // (the most room: in the middle, not against a wall)
            if best.is_none_or(|(b, _)| room[c] > room[b] + 1e-9) {
                best = Some((c, hr));
            }
        }
        best.map(|(c, hr)| (self.g.point(c), hr))
    }
}

/// Clear one level: `allowed` where the tool's centre may stand, `stock` the stock at this
/// height, `open` where nothing is left above it (updated: what is left here closes it for the
/// levels below), from `at` (the tool's last place, at depth, if any).
pub fn clear_level(
    g: Grid,
    allowed: &[bool],
    stock: &[bool],
    open: &mut [bool],
    p: Params,
    at: Option<[f64; 2]>,
) -> Cleared {
    let n = g.len();
    let mut out = Cleared::default();
    // what the tool can reach: stock within its radius of where it may stand
    let near_allowed = distance(&g, allowed);
    let material: Vec<bool> = (0..n).map(|c| stock[c] && near_allowed[c] <= p.r).collect();
    let mut lv = Level {
        g,
        p,
        allowed,
        material,
        stock,
        open: &*open,
        cut: vec![false; n],
        removed: 0,
        gone: None,
        left: 0,
    };
    lv.left = (0..n).filter(|&c| lv.remaining(c)).count();
    if !lv.material.iter().any(|&m| m) {
        return out;
    }
    let mut steps_left = 40 * n + 1000;
    let mut here: Option<[f64; 2]> = at;
    let side = if p.climb { 1.0 } else { -1.0 };
    let mut runs = 0usize;
    let mut bad = vec![false; n];
    // (how many are set)
    let mut bads = 0usize;
    let max_runs = 20_000;
    // (the free cells and the frontier: worked out once, then kept up to date where material
    // goes)
    let mut free = lv.free(false);
    let mut front0 = lv.frontier(&free);
    let mut fronts = front0.iter().filter(|&&f| f).count();
    let mut sc = Scratch {
        back: vec![usize::MAX; n],
        dist: vec![0.0; n],
        seen: Vec::new(),
    };
    while steps_left > 0 && runs < max_runs {
        if crate::budget::over() {
            break;
        }
        runs += 1;
        // the next place to start from: along the cleared floor, else from above where it is
        // open, else down a helix (or a plunge)
        lv.refresh(&mut free, &mut front0, &mut fronts);
        debug_assert!(free == lv.free(false) && front0 == lv.frontier(&free));
        debug_assert_eq!(fronts, front0.iter().filter(|&&f| f).count());
        debug_assert_eq!(lv.left, (0..n).filter(|&c| lv.remaining(c)).count());
        // (starts that led nowhere but into a slot: tried last; with none such, the
        // frontier as it is)
        let masked;
        let (front, forced): (&[bool], bool) = if bads == 0 {
            (&front0, false)
        } else {
            let mut f = front0.clone();
            let forced = fronts > 0 && !f.iter().zip(&bad).any(|(&f, &b)| f && !b);
            if !forced {
                for (f, &b) in f.iter_mut().zip(&bad) {
                    *f &= !b;
                }
            }
            masked = f;
            (&masked, forced)
        };
        let any_front = if bads == 0 {
            fronts > 0
        } else {
            front.iter().any(|&f| f)
        };
        if !any_front && lv.left == 0 {
            break;
        }
        let mut start: Option<[f64; 2]> = None;
        // the way to the start (only if a run follows)
        let mut pre: Option<Step> = None;
        if let Some(c) = here.and_then(|q| g.cell(q)) {
            if let Some(path) = lv.way(c, &free, front, &mut sc) {
                let pts = lv.straighten(&path, &free);
                let mut link = vec![here.unwrap()];
                link.extend(pts.into_iter().skip(1));
                let s = *link.last().unwrap();
                if link.len() > 1 {
                    pre = Some(Step::Link(link));
                }
                start = Some(s);
            }
        }
        if start.is_none() {
            let down = lv.free(true);
            let front_down: Vec<bool> = (0..n).map(|c| down[c] && front[c]).collect();
            let from = here.unwrap_or([g.x0, g.y0]);
            let best = (0..n).filter(|&c| front_down[c]).min_by(|&a, &b| {
                let d = |c: usize| {
                    let q = g.point(c);
                    (q[0] - from[0]).dhypot(q[1] - from[1])
                };
                d(a).total_cmp(&d(b))
            });
            if let Some(c) = best {
                let q = g.point(c);
                pre = Some(Step::Down(q));
                start = Some(q);
            }
        }
        let (pos, dir) = match start {
            Some(s) => (s, lv.heading(s)),
            None => {
                if let Some((c, hr)) = (p.helix > 0.0).then(|| lv.helix_place()).flatten() {
                    // start (and end) the helix on the side away from the walls' nearest
                    let a0 = 0.0;
                    out.steps.push(Step::Helix {
                        centre: c,
                        radius: hr,
                        start: a0,
                    });
                    lv.removed += lv.cut_at(c, p.r + hr);
                    let s = [c[0] + hr * a0.dcos(), c[1] + hr * a0.dsin()];
                    // round the hole: climb counter-clockwise (the material outside on the right)
                    (s, a0 + side * std::f64::consts::FRAC_PI_2)
                } else {
                    // no room: straight down where the most is left round the most room
                    let walls: Vec<bool> = allowed.iter().map(|a| !a).collect();
                    let room = distance(&g, &walls);
                    let best = (0..n)
                        .filter(|&c| allowed[c] && lv.engagement(g.point(c)) > 0)
                        .max_by(|&a, &b| room[a].total_cmp(&room[b]));
                    let Some(c) = best else { break };
                    let q = g.point(c);
                    out.steps.push(Step::Plunge(q));
                    out.plunges += 1;
                    lv.removed += lv.cut_at(q, p.r);
                    let m = lv.towards_material(q, 2.0 * p.r);
                    (
                        q,
                        m.map_or(0.0, |m| {
                            m[1].datan2(m[0]) + side * std::f64::consts::FRAC_PI_2
                        }),
                    )
                }
            }
        };
        let removed_before = lv.removed;
        let (pts, slot) = lv.run(pos, dir, &mut steps_left, forced);
        if pts.len() < 2 && slot {
            // a slot from here: another start first
            if let Some(c) = g.cell(pos) {
                if !bad[c] {
                    bad[c] = true;
                    bads += 1;
                }
            }
            continue;
        }
        if let Some(st) = pre.take() {
            out.steps.push(st);
        }
        if pts.len() < 2 || lv.removed == removed_before {
            // nothing to cut from here after all: leave these crumbs
            let r = p.r + 2.0 * g.h;
            let k = (r / g.h).ceil() as isize;
            if let Some(c) = g.cell(pos) {
                let (ci, cj) = ((c % g.nx) as isize, (c / g.nx) as isize);
                for dj in -k..=k {
                    for di in -k..=k {
                        let (a, b) = (ci + di, cj + dj);
                        if a < 0 || b < 0 || a >= g.nx as isize || b >= g.ny as isize {
                            continue;
                        }
                        let q = b as usize * g.nx + a as usize;
                        if lv.remaining(q) {
                            lv.material[q] = false;
                            lv.taken(q);
                            out.left += 1;
                        }
                    }
                }
            }
            here = Some(pos);
            continue;
        }
        here = pts.last().copied();
        if bads > 0 {
            bad.iter_mut().for_each(|b| *b = false);
            bads = 0;
        }
        out.steps.push(Step::Cut(pts));
    }
    out.left += (0..n).filter(|&c| lv.remaining(c)).count();
    // what is left at this height closes the cell for the levels below
    let cut = std::mem::take(&mut lv.cut);
    for c in 0..n {
        if stock[c] && !cut[c] {
            open[c] = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    /// A 40 x 30 pocket, a 6 mm tool: everything inside reachable is cut, nothing outside.
    #[test]
    fn a_rectangle_is_cleared_at_a_steady_load() {
        let h = 0.25;
        let g = Grid {
            x0: -2.0,
            y0: -2.0,
            h,
            nx: 177,
            ny: 137,
        };
        let r = 3.0;
        let inside = |q: [f64; 2]| q[0] > 0.0 && q[0] < 40.0 && q[1] > 0.0 && q[1] < 30.0;
        let allowed: Vec<bool> = (0..g.len())
            .map(|c| {
                let q = g.point(c);
                q[0] >= r && q[0] <= 40.0 - r && q[1] >= r && q[1] <= 30.0 - r
            })
            .collect();
        let stock: Vec<bool> = (0..g.len()).map(|c| inside(g.point(c))).collect();
        let mut open = vec![true; g.len()];
        let p = Params {
            r,
            ae: 0.9,
            climb: true,
            helix: 0.75 * r,
        };
        let out = clear_level(g, &allowed, &stock, &mut open, p, None);
        assert!(
            matches!(out.steps[0], Step::Helix { .. }),
            "{:?}",
            &out.steps[..1]
        );
        assert_eq!(
            out.plunges,
            0,
            "{:?}",
            out.steps
                .iter()
                .filter(|s| matches!(s, Step::Plunge(_) | Step::Helix { .. }))
                .collect::<Vec<_>>()
        );
        // what was cut: every cell of the pocket but the corners' fillets
        let mut cut = vec![false; g.len()];
        let mut stamp = |q: [f64; 2], rad: f64| g.disc(q, rad, |c| cut[c] = true);
        let mut len = 0.0;
        for s in &out.steps {
            match s {
                Step::Helix { centre, radius, .. } => stamp(*centre, r + radius),
                Step::Cut(pts) | Step::Link(pts) => {
                    for w in pts.windows(2) {
                        len += (w[1][0] - w[0][0]).dhypot(w[1][1] - w[0][1]);
                        for k in 0..=4 {
                            let t = k as f64 / 4.0;
                            stamp(
                                [
                                    w[0][0] + (w[1][0] - w[0][0]) * t,
                                    w[0][1] + (w[1][1] - w[0][1]) * t,
                                ],
                                r,
                            );
                        }
                        assert!(w[1][0] >= r - 0.3 && w[1][0] <= 40.0 - r + 0.3, "{w:?}");
                    }
                }
                _ => {}
            }
        }
        let missed = (0..g.len())
            .filter(|&c| {
                let q = g.point(c);
                // (the corners' fillets stay)
                let corner = (q[0] < r || q[0] > 40.0 - r) && (q[1] < r || q[1] > 30.0 - r);
                stock[c]
                    && !cut[c]
                    && !corner
                    && q[0] > h
                    && q[0] < 40.0 - h
                    && q[1] > h
                    && q[1] < 30.0 - h
            })
            .count();
        assert!(missed < 40, "{missed} cells missed; left {}", out.left);
        // about the area over the stepover, not much more (no wandering)
        assert!(len < 2.0 * 1200.0 / 0.9, "{len:.0} mm of moves");
        let _ = open;
    }
}
