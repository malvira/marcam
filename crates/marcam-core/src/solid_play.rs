//! A playback of a rotary job on the stock in 3D (Mar: "the simulation should be more
//! realistic: there is no way these weird rectangular spiky slices would happen"): a
//! tri-dexel of the bar cut move by move, its surface in tiles, and after each step only the
//! tiles the tool went through made again. The radial map's playback kept one radius a ray
//! from the axis: under the impeller's leaning blades it filled the hollows, and drawn cell
//! by cell it stepped along their edges.

use crate::dexel::{self, Dexel, SurfaceMesh, ToolSolid};
use crate::job::Job;
use crate::sim::Segment;
use crate::Toolpath;
use std::collections::BTreeSet;

/// Grid points a tile is along each axis.
pub const TILE: usize = 24;

/// Snapshots of the stock kept along a job (a backwards scrub plays on from the nearest).
pub const SNAPSHOTS: usize = 8;

pub struct SolidPlayback {
    radius: f64,
    length: f64,
    h: f64,
    dx: Dexel,
    pub segments: Vec<Segment>,
    tools: Vec<(u32, ToolSolid)>,
    pos: usize,
    /// Tiles cut since they were last made.
    dirty: BTreeSet<[usize; 3]>,
    /// The stock at some segments, played to: (segment, stock).
    snapshots: Vec<(usize, Dexel)>,
}

/// One tile's surface: its place in the grid of tiles, and its triangles.
pub struct Tile {
    pub key: [usize; 3],
    pub mesh: SurfaceMesh,
}

impl SolidPlayback {
    /// The job's bar, rays `h` apart, and its programs' moves to play (`solid`: the tool's
    /// solid of a tool number, as the 3D checks use).
    pub fn new(
        job: &Job,
        tps: &[Toolpath],
        h: f64,
        solid: &dyn Fn(u32) -> Option<ToolSolid>,
    ) -> SolidPlayback {
        let segments = crate::sim::segments(tps);
        let mut tools: Vec<(u32, ToolSolid)> = Vec::new();
        for s in &segments {
            if let Some(t) = s.tool {
                if !tools.iter().any(|(n, _)| *n == t) {
                    if let Some(ts) = solid(t) {
                        tools.push((t, ts));
                    }
                }
            }
        }
        let mut dx = Dexel::bar(job.stock.radius, job.stock.length, h);
        dx.track_ops();
        let mut p = SolidPlayback {
            radius: job.stock.radius,
            length: job.stock.length,
            h,
            dx,
            segments,
            tools,
            pos: 0,
            dirty: BTreeSet::new(),
            snapshots: Vec::new(),
        };
        p.all_dirty();
        p
    }

