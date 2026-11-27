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

/// Memory the snapshots may hold unless told (bytes): the page says its own, by the device's.
pub const SNAPSHOT_CAP: usize = 256 << 20;

/// At least this many snapshots (as the cap allows), however short the job.
const SNAPSHOTS: usize = 8;

/// At most this many snapshots, however small the stock.
const MAX_SNAPSHOTS: usize = 64;

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
    /// The stock at some segments, played to: (segment, stock, its bytes).
    snapshots: Vec<(usize, Dexel, usize)>,
    /// The replay's cost (about) before each segment, summed: `cost[k]` for segments `..k`.
    cost: Vec<f64>,
    /// The segments a snapshot is wanted at: the job's cost in equal parts.
    targets: Vec<usize>,
    /// What the snapshots (and the stock building them) may hold, bytes.
    cap: usize,
    /// The replay's cost a snapshot saves at least (0: no least): a short job's playback has
    /// none, a long jump in it quick enough from the bar.
    min_spacing: f64,
    /// The stock building snapshots ahead in the background ([`SolidPlayback::warm`]): its
    /// place and itself.
    builder: Option<(usize, Dexel)>,
}

/// One tile's surface: its place in the grid of tiles, and its triangles.
pub struct Tile {
    pub key: [usize; 3],
    pub mesh: SurfaceMesh,
}

