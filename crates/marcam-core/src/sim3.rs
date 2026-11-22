//! Material removal for 3-axis programs: the block as a Z heightmap (one top per XY cell),
//! with the operation that last cut each cell, a move-by-move playback and a comparison
//! against the part. It mirrors the rotary [`crate::sim`] module (same segments, effects,
//! playback and deviation types), on an X/Y grid instead of X/angle.

use crate::dropcutter::Cutter;
use crate::job::StockBox;
use crate::mesh::Mesh;
use crate::sim::{Deviation, Segment, SegmentEffect, SimStats};
use crate::strategy::three_axis::DropCutter3;
use crate::{Move, Toolpath};

#[derive(Debug, Clone)]
pub struct Heightmap3 {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    /// Top Z per cell, row-major (index j * nx + i): a "row" is one Y index.
    pub z: Vec<f64>,
    /// Which operation last cut each cell: operation id + 1 (0 = never cut).
    pub op: Vec<u16>,
    /// Bottom of the block.
    pub bottom: f64,
    /// Top of the block (cutting only lowers it): moves wholly above it are not sampled.
    pub top: f64,
}

impl Heightmap3 {
    pub fn new(stock: &StockBox, cell: f64) -> Heightmap3 {
        let nx = (((stock.max[0] - stock.min[0]) / cell).ceil() as usize).max(2);
        let ny = (((stock.max[1] - stock.min[1]) / cell).ceil() as usize).max(2);
        Heightmap3 {
            x0: stock.min[0],
            y0: stock.min[1],
            cell,
            nx,
            ny,
            z: vec![stock.max[2]; nx * ny],
            op: vec![0; nx * ny],
            bottom: stock.min[2],
            top: stock.max[2],
        }
    }

    /// The job's stock as it comes: the block, or a bar lying in the vise (each row at the
    /// bar's top there, nothing beside it).
    pub fn of_job(job: &crate::Job, stock: &StockBox, cell: f64) -> Heightmap3 {
        // what the setup before left, when it carries its stock over
        if let Some(h) = job.carried.as_ref().and_then(|c| c.heightmap(stock, cell)) {
            return h;
        }
        let mut hm = Heightmap3::new(stock, cell);
        if let (Some([xc, yc]), Some("z")) = (job.stock_axis, job.stock_along.as_deref()) {
            // standing up: full height where the section is, nothing beside it
            let outline = job.stock.outline(1440);
            for j in 0..hm.ny {
                for i in 0..hm.nx {
                    let (x, y) = hm.centre(i, j);
                    if !inside(&outline, x - xc, y - yc) {
                        hm.z[j * hm.nx + i] = hm.bottom;
                    }
                }
            }
        } else if let (Some([xc, zc]), Some("y")) = (job.stock_axis, job.stock_along.as_deref()) {
            // along Y: each column (along X) at the bar's top there
            let outline = job.stock.outline(1440);
            for i in 0..hm.nx {
                let (x, _) = hm.centre(i, 0);
                let top = job
                    .stock
                    .top_at(x - xc, &outline)
                    .map_or(hm.bottom, |t| (t + zc).max(hm.bottom));
                for j in 0..hm.ny {
                    hm.z[j * hm.nx + i] = top;
                }
            }
        } else if let Some([yc, zc]) = job.stock_axis {
            let outline = job.stock.outline(1440);
            for j in 0..hm.ny {
                let (_, y) = hm.centre(0, j);
                let top = job
                    .stock
                    .top_at(y - yc, &outline)
                    .map_or(hm.bottom, |t| (t + zc).max(hm.bottom));
                for i in 0..hm.nx {
                    hm.z[j * hm.nx + i] = top;
                }
            }
        }
        hm
    }

    /// Top of the cell containing (x, y) (clamped to the block).
    pub fn at(&self, x: f64, y: f64) -> f64 {
        let i = ((x - self.x0) / self.cell).max(0.0) as usize;
        let j = ((y - self.y0) / self.cell).max(0.0) as usize;
        self.z[j.min(self.ny - 1) * self.nx + i.min(self.nx - 1)]
    }

    /// Centre of cell (i, j).
    pub fn centre(&self, i: usize, j: usize) -> (f64, f64) {
        (
            self.x0 + (i as f64 + 0.5) * self.cell,
            self.y0 + (j as f64 + 0.5) * self.cell,
        )
    }