    pub fn h(&self) -> f64 {
        self.h
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// The grid of tiles: how many along each axis.
    pub fn tiles(&self) -> [usize; 3] {
        self.dx.n.map(|n| n.div_ceil(TILE))
    }

    fn all_dirty(&mut self) {
        let t = self.tiles();
        for i in 0..t[0] {
            for j in 0..t[1] {
                for k in 0..t[2] {
                    self.dirty.insert([i, j, k]);
                }
            }
        }
    }

    /// Played to `n` segments; the tiles cut on the way are marked to be made again. Back:
    /// from the nearest snapshot before `n` (one is kept each [`SNAPSHOTS`]th of the job as
    /// it is played forward), else from the bar.
    pub fn seek(&mut self, n: usize) {
        let n = n.min(self.segments.len());
        if n < self.pos {
            match self.snapshots.iter().rev().find(|(p, _)| *p <= n) {
                Some((p, dx)) => {
                    self.dx = dx.clone();
                    self.pos = *p;
                }
                None => {
                    self.dx = Dexel::bar(self.radius, self.length, self.h);
                    self.dx.track_ops();
                    self.pos = 0;
                }
            }
            self.all_dirty();
        }
        let every = self.segments.len().div_ceil(SNAPSHOTS).max(1);
        while self.pos < n {
            let next = (self.pos / every + 1) * every;
            let stop = n.min(next);
            self.play(self.pos, stop);
            self.pos = stop;
            if stop == next
                && stop < self.segments.len()
                && !self.snapshots.iter().any(|(p, _)| *p == stop)
            {
                self.snapshots.push((stop, self.dx.clone()));
                self.snapshots.sort_by_key(|(p, _)| *p);
            }
        }
    }

    /// Segments `from..to` taken out of the stock, the tiles they went through marked.
    fn play(&mut self, from: usize, to: usize) {
        let tools = &self.tools;
        let tool = |t: u32| tools.iter().find(|(n, _)| *n == t).map(|(_, s)| *s);
        let segs = &self.segments[from..to];
        dexel::replay(&mut self.dx, segs, &tool, self.h.max(0.2), 0.0);
        // where the tool went: the tiles its sweeps touch
        let (lo, h, ng) = (self.dx.lo, self.h, self.dx.n);
        for s in segs.iter().filter(|s| !s.rapid) {
            let Some(t) = s.tool.and_then(tool) else {
                continue;
            };
            let Some((a, b)) = dexel::sweep_box(s, &t, h) else {
                continue;
            };
            let g = |c: f64, k: usize| (((c - lo[k]) / h).floor().max(0.0) as usize).min(ng[k] - 1);
            let (t0, t1) = (
                [0, 1, 2].map(|k| g(a[k], k).saturating_sub(1) / TILE),
                [0, 1, 2].map(|k| (g(b[k], k) + 1).min(ng[k] - 1) / TILE),
            );
            for i in t0[0]..=t1[0] {
                for j in t0[1]..=t1[1] {
                    for k in t0[2]..=t1[2] {
                        self.dirty.insert([i, j, k]);
                    }
                }
            }
        }
    }

    /// The tiles cut since they were last made, made again (and no longer dirty); empty ones
    /// too (their old triangles go).
    pub fn take_dirty(&mut self) -> Vec<Tile> {
        let keys: Vec<[usize; 3]> = std::mem::take(&mut self.dirty).into_iter().collect();
        keys.into_iter()
            .map(|key| {
                let lo = key.map(|k| k * TILE);
                let hi = key.map(|k| (k + 1) * TILE);
                Tile {
                    key,
                    mesh: self.dx.surface_mesh_region(lo, hi),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolpath::Builder;

    /// Played in steps or in one go, back and forward again: the same surface, tile by tile.
    #[test]
    fn played_in_steps_the_same_as_at_once() {
        let job = Job::from_json(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/knob/knob.job.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut b = Builder::new();
        b.moves.push(crate::Move::Operation {
            id: 0,
            kind: "groove".into(),
            arcs: None,
            cycle: None,
            verbatim: None,
        });
        b.tool_change(1, 12000);
        b.rapid_r(15.0);
        b.rapid(Some(-30.0), None, Some(0.0));
        b.plunge(10.0, 60.0);
        for i in 1..=40 {
            b.cut(-30.0 + i as f64 * 0.5, 10.0, i as f64 * 9.0, 300.0, 60.0);
        }
        b.rapid_r(15.0);
        let tp = Toolpath {
            name: "p".into(),
            title: "p".into(),
            tool: Some((1, "flat".into())),
            notes: Vec::new(),
            reset_a: true,
            safe_r: 15.0,
            moves: b.moves,
            origin_comment: None,
            plunge_limit: None,
            ramp_angle: None,
            post: None,
        };
        let solid = |_: u32| {
            Some(ToolSolid {
                radius: 1.5875,
                ball: false,
                height: 12.0,
            })
        };
        let tris = |p: &mut SolidPlayback| -> usize {
            let t = p.tiles();
            let mut n = 0;
            for i in 0..t[0] {
                for j in 0..t[1] {
                    for k in 0..t[2] {
                        n +=
                            p.dx.surface_mesh_region(
                                [i * TILE, j * TILE, k * TILE],
                                [(i + 1) * TILE, (j + 1) * TILE, (k + 1) * TILE],
                            )
                            .indices
                            .len();
                    }
                }
            }
            n
        };
        let mut once = SolidPlayback::new(&job, std::slice::from_ref(&tp), 0.3, &solid);
        let end = once.segments.len();
        once.seek(end);
        let whole = tris(&mut once);
        // (each vertex the operation that last cut there: the groove's by the one operation,
        // the bar's own outside by none)
        let made = once.take_dirty();
        let ops: Vec<u16> = made
            .iter()
            .flat_map(|t| t.mesh.ops.iter().copied())
            .collect();
        assert!(
            ops.iter().any(|&o| o > 0) && ops.contains(&0),
            "{} vertices",
            ops.len()
        );
        let mut steps = SolidPlayback::new(&job, std::slice::from_ref(&tp), 0.3, &solid);
        assert!(!steps.take_dirty().is_empty(), "at first every tile");
        for k in 1..=end {
            steps.seek(k);
            let d = steps.take_dirty();
            assert!(d.len() < 60, "step {k}: {} tiles made again", d.len());
        }
        assert_eq!(tris(&mut steps), whole);
        // back to the start and on again; back half way (from a snapshot) and on again
        steps.seek(1);
        steps.seek(end);
        assert_eq!(tris(&mut steps), whole);
        assert!(!steps.snapshots.is_empty());
        steps.seek(end / 2);
        steps.seek(end);
        assert_eq!(tris(&mut steps), whole);
    }
}