/// Segments taken out of a stock, as the playback does (the rays' spacing between poses).
fn replay_into(dx: &mut Dexel, segs: &[Segment], tools: &[(u32, ToolSolid)], h: f64) {
    let tool = |t: u32| tools.iter().find(|(n, _)| *n == t).map(|(_, s)| *s);
    dexel::replay(dx, segs, &tool, h.max(0.2), 0.0);
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
        // (each segment's cost: its poses, each as many rays as the tool's footprint crosses)
        let step = h.max(0.2);
        let mut cost = Vec::with_capacity(segments.len() + 1);
        let mut sum = 0.0;
        cost.push(0.0);
        for s in &segments {
            let t = s.tool.and_then(|n| tools.iter().find(|(m, _)| *m == n));
            sum += match (s.from, t) {
                (Some(from), Some((_, t))) => {
                    let poses = (dexel::seg_len(s, from) / step).ceil().max(1.0);
                    poses * (1.0 + (2.0 * t.radius / h).powi(2))
                }
                _ => 1.0,
            };
            cost.push(sum);
        }
        let mut dx = Dexel::bar(job.stock.radius, job.stock.length, h);
        dx.track_ops();
        dx.track_normals();
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
            cost,
            targets: Vec::new(),
            cap: SNAPSHOT_CAP,
            min_spacing: 0.0,
            builder: None,
        };
        p.plan_targets();
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

    fn bar(&self) -> Dexel {
        let mut dx = Dexel::bar(self.radius, self.length, self.h);
        dx.track_ops();
        dx.track_normals();
        dx
    }

    /// The memory the snapshots may hold (bytes); the snapshots no longer wanted go.
    pub fn set_cap(&mut self, bytes: usize) {
        self.cap = bytes;
        self.replan();
    }

    /// The least replay cost between snapshots (the page's, as long as a moment of playing
    /// takes it); the snapshots no longer wanted go. Unchanged within a quarter: kept as is.
    pub fn set_min_spacing(&mut self, cost: f64) {
        let cost = cost.max(0.0);
        if (cost - self.min_spacing).abs() <= 0.25 * self.min_spacing.max(1e-9) {
            return;
        }
        self.min_spacing = cost;
        self.replan();
    }

    fn replan(&mut self) {
        let before = self.targets.clone();
        self.plan_targets();
        if self.targets == before {
            return;
        }
        let targets = &self.targets;
        self.snapshots
            .retain(|(p, _, _)| targets.binary_search(p).is_ok());
    }

    /// Where the snapshots go: as many as the cap holds (each about the bar's size; one
    /// more for the stock building them), the job's cost cut in equal parts.
    fn plan_targets(&mut self) {
        let size = self.bar().bytes().max(1);
        let total = self.total_cost();
        let fit = (self.cap / size).saturating_sub(1).clamp(2, MAX_SNAPSHOTS);
        // (no closer than the least spacing, but always as many as were kept before there was
        // one, SNAPSHOTS: those a playback takes as it plays on, cheap; and no more than those
        // until the spacing is known: a job too quick to time wants no more)
        let spaced = if self.min_spacing > 0.0 {
            ((total / self.min_spacing).floor() as usize).saturating_sub(1)
        } else {
            0
        };
        let k = spaced.max(SNAPSHOTS).min(fit);
        let n = self.segments.len();
        let mut t: Vec<usize> = (1..=k)
            .map(|i| {
                let c = total * i as f64 / (k + 1) as f64;
                self.cost.partition_point(|&x| x < c)
            })
            .filter(|&s| s > 0 && s < n)
            .collect();
        t.dedup();
        self.targets = t;
    }

    /// The segments the snapshots are at.
    pub fn snapshot_positions(&self) -> Vec<usize> {
        self.snapshots.iter().map(|s| s.0).collect()
    }

    /// How many snapshots are wanted (as the cap allows).
    pub fn snapshots_wanted(&self) -> usize {
        self.targets.len()
    }

    /// The memory the snapshots (and the stock building them) hold, bytes.
    pub fn snapshot_bytes(&self) -> usize {
        self.snapshots.iter().map(|s| s.2).sum::<usize>()
            + self.builder.as_ref().map_or(0, |(_, d)| d.bytes())
    }

    /// The whole job's replay cost, in [`SolidPlayback::warm`]'s units.
    pub fn total_cost(&self) -> f64 {
        *self.cost.last().unwrap_or(&0.0)
    }

    /// A snapshot at segment `p`, kept within the cap: the ones furthest from the playhead go
    /// first (and are not made again).
    fn keep(&mut self, p: usize, dx: Dexel) {
        if self.snapshots.iter().any(|s| s.0 == p) {
            return;
        }
        let b = dx.bytes();
        let at = self.snapshots.partition_point(|s| s.0 < p);
        self.snapshots.insert(at, (p, dx, b));
        let room = self
            .cap
            .saturating_sub(self.builder.as_ref().map_or(0, |(_, d)| d.bytes()));
        while self.snapshots.len() > 1 && self.snapshots.iter().map(|s| s.2).sum::<usize>() > room {
            let pos = self.pos;
            let far = (0..self.snapshots.len())
                .max_by_key(|&i| self.snapshots[i].0.abs_diff(pos))
                .unwrap();
            let (q, _, _) = self.snapshots.remove(far);
            self.targets.retain(|&t| t != q);
        }
    }

    /// Played to `n` segments; the tiles cut on the way are marked to be made again. From the
    /// nearest snapshot before `n` when that is less to play than from here (back: that or
    /// the bar), a snapshot kept at each place wanted passed on the way.
    pub fn seek(&mut self, n: usize) {
        let n = n.min(self.segments.len());
        let best = self.snapshots.iter().rev().find(|s| s.0 <= n).map(|s| s.0);
        let spacing = self.total_cost() / (self.targets.len() + 1) as f64;
        let jump = if n < self.pos {
            true
        } else {
            // (on from a snapshot ahead when that saves half a snapshot's spacing of playing:
            // the whole stock is made again then)
            best.is_some_and(|b| b > self.pos && self.cost[b] - self.cost[self.pos] > 0.5 * spacing)
        };
        if jump {
            match best.and_then(|b| self.snapshots.iter().find(|s| s.0 == b)) {
                Some((p, dx, _)) => {
                    self.dx = dx.clone();
                    self.pos = *p;
                }
                None => {
                    self.dx = self.bar();
                    self.pos = 0;
                }
            }
            self.all_dirty();
        }
        while self.pos < n {
            let next = self.targets.iter().copied().find(|&t| t > self.pos);
            let stop = next.map_or(n, |t| t.min(n));
            self.play(self.pos, stop);
            self.pos = stop;
            if next == Some(stop) {
                self.keep(stop, self.dx.clone());
            }
        }
    }

    /// The snapshots not made yet, built ahead in the background: about `budget` of the
    /// replay's cost (of [`SolidPlayback::total_cost`]) played on a stock of their own, the
    /// playback's untouched. Returns whether there is more to build.
    pub fn warm(&mut self, budget: f64) -> bool {
        let mut left = budget;
        loop {
            let have = |p: usize| self.snapshots.iter().any(|s| s.0 == p);
            let Some(t) = self.targets.iter().copied().find(|&t| !have(t)) else {
                self.builder = None;
                return false;
            };
            if left <= 0.0 {
                return true;
            }
            // (from the latest snapshot before it, unless the builder is further on already)
            let from = self.snapshots.iter().rev().find(|s| s.0 < t).map(|s| s.0);
            let fresh = match &self.builder {
                Some((bp, _)) => *bp > t || from.is_some_and(|f| f > *bp),
                None => true,
            };
            if fresh {
                let start = match from.and_then(|f| self.snapshots.iter().find(|s| s.0 == f)) {
                    Some((p, dx, _)) => (*p, dx.clone()),
                    None => (0, self.bar()),
                };
                self.builder = Some(start);
            }
            let (bp, mut bdx) = self.builder.take().expect("made above");
            // (as far towards it as the budget goes, one segment at least)
            let limit = self.cost[bp] + left;
            let stop = self
                .cost
                .partition_point(|&c| c <= limit)
                .saturating_sub(1)
                .clamp(bp + 1, t);
            replay_into(&mut bdx, &self.segments[bp..stop], &self.tools, self.h);
            left -= self.cost[stop] - self.cost[bp];
            if stop == t {
                let snap = bdx.clone();
                self.builder = Some((stop, bdx));
                self.keep(t, snap);
            } else {
                self.builder = Some((stop, bdx));
            }
        }
    }

    /// Segments `from..to` taken out of the stock, the tiles they went through marked.
    fn play(&mut self, from: usize, to: usize) {
        let segs = &self.segments[from..to];
        replay_into(&mut self.dx, segs, &self.tools, self.h);
        let tools = &self.tools;
        let tool = |t: u32| tools.iter().find(|(n, _)| *n == t).map(|(_, s)| *s);
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
                cone: 0.0,
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

        // the snapshots built in the background, a little at a time, the playback untouched;
        // then a jump on from one, and back to one: the same stock
        let mut warm = SolidPlayback::new(&job, std::slice::from_ref(&tp), 0.3, &solid);
        warm.take_dirty();
        // (the eight kept before until the page says how far apart: then as many as that wants)
        assert_eq!(warm.snapshots_wanted(), SNAPSHOTS);
        warm.set_min_spacing(warm.total_cost() / 16.5);
        let mut calls = 0;
        while warm.warm(warm.total_cost() / 50.0) {
            calls += 1;
            assert!(calls < 1000);
        }
        assert!(calls > 10, "{calls} calls");
        assert_eq!(warm.position(), 0);
        assert!(
            warm.take_dirty().is_empty(),
            "the playback's stock untouched"
        );
        assert_eq!(warm.snapshot_positions().len(), warm.snapshots_wanted());
        assert!(warm.snapshots_wanted() > 8);
        warm.seek(end);
        assert_eq!(tris(&mut warm), whole);
        warm.seek(end / 3);
        warm.seek(end);
        assert_eq!(tris(&mut warm), whole);

        // held under a cap: three bars' worth, two snapshots and the stock building them
        let mut capped = SolidPlayback::new(&job, std::slice::from_ref(&tp), 0.3, &solid);
        let bar = capped.dx.bytes();
        capped.set_cap(3 * bar + bar / 2);
        while capped.warm(f64::INFINITY) {}
        assert_eq!(capped.snapshots_wanted(), 2);
        assert!(capped.snapshot_bytes() <= 3 * bar + bar / 2);
        capped.seek(end);
        assert_eq!(tris(&mut capped), whole);
        // the least spacing: no more than it leaves room for, never under the 8 kept before
        // there was one (as the cap allows)
        warm.set_min_spacing(warm.total_cost());
        assert_eq!(warm.snapshots_wanted(), SNAPSHOTS);
        // (11 parts, 10 places; two may fall on one of the test's 44 moves)
        warm.set_min_spacing(warm.total_cost() / 11.5);
        assert!((SNAPSHOTS + 1..=10).contains(&warm.snapshots_wanted()));
        warm.seek(end / 2);
        warm.seek(end);
        assert_eq!(tris(&mut warm), whole);
        capped.set_min_spacing(capped.total_cost());
        assert_eq!(capped.snapshots_wanted(), 2);
    }
}