    /// Lower every cell under the cutter with its tip at (x, y, z), tagging it with `tag`.
    /// Returns the cells changed by more than 1 µm and the rows (Y indices) they are in.
    pub fn stamp(
        &mut self,
        c: &Cutter,
        x: f64,
        y: f64,
        z: f64,
        tag: u16,
    ) -> (usize, Option<(usize, usize)>) {
        let r = c.radius();
        let lim = r * (1.0 - 1e-9);
        let lo = |v: f64, o: f64| (((v - r - o) / self.cell).floor()).max(0.0) as usize;
        let hi = |v: f64, o: f64, n: usize| {
            ((((v + r - o) / self.cell).floor()) as isize).min(n as isize - 1)
        };
        let (i0, j0) = (lo(x, self.x0), lo(y, self.y0));
        let (i1, j1) = (hi(x, self.x0, self.nx), hi(y, self.y0, self.ny));
        if i1 < 0 || j1 < 0 {
            return (0, None);
        }
        let mut n = 0;
        let mut rows: Option<(usize, usize)> = None;
        for j in j0..=j1 as usize {
            for i in i0..=i1 as usize {
                let (cx, cy) = self.centre(i, j);
                let d = ((cx - x).powi(2) + (cy - y).powi(2)).sqrt(); // not hypot: slow on wasm32
                if d >= lim {
                    continue;
                }
                let zt = z + c.height(d);
                let k = j * self.nx + i;
                if zt < self.z[k] - 1e-6 {
                    self.z[k] = zt;
                    self.op[k] = tag;
                    n += 1;
                    rows = Some(rows.map_or((j, j), |(a, b)| (a.min(j), b.max(j))));
                }
            }
        }
        (n, rows)
    }

    /// Cut a 3-axis toolpath, sampling each move every `step` mm. Returns the indices (in
    /// `tp.moves`) of rapids that removed material (should be none).
    pub fn cut(
        &mut self,
        tp: &Toolpath,
        cutter_for: &dyn Fn(u32) -> Option<Cutter>,
        step: f64,
    ) -> Vec<usize> {
        simulate3(self, tp, cutter_for, step).rapid_hits
    }
}

/// A 3-axis job's motion as segments, in the rotary module's [`Segment`] with `from`/`to` as
/// work [x, y, z]. Each program starts from an unknown position.
pub fn segments3(tps: &[Toolpath]) -> Vec<Segment> {
    let mut out = Vec::new();
    for (p, tp) in tps.iter().enumerate() {
        out.extend(segments_of3(&tp.moves, p as u16));
    }
    out
}

/// The segments of one program's `moves` (see [`segments3`]), numbered as program `p`.
pub fn segments_of3(moves: &[Move], p: u16) -> Vec<Segment> {
    let mut out = Vec::new();
    {
        let mut at: [Option<f64>; 3] = [None; 3];
        let mut tool = None;
        let mut op = 0u16;
        for (idx, m) in moves.iter().enumerate() {
            let (to, rapid) = match *m {
                Move::ToolChange { tool: t, .. } => {
                    tool = Some(t);
                    continue;
                }
                Move::Operation { id, .. } => {
                    op = (id + 1).min(u16::MAX as u32) as u16;
                    continue;
                }
                Move::Rapid3 { x, y, z } => ([x.or(at[0]), y.or(at[1]), z.or(at[2])], true),
                Move::Line3 { x, y, z, .. } => ([Some(x), Some(y), Some(z)], false),
                _ => continue,
            };
            if let [Some(x), Some(y), Some(z)] = to {
                let from = match at {
                    [Some(a), Some(b), Some(c)] => Some([a, b, c]),
                    _ => None,
                };
                out.push(Segment {
                    from,
                    to: [x, y, z],
                    rapid,
                    tool,
                    op,
                    program: p,
                    move_index: idx,
                    index_a: Some(0.0),
                });
            }
            at = to;
        }
    }
    out
}

/// Remove material along one segment, sampling at most `step` mm apart.
pub fn apply_segment3(
    hm: &mut Heightmap3,
    seg: &Segment,
    cutter: Option<&Cutter>,
    step: f64,
) -> SegmentEffect {
    let mut e = SegmentEffect::default();
    let (Some(c), Some(from)) = (cutter, seg.from) else {
        return e;
    };
    // only where the tip is below the block's top; far under the block it cuts what it
    // would a millimetre under it
    let low = hm.bottom - 1.0 - c.height(c.radius());
    let pieces = crate::sim::clip_height(from, seg.to, 2, low, hm.top)
        .unwrap_or_else(|| vec![(from, seg.to)]);
    for (a, b) in pieces {
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        let n = ((len / step).ceil() as usize).max(1);
        for k in 0..=n {
            let t = k as f64 / n as f64;
            let (cut, rows) = hm.stamp(
                c,
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
                seg.op,
            );
            e.samples += 1;
            if let Some((lo, hi)) = rows {
                e.rows = Some(e.rows.map_or((lo, hi), |(p, q)| (p.min(lo), q.max(hi))));
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

/// About how many cell visits simulating `segs` on a block heightmap of `cell` mm cells,
/// from `bottom` to `top`, takes (see [`crate::sim::work_estimate`]; no heightmap is made).
pub fn work_estimate3(
    cell: f64,
    bottom: f64,
    top: f64,
    segs: &[Segment],
    cutter_for: &dyn Fn(u32) -> Option<Cutter>,
    step: f64,
) -> f64 {
    let mut total = 0.0;
    for seg in segs {
        let (Some(c), Some(from)) = (seg.tool.and_then(cutter_for), seg.from) else {
            continue;
        };
        let rad = c.radius();
        let side = (2.0 * rad / cell).floor() + 1.0;
        let low = bottom - 1.0 - c.height(rad);
        let pieces = crate::sim::clip_height(from, seg.to, 2, low, top)
            .unwrap_or_else(|| vec![(from, seg.to)]);
        for (a, b) in pieces {
            let len =
                ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
            // (and a stamp's own cost, whatever its size: about 64 cells' worth)
            total += ((len / step).ceil().max(1.0) + 1.0) * (side * side + 64.0);
        }
    }
    total
}

/// Remove material along a 3-axis toolpath (see [`crate::sim::simulate`]).
pub fn simulate3(
    hm: &mut Heightmap3,
    tp: &Toolpath,
    cutter_for: &dyn Fn(u32) -> Option<Cutter>,
    step: f64,
) -> SimStats {
    let mut st = SimStats::default();
    for seg in segments3(std::slice::from_ref(tp)) {
        let c = seg.tool.and_then(cutter_for);
        let e = apply_segment3(hm, &seg, c.as_ref(), step);
        st.samples += e.samples;
        st.cells_cut += e.cells_cut;
        if e.rapid_hit {
            st.rapid_hits.push(seg.move_index);
        }
    }
    st
}

/// Top of the part over each cell centre (None where there is no part), for comparing the
/// cut block against it.
pub fn part_top(hm: &Heightmap3, mesh: &Mesh) -> Vec<Option<f64>> {
    let dc = DropCutter3::new(mesh, Cutter::Flat { radius: 1e-4 });
    let mut out = Vec::with_capacity(hm.nx * hm.ny);
    for j in 0..hm.ny {
        for i in 0..hm.nx {
            let (x, y) = hm.centre(i, j);
            let z = dc.z_at(x, y, f64::NEG_INFINITY);
            out.push(z.is_finite().then_some(z));
        }
    }
    out
}

/// Material left above the part and gouges below it, over cells that have part; `tol` is
/// the gouge threshold (mm). `gouge_at` is (x, y).
pub fn deviation3(hm: &Heightmap3, part: &[Option<f64>], tol: f64) -> Deviation {
    deviation3_with(hm, part, tol, &|_| false)
}

/// [`deviation3`], leaving out cells cut below the part on purpose (see
/// [`crate::sim::deviation_with`]).
pub fn deviation3_with(
    hm: &Heightmap3,
    part: &[Option<f64>],
    tol: f64,
    intended: &dyn Fn(u16) -> bool,
) -> Deviation {
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
    for (k, p) in part.iter().enumerate() {
        let Some(p) = *p else { continue };
        let diff = hm.z[k] - p;
        if diff < -tol && intended(hm.op[k]) {
            d.feature_cells += 1;
            continue;
        }
        d.cells += 1;
        if diff >= 0.0 {
            d.max_left = d.max_left.max(diff);
            sum += diff;
        } else if -diff > d.max_gouge {
            d.max_gouge = -diff;
            d.gouge_at = Some(hm.centre(k % hm.nx, k / hm.nx));
        }
        if diff < -tol {
            d.gouge_cells += 1;
        }
    }
    if d.cells > 0 {
        d.mean_left = sum / d.cells as f64;
    }
    d
}

/// Step through a 3-axis job's material removal, forwards or backwards (see
/// [`crate::sim::Playback`]). Rows are Y indices.
pub struct Playback3 {
    pub segments: Vec<Segment>,
    cutters: std::collections::HashMap<u32, Cutter>,
    step: f64,
    pub stock: Heightmap3,
    initial: Heightmap3,
    pos: usize,
    every: usize,
    checkpoints: Vec<(usize, Heightmap3)>,
}

impl Playback3 {
    pub fn new(
        initial: Heightmap3,
        tps: &[Toolpath],
        cutter_for: &dyn Fn(u32) -> Option<Cutter>,
        step: f64,
    ) -> Playback3 {
        let segments = segments3(tps);
        let cutters = segments
            .iter()
            .filter_map(|s| s.tool)
            .filter_map(|t| cutter_for(t).map(|c| (t, c)))
            .collect();
        let every = crate::sim::checkpoint_spacing(segments.len(), initial.z.len() * 10);
        Playback3 {
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

    /// Show the block after the first `n` segments. Returns the rows that may have changed.
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
            dirty = Some((0, self.stock.ny - 1));
        }
        while self.pos < n {
            let seg = self.segments[self.pos];
            let c = seg.tool.and_then(|t| self.cutters.get(&t));
            let e = apply_segment3(&mut self.stock, &seg, c, self.step);
            if let Some((lo, hi)) = e.rows {
                dirty = Some(dirty.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
            }
            self.pos += 1;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolpath::Builder;

    fn slot() -> Toolpath {
        let mut b = Builder::new();
        b.tool_change(1, 10000);
        b.operation(0, "pocket");
        b.rapid3(None, None, Some(5.0));
        b.rapid3(Some(2.0), Some(5.0), None);
        b.line3(2.0, 5.0, -1.0, [0.0, 0.0, 1.0], 60.0);
        b.line3(8.0, 5.0, -1.0, [0.0, 0.0, 1.0], 300.0);
        b.rapid3(None, None, Some(5.0));
        b.operation(1, "drill");
        b.rapid3(Some(5.0), Some(8.0), None);
        b.line3(5.0, 8.0, -2.0, [0.0, 0.0, 1.0], 60.0);
        b.rapid3(None, None, Some(5.0));
        Toolpath {
            name: "t".into(),
            title: "t".into(),
            tool: None,
            notes: vec![],
            reset_a: false,
            safe_r: 5.0,
            moves: b.moves,
            origin_comment: None,
            plunge_limit: None,
            ramp_angle: None,
            post: None,
        }
    }

    fn block() -> Heightmap3 {
        Heightmap3::new(
            &StockBox {
                min: [0.0, 0.0, -5.0],
                max: [10.0, 10.0, 0.0],
            },
            0.25,
        )
    }

    #[test]
    fn cuts_are_tagged_by_operation_and_playback_matches_the_batch() {
        let cutter = |_| Some(Cutter::Flat { radius: 1.0 });
        let tp = slot();
        let mut batch = block();
        let st = simulate3(&mut batch, &tp, &cutter, 0.1);
        assert!(st.rapid_hits.is_empty());
        assert!((batch.at(5.0, 5.0) + 1.0).abs() < 1e-9);
        assert!((batch.at(5.0, 8.0) + 2.0).abs() < 1e-9);
        assert_eq!(batch.at(5.0, 2.0), 0.0);
        let k = |x: f64, y: f64| ((y / 0.25) as usize) * batch.nx + (x / 0.25) as usize;
        assert_eq!((batch.op[k(3.0, 5.0)], batch.op[k(5.0, 8.4)]), (1, 2));
        assert_eq!(batch.op[k(5.0, 2.0)], 0);

        let mut pb = Playback3::new(block(), std::slice::from_ref(&tp), &cutter, 0.1);
        let rows = pb.seek(pb.len()).unwrap();
        assert_eq!(pb.stock.z, batch.z);
        assert_eq!(pb.stock.op, batch.op);
        assert_eq!(
            rows,
            (16, 35),
            "Y rows of the slot (Y4..6) to the hole (Y7..9)"
        );
        // back to the start, then halfway: the drill has not happened yet
        pb.seek(0);
        assert!(pb.stock.z.iter().all(|&z| z == 0.0));
        let half = pb.segments.iter().position(|s| s.op == 2).unwrap();
        pb.seek(half);
        assert_eq!(pb.stock.at(5.0, 8.4), 0.0);
        assert!((pb.stock.at(5.0, 5.0) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn deviation_reports_material_left_and_gouges() {
        let mut hm = block();
        let part: Vec<Option<f64>> = (0..hm.z.len()).map(|k| (k < 10).then_some(-1.0)).collect();
        hm.z[3] = -1.5;
        let d = deviation3(&hm, &part, 0.02);
        assert_eq!((d.cells, d.gouge_cells), (10, 1));
        assert!((d.max_left - 1.0).abs() < 1e-9 && (d.max_gouge - 0.5).abs() < 1e-9);
    }
}

/// Whether (`u`, `v`) is inside the closed `outline` (even-odd).
fn inside(outline: &[[f64; 2]], u: f64, v: f64) -> bool {
    let mut c = false;
    for k in 0..outline.len() {
        let [ua, va] = outline[k];
        let [ub, vb] = outline[(k + 1) % outline.len()];
        if (va > v) != (vb > v) && u < ua + (v - va) * (ub - ua) / (vb - va) {
            c = !c;
        }
    }
    c
}
