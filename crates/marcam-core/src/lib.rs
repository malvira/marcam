//! marcam: a 4th-axis (rotary) CAM engine.
//!
//! Pipeline: [`Job`] (JSON) -> [`plan`] -> machine-neutral [`Toolpath`]s -> [`post`] with a
//! [`Machine`] profile -> G-code, which [`verify_gcode`] can sanity-check.

pub mod archive;
pub mod body;
pub mod budget;
pub mod carry;
pub mod carry_mesh;
pub mod collision;
pub mod det;
pub mod dexel;
pub mod dropcutter;
pub mod engrave;
pub mod estimate;
pub mod feeds;
pub mod gcode_in;
pub mod geom;
pub mod geometry;
pub mod gpu_input;
pub mod job;
pub mod kinematics;
pub mod left3d;
pub mod machine;
pub mod mesh;
pub mod numeric;
pub mod offset;
pub mod ops;
pub mod pickables;
pub mod post;
pub mod profiles;
pub mod project;
pub mod project_file;
pub mod relief;
pub mod render;
pub mod replace;
pub mod sim;
pub mod sim3;
pub mod smoothing;
pub mod solid_play;
pub mod strategy;
pub mod toolpath;
pub mod verify;
pub mod wizards;

use crate::det::Det;
pub use job::Job;
pub use machine::{FeedMode, Machine};
pub use mesh::Mesh;
pub use toolpath::{Move, Toolpath};
pub use verify::{Issue, IssueKind, Limits};

use dropcutter::{Cutter, DropCutter};
use geom::RevolvedProfile;
use job::{Operation, Program, ToolShape};
use std::fmt;
use toolpath::Builder;

/// Errors found while planning a job.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    UnsupportedSchema(String),
    UnknownTool {
        program: String,
        tool: u32,
    },
    WrongToolShape {
        program: String,
        tool: u32,
        operation: &'static str,
        needs: &'static str,
    },
    FluteTooShort {
        tool: u32,
        depth: f64,
        flute_length: f64,
    },
    /// Anything else wrong with the job.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnsupportedSchema(v) => write!(
                f,
                "unsupported schema_version {v:?} (expected {:?})",
                job::SCHEMA_VERSION
            ),
            Error::UnknownTool { program, tool } => {
                write!(f, "program {program}: tool T{tool} is not defined")
            }
            Error::WrongToolShape {
                program,
                tool,
                operation,
                needs,
            } => write!(
                f,
                "program {program}: {operation} needs a {needs} tool, T{tool} is not one"
            ),
            Error::FluteTooShort {
                tool,
                depth,
                flute_length,
            } => write!(
                f,
                "T{tool}: radial depth {depth:.3} mm exceeds usable flute length ({flute_length} mm - 1 mm)"
            ),
            Error::Invalid(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

fn op_name(op: &Operation) -> &'static str {
    match op {
        Operation::RotaryTurning(_) => "rotary_turning",
        Operation::HelicalKnurl(_) => "helical_knurl",
        Operation::FlankChamfer(_) => "flank_chamfer",
        Operation::RotaryFinish(_) => "rotary_finish",
        Operation::RotaryParallel(_) => "rotary_parallel",
        Operation::RotaryFlowline(_) => "rotary_flowline",
        Operation::RotaryWall(_) => "rotary_wall",
        Operation::RotaryPocket(_) => "rotary_pocket",
        Operation::RotaryRough(_) => "rotary_rough",
        Operation::Face(_) => "face",
        Operation::Pocket(_) => "pocket",
        Operation::Drill(_) => "drill",
        Operation::Bore(_) => "bore",
        Operation::Chamfer(_) => "chamfer",
        Operation::ThreadMill(_) => "thread_mill",
        Operation::Contour(_) => "contour",
        Operation::Raster3d(_) => "raster3d",
        Operation::Waterline(_) => "waterline",
        Operation::Rough3d(_) => "rough3d",
        Operation::Engrave(_) => "engrave",
        Operation::Vcarve(_) => "vcarve",
        Operation::Parting(_) => "parting",
        Operation::Custom(_) => "custom",
    }
}

/// Facets around the axis when a profile is revolved into a finishing mesh.
pub const PROFILE_SEGMENTS: usize = 720;

/// Planning context shared by the programs of a job.
/// The part's largest radius (about X) over `a <= x <= b`: each triangle cut to that slab,
/// the farthest corner of what is left (0 where the part has nothing there).
fn part_radius_over(m: &Mesh, a: f64, b: f64) -> f64 {
    if b < a {
        return 0.0;
    }
    let rad = |p: [f64; 3]| p[1].hypot(p[2]);
    let mut best = 0.0f64;
    for t in &m.triangles {
        let (lo, hi) = t
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), p| {
                (l.min(p[0]), h.max(p[0]))
            });
        if hi < a || lo > b {
            continue;
        }
        for i in 0..3 {
            let (p, q) = (t[i], t[(i + 1) % 3]);
            if (a..=b).contains(&p[0]) {
                best = best.max(rad(p));
            }
            // where the edge crosses the slab's two faces
            for x in [a, b] {
                if (p[0] - x) * (q[0] - x) < 0.0 {
                    let u = (x - p[0]) / (q[0] - p[0]);
                    best = best.max(rad([x, p[1] + u * (q[1] - p[1]), p[2] + u * (q[2] - p[2])]));
                }
            }
        }
    }
    best
}

struct Ctx<'a> {
    job: &'a Job,
    profile: Option<RevolvedProfile>,
    mesh: Option<&'a Mesh>,
    revolved: Option<Mesh>,
    /// Operations planned before, reused when nothing they depend on changed.
    cache: Option<&'a mut PlanCache>,
    /// What every operation of this job depends on besides itself (stock, part, ...): part of
    /// each cache key.
    job_key: u64,
    /// The moves of the programs planned so far, kept when a rest roughing needs them.
    prior: Vec<Vec<Move>>,
}

/// A hasher fed by a writer (a value serialised straight into it, without the text).
struct HashWriter(std::collections::hash_map::DefaultHasher);

impl std::io::Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::hash::Hasher;
        self.0.write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the moves before a rest roughing are, for its cache key.
fn moves_key(prior: &[Vec<Move>], before: &[Move]) -> u64 {
    use std::hash::Hasher;
    let mut w = HashWriter(Default::default());
    for m in prior.iter().flatten().chain(before) {
        let _ = serde_json::to_writer(&mut w, m);
    }
    w.0.finish()
}

/// Whether an operation is planned on the stock the moves before it left (so its plan
/// depends on them).
fn uses_prior(op: &Operation) -> bool {
    match op {
        Operation::Rough3d(r) => r.rest || !r.indexes.is_empty() || r.index_count > 0,
        Operation::RotaryRough(r) => r.rest,
        // (its approach from the bar as the moves before left it)
        Operation::HelicalKnurl(_) => true,
        _ => false,
    }
}

/// Where the moves before cut (not how fast), for an in-process operation's cache key: a
/// feed changed before it does not plan it again.
fn cuts_key(prior: &[Vec<Move>], before: &[Move]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in prior.iter().flatten().chain(before) {
        match m {
            Move::Line3 { x, y, z, .. } => {
                (0u8, x.to_bits(), y.to_bits(), z.to_bits()).hash(&mut h)
            }
            Move::Rapid3 { x, y, z } => (
                1u8,
                x.map(f64::to_bits),
                y.map(f64::to_bits),
                z.map(f64::to_bits),
            )
                .hash(&mut h),
            Move::ToolChange { tool, .. } => (2u8, *tool).hash(&mut h),
            _ => {}
        }
    }
    h.finish()
}

/// The block left by the moves before a rest roughing (the programs before it and its own
/// program so far), widened by its tool: see [`strategy::three_axis::RestMap`]. Its work
/// counts against the operation's budget.
fn rest_map(
    job: &Job,
    prior: &[Vec<Move>],
    before: &[Move],
    sb: &job::StockBox,
    c: &Cutter,
) -> Result<strategy::three_axis::RestMap, String> {
    let hm = rest_heightmap(job, prior, before, sb, c)?;
    // (a block carried over: its other side's cuts too)
    let bottoms = job.carried.as_ref().map(|k| k.bottoms(&hm));
    Ok(strategy::three_axis::RestMap::with_bottoms(
        &hm,
        c.radius(),
        bottoms.as_deref(),
    ))
}

/// What the moves before (the earlier programs', and this one's so far) leave of the stock.
fn rest_heightmap(
    job: &Job,
    prior: &[Vec<Move>],
    before: &[Move],
    sb: &job::StockBox,
    c: &Cutter,
) -> Result<sim3::Heightmap3, String> {
    // cells an eighth of the tool's radius (between 0.02 and 0.25 mm)
    let cell = (c.radius() / 8.0).clamp(0.02, 0.25);
    cut_heightmap(job, prior, before, sb, cell, c.radius(), "rest roughing")
}

/// (Mar) Operation `op` with the stock it leaves taken off what it cuts: its floors (a
/// shallower depth; a drill's bottom; a chamfer raised), and a bore's or a thread's wall (a
/// smaller hole, a shallower thread). Also the walls' stock for a pocket or contour (planned
/// with [`strategy::three_axis::LeaveGuard`]). None: as it is.
fn leave_of(op: &Operation, sb: &job::StockBox) -> Result<(Option<Operation>, f64), String> {
    let less = |depth: f64, z: f64| {
        if z != 0.0 && depth - z <= 0.0 {
            Err(format!(
                "stock to leave on the floor ({z} mm) is as deep as the cut ({depth} mm): nothing left to cut"
            ))
        } else {
            Ok(depth - z)
        }
    };
    let mut o = op.clone();
    let walls = match &mut o {
        Operation::Face(f) => {
            if f.leave_z == 0.0 && f.leave_xy == 0.0 {
                return Ok((None, 0.0));
            }
            f.depth = less(f.depth, f.leave_z)?;
            for r in &mut f.regions {
                r.depth = less(r.depth, f.leave_z)?;
            }
            f.boundary_offset -= f.leave_xy;
            0.0
        }
        Operation::Pocket(p) => {
            if p.leave_z == 0.0 && p.leave_xy == 0.0 {
                return Ok((None, 0.0));
            }
            if p.adaptive && p.leave_xy != 0.0 {
                return Err("stock to leave on the walls of an adaptive pocket: not yet (use a contour finish, or leave 0)".into());
            }
            p.depth = less(p.depth, p.leave_z)?;
            for r in &mut p.regions {
                r.depth = less(r.depth, p.leave_z)?;
            }
            p.leave_xy
        }
        Operation::Contour(c) => {
            if c.leave_z == 0.0 && c.leave_xy == 0.0 {
                return Ok((None, 0.0));
            }
            c.depth = less(c.depth, c.leave_z)?;
            for r in &mut c.regions {
                r.depth = less(r.depth, c.leave_z)?;
            }
            c.leave_xy
        }
        Operation::Drill(d) => {
            if d.leave_z == 0.0 {
                return Ok((None, 0.0));
            }
            d.depth = less(d.depth, d.leave_z)?;
            0.0
        }
        Operation::Bore(b) => {
            if b.leave_z == 0.0 && b.leave_xy == 0.0 {
                return Ok((None, 0.0));
            }
            b.depth = less(b.depth, b.leave_z)?;
            b.diameter -= 2.0 * b.leave_xy;
            if b.diameter <= 0.0 {
                return Err(format!(
                    "stock to leave on the wall ({} mm) closes the hole",
                    b.leave_xy
                ));
            }
            0.0
        }
        Operation::Chamfer(c) => {
            if c.leave_z == 0.0 {
                return Ok((None, 0.0));
            }
            c.top = Some(c.top.unwrap_or(sb.max[2]) + c.leave_z);
            0.0
        }
        Operation::ThreadMill(t) => {
            if t.leave_xy == 0.0 {
                return Ok((None, 0.0));
            }
            let full = t
                .thread_depth
                .unwrap_or(if t.internal { 0.5413 } else { 0.6134 } * t.pitch);
            if full - t.leave_xy <= 0.0 {
                return Err(format!(
                    "stock to leave on the thread ({} mm) is as deep as the thread",
                    t.leave_xy
                ));
            }
            t.thread_depth = Some(full - t.leave_xy);
            0.0
        }
        _ => return Ok((None, 0.0)),
    };
    Ok((Some(o), walls))
}

/// `c` wider by `d` all round (an axial stock to leave on a bar), or why not.
fn grown_for(c: Cutter, d: f64, prog: &str) -> Result<Cutter, Error> {
    c.grown(d).ok_or_else(|| {
        Error::Invalid(format!(
            "program {prog}: stock to leave along X ({d} mm) takes a flat or ball tool, no thinner than it"
        ))
    })
}

/// Whether an in-process stock is planned from (the operation's `in_process`; 2D vise
/// operations).
fn in_process(op: &Operation) -> bool {
    match op {
        Operation::Face(o) => o.in_process,
        Operation::Pocket(o) => o.in_process,
        Operation::Contour(o) => o.in_process,
        Operation::Drill(o) => o.in_process,
        Operation::Bore(o) => o.in_process,
        _ => false,
    }
}

/// Whether the moves before cut anything (a feed move).
fn has_cuts(prior: &[Vec<Move>], before: &[Move]) -> bool {
    prior.iter().flatten().chain(before).any(|m| {
        matches!(
            m,
            Move::Line3 { .. } | Move::Cut { .. } | Move::Plunge { .. }
        )
    })
}

/// The highest a move of `moves` cuts at (its feed moves' ends).
fn highest_cut(moves: &[Move]) -> f64 {
    moves
        .iter()
        .filter_map(|m| match m {
            Move::Line3 { z, .. } => Some(*z),
            _ => None,
        })
        .fold(f64::NEG_INFINITY, f64::max)
}

/// (Mar) The highest material the moves before (`prior`, `before`) left in the way of `ops`
/// (an operation planned from the stock's top): within its tool's radius `r` of its feed
/// moves. None: none in its way.
fn material_in_way(
    job: &Job,
    prior: &[Vec<Move>],
    before: &[Move],
    ops: &[Move],
    sb: &job::StockBox,
    r: f64,
) -> Result<Option<f64>, String> {
    let cell = (r / 4.0).clamp(0.05, 0.25);
    let hm = cut_heightmap(job, prior, before, sb, cell, r, "in-process stock")?;
    let reach = r + cell * 0.75;
    let mut top: Option<f64> = None;
    for seg in sim3::segments_of3(ops, 0) {
        let (Some(a), false) = (seg.from, seg.rapid) else {
            continue;
        };
        let b = seg.to;
        let lo = |v: f64, o: f64| (((v - reach - o) / hm.cell).floor().max(0.0)) as usize;
        let hi = |v: f64, o: f64, n: usize| {
            ((((v + reach - o) / hm.cell).ceil()) as isize).min(n as isize - 1)
        };
        let (i0, j0) = (lo(a[0].min(b[0]), hm.x0), lo(a[1].min(b[1]), hm.y0));
        let (i1, j1) = (
            hi(a[0].max(b[0]), hm.x0, hm.nx),
            hi(a[1].max(b[1]), hm.y0, hm.ny),
        );
        if i1 < i0 as isize || j1 < j0 as isize {
            continue;
        }
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let l2 = dx * dx + dy * dy;
        for j in j0..=j1 as usize {
            for i in i0..=i1 as usize {
                let (x, y) = hm.centre(i, j);
                let t = if l2 > 1e-18 {
                    (((x - a[0]) * dx + (y - a[1]) * dy) / l2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (ex, ey) = (a[0] + t * dx - x, a[1] + t * dy - y);
                if ex * ex + ey * ey <= reach * reach {
                    let z = hm.z[j * hm.nx + i];
                    top = Some(top.map_or(z, |t: f64| t.max(z)));
                }
            }
        }
    }
    Ok(top)
}

/// (Mar's impeller) The bar as the programs before left it (the carried bar, or the job's),
/// for [`indexed_stock`] to go on cutting with this program's moves.
fn indexed_bar(job: &Job, prior: &[Vec<Move>]) -> sim::RadialMap {
    let cutter_of = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let mut map = sim::RadialMap::of_job(carry::bar_grid(&job.stock), job);
    if let Some(carry::Carried::Bar(c)) = job.carried.as_deref() {
        map = c.clone();
    }
    let tps: Vec<Toolpath> = prior.iter().map(|m| bar_toolpath(job, m)).collect();
    for seg in sim::segments(&tps) {
        let cut = seg.tool.and_then(cutter_of);
        sim::apply_segment(&mut map, &seg, cut.as_ref(), indexed_step(cut.as_ref()));
    }
    map
}

/// (Mar: "lets try the coarser sampling") how far apart the in-process stock of indexed
/// roughing is cut along each move: half the tool's radius (not less than 0.2 mm). Coarser
/// only ever leaves more material in the simulation (thin slivers at the edges of a tool's
/// path, under 0.03 of its radius deep), never less: the angles after it plan for at least
/// what is there.
fn indexed_step(c: Option<&Cutter>) -> f64 {
    c.map_or(0.2, |c| (0.5 * c.radius()).max(0.2))
}

/// The bar (or the piece carried in) over X `xa..xb` as the moves before (the programs
/// before, and this one's so far) left it: a radial map of it cut by them.
fn bar_after(job: &Job, prior: &[Vec<Move>], before: &[Move], xa: f64, xb: f64) -> sim::RadialMap {
    let grid = sim::Grid::new(xa - 0.5, xb + 0.5, 0.25, 72);
    let mut st = sim::RadialMap::of_job(grid, job);
    let moves: Vec<Move> = prior.iter().flatten().chain(before).cloned().collect();
    let cutter = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let sg = sim::segments(&[bar_toolpath(job, &moves)]);
    for seg in sg {
        if !seg.rapid {
            let c = seg.tool.and_then(cutter);
            sim::apply_segment(&mut st, &seg, c.as_ref(), 0.1);
        }
    }
    st
}

/// The highest a radial map stands over X `xa..xb`.
fn map_top(m: &sim::RadialMap, xa: f64, xb: f64) -> f64 {
    let mut top = 0.0f64;
    for i in 0..m.grid.nx {
        let x = m.grid.x(i);
        if x < xa || x > xb {
            continue;
        }
        for j in 0..m.grid.na {
            top = top.max(m.r[i * m.grid.na + j] as f64);
        }
    }
    top
}

fn bar_toolpath(job: &Job, moves: &[Move]) -> Toolpath {
    Toolpath {
        name: String::new(),
        title: String::new(),
        tool: None,
        notes: Vec::new(),
        reset_a: false,
        safe_r: job.safe_r(),
        moves: moves.to_vec(),
        origin_comment: None,
        plunge_limit: None,
        ramp_angle: None,
        post: None,
    }
}

/// (Mar's impeller) The bar as the moves before left it, seen from above with the part turned
/// to A `index` (indexed 3+1): each column's top and bottom over the block `sb` at `cell` mm.
/// `map` is the bar with this program's moves up to `applied` cut from it: the rest of
/// `before` is cut now (in order, as a simulation of them all from the start would), and
/// `applied` moved on.
fn indexed_stock(
    job: &Job,
    map: &mut sim::RadialMap,
    applied: &mut usize,
    before: &[Move],
    sb: &job::StockBox,
    cell: f64,
    index: f64,
) -> Result<(sim3::Heightmap3, Vec<f64>), String> {
    let cutter_of = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    for seg in sim::segments(&[bar_toolpath(job, before)]) {
        if seg.move_index < *applied {
            continue;
        }
        let cut = seg.tool.and_then(cutter_of);
        sim::apply_segment(map, &seg, cut.as_ref(), indexed_step(cut.as_ref()));
    }
    *applied = before.len();
    let m = carry_mesh::bar_mesh(map).rotated_x(index);
    let (x0, y0, nx, ny, top, bot) = carry_mesh::sample_block(&m, cell);
    let block = carry::Carried::Block {
        x0,
        y0,
        cell,
        nx,
        ny,
        top,
        bot,
    };
    let hm = block
        .heightmap(sb, cell)
        .ok_or("the bar could not be seen from above")?;
    let bots = block.bottoms(&hm);
    Ok((hm, bots))
}

/// The rays of the 3D in-process stock of indexed roughing, this far apart (mm).
const DEXEL_H: f64 = 0.2;

/// The rays of the finer 3D stock a rapid the coarse one cannot tell from a graze is looked at
/// on, round it only (mm).
const DEXEL_FINE_H: f64 = 0.05;

/// The operations that take more than [`dexel::HEAVY`] times their tool's normal bite, by
/// program: the bar cut by every program in turn on the 3D stock (rays [`DEXEL_H`] apart), as
/// [`confirm_rapids_3d`] does. For a simulation that has no 3D replay of its own (the GPU's).
pub fn heavy_bites(job: &Job, tps: &[Toolpath]) -> Vec<Vec<dexel::Heavy>> {
    let tool = |t: u32| tool_solid(job, t);
    let mut solid = dexel::Dexel::bar(job.stock.radius, job.stock.length, DEXEL_H);
    let moves: usize = tps
        .iter()
        .map(|tp| sim::segments(std::slice::from_ref(tp)).len())
        .sum();
    let began = work_begin(moves);
    let out = tps
        .iter()
        .enumerate()
        .map(|(k, tp)| {
            let sg = sim::segments(std::slice::from_ref(tp));
            dexel::replay(&mut solid, &sg, &tool, 0.25, DEXEL_H)
                .heavy
                .into_iter()
                .map(|h| dexel::Heavy {
                    program: k as u16,
                    ..h
                })
                .collect()
        })
        .collect();
    work_end(began);
    out
}

/// The GPU's simulation of a bar on the rotary axis checked as the CPU's is (it has no 3D
/// replay): the rapids it flagged, `flagged` (its segments of the whole job, in
/// [`sim::segments`]'s order, and 2 through the stock or 1 grazing it), confirmed on the 3D
/// stock, and each program's overloaded operations. Each program's (rapids through, rapids
/// grazing: move indices, as the CPU's `rapid_hits`), and its heavy. A carried piece: as
/// flagged (no 3D stock of it), its heavy none.
pub fn confirm_gpu(
    job: &Job,
    tps: &[Toolpath],
    flagged: &[(usize, u8)],
) -> Vec<(Vec<usize>, Vec<usize>, Vec<dexel::Heavy>)> {
    let segs = sim::segments(tps);
    let mut stats: Vec<sim::SimStats> = vec![sim::SimStats::default(); tps.len()];
    for &(s, level) in flagged {
        let Some(seg) = segs.get(s) else {
            continue;
        };
        let st = &mut stats[seg.program as usize];
        if level >= 2 {
            st.rapid_hits.push(seg.move_index);
        } else {
            st.rapid_grazes.push(seg.move_index);
        }
    }
    if job.carried.is_none() {
        let began = work_begin(segs.len());
        let mut st: Vec<&mut sim::SimStats> = stats.iter_mut().collect();
        confirm_rapids_3d(job, tps, &mut st);
        work_end(began);
    }
    stats
        .into_iter()
        .map(|s| (s.rapid_hits, s.rapid_grazes, s.heavy))
        .collect()
}

/// The rapids the radial map flags as through stock or grazing it, confirmed on the 3D stock
/// (a tri-dexel of the bar cut by every move before them): through it (deeper than the radial
/// sim's own [`sim::RAPID_DEPTH`]) still counted; into it by less, a graze, said as one; clear
/// only where the tool is not into it by more than [`dexel::GRAZE`]. First on rays
/// [`DEXEL_H`] apart, where through means deeper than that spacing (the rays are that far
/// apart: a rapid within it of stock it only grazes reads as in it), and the ones in between
/// on rays [`DEXEL_FINE_H`] apart round each.
fn confirm_rapids_3d(job: &Job, tps: &[Toolpath], programs: &mut [&mut sim::SimStats]) {
    use std::collections::HashSet;
    // (every program: the replay also says which operations take more than their tool's
    // bite; Mar's impeller-6, its ball finish after a roughing stopped where its tool
    // reaches, 5.7 times)
    if programs.is_empty() {
        return;
    }
    let last = programs.len() - 1;
    let tool = |t: u32| tool_solid(job, t);
    let segs: Vec<Vec<sim::Segment>> = tps
        .iter()
        .take(last + 1)
        .map(|tp| sim::segments(std::slice::from_ref(tp)))
        .collect();
    let mut solid = dexel::Dexel::bar(job.stock.radius, job.stock.length, DEXEL_H);
    // the ones into the coarse stock but not through it by its spacing
    let mut unsure: Vec<(usize, usize)> = Vec::new();
    for (k, sg) in segs.iter().enumerate() {
        let r = dexel::replay(&mut solid, sg, &tool, 0.25, DEXEL_H);
        let through: HashSet<usize> = r.rapid_hits.iter().map(|h| h.1).collect();
        let touching: HashSet<usize> = r.rapid_grazes.iter().map(|h| h.1).collect();
        let st = &mut *programs[k];
        st.heavy = r
            .heavy
            .into_iter()
            .map(|h| dexel::Heavy {
                program: k as u16,
                ..h
            })
            .collect();
        let mut flagged = std::mem::take(&mut st.rapid_hits);
        flagged.append(&mut st.rapid_grazes);
        flagged.sort_unstable();
        flagged.dedup();
        for i in flagged {
            if through.contains(&i) {
                st.rapid_hits.push(i);
            } else if touching.contains(&i) {
                unsure.push((k, i));
            }
        }
    }
    // each on a fine stock round it, cut by every move before it
    for (k, i) in unsure {
        let Some(at) = segs[k].iter().position(|s| s.move_index == i && s.rapid) else {
            continue;
        };
        let seg = &segs[k][at];
        let Some(t) = seg.tool.and_then(tool) else {
            continue;
        };
        let Some((lo, hi)) = dexel::sweep_box(seg, &t, 1.0) else {
            continue;
        };
        let mut fine =
            dexel::Dexel::bar_within(job.stock.radius, job.stock.length, DEXEL_FINE_H, lo, hi);
        for sg in &segs[..k] {
            dexel::replay(&mut fine, sg, &tool, 0.25, DEXEL_H);
        }
        dexel::replay(&mut fine, &segs[k][..at], &tool, 0.25, DEXEL_H);
        let r = dexel::replay(
            &mut fine,
            std::slice::from_ref(seg),
            &tool,
            0.05,
            sim::RAPID_DEPTH,
        );
        let st = &mut *programs[k];
        if !r.rapid_hits.is_empty() {
            st.rapid_hits.push(i);
        } else if !r.rapid_grazes.is_empty() {
            st.rapid_grazes.push(i);
        }
    }
    for p in programs.iter_mut() {
        p.rapid_hits.sort_unstable();
        p.rapid_grazes.sort_unstable();
    }
}

/// A tool's solid for the 3D in-process stock (its flutes and shank: what may not pass
/// through the stock).
fn tool_solid(job: &Job, t: u32) -> Option<dexel::ToolSolid> {
    let c = job.tool(t).and_then(|d| cutter_for(t, d.shape).ok())?;
    Some(match c {
        Cutter::Ball { radius } => dexel::ToolSolid {
            radius,
            ball: true,
            height: 40.0,
            cone: 0.0,
        },
        // (a V-bit: its cone, a knurl's grooves V-shaped as cut)
        Cutter::Cone { radius, half_angle } => dexel::ToolSolid {
            radius,
            ball: false,
            height: 40.0,
            cone: half_angle.tan(),
        },
        c => dexel::ToolSolid {
            radius: c.radius(),
            ball: false,
            height: 40.0,
            cone: 0.0,
        },
    })
}

/// (Mar's impeller; the overhang fixture's wall, 16 normal steps' worth) the bar as the
/// programs before left it in 3D: what an indexed roughing's angles see from above. The
/// radial map cannot hold what an angle before took from the side, under what stays above
/// it. None: a piece carried in (the radial map, as before).
fn indexed_dexel(job: &Job, prior: &[Vec<Move>]) -> Option<dexel::Dexel> {
    if job.carried.is_some() {
        return None;
    }
    let mut dx = dexel::Dexel::bar(job.stock.radius, job.stock.length, DEXEL_H);
    let tps: Vec<Toolpath> = prior.iter().map(|m| bar_toolpath(job, m)).collect();
    let segs: Vec<sim::Segment> = sim::segments(&tps)
        .into_iter()
        .filter(|s| !s.rapid)
        .collect();
    dexel::replay(&mut dx, &segs, &|t| tool_solid(job, t), 0.25, 0.0);
    Some(dx)
}

/// [`indexed_stock`] from the 3D in-process stock.
fn indexed_stock_dexel(
    job: &Job,
    dx: &mut dexel::Dexel,
    applied: &mut usize,
    before: &[Move],
    sb: &job::StockBox,
    cell: f64,
    index: f64,
) -> (sim3::Heightmap3, Vec<f64>) {
    let segs: Vec<sim::Segment> = sim::segments(&[bar_toolpath(job, before)])
        .into_iter()
        .filter(|s| s.move_index >= *applied && !s.rapid)
        .collect();
    dexel::replay(dx, &segs, &|t| tool_solid(job, t), 0.25, 0.0);
    *applied = before.len();
    let mut hm = sim3::Heightmap3::new(sb, cell);
    let mut bots = vec![hm.bottom; hm.nx * hm.ny];
    for j in 0..hm.ny {
        for i in 0..hm.nx {
            let (x, y) = hm.centre(i, j);
            let k = j * hm.nx + i;
            match dx.column(x, y, index, sb.min[2], sb.max[2], 0.5 * DEXEL_H) {
                Some((t, u)) => {
                    hm.z[k] = t.min(sb.max[2]);
                    bots[k] = u;
                }
                None => hm.z[k] = sb.min[2],
            }
        }
    }
    hm.top = hm.z.iter().copied().fold(sb.min[2], f64::max);
    (hm, bots)
}

/// `n` A angles equally apart, the first where the part is lowest under the upright tool
/// (between its blades): the part turned to each candidate as the indexed roughing turns
/// it, the tool dropped on it along X at Y0; the angle whose `n` turns, and the turns a
/// quarter of the way to the next either side, leave it deepest. (Mar: "the coarser
/// sampling") candidates every 6 degrees, then every 2 round the best of those. The turns'
/// depths are independent of each other: found in parallel natively, and across workers in
/// the browser (see [`gap_keys`], [`gap_fine_keys`], [`gap_depth`] and [`gap_pick`]).
fn gap_angles(mesh: &Mesh, c: Cutter, n: u32, x0: f64, x1: f64) -> Vec<f64> {
    let depths_of = |keys: &[i64]| -> Vec<f64> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            keys.par_iter()
                .map(|&k| gap_depth(mesh, c, x0, x1, k))
                .collect()
        }
        #[cfg(target_arch = "wasm32")]
        keys.iter()
            .map(|&k| gap_depth(mesh, c, x0, x1, k))
            .collect()
    };
    let keys = gap_keys(n);
    let mut at: std::collections::HashMap<i64, f64> =
        keys.iter().copied().zip(depths_of(&keys)).collect();
    let fine = gap_fine_keys(n, |k| at[&k]);
    let fine: Vec<i64> = fine.into_iter().filter(|k| !at.contains_key(k)).collect();
    at.extend(fine.iter().copied().zip(depths_of(&fine)));
    gap_pick(n, |k| at[&k])
}

/// The candidates every `every` degrees across the first step of `n` angles, each with its
/// `n` turns and a quarter of a step either side.
fn gap_candidates(n: u32, every: f64) -> Vec<(f64, Vec<f64>)> {
    let step = 360.0 / n.max(1) as f64;
    let mut out = Vec::new();
    let mut a0 = 0.0;
    while n > 0 && a0 < step - 1e-9 {
        out.push((a0, gap_turns(n, a0)));
        a0 += every;
    }
    out
}

/// Candidate `a0`'s turns: its `n` angles, and a quarter of a step either side of each.
fn gap_turns(n: u32, a0: f64) -> Vec<f64> {
    let step = 360.0 / n as f64;
    (0..n)
        .flat_map(|k| [-0.25, 0.0, 0.25].map(|f| a0 + (k as f64 + f) * step))
        .collect()
}

/// The coarse candidates' best (the first of the deepest), and the fine ones round it: every 2
/// degrees within 4 of it (the first step's angles, wrapped).
fn gap_fine(n: u32, depth: &impl Fn(i64) -> f64) -> Vec<f64> {
    let step = 360.0 / n as f64;
    let mut best = (f64::INFINITY, 0.0);
    for (a0, turns) in gap_candidates(n, GAP_COARSE) {
        let s: f64 = turns.iter().map(|&a| depth(gap_key(a))).sum();
        if s < best.0 {
            best = (s, a0);
        }
    }
    [-4.0, -2.0, 2.0, 4.0]
        .iter()
        .map(|d| (best.1 + d).rem_euclid(step))
        .filter(|a| *a < step - 1e-9)
        .collect()
}

/// Degrees between the coarse candidates.
const GAP_COARSE: f64 = 6.0;

/// An angle as [`gap_angles`] keys its depths: tenths of a degree, 0 to 3599.
fn gap_key(a: f64) -> i64 {
    (a.rem_euclid(360.0) * 10.0).round() as i64
}

fn keys_of(angles: impl Iterator<Item = f64>) -> Vec<i64> {
    let mut seen = std::collections::HashSet::new();
    angles.map(gap_key).filter(|k| seen.insert(*k)).collect()
}

/// The turns (keys: tenths of a degree) whose depths the coarse search needs for `n` angles,
/// each once, in the order they are first wanted.
pub fn gap_keys(n: u32) -> Vec<i64> {
    keys_of(
        gap_candidates(n, GAP_COARSE)
            .into_iter()
            .flat_map(|(_, t)| t),
    )
}

/// The turns the fine search needs, given the coarse ones' depths (`depth` of a key).
pub fn gap_fine_keys(n: u32, depth: impl Fn(i64) -> f64) -> Vec<i64> {
    if n == 0 {
        return Vec::new();
    }
    keys_of(
        gap_fine(n, &depth)
            .into_iter()
            .flat_map(|a0| gap_turns(n, a0)),
    )
}

/// How deep the upright tool `c` drops on `mesh` turned to `key` (tenths of a degree): the sum
/// of its depths along X (`x0`..`x1`) at Y0.
pub fn gap_depth(mesh: &Mesh, c: Cutter, x0: f64, x1: f64, key: i64) -> f64 {
    let xs: Vec<f64> = (0..=16).map(|i| x0 + (x1 - x0) * i as f64 / 16.0).collect();
    // (only the triangles the tool can touch along its line: the strip it travels, as wide as
    // the tool; the depths are the same, the drop cutter far quicker to make)
    let (sn, cs) = (key as f64 / 10.0).to_radians().dsin_cos();
    let r = c.radius() + 1e-6;
    let strip = Mesh {
        triangles: mesh
            .triangles
            .iter()
            .filter_map(|t| {
                let q = t.map(|p| [p[0], cs * p[1] - sn * p[2], sn * p[1] + cs * p[2]]);
                let (xa, xb) = (
                    q[0][0].min(q[1][0]).min(q[2][0]),
                    q[0][0].max(q[1][0]).max(q[2][0]),
                );
                let (ya, yb) = (
                    q[0][1].min(q[1][1]).min(q[2][1]),
                    q[0][1].max(q[1][1]).max(q[2][1]),
                );
                (xb >= x0 - r && xa <= x1 + r && yb >= -r && ya <= r).then_some(q)
            })
            .collect(),
        faces: Vec::new(),
    };
    let dc = strategy::three_axis::DropCutter3::new(&strip, c);
    xs.iter()
        .map(|&x| dc.z_at(x, 0.0, -1e9).max(-1e3))
        .sum::<f64>()
}

/// The `n` angles from the turns' depths (`depth` of a key, see [`gap_keys`] and
/// [`gap_fine_keys`]): of the coarse candidates and the fine ones round their best, in order,
/// the first of the deepest.
pub fn gap_pick(n: u32, depth: impl Fn(i64) -> f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let step = 360.0 / n as f64;
    let mut cands: Vec<f64> = gap_candidates(n, GAP_COARSE)
        .into_iter()
        .map(|c| c.0)
        .collect();
    cands.extend(gap_fine(n, &depth));
    cands.sort_by(f64::total_cmp);
    cands.dedup();
    let mut best = (f64::INFINITY, 0.0);
    for a0 in cands {
        // (with the turns either side: in the middle of a gap, not at its edge)
        let s: f64 = gap_turns(n, a0).iter().map(|&a| depth(gap_key(a))).sum();
        if s < best.0 {
            best = (s, a0);
        }
    }
    (0..n).map(|k| best.1 + k as f64 * step).collect()
}

/// (The browser's workers) the depths [`gap_angles`] finds for indexed 3D roughing with tool
/// `tool` in `job` (its part `mesh`), at `keys`: the part as the setup places it, the X range
/// it spans.
pub fn indexed_gap_depths(
    job: &Job,
    mesh: Option<&Mesh>,
    tool: u32,
    keys: &[i64],
) -> Result<Vec<f64>, Error> {
    let t = job
        .tool(tool)
        .ok_or_else(|| Error::Invalid(format!("no tool T{tool}")))?;
    let c = cutter_for(tool, t.shape)?;
    let revolved;
    let m = match mesh {
        Some(m) => m,
        None => {
            let p = job.part.profile.as_ref().ok_or_else(|| {
                Error::Invalid("indexed roughing needs the part's mesh or profile".into())
            })?;
            revolved = Mesh::revolve(p, PROFILE_SEGMENTS);
            &revolved
        }
    };
    // (the block's X range, as planning finds it: the part's)
    let profile = job
        .part
        .profile
        .as_ref()
        .map(|p| RevolvedProfile::new(p, job.stock.radius));
    let (x0, x1) = match (&profile, mesh.and_then(Mesh::bounds)) {
        (_, Some((lo, hi))) => (lo[0], hi[0]),
        (Some(p), None) => (p.x_min(), p.x_max()),
        _ => (-job.stock.length, 0.0),
    };
    Ok(keys.iter().map(|&k| gap_depth(m, c, x0, x1, k)).collect())
}

/// What the moves before leave of the stock, at `cell` mm cells (for a tool of `radius`;
/// `what` for the error).
fn cut_heightmap(
    job: &Job,
    prior: &[Vec<Move>],
    before: &[Move],
    sb: &job::StockBox,
    cell: f64,
    radius: f64,
    what: &str,
) -> Result<sim3::Heightmap3, String> {
    let cutter_of = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let mut segs: Vec<sim::Segment> = Vec::new();
    for (p, moves) in prior.iter().map(Vec::as_slice).chain([before]).enumerate() {
        segs.extend(sim3::segments_of3(moves, p as u16));
    }
    let work = sim3::work_estimate3(cell, sb.min[2], sb.max[2], &segs, &cutter_of, cell);
    let mut hm = sim3::Heightmap3::of_job(job, sb, cell);
    let disc = (radius / cell).ceil().powi(2) * 4.0;
    if !budget::spend((work + (hm.nx * hm.ny) as f64 * disc) as u64) {
        return Err(format!(
            "{what}: the operations before it are too much to simulate at {cell} mm cells"
        ));
    }
    for seg in &segs {
        let cut = seg.tool.and_then(cutter_of);
        sim3::apply_segment3(&mut hm, seg, cut.as_ref(), cell);
    }
    Ok(hm)
}

/// Operations planned before, kept between planning runs: an operation is planned again only
/// when something it depends on changed. Its key covers the operation, its tool, the job's
/// stock, clearance, spindle and part (the mesh's triangles, or the profile), the program's
/// frame, and where the tool was when it started (position, A, the tool loaded): anything
/// else gives the same moves, so a hit replays them (bit-identical to planning again).
#[derive(Debug, Default)]
pub struct PlanCache {
    map: std::collections::HashMap<u64, CachedOp>,
    /// Operations reused / planned in the last run (see [`PlanCache::take_counts`]).
    pub hits: usize,
    pub misses: usize,
}

#[derive(Debug, Clone)]
struct CachedOp {
    /// The operation's moves (from its marker on), with its marker's number to renumber.
    moves: Vec<Move>,
    /// The builder after it.
    exit: (Option<f64>, Option<f64>, f64, Option<[f64; 3]>),
    /// What it warned about.
    warnings: Vec<String>,
    /// Refused as too fine to plan ([`too_fine`]): the same inputs are refused again at once
    /// (the budget counts work, not time, so the outcome is the same).
    refused: Option<String>,
}

impl PlanCache {
    /// Most operations kept (the oldest go first: the whole cache is dropped past it).
    pub const MAX_OPS: usize = 4096;

    pub fn new() -> PlanCache {
        PlanCache::default()
    }

    /// (reused, planned) since the last call, and the counts reset.
    pub fn take_counts(&mut self) -> (usize, usize) {
        let c = (self.hits, self.misses);
        self.hits = 0;
        self.misses = 0;
        c
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

pub(crate) fn hash_of<T: std::hash::Hash + ?Sized>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// What a job's operations share: everything they may depend on but themselves.
pub(crate) fn job_key(job: &Job, mesh: Option<&Mesh>) -> u64 {
    let mut bits: Vec<u64> = Vec::new();
    if let Some(m) = mesh {
        bits.reserve(m.triangles.len() * 9);
        for t in &m.triangles {
            for p in t {
                bits.extend(p.iter().map(|c| c.to_bits()));
            }
        }
        bits.extend(m.faces.iter().map(|&f| f as u64));
    }
    let rest = serde_json::to_string(&(
        &job.stock,
        &job.stock_box,
        job.clearance,
        job.spindle_rpm,
        &job.part.profile,
        &job.part.mesh,
        &job.tools,
    ))
    .unwrap_or_default();
    // (the stock carried from the setup before)
    let carried = job.carried.as_ref().map(|c| c.key());
    hash_of(&(bits, rest, carried, env!("CARGO_PKG_VERSION")))
}

impl Ctx<'_> {
    /// Finishing surface: the loaded mesh, else the revolved profile.
    fn surface(&mut self, program: &str) -> Result<&Mesh, Error> {
        if let Some(m) = self.mesh {
            return Ok(m);
        }
        if self.job.part.mesh.is_some() {
            return Err(Error::Invalid(format!(
                "program {program}: part.mesh is set but the mesh was not loaded (see load_mesh)"
            )));
        }
        let profile = self.job.part.profile.as_ref().ok_or_else(|| {
            Error::Invalid(format!(
                "program {program}: mesh operations need part.mesh or part.profile"
            ))
        })?;
        Ok(self
            .revolved
            .get_or_insert_with(|| Mesh::revolve(profile, PROFILE_SEGMENTS)))
    }
}

/// The triangles of `mesh` in its face regions `ids` ([`geometry::face_regions`] at 30°,
/// the same regions a face selection names).
pub fn face_subset(mesh: &Mesh, ids: &[String]) -> Result<Mesh, String> {
    let (regions, label) = geometry::face_regions(mesh, 30.0);
    let want: Vec<usize> = ids
        .iter()
        .map(|id| {
            regions
                .iter()
                .position(|r| &r.id == id)
                .ok_or(format!("the part has no face {id:?}"))
        })
        .collect::<Result<_, _>>()?;
    // the triangles of those faces (with their B-rep faces, for a STEP import)
    let keep: Vec<usize> = (0..mesh.triangles.len())
        .filter(|&i| want.contains(&label[i]))
        .collect();
    Ok(Mesh {
        triangles: keep.iter().map(|&i| mesh.triangles[i]).collect(),
        faces: if mesh.faces.len() == mesh.triangles.len() {
            keep.iter().map(|&i| mesh.faces[i]).collect()
        } else {
            Vec::new()
        },
    })
}

fn check_flute(tool: &job::Tool, stock_r: f64, r_min: f64) -> Result<(), Error> {
    if let Some(flute) = tool.flute_length {
        let depth = stock_r - r_min;
        if depth >= flute - 1.0 {
            return Err(Error::FluteTooShort {
                tool: tool.number,
                depth,
                flute_length: flute,
            });
        }
    }
    Ok(())
}

pub fn cutter_for(number: u32, shape: ToolShape) -> Result<Cutter, Error> {
    Ok(match shape {
        ToolShape::Flat { diameter } => Cutter::Flat {
            radius: diameter / 2.0,
        },
        ToolShape::Ball { diameter } => Cutter::Ball {
            radius: diameter / 2.0,
        },
        ToolShape::Vbit {
            angle,
            diameter: Some(d),
        } => Cutter::Cone {
            radius: d / 2.0,
            half_angle: (angle / 2.0).to_radians(),
        },
        ToolShape::Vbit { diameter: None, .. } => {
            return Err(Error::Invalid(format!(
                "T{number}: rotary_finish with a V-bit needs the tool's diameter"
            )))
        }
        ToolShape::TaperedBall {
            tip_diameter,
            taper_angle,
            diameter,
        } => {
            if !(tip_diameter > 0.0 && (0.0..90.0).contains(&taper_angle))
                || diameter < tip_diameter
            {
                return Err(Error::Invalid(format!(
                    "T{number}: a tapered ball needs tip_diameter > 0, 0 <= taper_angle < 90 deg                      (each side from the axis) and diameter >= tip_diameter"
                )));
            }
            if taper_angle < 1e-6 {
                // straight sides: a ball end mill of the tip's size
                return Ok(Cutter::Ball {
                    radius: tip_diameter / 2.0,
                });
            }
            Cutter::TaperedBall {
                tip_radius: tip_diameter / 2.0,
                half_angle: taper_angle.to_radians(),
                radius: diameter / 2.0,
            }
        }
    })
}

fn is_three_axis(op: &Operation) -> bool {
    matches!(
        op,
        Operation::Face(_)
            | Operation::Pocket(_)
            | Operation::Drill(_)
            | Operation::Bore(_)
            | Operation::Contour(_)
            | Operation::Raster3d(_)
            | Operation::Rough3d(_)
            | Operation::Waterline(_)
            | Operation::Chamfer(_)
            | Operation::ThreadMill(_)
    )
}

/// Turn machine-frame 3-axis moves made with the rotary at `a_deg` into general moves in
/// the part frame (`Move::Pose`); `start` is the machine position before them. Rapids that do
/// not move are dropped.
fn general_moves(moves: Vec<Move>, start: [f64; 3], a_deg: f64) -> Vec<Move> {
    let axis = kinematics::to_part([0.0, 0.0, 1.0], a_deg);
    let mut p = start;
    let mut out = Vec::with_capacity(moves.len());
    for m in moves {
        let feed = match m {
            Move::Rapid3 { x, y, z } => {
                let q = [x.unwrap_or(p[0]), y.unwrap_or(p[1]), z.unwrap_or(p[2])];
                if q == p {
                    continue;
                }
                p = q;
                None
            }
            Move::Line3 { x, y, z, feed, .. } => {
                p = [x, y, z];
                Some(feed)
            }
            other => {
                out.push(other);
                continue;
            }
        };
        out.push(Move::Pose {
            tip: kinematics::to_part(p, a_deg),
            axis,
            feed,
        });
    }
    out
}

/// The deepest cut into the part of 3-axis moves (more than 0.05 mm): where (x, y), the tip's
/// Z there and the part's (the lowest tip Z that does not cut it), or None.
fn gouge_of(moves: &[Move], mesh: &Mesh, c: Cutter) -> Option<(f64, f64, f64, f64)> {
    // (a tool running along a wall touches it: checked a hair thinner, only what cuts into it)
    let c = c.grown(-0.05).unwrap_or(c);
    let dc = strategy::three_axis::DropCutter3::new(mesh, c);
    let mut worst: Option<(f64, f64, f64, f64)> = None;
    let mut prev: Option<[f64; 3]> = None;
    for m in moves {
        let (p, feed) = match m {
            Move::Line3 { x, y, z, .. } => ([*x, *y, *z], true),
            Move::Rapid3 { x, y, z, .. } => (
                [
                    x.unwrap_or(prev.map_or(0.0, |p| p[0])),
                    y.unwrap_or(prev.map_or(0.0, |p| p[1])),
                    z.unwrap_or(prev.map_or(0.0, |p| p[2])),
                ],
                false,
            ),
            _ => continue,
        };
        if feed {
            if let Some(q) = prev {
                // (along the move, every half millimetre)
                let n = ((p[0] - q[0]).hypot(p[1] - q[1]) / 0.5).ceil().max(1.0) as usize;
                for k in 1..=n {
                    let t = k as f64 / n as f64;
                    let (x, y, z) = (
                        q[0] + (p[0] - q[0]) * t,
                        q[1] + (p[1] - q[1]) * t,
                        q[2] + (p[2] - q[2]) * t,
                    );
                    let top = dc.z_at(x, y, -1e9);
                    if top - z > 0.05 && worst.is_none_or(|w| top - z > w.3 - w.2) {
                        worst = Some((x, y, z, top));
                    }
                }
            }
        }
        prev = Some(p);
    }
    worst
}

/// Operations that work on either setup: flat on a vise, wrapped on a rotary setup.
pub fn is_surface_op(op: &Operation) -> bool {
    matches!(
        op,
        Operation::Engrave(_) | Operation::Vcarve(_) | Operation::Custom(_)
    )
}

/// Keep refusal `e` of the operation with cache key `key`: the same inputs are refused again
/// without planning them. Returns `e`.
fn refuse(cache: Option<&mut PlanCache>, key: Option<u64>, e: Error) -> Error {
    if let (Some(cache), Some(key)) = (cache, key) {
        if cache.map.len() >= PlanCache::MAX_OPS {
            cache.map.clear();
        }
        cache.map.insert(
            key,
            CachedOp {
                moves: Vec::new(),
                exit: (None, None, 0.0, None),
                warnings: Vec::new(),
                refused: Some(e.to_string()),
            },
        );
    }
    e
}

/// The error for an operation past its planning budget ([`budget`]).
fn too_fine(program: &str, op: &Operation) -> Error {
    Error::Invalid(format!(
        "program {program}: {} is set too fine to plan: it needs more than {} million moves or {} billion surface checks (minutes, and more memory than the app has). Raise its stepover (or scallop), sample spacing, step-down or helix pitch, or narrow its range",
        op_name(op),
        budget::MOVE_LIMIT / 1_000_000,
        budget::OP_LIMIT as f64 / 1e9
    ))
}

static PHASE: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// Where the work is, told as it goes (which operation is planned, the checks): for
/// diagnostics, e.g. the browser's worker passes it to the page while a long call runs. Set
/// once; nothing is told without it.
pub fn set_phase_hook(f: fn(&str)) {
    let _ = PHASE.set(f);
}

/// Tell the phase hook (if any) what is done now.
pub fn phase(what: &str) {
    if let Some(f) = PHASE.get() {
        f(what);
    }
}

/// How much of a long job is done: (units in all, done, the last per mille told).
static WORK: std::sync::Mutex<(usize, usize, usize)> = std::sync::Mutex::new((0, 0, usize::MAX));

/// A long job of `total` units (moves simulated, rows meshed) begins: [`work`] counts them
/// and the phase hook is told `[work d/1000]` as each per mille passes (the page's progress
/// from work done, not a time estimate). Only the outermost job counts (its parts, begun
/// inside it, are already in its total): true when this one is it, to pass to [`work_end`].
pub fn work_begin(total: usize) -> bool {
    let mut w = WORK.lock().unwrap_or_else(|e| e.into_inner());
    if w.0 != 0 || total == 0 {
        return false;
    }
    *w = (total, 0, usize::MAX);
    drop(w);
    work(0);
    true
}

/// `n` more units of the job begun done (nothing without one).
pub fn work(n: usize) {
    let mut w = WORK.lock().unwrap_or_else(|e| e.into_inner());
    if w.0 == 0 {
        return;
    }
    w.1 = (w.1 + n).min(w.0);
    let pm = w.1 * 1000 / w.0;
    if pm != w.2 {
        w.2 = pm;
        drop(w);
        phase(&format!("[work {pm}/1000]"));
    }
}

/// The job [`work_begin`] began (when it said true) over.
pub fn work_end(began: bool) {
    if began {
        *WORK.lock().unwrap_or_else(|e| e.into_inner()) = (0, 0, usize::MAX);
    }
}

/// How far past the part's ends an indexed operation's box takes in the bar (mm): more than
/// a tool's radius beside the part.
const INDEXED_REACH: f64 = 5.0;

fn plan_program(ctx: &mut Ctx, prog: &Program, next_op: &mut u32) -> Result<Toolpath, Error> {
    let job = ctx.job;
    // a vise setup (box stock) runs only 3-axis operations; a rotary setup runs both, the
    // 3-axis ones indexed to a fixed A (3+1)
    let three = job.stock_box.is_some();
    if three
        && prog
            .operations
            .iter()
            .any(|o| !is_three_axis(o) && !is_surface_op(o))
    {
        return Err(Error::Invalid(format!(
            "program {}: rotary operations need bar stock on the rotary axis, not a vise setup",
            prog.name
        )));
    }
    if three {
        if let Some(a) = prog
            .operations
            .iter()
            .filter_map(Operation::index_a)
            .find(|a| *a != 0.0)
        {
            return Err(Error::Invalid(format!(
                "program {}: index_a {a} on a vise setup (it has no rotary axis)",
                prog.name
            )));
        }
    }
    // the block the 3-axis strategies work in: the vise stock, or around the bar (its
    // radius in Y and Z over the part's X range) for indexed operations
    let stock_box = job.stock_box.or_else(|| {
        prog.operations.iter().any(is_three_axis).then(|| {
            let rad = job.stock.radius;
            let (xlo, xhi) = match (&ctx.profile, ctx.mesh.and_then(Mesh::bounds)) {
                (_, Some((lo, hi))) => (lo[0], hi[0]),
                (Some(p), None) => (p.x_min(), p.x_max()),
                _ => (-job.stock.length, 0.0),
            };
            // (the impeller past its nose: the bar there, a tool's reach from the part, left
            // out of the box, was taken for what the indexes before had left at its edge, and
            // slotted 6-11 mm deep) a little of the bar past each end of the part too
            let (xlo, xhi) = (
                (xlo - INDEXED_REACH).max(-job.stock.length),
                (xhi + INDEXED_REACH).min(0.0),
            );
            job::StockBox {
                min: [xlo, -rad, -rad],
                max: [xhi, rad, rad],
            }
        })
    });
    // (a bar on the 4th axis: the stock along all its length, past the part's ends)
    let bar_x = job.stock_box.is_none().then(|| [-job.stock.length, 0.0]);
    // safe radius above the rotary axis, or safe Z above the block
    let safe_r = match (three, stock_box) {
        (true, Some(b)) => b.max[2] + job.clearance,
        _ => job.safe_r(),
    };
    let rapid_safe = |b: &mut Builder| {
        if three {
            b.rapid3(None, None, Some(safe_r));
        } else {
            b.rapid_r(safe_r);
        }
    };
    let mut b = Builder::new();
    if prog.reset_a {
        b.reset_a();
    }
    let mut current: Option<u32> = None;
    let mut first_tool = None;
    for op in &prog.operations {
        // the work it may take (see budget): counted until the end of this iteration
        // (Mar: "a real progress wheel") with where it is: operation i of n
        let op_at = *next_op + 1;
        let op_total: usize = job.programs.iter().map(|p| p.operations.len()).sum();
        phase(&format!(
            "planning {} ({}) [op {op_at}/{op_total}]",
            op_name(op),
            prog.name
        ));
        let _budget = budget::Guard::start(budget::OP_LIMIT);
        b.ramp_tan = op
            .ramp_angle()
            .map_or(0.0, |deg| deg.clamp(0.0, 89.0).to_radians().dtan());
        let number = op.tool();
        let tool = job.tool(number).ok_or_else(|| Error::UnknownTool {
            program: prog.name.clone(),
            tool: number,
        })?;
        let wrong = |needs| Error::WrongToolShape {
            program: prog.name.clone(),
            tool: number,
            operation: op_name(op),
            needs,
        };
        // validate before emitting anything
        let cutter = match (op, tool.shape) {
            (Operation::RotaryTurning(_), ToolShape::Flat { .. }) => {
                let geom = ctx.profile.as_ref().ok_or_else(|| {
                    Error::Invalid(format!(
                        "program {}: rotary_turning needs part.profile",
                        prog.name
                    ))
                })?;
                check_flute(tool, job.stock.radius, geom.min_radius())?;
                None
            }
            (Operation::RotaryTurning(_), _) => return Err(wrong("flat")),
            (Operation::HelicalKnurl(_) | Operation::FlankChamfer(_), ToolShape::Vbit { .. }) => {
                None
            }
            (Operation::HelicalKnurl(_) | Operation::FlankChamfer(_), _) => {
                return Err(wrong("vbit"))
            }
            (Operation::Parting(_), ToolShape::Flat { .. } | ToolShape::Vbit { .. }) => None,
            (Operation::Parting(_), _) => return Err(wrong("flat or vbit")),
            (
                Operation::RotaryFinish(_)
                | Operation::RotaryParallel(_)
                | Operation::RotaryFlowline(_)
                | Operation::RotaryWall(_)
                | Operation::RotaryPocket(_)
                | Operation::RotaryRough(_)
                | Operation::Raster3d(_)
                | Operation::Rough3d(_)
                | Operation::Waterline(_),
                shape,
            ) => Some(cutter_for(number, shape)?),
            (
                Operation::Face(_)
                | Operation::Pocket(_)
                | Operation::Drill(_)
                | Operation::Bore(_)
                | Operation::Contour(_),
                ToolShape::Flat { .. },
            ) => None,
            (Operation::Engrave(_), _) => None,
            (Operation::Custom(_), _) => None,
            (Operation::Vcarve(_), ToolShape::Vbit { .. }) => None,
            (Operation::Chamfer(_), ToolShape::Vbit { .. }) => None,
            (Operation::Chamfer(_), _) => return Err(wrong("vbit")),
            (Operation::ThreadMill(_), ToolShape::Vbit { .. } | ToolShape::Flat { .. }) => None,
            (Operation::ThreadMill(_), _) => return Err(wrong("vbit or flat")),
            (Operation::Vcarve(_), _) => return Err(wrong("vbit")),
            (
                Operation::Face(_)
                | Operation::Pocket(_)
                | Operation::Drill(_)
                | Operation::Bore(_)
                | Operation::Contour(_),
                _,
            ) => return Err(wrong("flat")),
        };
        // (Mar) in-process stock: a 2D operation after cuts in the setup (or on a stock carried
        // in) starts at the material they left in its way
        let aware = three
            && op.index_a().unwrap_or(0.0) == 0.0
            && in_process(op)
            && (job.carried.is_some() || has_cuts(&ctx.prior, &b.moves));
        // planned before with everything the same: replay it (renumbered)
        let key = ctx.cache.as_ref().map(|_| {
            hash_of(&(
                ctx.job_key,
                serde_json::to_string(&(
                    op,
                    tool,
                    three,
                    stock_box,
                    safe_r.to_bits(),
                    current,
                    b.x.map(f64::to_bits),
                    b.r.map(f64::to_bits),
                    b.a.to_bits(),
                    b.p3.map(|p| p.map(f64::to_bits)),
                    // a rest roughing, or one on the stock the moves before left (indexed
                    // roughing, rotary rest roughing): and the moves before it (an earlier
                    // operation changed, it is planned again; changed itself or after, the
                    // ones before are kept)
                    uses_prior(op).then(|| moves_key(&ctx.prior, &b.moves)),
                    // in-process stock: and the moves before it (what they left)
                    aware.then(|| cuts_key(&ctx.prior, &b.moves)),
                ))
                .unwrap_or_default(),
            ))
        });
        if let (Some(cache), Some(key)) = (ctx.cache.as_deref_mut(), key) {
            if let Some(hit) = cache.map.get(&key) {
                cache.hits += 1;
                if let Some(why) = &hit.refused {
                    return Err(Error::Invalid(why.clone()));
                }
                let mut moves = hit.moves.clone();
                if let Some(Move::Operation { id, .. }) = moves.first_mut() {
                    *id = *next_op;
                }
                b.moves.extend(moves);
                (b.x, b.r, b.a, b.p3) = hit.exit;
                b.warnings.extend(hit.warnings.iter().cloned());
                *next_op += 1;
                if current != Some(number) {
                    current = Some(number);
                    first_tool.get_or_insert((number, tool.description.clone()));
                }
                continue;
            }
            cache.misses += 1;
        }
        let cache_from = b.moves.len();
        let warn_from = b.warnings.len();
        b.operation(*next_op, op_name(op));
        // boring on a vise (not indexed): the post writes its circles as arcs
        if let Operation::Bore(o) = op {
            if o.arcs && o.index_a == 0.0 {
                if let Some(Move::Operation { arcs, .. }) = b.moves.last_mut() {
                    *arcs = Some(o.tolerance.max(1e-4));
                }
            }
        }
        *next_op += 1;
        // smoothing: on a vise (or a flat engraving) the post also writes arcs as G2/G3
        let smooth_tol = op.smoothing();
        let flat = three
            && (is_three_axis(op) || matches!(op, Operation::Engrave(_) | Operation::Vcarve(_)));
        if smooth_tol > 0.0 && flat && op.index_a().unwrap_or(0.0) == 0.0 {
            if let Some(Move::Operation { arcs, .. }) = b.moves.last_mut() {
                arcs.get_or_insert(smooth_tol);
            }
        }
        let op_start = b.moves.len();
        let a_before = b.a;
        let rot_before = (b.x, b.r, b.a);
        if current != Some(number) {
            b.tool_change(number, job.spindle_rpm);
            rapid_safe(&mut b);
            current = Some(number);
            first_tool.get_or_insert((number, tool.description.clone()));
        }
        match (op, tool.shape) {
            (Operation::RotaryTurning(o), ToolShape::Flat { diameter }) => {
                let geom = ctx.profile.as_ref().expect("checked above");
                strategy::rotary_turning(&mut b, geom, diameter, job.stock.radius, safe_r, o)
            }
            (Operation::HelicalKnurl(o), ToolShape::Vbit { angle, diameter }) => {
                // (its radial stock to leave: the body it knurls that much bigger)
                let mut o = o.clone();
                o.body_back[1] += o.leave_radial;
                o.body_front[1] += o.leave_radial;
                // (Mar's walkthrough: a knurl on a Ø25 step next to the square base ran out into
                // the base, the holder 2.7 mm into it) an end at a shoulder of the part: no run
                // out there, and the tool's widest kept off the shoulder's face
                let top = 0.5 * diameter.unwrap_or(0.0);
                let body = o.body_back[1].max(o.body_front[1]) + 0.05;
                let mut runout = [o.runout, o.runout];
                if let Some(m) = ctx.mesh {
                    // what stands above the tip, nearest first: the V-bit's widest, the shank
                    // (above the flutes), the holder (above the stickout); each kept off a
                    // shoulder that rises to its height (the holder: from the tool's data, else
                    // the usual 20 mm collet 5 mm above the flutes)
                    let tb = collision::ToolBody::of(tool);
                    let tip = o.body_back[1].min(o.body_front[1]) - 2.0;
                    let zones = [
                        (top, 0.0),
                        (tb.shank_radius, tb.flute_length),
                        (tb.holder_radius, tb.stickout),
                    ];
                    let inset = |side: f64, x: f64| -> f64 {
                        let mut need = 0.0f64;
                        for (i, &(zr, from)) in zones.iter().enumerate() {
                            let reach = if i == 0 { o.runout + zr } else { zr };
                            let (a, b) = if side < 0.0 {
                                (x - reach, x - 1e-3)
                            } else {
                                (x + 1e-3, x + reach)
                            };
                            let rise = part_radius_over(m, a, b);
                            if rise > body && (i == 0 || rise > tip + from) {
                                need = need.max(zr + 0.25);
                            }
                        }
                        need
                    };
                    let back = inset(-1.0, o.body_back[0]);
                    if back > 0.0 {
                        runout[0] = 0.0;
                        o.body_back[0] += back;
                    }
                    let front = inset(1.0, o.body_front[0]);
                    if front > 0.0 {
                        runout[1] = 0.0;
                        o.body_front[0] -= front;
                    }
                    if back > top + 0.25 + 1e-9 || front > top + 0.25 + 1e-9 {
                        b.warnings.push(format!(
                            "the knurl stops {:.1} mm short of the part's shoulder: the tool's holder (Ø{:.0} from {:.1} mm above the tip) would touch it; a longer stickout in the tool's data lets it go nearer",
                            back.max(front),
                            2.0 * tb.holder_radius,
                            tb.stickout
                        ));
                    }
                    if o.body_front[0] <= o.body_back[0] + 0.5 {
                        return Err(Error::Invalid(format!(
                            "program {}: the knurl's body is too short once kept off the part's shoulders (the tool's holder)",
                            prog.name
                        )));
                    }
                }
                // (the walkthrough: the knurl came in at its body's radius + 0.5 into a step
                // the roughing had left 1 mm proud; the holder hit it 280 times) on a carried
                // bar: its approach from the bar as it is, and said when the bar stands above
                // the body (it was to be turned or finished first)
                // (and on a bar not turned down first, as the knob's knurl on its own: it came
                // in by rapids 6 mm into the Ø25 bar, 13 times) from the bar as the moves
                // before left it, where each groove's rapid comes down (its start, the
                // V-bit's width round it); said when the bar stands above the body
                {
                    let (xa, xb) = (o.body_back[0] - runout[0], o.body_front[0] + runout[1]);
                    let m = bar_after(job, &ctx.prior, &b.moves, xa - top, xb + top);
                    let body = o.body_back[1].max(o.body_front[1]);
                    // (a carried bar: from it whenever it stands above the body anywhere, as
                    // before; a bar cut here: where the rapid comes down, when it would end
                    // within a tenth of a mm of it)
                    let carried = matches!(job.carried.as_deref(), Some(carry::Carried::Bar(_)));
                    let raise = if carried {
                        let t = map_top(&m, xa, xb);
                        (t > body + 1e-6).then_some(t)
                    } else {
                        let t = map_top(&m, xa - top - 0.25, xa + top + 0.25);
                        (t + 0.1 > body + o.approach_clearance).then_some(t)
                    };
                    if let Some(t) = raise {
                        o.approach_clearance = o.approach_clearance.max(t - body + 0.5);
                    }
                    let over = map_top(&m, o.body_back[0], o.body_front[0]);
                    if over > body + 0.1 {
                        let w = format!(
                            "the stock is up to {:.2} mm above the knurl's body (X {:.1}..{:.1}): turn or finish it first",
                            over - body,
                            o.body_back[0],
                            o.body_front[0]
                        );
                        if !b.warnings.contains(&w) {
                            b.warnings.push(w);
                        }
                    }
                }
                strategy::helical_knurl(&mut b, angle, safe_r, &o, runout)
                    .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?
            }
            (Operation::FlankChamfer(o), ToolShape::Vbit { angle, .. }) => {
                // (its radial stock to leave: on every pass)
                let mut o = o.clone();
                for p in &mut o.passes {
                    *p += o.leave_radial;
                }
                strategy::flank_chamfer(&mut b, angle, safe_r, &o)
            }
            (Operation::Parting(o), shape) => {
                let t = match shape {
                    ToolShape::Vbit { angle, .. } => strategy::PartingTool::Vbit {
                        half_angle: angle / 2.0,
                    },
                    ToolShape::Flat { diameter } => strategy::PartingTool::Flat {
                        radius: diameter / 2.0,
                    },
                    _ => unreachable!("tool shape validated above"),
                };
                // (its axial stock to leave: cut that much further from the front, the part
                // longer)
                let mut o = o.clone();
                o.x -= o.leave_axial;
                let r_min = strategy::parting(&mut b, t, job.stock.radius, safe_r, &o)
                    .map_err(|e| Error::Invalid(format!("program {}: parting: {e}", prog.name)))?;
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::RotaryWall(o), _) => {
                let mesh = ctx.surface(&prog.name)?;
                let c = grown_for(cutter.expect("set above"), o.leave_axial, &prog.name)?;
                let r_min = if o.y_offset != 0.0 {
                    // (the tool upright off the axis: the drop along its own line)
                    let dc = DropCutter::reaching(mesh, c, o.y_offset.abs());
                    strategy::rotary_wall_offset(&mut b, &dc, safe_r, o)
                } else {
                    let dc = DropCutter::new(mesh, c);
                    strategy::rotary_wall(&mut b, &dc, safe_r, o)
                }
                .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::RotaryPocket(o), _) => {
                let mesh = ctx.surface(&prog.name)?;
                let dc = DropCutter::new(mesh, cutter.expect("set above"));
                // the stock: the bar (or the piece carried in), or with `rest` as the
                // operations before (and this program so far) left it, in 3D
                let rest3d = if o.rest {
                    indexed_dexel(job, &ctx.prior).map(|mut dx| {
                        let segs: Vec<sim::Segment> = sim::segments(&[bar_toolpath(job, &b.moves)])
                            .into_iter()
                            .filter(|s| !s.rapid)
                            .collect();
                        dexel::replay(&mut dx, &segs, &|t| tool_solid(job, t), 0.25, 0.0);
                        dx
                    })
                } else {
                    None
                };
                let (x0, x1) = (
                    o.x_range[0].min(o.x_range[1]),
                    o.x_range[0].max(o.x_range[1]),
                );
                let bar = (!o.rest || rest3d.is_none()).then(|| bar_after(job, &[], &[], x0, x1));
                let top = |x: f64, a: f64| -> f64 {
                    if let Some(dx) = &rest3d {
                        return dx
                            .radial_top(x, 90.0 - a, job.stock.radius + 0.5, 0.1)
                            .max(0.0);
                    }
                    let m = bar.as_ref().expect("one of the two");
                    let g = m.grid;
                    let i = (((x - g.x0) / g.dx).round().max(0.0) as usize).min(g.nx - 1);
                    let j = ((a.rem_euclid(360.0) / 360.0 * g.na as f64).round() as usize) % g.na;
                    m.r[i * g.na + j] as f64
                };
                let rep = strategy::rotary_pocket(&mut b, &dc, &top, safe_r, o)
                    .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                if rep.plunges > 0 {
                    b.warnings.push(format!(
                        "rotary pocket: {} plunge(s) straight into the material (no room for a helix)",
                        rep.plunges
                    ));
                }
                if rep.levels > 0 {
                    check_flute(tool, job.stock.radius, rep.floor)?;
                }
            }
            (Operation::RotaryFlowline(o), _) => {
                let mesh = ctx.surface(&prog.name)?;
                let c = grown_for(cutter.expect("set above"), o.leave_axial, &prog.name)?;
                let dc = DropCutter::new(mesh, c);
                let r_min = strategy::rotary_flowline(&mut b, &dc, safe_r, o)
                    .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::RotaryFinish(o) | Operation::RotaryParallel(o), _) => {
                let mesh = ctx.surface(&prog.name)?;
                // (its axial stock to leave: the tool wider by it)
                let c = grown_for(cutter.expect("set above"), o.leave_axial, &prog.name)?;
                let dc = DropCutter::new(mesh, c);
                let sel = if o.faces.is_empty() {
                    None
                } else {
                    let sub = face_subset(mesh, &o.faces)
                        .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                    Some(DropCutter::new(&sub, c))
                };
                // (rotary parallel's tool offset in Y: a ball's, at most its radius; planned
                // over the axis, the post puts it off it)
                // (the tool's own ball: the planning's is wider by the axial stock to leave)
                let offset = match (o.y_offset, cutter.expect("set above")) {
                    (0.0, _) => None,
                    (y, Cutter::Ball { radius }) if y.abs() <= radius + 1e-9 => Some((y, radius)),
                    (y, Cutter::Ball { radius }) => {
                        return Err(Error::Invalid(format!(
                            "program {}: a Y offset of {y} mm is more than the ball's radius ({radius} mm)",
                            prog.name
                        )))
                    }
                    _ => {
                        return Err(Error::Invalid(format!(
                            "program {}: a Y offset needs a ball tool",
                            prog.name
                        )))
                    }
                };
                if let Some((y, ball_r)) = offset {
                    b.rapid_r(safe_r);
                    b.moves.push(Move::YOffset { y, ball_r });
                }
                let r_min = strategy::rotary_finish_on(
                    &mut b,
                    &dc,
                    sel.as_ref(),
                    mesh.max_radius(),
                    safe_r,
                    o,
                )
                .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                if let Some((_, ball_r)) = offset {
                    b.rapid_r(safe_r);
                    b.moves.push(Move::YOffset { y: 0.0, ball_r });
                }
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::RotaryRough(o), _) => {
                // (Mar's impeller: the hub under the blades, which no index of the roughing
                // reached) rest roughing: the bar as the operations before left it, simulated
                // (in 3D: the radial map kept what the indexed roughing took from the side,
                // and the rest went round air for 4 h)
                let rest3d = if o.rest {
                    indexed_dexel(job, &ctx.prior).map(|mut dx| {
                        let segs: Vec<sim::Segment> = sim::segments(&[bar_toolpath(job, &b.moves)])
                            .into_iter()
                            .filter(|s| !s.rapid)
                            .collect();
                        dexel::replay(&mut dx, &segs, &|t| tool_solid(job, t), 0.25, 0.0);
                        dx
                    })
                } else {
                    None
                };
                let rest_bar: Option<sim::RadialMap> = None;
                let mesh = ctx.surface(&prog.name)?;
                let c = grown_for(cutter.expect("set above"), o.leave_axial, &prog.name)?;
                let dc = DropCutter::new(mesh, c);
                // a bar carried from an earlier setup: where it is turned down already
                let column = rest_bar
                    .as_ref()
                    .or(job.carried.as_deref().and_then(|c| match c {
                        carry::Carried::Bar(m) => Some(m),
                        _ => None,
                    }));
                let tops: Option<Vec<f64>> = column.map(|m| {
                    (0..m.grid.nx)
                        .map(|i| {
                            m.r[i * m.grid.na..(i + 1) * m.grid.na]
                                .iter()
                                .fold(0.0f32, |a, &b| a.max(b)) as f64
                        })
                        .collect()
                });
                // (Mar's impeller: its part and its range lay in front of the bar, X 0..43 with
                // the bar X -63..0; every layer from the bar's radius cut air for 28 h) only
                // where the bar is, the tool's width reaching it: none in front of its face
                // or past its back
                let tr = match tool.shape {
                    ToolShape::Flat { diameter } | ToolShape::Ball { diameter } => diameter / 2.0,
                    ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0) / 2.0,
                    ToolShape::TaperedBall { diameter, .. } => diameter / 2.0,
                };
                let (back, front) = match column {
                    Some(m) => (m.grid.x0, m.grid.x0 + m.grid.nx as f64 * m.grid.dx),
                    None => (-job.stock.length, 0.0),
                };
                let on_bar = |x: f64| x - tr <= front + 1e-9 && x + tr >= back - 1e-9;
                // (and said: the part where there is no bar)
                if let Some((lo, hi)) = mesh.bounds() {
                    let bar = if column.is_some() {
                        (-job.stock.length, 0.0)
                    } else {
                        (back, front)
                    };
                    if hi[0] > bar.1 + 1.0 || lo[0] < bar.0 - 1.0 {
                        let w = format!(
                            "the part lies outside the stock: X {:.1}..{:.1} against the bar's X {:.1}..{:.1}; nothing is cut where there is no bar; its Part has Center on axis and Flush to front",
                            lo[0], hi[0], bar.0, bar.1
                        );
                        if !b.warnings.contains(&w) {
                            b.warnings.push(w);
                        }
                    }
                }
                // (rest roughing in 3D: the stock's top round the axis at each X, a quarter mm
                // apart, from 36 angles)
                let rest_tops: Option<(f64, Vec<f64>)> = rest3d.as_ref().map(|dx| {
                    let (x0, x1) = (
                        o.x_range[0].min(o.x_range[1]) - tr,
                        o.x_range[0].max(o.x_range[1]) + tr,
                    );
                    let n = ((x1 - x0) / 0.25).ceil() as usize + 1;
                    let tops = (0..n)
                        .map(|i| {
                            let x = x0 + i as f64 * 0.25;
                            (0..36)
                                .map(|k| {
                                    dx.radial_top(x, k as f64 * 10.0, job.stock.radius + 0.5, 0.1)
                                })
                                .fold(0.0f64, f64::max)
                        })
                        .collect();
                    (x0, tops)
                });
                let stock_at = |x: f64| {
                    if !on_bar(x) {
                        return 0.0;
                    }
                    if let Some((x0, t)) = &rest_tops {
                        let i = (((x - x0) / 0.25).round().max(0.0) as usize).min(t.len() - 1);
                        let j = (i + 1).min(t.len() - 1);
                        return t[i].max(t[j]);
                    }
                    match (column, &tops) {
                        (Some(m), Some(t)) => {
                            let i = ((x - m.grid.x0) / m.grid.dx - 0.5).round();
                            if i < 0.0 || i as usize >= m.grid.nx {
                                job.stock.radius
                            } else {
                                t[i as usize]
                            }
                        }
                        _ => job.stock.radius,
                    }
                };
                // (a carried piece is not round about the axis: where round a level there is
                // none of it, under the tool's width) the highest of it in a window
                // (rest roughing in 3D: at the stock's radius, its top along the ray the tool
                // meets at that angle; lower, whether the tool's end there takes any of it)
                let near_tool = tool_solid(job, o.tool).map(|t| dexel::ToolSolid {
                    height: 2.0 * t.radius + o.step_down,
                    ..t
                });
                let stock_near = |x0: f64, x1: f64, a0: f64, a1: f64, r: f64| -> f64 {
                    if let (Some(dx), Some(t)) = (rest3d.as_ref(), near_tool) {
                        if r >= job.stock.radius - 1e-9 {
                            return dx.radial_top(x0, 90.0 - a0, job.stock.radius + 0.5, 0.1);
                        }
                        // (a negative radius: whether more than a trace stands there, worth a
                        // rest pass: 0.5 mm3, a fifth of a mm round a third of the ball)
                        let (r, least) = if r < 0.0 { (-r, 0.5) } else { (r, 0.05) };
                        let any = [(x0, a0), (x1, a1), (0.5 * (x0 + x1), 0.5 * (a0 + a1))]
                            .iter()
                            .any(|&(x, a)| {
                                let (tip, dir) = dexel::pose(x, 0.0, r, a);
                                // (more than the stock's sampling round the part)
                                dx.inside(&t, tip, dir) > least
                            });
                        return if any { r } else { 0.0 };
                    }
                    let Some(m) = column else {
                        return job.stock.radius;
                    };
                    let g = &m.grid;
                    let i0 = ((x0 - tr - g.x0) / g.dx - 0.5).floor().max(0.0) as usize;
                    let i1 =
                        (((x1 + tr - g.x0) / g.dx - 0.5).ceil().max(0.0) as usize).min(g.nx - 1);
                    if i0 > i1 || i0 >= g.nx {
                        return 0.0;
                    }
                    // the tool at A meets the part's angle 90 - A; its half width there
                    let half = if r <= tr {
                        90.0
                    } else {
                        (tr / r).asin().to_degrees()
                    };
                    let (p0, p1) = (90.0 - a1 - half, 90.0 - a0 + half);
                    let d = 360.0 / g.na as f64;
                    let (j0, j1) = ((p0 / d - 0.5).floor() as i64, (p1 / d - 0.5).ceil() as i64);
                    let span = (j1 - j0 + 1).min(g.na as i64);
                    let mut hi = 0.0f32;
                    for i in i0..=i1 {
                        let row = &m.r[i * g.na..(i + 1) * g.na];
                        for k in 0..span {
                            hi = hi.max(row[(j0 + k).rem_euclid(g.na as i64) as usize]);
                        }
                    }
                    hi as f64
                };
                // (a limit on the bite: the stock taken from as the passes are planned, to
                // say what each would take)
                // (by default 1.5 times the tool's normal bite: buried to its centre, a ball's
                // half disc, a flat's square of its radius by its width: the impeller's ball
                // 3.96, so 5.94 mm3/mm)
                let mut live = rest3d
                    .clone()
                    .and_then(|dx| Some((dx, tool_solid(job, o.tool)?)));
                let bite_limit = o.max_bite.unwrap_or_else(|| {
                    live.as_ref().map_or(f64::INFINITY, |(_, t)| {
                        let normal = if t.ball {
                            0.5 * std::f64::consts::PI * t.radius * t.radius
                        } else {
                            2.0 * t.radius * t.radius
                        };
                        1.5 * normal
                    })
                });
                let r_min = strategy::rotary_rough(
                    &mut b,
                    &dc,
                    job.stock.radius,
                    safe_r,
                    o,
                    Some(&stock_at as &dyn Fn(f64) -> f64),
                    (column.is_some() || rest3d.is_some())
                        .then_some(&stock_near as strategy::StockNear),
                    live.as_mut().map(|(stock, tool)| strategy::Bite {
                        stock,
                        tool: *tool,
                        limit: bite_limit,
                    }),
                )
                .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::Engrave(_) | Operation::Vcarve(_), shape) => {
                use strategy::engrave as eg;
                let err = |e: String| Error::Invalid(format!("program {}: {e}", prog.name));
                let top_of = |t: Option<f64>| match stock_box {
                    Some(sb) if three => eg::Surface::Flat {
                        top: t.unwrap_or(sb.max[2]),
                    },
                    _ => eg::Surface::Wrapped {
                        top: t.unwrap_or(job.stock.radius),
                    },
                };
                // wrapped cuts start on the axis centreline from the rotary state
                if !three && b.p3.is_some_and(|p| p[1] != 0.0) {
                    b.rapid3(None, None, Some(safe_r));
                    b.rapid3(None, Some(0.0), None);
                }
                match op {
                    Operation::Engrave(o) => {
                        eg::engrave(&mut b, top_of(o.top), safe_r, o).map_err(err)?
                    }
                    Operation::Vcarve(o) => {
                        let ToolShape::Vbit { angle, .. } = shape else {
                            unreachable!("checked above")
                        };
                        eg::vcarve(&mut b, top_of(o.top), safe_r, (angle / 2.0).to_radians(), o)
                            .map_err(err)?
                    }
                    _ => unreachable!(),
                }
                if three {
                    b.x = None;
                } else {
                    b.p3 = None;
                }
            }
            // (Mar's impeller) roughing on the 4th axis at several angles in turn: at each, the
            // part turned to it, the stock as the angles before left it, cleared adaptively
            // with the tool upright (Y used), then on to the next
            (Operation::Rough3d(o), _)
                if !three && (!o.indexes.is_empty() || o.index_count > 0) =>
            {
                let sb = *stock_box.as_ref().expect("set above");
                let err = |e: String| Error::Invalid(format!("program {}: {e}", prog.name));
                let c = cutter.expect("set above");
                let mesh = ctx.surface(&prog.name)?.clone();
                let angles = if o.indexes.is_empty() {
                    gap_angles(&mesh, c, o.index_count, sb.min[0], sb.max[0])
                } else {
                    o.indexes.clone()
                };
                let cg = c.grown(o.leave_xy).ok_or_else(|| {
                    err(format!(
                        "stock to leave on the walls ({} mm) takes a flat or ball tool, no thinner than it",
                        o.leave_xy
                    ))
                })?;
                // (the bar as cut so far: the programs before simulated once, then each
                // angle's moves on top as they come)
                let mut bar = indexed_bar(job, &ctx.prior);
                let mut stock3d = indexed_dexel(job, &ctx.prior);
                let mut applied = 0;
                let n_angles = angles.len();
                for (k_angle, index) in angles.into_iter().enumerate() {
                    // (and within it, angle k of m)
                    phase(&format!(
                        "planning {} ({}) [op {op_at}/{op_total}] [angle {}/{n_angles}]",
                        op_name(op),
                        prog.name,
                        k_angle + 1
                    ));
                    // (each angle its own share of the work: one is a 3-axis roughing)
                    budget::start(budget::OP_LIMIT);
                    let turned = mesh.rotated_x(index);
                    let dc = strategy::three_axis::DropCutter3::new(&turned, cg);
                    let cell = (c.radius() / 4.0).clamp(0.1, 0.5);
                    let (hm, bots) = match stock3d.as_mut() {
                        Some(dx) => {
                            indexed_stock_dexel(job, dx, &mut applied, &b.moves, &sb, cell, index)
                        }
                        None => {
                            indexed_stock(job, &mut bar, &mut applied, &b.moves, &sb, cell, index)
                                .map_err(err)?
                        }
                    };
                    let bottom = |x: f64, y: f64| {
                        let i = (((x - hm.x0) / hm.cell).max(0.0) as usize).min(hm.nx - 1);
                        let j = (((y - hm.y0) / hm.cell).max(0.0) as usize).min(hm.ny - 1);
                        bots[j * hm.nx + i]
                    };
                    let indexed_from = b.moves.len();
                    let indexed_start = [b.x.unwrap_or(sb.min[0]), 0.0, safe_r];
                    b.rapid_r(safe_r);
                    b.rapid(None, None, Some(index));
                    b.p3 = Some(indexed_start);
                    let mut oi = o.clone();
                    oi.index_a = index;
                    oi.pattern = "adaptive".into();
                    // (Mar's impeller on a 24 mm bar, roughed at each angle to Z 0 with a
                    // 3.175 x 12 tool: its holder, 17 mm up the tool, went 6.7 mm into the bar
                    // round the pocket, 6845 moves; its shank, deeper than the flutes, rubbing
                    // the pocket's walls) no deeper than the flutes reach (and the holder
                    // clears) from the highest stock left at this angle, by twice the near-miss
                    // margin (no near miss either); what is under that is left for the
                    // operations after, and said
                    if let Some(body) = job.tool(o.tool).map(collision::ToolBody::of) {
                        let top =
                            hm.z.iter()
                                .copied()
                                .filter(|z| z.is_finite())
                                .fold(f64::NEG_INFINITY, f64::max);
                        let lowest =
                            top - body.stickout.min(body.flute_length) + 2.0 * collision::NEAR_MISS;
                        if top.is_finite() && oi.floor < lowest {
                            oi.floor = lowest;
                            let what = if body.flute_length < body.stickout {
                                format!(
                                    "the shank, above T{}'s {:.1} mm of flutes,",
                                    o.tool, body.flute_length
                                )
                            } else {
                                format!("the holder, {:.1} mm up T{},", body.stickout, o.tool)
                            };
                            b.warnings.push(format!(
                                "indexed roughing at A {index:.1}: no deeper than Z {lowest:.2} (asked {:.2}): {what} would rub the walls of the cut below that; the rest is left for the operations after",
                                o.floor
                            ));
                        }
                    }
                    strategy::three_axis::adaptive_rough(
                        &mut b,
                        &dc,
                        &|x, y| hm.at(x, y),
                        &bottom,
                        &sb,
                        bar_x,
                        safe_r,
                        &oi,
                    )
                    .map_err(err)?;
                    if budget::over() {
                        break;
                    }
                    b.rapid3(None, None, Some(safe_r));
                    b.rapid3(None, Some(0.0), None);
                    b.x = b.p3.map(|p| p[0]);
                    b.r = Some(safe_r);
                    let tail = b.moves.split_off(indexed_from);
                    b.moves.extend(general_moves(tail, indexed_start, index));
                }
            }
            (Operation::Custom(o), _) => {
                let err = |e: String| Error::Invalid(format!("program {}: {e}", prog.name));
                let own = strategy::custom::plan(&mut b, o, three, safe_r).map_err(err)?;
                // (verbatim: its own lines posted for its moves; see post)
                if let Some(lines) = own {
                    if let Some(Move::Operation { verbatim, .. }) = b.moves[..op_start]
                        .iter_mut()
                        .rev()
                        .find(|m| matches!(m, Move::Operation { .. }))
                    {
                        *verbatim = Some(Box::new(lines));
                    }
                }
                if three {
                    b.x = None;
                } else {
                    b.p3 = None;
                }
            }
            (op3, shape) if is_three_axis(op3) => {
                let sb = stock_box.as_ref().expect("set above");
                let index = op3.index_a().unwrap_or(0.0);
                let indexed_from = b.moves.len();
                let indexed_start = [b.x.unwrap_or(sb.min[0]), 0.0, safe_r];
                if !three {
                    // indexed 3+1: up to the safe radius, turn A, then cut in X/Y/Z
                    b.rapid_r(safe_r);
                    b.rapid(None, None, Some(index));
                    b.p3 = Some(indexed_start);
                }
                let d = match shape {
                    ToolShape::Flat { diameter } | ToolShape::Ball { diameter } => diameter,
                    ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0),
                    ToolShape::TaperedBall { tip_diameter, .. } => tip_diameter,
                };
                let err = |e: String| Error::Invalid(format!("program {}: {e}", prog.name));
                use strategy::three_axis as t3;
                // (in-process stock: planned once, then again from the material left in the
                // way of its moves, when they start above it)
                let start = (b.moves.len(), b.warnings.len(), b.x, b.r, b.a, b.p3);
                let mut material: Option<f64> = None;
                loop {
                    let guard = t3::MaterialGuard::set(material);
                    // (Mar) the stock it leaves: its floors and its walls
                    let left = leave_of(op3, sb).map_err(err)?;
                    let op3 = left.0.as_ref().unwrap_or(op3);
                    let _walls = t3::LeaveGuard::set(left.1);
                    match op3 {
                        // several picked faces / pockets / outlines: each in turn, nearest next
                        Operation::Face(o) if !o.regions.is_empty() => {
                            for i in job::region_order(&o.regions, [sb.min[0], sb.min[1]]) {
                                let r = &o.regions[i];
                                let mut one = o.clone();
                                one.regions.clear();
                                one.paths = r.paths.clone();
                                one.depth = r.depth;
                                t3::face(&mut b, d, sb, safe_r, &one).map_err(err)?
                            }
                        }
                        Operation::Pocket(o) if !o.regions.is_empty() => {
                            for i in job::region_order(&o.regions, [sb.min[0], sb.min[1]]) {
                                let r = &o.regions[i];
                                let mut one = o.clone();
                                one.regions.clear();
                                one.paths = r.paths.clone();
                                one.top = r.top.or(o.top);
                                one.depth = r.depth;
                                t3::pocket(&mut b, d, sb, safe_r, &one).map_err(err)?
                            }
                        }
                        Operation::Contour(o) if !o.regions.is_empty() => {
                            for i in job::region_order(&o.regions, [sb.min[0], sb.min[1]]) {
                                let r = &o.regions[i];
                                let mut one = o.clone();
                                one.regions.clear();
                                one.path = r.path.clone();
                                if let Some(side) = &r.side {
                                    one.side = side.clone();
                                }
                                one.top = r.top.or(o.top);
                                one.depth = r.depth;
                                t3::contour(&mut b, d, sb, safe_r, &one).map_err(err)?
                            }
                        }
                        // (Mar) everything of the stock at its depth, round the part
                        Operation::Face(o) if o.clear_to_stock => {
                            let mesh = ctx.mesh.ok_or_else(|| err("facing to the stock's edges needs the part's model (to go round it)".to_string()))?;
                            let c = cutter_for(o.tool, shape).map_err(|e| err(e.to_string()))?;
                            let dc = t3::DropCutter3::new(mesh, c);
                            t3::face_clear(&mut b, d, sb, safe_r, o, &dc).map_err(err)?
                        }
                        Operation::Face(o) => t3::face(&mut b, d, sb, safe_r, o).map_err(err)?,
                        Operation::Pocket(o) => {
                            t3::pocket(&mut b, d, sb, safe_r, o).map_err(err)?
                        }
                        // (a canned cycle: on a vise, the drill along Z)
                        Operation::Drill(o) => t3::drill(
                            &mut b,
                            sb,
                            safe_r,
                            o,
                            o.canned && job.stock_box.is_some() && o.index_a == 0.0,
                        ),
                        Operation::Bore(o) => {
                            strategy::bore::bore(&mut b, d, sb, safe_r, o).map_err(err)?
                        }
                        Operation::Contour(o) => {
                            t3::contour(&mut b, d, sb, safe_r, o).map_err(err)?
                        }
                        Operation::Chamfer(o) => {
                            let half = match tool.shape {
                                ToolShape::Vbit { angle, .. } => (angle / 2.0).to_radians(),
                                _ => unreachable!("checked above"),
                            };
                            strategy::edges::chamfer(&mut b, half, sb, safe_r, o).map_err(err)?
                        }
                        Operation::ThreadMill(o) => {
                            // the thread mill's radius at its tip: a single-form one's diameter
                            let r = match tool.shape {
                                ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0) / 2.0,
                                ToolShape::Flat { diameter } => diameter / 2.0,
                                _ => unreachable!("checked above"),
                            };
                            if r <= 0.0 {
                                return Err(err(
                                    "thread mill: the tool needs a diameter (its cutting width)"
                                        .to_string(),
                                ));
                            }
                            // its helices written as G2/G3 arcs on a vise
                            if let Some(Move::Operation { arcs, .. }) = b
                                .moves
                                .iter_mut()
                                .rev()
                                .find(|m| matches!(m, Move::Operation { .. }))
                            {
                                *arcs = Some(0.005);
                            }
                            strategy::edges::thread_mill(&mut b, r, sb, safe_r, o).map_err(err)?
                        }
                        Operation::Raster3d(o)
                        | Operation::Rough3d(o)
                        | Operation::Waterline(o) => {
                            let adaptive = o.pattern == "adaptive";
                            if adaptive && !matches!(op, Operation::Rough3d(_)) {
                                return Err(err(
                                    "the adaptive pattern is for 3D roughing only".to_string()
                                ));
                            }
                            if adaptive && !three {
                                return Err(err(
                                    "adaptive roughing needs a 3-axis vise setup".to_string()
                                ));
                            }
                            // rest roughing: only where the moves before it left material (and
                            // in a setup whose stock the one before left: always)
                            let carried_rough = three
                                && job.carried.is_some()
                                && matches!(op, Operation::Rough3d(_));
                            let rest = if (o.rest || carried_rough) && !adaptive {
                                if !matches!(op, Operation::Rough3d(_)) {
                                    return Err(err("rest is for 3D roughing only".to_string()));
                                }
                                if !three {
                                    return Err(err(
                                        "rest roughing needs a 3-axis vise setup".to_string()
                                    ));
                                }
                                let c = cutter.expect("set above");
                                Some(rest_map(job, &ctx.prior, &b.moves, sb, &c).map_err(err)?)
                            } else {
                                None
                            };
                            let mesh: &Mesh = if three {
                                ctx.mesh.ok_or_else(|| {
                                    Error::Invalid(format!(
                                        "program {}: raster3d needs part.mesh",
                                        prog.name
                                    ))
                                })?
                            } else {
                                ctx.surface(&prog.name)?
                            };
                            let turned = (index != 0.0).then(|| mesh.rotated_x(index));
                            let c = cutter.expect("set above");
                            // (the walls' stock to leave: the tool wider by it)
                            let c = c.grown(o.leave_xy).ok_or_else(|| {
                                err(format!(
                                    "stock to leave on the walls ({} mm) takes a flat or ball tool, no thinner than it",
                                    o.leave_xy
                                ))
                            })?;
                            let dc = t3::DropCutter3::new(turned.as_ref().unwrap_or(mesh), c);
                            // rest finishing: the earlier finish's tool, to find where it could not reach
                            let bigger = if o.rest_tool > 0 && matches!(op, Operation::Raster3d(_))
                            {
                                let t = job.tool(o.rest_tool).ok_or_else(|| {
                                    err(format!(
                                        "rest_tool {}: the project has no such tool",
                                        o.rest_tool
                                    ))
                                })?;
                                let gc = cutter_for(o.rest_tool, t.shape)?;
                                let surface = turned.as_ref().unwrap_or(mesh);
                                let big_dc = t3::DropCutter3::new(surface, gc);
                                let point =
                                    t3::DropCutter3::new(surface, Cutter::Flat { radius: 1e-4 });
                                Some(t3::RestFinish::new(
                                    &big_dc,
                                    &point,
                                    o.x_range,
                                    o.y_range,
                                    gc.radius(),
                                    c.radius(),
                                    o.floor,
                                ))
                            } else {
                                None
                            };
                            if adaptive {
                                // the material's top: the stock, or what the moves before left
                                let c = cutter.expect("set above");
                                let hm = if o.rest || carried_rough {
                                    rest_heightmap(job, &ctx.prior, &b.moves, sb, &c)
                                        .map_err(err)?
                                } else {
                                    sim3::Heightmap3::of_job(
                                        job,
                                        sb,
                                        (c.radius() / 8.0).clamp(0.02, 0.25),
                                    )
                                };
                                // (a block carried over: its other side's cuts below)
                                let bots = job.carried.as_ref().map(|k| k.bottoms(&hm));
                                let bottom = |x: f64, y: f64| match &bots {
                                    Some(bs) => {
                                        let i = (((x - hm.x0) / hm.cell).max(0.0) as usize)
                                            .min(hm.nx - 1);
                                        let j = (((y - hm.y0) / hm.cell).max(0.0) as usize)
                                            .min(hm.ny - 1);
                                        bs[j * hm.nx + i]
                                    }
                                    None => f64::NEG_INFINITY,
                                };
                                t3::adaptive_rough(
                                    &mut b,
                                    &dc,
                                    &|x, y| hm.at(x, y),
                                    &bottom,
                                    sb,
                                    bar_x,
                                    safe_r,
                                    o,
                                )
                                .map_err(err)?
                            } else if matches!(op, Operation::Waterline(_)) {
                                t3::waterline(&mut b, &dc, sb, safe_r, o).map_err(err)?
                            } else if o.faces.is_empty() {
                                t3::raster3d_on(
                                    &mut b,
                                    &dc,
                                    None,
                                    rest.as_ref(),
                                    bigger.as_ref(),
                                    sb,
                                    safe_r,
                                    o,
                                    matches!(op, Operation::Rough3d(_)),
                                )
                                .map_err(err)?
                            } else {
                                let sub = face_subset(mesh, &o.faces).map_err(err)?;
                                let sub = if index != 0.0 {
                                    sub.rotated_x(index)
                                } else {
                                    sub
                                };
                                // the passes need cover only the faces, and the tool beside them
                                let mut o = o.clone();
                                if let Some((lo, hi)) = sub.bounds() {
                                    let r = c.radius();
                                    let clip = |range: [f64; 2], a: f64, b: f64| {
                                        let (l, h) =
                                            (range[0].min(range[1]), range[0].max(range[1]));
                                        let (l, h) = (l.max(a - r), h.min(b + r));
                                        if range[0] <= range[1] {
                                            [l, h]
                                        } else {
                                            [h, l]
                                        }
                                    };
                                    o.x_range = clip(o.x_range, lo[0], hi[0]);
                                    o.y_range = clip(o.y_range, lo[1], hi[1]);
                                }
                                let seld = t3::DropCutter3::new(&sub, c);
                                t3::raster3d_on(
                                    &mut b,
                                    &dc,
                                    Some(&seld),
                                    rest.as_ref(),
                                    bigger.as_ref(),
                                    sb,
                                    safe_r,
                                    &o,
                                    matches!(op, Operation::Rough3d(_)),
                                )
                                .map_err(err)?
                            }
                        }
                        _ => unreachable!(),
                    }
                    drop(guard);
                    if !aware || material.is_some() {
                        break;
                    }
                    let m = material_in_way(
                        job,
                        &ctx.prior,
                        &b.moves[..start.0],
                        &b.moves[start.0..],
                        sb,
                        d / 2.0,
                    )
                    .map_err(err)?;
                    match m {
                        Some(m) if highest_cut(&b.moves[start.0..]) >= m - 1e-6 => {
                            b.moves.truncate(start.0);
                            b.warnings.truncate(start.1);
                            (b.x, b.r, b.a, b.p3) = (start.2, start.3, start.4, start.5);
                            material = Some(m);
                        }
                        _ => break,
                    }
                }
                // (past the budget: failing, nothing to smooth)
                if budget::over() {
                    return Err(refuse(
                        ctx.cache.as_deref_mut(),
                        key,
                        too_fine(&prog.name, op),
                    ));
                }
                // smoothed in the machine frame, before any conversion to general moves
                if smooth_tol > 0.0 {
                    let mut tail = b.moves.split_off(indexed_from);
                    smoothing::smooth(&mut tail, smooth_tol, true);
                    b.moves.extend(tail);
                }
                if !three {
                    // back on the axis centreline before any rotary move
                    b.rapid3(None, None, Some(safe_r));
                    b.rapid3(None, Some(0.0), None);
                    b.x = b.p3.map(|p| p[0]);
                    b.r = Some(safe_r);
                    // the strategy worked in the machine frame at A = index: hand the post
                    // general moves (tip and tool direction in the part frame) to solve
                    let tail = b.moves.split_off(indexed_from);
                    b.moves.extend(general_moves(tail, indexed_start, index));
                }
            }
            _ => unreachable!("tool shape validated above"),
        }
        if budget::over() {
            return Err(refuse(
                ctx.cache.as_deref_mut(),
                key,
                too_fine(&prog.name, op),
            ));
        }
        // a number that is not one (a strategy dividing by a zero radius, say) is stopped
        // here, with the operation named, before anything downstream sizes work by it
        if let Some(m) = b.moves[op_start..].iter().find(|m| !m.is_finite()) {
            return Err(Error::Invalid(format!(
                "program {}: {} made a move that is not a number ({m:?}): check its parameters",
                prog.name,
                op_name(op)
            )));
        }
        // (Mar's knob3: an outside contour 5 mm out and 10 mm down cut 8.9 mm into the knob's
        // wider body below its picked edge, said only by the simulation's numbers) a vise's
        // 2.5D cuts checked against the part: one into it is the operation's error, with
        // where and how deep
        if three
            && op.index_a().unwrap_or(0.0) == 0.0
            && matches!(
                op,
                Operation::Contour(_) | Operation::Pocket(_) | Operation::Face(_)
            )
        {
            if let (Some(mesh), Ok(c)) = (ctx.mesh, cutter_for(number, tool.shape)) {
                if let Some((x, y, z, top)) = gouge_of(&b.moves[op_start..], mesh, c) {
                    let hint = match op {
                        Operation::Contour(_) => {
                            "a smaller boundary offset, or less depth: the part is wider lower down"
                        }
                        _ => "less depth, or a smaller region",
                    };
                    // (planned on, as asked: it may be meant, a model short of the finished
                    // part; the app shows it as an error)
                    // (its number in the plan: the app names it by its id in the project)
                    let n = b.moves.iter().rev().find_map(|m| match m {
                        Move::Operation { id, .. } => Some(*id),
                        _ => None,
                    });
                    b.warnings.push(format!(
                        "GOUGE: {} [op#{}] cuts {:.2} mm into the part at X{x:.2} Y{y:.2}, down to Z{z:.2} where the part is at Z{top:.2}: {hint}",
                        op_name(op),
                        n.map_or("?".to_string(), |n| n.to_string()),
                        top - z
                    ));
                }
            }
        }
        // rotary operations and engravings: smoothed here (3-axis ones above)
        if smooth_tol > 0.0 && !is_three_axis(op) {
            let mut tail = b.moves.split_off(op_start);
            smoothing::smooth_from(&mut tail, smooth_tol, flat, rot_before);
            b.moves.extend(tail);
        }
        // (Mar's impeller-6: the parting after a flowline began with 58 turns of A in the air,
        // 18 min) a pose solves its own A, as the machine turns to it: the builder's A after
        // poses is where the machine is, not where it was before them
        if b.moves[op_start..]
            .iter()
            .any(|m| matches!(m, Move::Pose { .. }))
        {
            let mut a = a_before;
            for m in &b.moves[op_start..] {
                match *m {
                    Move::Rapid { a: Some(v), .. } | Move::Cut { a: v, .. } => a = v,
                    Move::Pose { axis, .. } => {
                        if let Ok(na) = kinematics::solve_a(axis, a) {
                            a = na;
                        }
                    }
                    _ => {}
                }
            }
            b.a = a;
        }
        if let (Some(cache), Some(key)) = (ctx.cache.as_deref_mut(), key) {
            if cache.map.len() >= PlanCache::MAX_OPS {
                cache.map.clear();
            }
            cache.map.insert(
                key,
                CachedOp {
                    moves: b.moves[cache_from..].to_vec(),
                    exit: (b.x, b.r, b.a, b.p3),
                    warnings: b.warnings[warn_from..].to_vec(),
                    refused: None,
                },
            );
        }
    }
    Ok(Toolpath {
        name: prog.name.clone(),
        title: prog.title.clone(),
        tool: first_tool,
        // (and what the operations warned about)
        notes: prog
            .notes
            .iter()
            .cloned()
            .chain(b.warnings.iter().map(|w| format!("(Warning: {w})")))
            .collect(),
        reset_a: prog.reset_a,
        safe_r,
        moves: b.moves,
        post: job.post.clone(),
        origin_comment: three.then(|| {
            "Zero: the setup's work coordinates, Z up (Z0 = top of the stock)".to_string()
        }),
        plunge_limit: prog
            .operations
            .iter()
            .any(|o| is_three_axis(o) || is_surface_op(o))
            .then(|| {
                prog.operations
                    .iter()
                    .filter_map(|op| match op {
                        Operation::Face(o) => Some(o.plunge_feed),
                        Operation::Pocket(o) => Some(o.plunge_feed),
                        Operation::Drill(o) => Some(o.feed),
                        Operation::Bore(o) => Some(o.plunge_feed),
                        Operation::Contour(o) => Some(o.plunge_feed),
                        Operation::Chamfer(o) => Some(o.plunge_feed),
                        Operation::ThreadMill(o) => Some(o.plunge_feed),
                        Operation::Raster3d(o)
                        | Operation::Rough3d(o)
                        | Operation::Waterline(o) => Some(o.plunge_feed),
                        Operation::Engrave(o) => Some(o.plunge_feed),
                        Operation::Vcarve(o) => Some(o.plunge_feed),
                        Operation::Custom(o) => Some(o.plunge_feed),
                        _ => None,
                    })
                    .fold(f64::INFINITY, f64::min)
            })
            .filter(|v| v.is_finite()),
        ramp_angle: prog
            .operations
            .iter()
            .filter_map(Operation::ramp_angle)
            .reduce(f64::max)
            .filter(|&r| r > 0.0),
    })
}

/// Plan every program of the job. Jobs with `part.mesh` need [`plan_with_mesh`].
pub fn plan(job: &Job) -> Result<Vec<Toolpath>, Error> {
    plan_with_mesh(job, None)
}

/// Plan every program, finishing against `mesh` (already oriented, see [`load_mesh`]).
pub fn plan_with_mesh(job: &Job, mesh: Option<&Mesh>) -> Result<Vec<Toolpath>, Error> {
    plan_cached(job, mesh, None)
}

/// [`plan_with_mesh`], reusing the operations `cache` has planned before (and keeping the new
/// ones): the same toolpaths, only the operations whose inputs changed are planned.
pub fn plan_with_mesh_cached(
    job: &Job,
    mesh: Option<&Mesh>,
    cache: &mut PlanCache,
) -> Result<Vec<Toolpath>, Error> {
    plan_cached(job, mesh, Some(cache))
}

fn plan_cached(
    job: &Job,
    mesh: Option<&Mesh>,
    cache: Option<&mut PlanCache>,
) -> Result<Vec<Toolpath>, Error> {
    if job.schema_version != job::SCHEMA_VERSION {
        return Err(Error::UnsupportedSchema(job.schema_version.clone()));
    }
    let mut ctx = Ctx {
        job,
        profile: job
            .part
            .profile
            .as_ref()
            .map(|p| RevolvedProfile::new(p, job.stock.radius)),
        mesh,
        revolved: None,
        job_key: if cache.is_some() {
            job_key(job, mesh)
        } else {
            0
        },
        cache,
        prior: Vec::new(),
    };
    // (a rest roughing works on what the programs before it left)
    let rest = job.programs.iter().any(|p| {
        p.operations.iter().any(|o| match o {
            Operation::Rough3d(r) => r.rest || job.carried.is_some() || job.stock_box.is_none(),
            // (rotary rest roughing; and indexed 3+1 roughing on a bar, whose angles see
            // the bar as the programs before left it)
            Operation::RotaryRough(r) => r.rest,
            // (a knurl comes in from the bar as the programs before left it)
            Operation::HelicalKnurl(_) => true,
            _ => false,
        })
    });
    let mut next_op = 0;
    let mut tps: Vec<Toolpath> = Vec::new();
    // (a block or a bar in the vise: the stock as the programs before left it, for the rapid
    // descents of each next one)
    let mut block = job
        .stock_box
        .map(|sb| sim3::Heightmap3::of_job(job, &sb, GUARD_CELL));
    for p in &job.programs {
        let mut tp = plan_program(&mut ctx, p, &mut next_op)?;
        if let Some(hm) = block.as_mut() {
            guard_descents(job, hm, &mut tp);
        }
        if rest {
            ctx.prior.push(tp.moves.clone());
        }
        tps.push(tp);
    }
    cut_off_warnings(job, &mut tps);
    Ok(tps)
}

/// The cells of the stock the rapid descents of a 3-axis job are checked on (mm).
const GUARD_CELL: f64 = 0.25;

/// How far above the stock a rapid descent into it stops instead (mm).
const DESCENT_CLEAR: f64 = 0.5;

/// How far into the stock a rapid descent's end counts as into it (mm): less is the stock's
/// own height, as a faced level.
const INTO: f64 = 0.01;

/// (the carry-through walkthrough's pocket: its picked face 0.5 mm under the stock's top, the
/// top never faced; it came down by rapid to 0.125 mm above the face, 0.375 mm into the
/// stock) every rapid descent of a 3-axis program (straight down) that would end more than
/// [`INTO`] into the stock as the moves before left it (`hm`, the programs before;
/// this one's moves are cut into it as they go): stopped [`DESCENT_CLEAR`] above it, and
/// fed the rest of the way down at the program's plunge feed (else a third of the next cut's).
/// Said in a warning.
fn guard_descents(job: &Job, hm: &mut sim3::Heightmap3, tp: &mut Toolpath) {
    let cutter_of = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let segs = sim3::segments_of3(&tp.moves, 0);
    // the stock's highest under the tool at (x, y): the cells within its radius less a cell
    // (a pocket's or a contour's walls, a tool's radius off, are beside it, not under it)
    let top_under = |hm: &sim3::Heightmap3, x: f64, y: f64, r: f64| {
        let reach = (r - hm.cell).max(0.0);
        let n = (reach / hm.cell).ceil() as i64;
        let mut top = f64::NEG_INFINITY;
        for i in -n..=n {
            for j in -n..=n {
                let (dx, dy) = (i as f64 * hm.cell, j as f64 * hm.cell);
                if dx * dx + dy * dy <= reach * reach {
                    top = top.max(hm.at(x + dx, y + dy));
                }
            }
        }
        top
    };
    let mut fixes: Vec<(usize, f64, f64)> = Vec::new();
    for (k, seg) in segs.iter().enumerate() {
        let cut = seg.tool.and_then(cutter_of);
        if seg.rapid {
            let (Some(from), Some(c)) = (seg.from, cut.as_ref()) else {
                continue;
            };
            let down = seg.to[2] < from[2] - 1e-9
                && (seg.to[0] - from[0]).abs() < 1e-9
                && (seg.to[1] - from[1]).abs() < 1e-9;
            if !down {
                continue;
            }
            let top = top_under(hm, seg.to[0], seg.to[1], c.radius());
            // (into it: one that ends on it, as at a faced height, is left as planned)
            if seg.to[2] < top - INTO {
                let stop = (top + DESCENT_CLEAR).min(from[2]);
                let feed = segs[k + 1..]
                    .iter()
                    .find(|s| !s.rapid)
                    .and_then(|s| match tp.moves[s.move_index] {
                        Move::Line3 { feed, .. } => Some(feed),
                        _ => None,
                    })
                    .unwrap_or(150.0);
                // (at the program's plunge feed, what the verifier holds its descents to;
                // else a third of the cut's)
                let plunge = tp.plunge_limit.unwrap_or((feed / 3.0).max(30.0)).min(feed);
                fixes.push((seg.move_index, stop, plunge));
            }
            continue;
        }
        sim3::apply_segment3(hm, seg, cut.as_ref(), hm.cell);
    }
    if fixes.is_empty() {
        return;
    }
    for &(i, stop, feed) in fixes.iter().rev() {
        let Move::Rapid3 { x, y, z: Some(z) } = tp.moves[i] else {
            continue;
        };
        let (Some(px), Some(py)) = (
            x.or_else(|| at_xy(&tp.moves[..i]).0),
            y.or_else(|| at_xy(&tp.moves[..i]).1),
        ) else {
            continue;
        };
        let feed_down = Move::Line3 {
            x: px,
            y: py,
            z,
            axis: [0.0, 0.0, 1.0],
            feed,
        };
        if stop > z + 1e-9 {
            tp.moves[i] = Move::Rapid3 {
                x,
                y,
                z: Some(stop),
            };
            tp.moves.insert(i + 1, feed_down);
        } else {
            tp.moves[i] = feed_down;
        }
    }
    tp.notes.push(format!(
        "(Warning: {} rapid descent(s) would have come down into the stock, which stands above where the operation starts there (not faced or cut first): they stop {DESCENT_CLEAR} mm above it and feed the rest of the way)",
        fixes.len()
    ));
}

/// The X and Y the moves end at.
fn at_xy(moves: &[Move]) -> (Option<f64>, Option<f64>) {
    let (mut x, mut y) = (None, None);
    for m in moves {
        match *m {
            Move::Rapid3 { x: a, y: b, .. } => {
                x = a.or(x);
                y = b.or(y);
            }
            Move::Line3 { x: a, y: b, .. } => {
                x = Some(a);
                y = Some(b);
            }
            _ => {}
        }
    }
    (x, y)
}

/// A full cut-off ([`job::Parting`] through to the axis) leaves nothing on the bar: operations
/// after it (in its program or a later one) would cut air, or the remainder. Its program says
/// so in a warning.
fn cut_off_warnings(job: &Job, tps: &mut [Toolpath]) {
    let ops: Vec<(usize, &Operation)> = job
        .programs
        .iter()
        .enumerate()
        .flat_map(|(p, prog)| prog.operations.iter().map(move |o| (p, o)))
        .collect();
    let Some(k) = ops
        .iter()
        .position(|(_, o)| matches!(o, Operation::Parting(c) if c.stop_radius <= 0.0))
    else {
        return;
    };
    let after = ops.len() - k - 1;
    if after == 0 {
        return;
    }
    let (p, Operation::Parting(c)) = ops[k] else {
        return;
    };
    if let Some(tp) = tps.get_mut(p) {
        tp.notes.push(format!(
            "(Warning: {after} operation(s) come after the cut-off at X{:.3}: the part is off the bar by then. Put the cut-off last.)",
            c.x
        ));
    }
}

/// Load and orient the job's `part.mesh` (path relative to `base_dir`), if any.
pub fn load_mesh(job: &Job, base_dir: &std::path::Path) -> Result<Option<Mesh>, Error> {
    let Some(src) = &job.part.mesh else {
        return Ok(None);
    };
    let mesh = src
        .load(base_dir)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(Some(mesh))
}

/// Post one toolpath to G-code for `machine`.
pub fn post(tp: &Toolpath, machine: &Machine) -> String {
    post::post(tp, machine)
}

/// Check G-code text against the limits.
pub fn verify_gcode(text: &str, limits: &Limits) -> Vec<Issue> {
    verify::verify(text, limits)
}

/// Limits matching a planned toolpath: its safe radius and slowest plunge feed.
pub fn limits_for(tp: &Toolpath, machine: &Machine) -> Limits {
    let plunge = tp
        .moves
        .iter()
        .filter_map(|m| match m {
            Move::Plunge { feed, .. } => Some(*feed),
            Move::Cut { plunge_cap, .. } => Some(*plunge_cap),
            _ => None,
        })
        .fold(tp.plunge_limit.unwrap_or(f64::INFINITY), f64::min);
    Limits {
        plunge_feed: if plunge.is_finite() { plunge } else { 60.0 },
        // (its G-code's numbers: from the work zero, see post::shift_work_zero)
        safe_r: tp.safe_r - tp.post.as_ref().map_or(0.0, |p| p.origin[2]),
        feed_mode: machine.feed_mode,
        surface_radius_pad: machine.surface_radius_pad,
        ramp_angle: tp.ramp_angle.unwrap_or(0.0),
    }
}

/// Gouge check of one program against the finished part.
#[derive(Debug, Clone, PartialEq)]
pub struct GougeReport {
    pub program: String,
    /// Cut moves checked (moves of tools used by turning, roughing and finishing operations;
    /// knurling and chamfering cut into the part on purpose and are skipped).
    pub moves_checked: usize,
    /// Deepest cut into the part: the smaller of the vertical depth and the axial distance
    /// to where the tool would clear the part (mm).
    pub max_depth: f64,
    /// Where: `(x, a)`.
    pub at: Option<(f64, f64)>,
}

/// Cuts that reach the vise's jaws: a tool below the jaws' top with its edge past the stock's
/// Y faces, where the jaws stand. One warning per operation, with where.
pub fn jaw_check(job: &Job, tps: &[Toolpath]) -> Vec<(String, String)> {
    let (Some(sb), Some(grip)) = (job.stock_box, job.jaws) else {
        return Vec::new();
    };
    let top = sb.min[2] + grip;
    let mut out = Vec::new();
    for tp in tps {
        let mut tool = None;
        let mut op = String::new();
        let mut seen = std::collections::HashSet::new();
        for m in &tp.moves {
            match m {
                Move::ToolChange { tool: t, .. } => tool = Some(*t),
                Move::Operation { kind, id, .. } => op = format!("{kind} (#{})", id + 1),
                Move::Line3 { x, y, z, .. } if *z < top - 1e-6 => {
                    let r = tool
                        .and_then(|t| job.tool(t))
                        .and_then(|d| cutter_for(0, d.shape).ok())
                        .map_or(0.0, |c| c.radius());
                    if *x + r < sb.min[0] || *x - r > sb.max[0] {
                        continue;
                    }
                    let into_jaw = *y - r < sb.min[1] - 1e-6 || *y + r > sb.max[1] + 1e-6;
                    if into_jaw && seen.insert(op.clone()) {
                        out.push((
                            tp.name.clone(),
                            format!("{op} cuts into the vise jaws: Z{z:.3} below their top (Z{top:.3}) at X{x:.3} Y{y:.3}"),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Check every program against the part surface (the mesh if given, else the revolved
/// profile) with a drop-cutter for each tool.
pub fn gouge_check(
    job: &Job,
    tps: &[Toolpath],
    mesh: Option<&Mesh>,
) -> Result<Vec<GougeReport>, Error> {
    phase("gouge check");
    let revolved;
    let surface = match (mesh, &job.part.profile) {
        (Some(m), _) => m,
        (None, Some(p)) => {
            revolved = Mesh::revolve(p, PROFILE_SEGMENTS);
            &revolved
        }
        (None, None) => return Ok(Vec::new()),
    };
    let mut cutters: std::collections::HashMap<u32, DropCutter> = Default::default();
    let mut out = Vec::new();
    for (tp, prog) in tps.iter().zip(&job.programs) {
        let checked: Vec<u32> = prog
            .operations
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    Operation::RotaryTurning(_)
                        | Operation::RotaryFinish(_)
                        | Operation::RotaryParallel(_)
                        | Operation::RotaryFlowline(_)
                        | Operation::RotaryWall(_)
                        | Operation::RotaryRough(_)
                )
            })
            .map(Operation::tool)
            .collect();
        let mut rep = GougeReport {
            program: tp.name.clone(),
            moves_checked: 0,
            max_depth: 0.0,
            at: None,
        };
        // (Mar's impeller) indexed 3+1 roughing on the 4th axis: its moves checked too, the
        // part turned to each angle and dropped on from above
        let indexed: Vec<u32> = prog
            .operations
            .iter()
            .filter(|o| matches!(o, Operation::Rough3d(r) if !r.indexes.is_empty() || r.index_count > 0))
            .map(Operation::tool)
            .collect();
        let mut turned: Vec<(f64, strategy::three_axis::DropCutter3)> = Vec::new();
        let mut itool: Option<(u32, Cutter)> = None;
        let (mut x, mut r, mut a) = (None::<f64>, None::<f64>, 0.0);
        let mut tool: Option<u32> = None;
        // (rotary wall finishing with the tool upright off the axis: its poses dropped on
        // along the tool's own line)
        let reach = prog
            .operations
            .iter()
            .filter_map(|o| match o {
                Operation::RotaryWall(w) => Some(w.y_offset.abs()),
                _ => None,
            })
            .fold(0.0f64, f64::max);
        let mut offset_dc: std::collections::HashMap<u32, DropCutter> = Default::default();
        for m in &tp.moves {
            if let (Move::Pose { tip, axis, .. }, Some(t), true) = (m, tool, itool.is_none()) {
                if reach > 0.0 {
                    if let Ok(na) = kinematics::solve_a(*axis, a) {
                        a = na;
                        if let std::collections::hash_map::Entry::Vacant(e) = offset_dc.entry(t) {
                            if let Some(c) = job.tool(t).and_then(|d| cutter_for(t, d.shape).ok()) {
                                e.insert(DropCutter::reaching(surface, c, reach));
                            }
                        }
                        if let Some(dc) = offset_dc.get(&t) {
                            let q = kinematics::to_machine(*tip, na);
                            rep.moves_checked += 1;
                            let deficit = dc.radius_at_offset(q[0], na, q[1], 0.0) - q[2];
                            if deficit > rep.max_depth {
                                rep.max_depth = deficit;
                                rep.at = Some((q[0], na));
                            }
                        }
                    }
                    continue;
                }
            }
            if let Move::Pose { tip, axis, .. } = *m {
                if let (Some((_, c)), Ok(na)) = (itool, kinematics::solve_a(axis, a)) {
                    a = na;
                    let key = (na * 1000.0).round() / 1000.0;
                    if !turned.iter().any(|(k, _)| *k == key) {
                        turned.push((
                            key,
                            strategy::three_axis::DropCutter3::new(&surface.rotated_x(key), c),
                        ));
                    }
                    let dc = &turned.iter().find(|(k, _)| *k == key).expect("made").1;
                    let q = kinematics::to_machine(tip, na);
                    rep.moves_checked += 1;
                    let deficit = dc.z_at(q[0], q[1], f64::NEG_INFINITY) - q[2];
                    if deficit > rep.max_depth {
                        rep.max_depth = deficit;
                        rep.at = Some((q[0], na));
                    }
                }
                continue;
            }
            match *m {
                Move::ToolChange { tool: t, .. } => {
                    itool = indexed
                        .contains(&t)
                        .then(|| {
                            job.tool(t)
                                .and_then(|d| cutter_for(t, d.shape).ok())
                                .map(|c| (t, c))
                        })
                        .flatten();
                    tool = checked.contains(&t).then_some(t);
                    if let (Some(t), Some(def)) = (tool, job.tool(t)) {
                        if let (Ok(c), std::collections::hash_map::Entry::Vacant(e)) =
                            (cutter_for(t, def.shape), cutters.entry(t))
                        {
                            e.insert(DropCutter::new(surface, c));
                        }
                    }
                }
                Move::Rapid {
                    x: nx,
                    r: nr,
                    a: na,
                } => {
                    x = nx.or(x);
                    r = nr.or(r);
                    a = na.unwrap_or(a);
                }
                Move::Plunge { r: nr, .. } => r = Some(nr),
                Move::Cut {
                    x: nx,
                    r: nr,
                    a: na,
                    ..
                } => {
                    if let (Some(dc), Some(x0), Some(r0)) =
                        (tool.and_then(|t| cutters.get(&t)), x, r)
                    {
                        rep.moves_checked += 1;
                        for k in 1..=4 {
                            let t = k as f64 / 4.0;
                            let (xi, ri, ai) =
                                (x0 + (nx - x0) * t, r0 + (nr - r0) * t, a + (na - a) * t);
                            let deficit = dc.radius_at(xi, ai, 0.0) - ri;
                            if deficit <= rep.max_depth {
                                continue;
                            }
                            // axial distance to where the tool clears the part (walls)
                            let mut depth = deficit;
                            for s in 1..=200 {
                                let d = s as f64 * 0.0005;
                                if dc.radius_at(xi - d, ai, 0.0) <= ri
                                    || dc.radius_at(xi + d, ai, 0.0) <= ri
                                {
                                    depth = depth.min(d);
                                    break;
                                }
                            }
                            if depth > rep.max_depth {
                                rep.max_depth = depth;
                                rep.at = Some((xi, ai));
                            }
                        }
                    }
                    x = Some(nx);
                    r = Some(nr);
                    a = na;
                }
                Move::Comment { .. }
                | Move::Operation { .. }
                | Move::Rapid3 { .. }
                | Move::Line3 { .. }
                | Move::YOffset { .. }
                | Move::Pose { .. } => {}
            }
        }
        out.push(rep);
    }
    Ok(out)
}

/// Operation tags (id + 1, as the simulations record them) of operations meant to cut
/// below the part model: those the registry marks as shaping the part (knurls, chamfers,
/// engraving, V-carving) and, on a rotary setup, indexed 3-axis features.
pub fn intended_tags(job: &Job, tps: &[Toolpath]) -> std::collections::HashSet<u16> {
    let mut out = std::collections::HashSet::new();
    for tp in tps {
        for m in &tp.moves {
            if let Move::Operation { id, kind, .. } = m {
                let t = ops::find(kind);
                let feature = t.is_some_and(|t| {
                    t.shapes_part || (job.stock_box.is_none() && t.machine_needs.contains(&"xyz"))
                });
                if feature {
                    out.insert((id + 1).min(u16::MAX as u32) as u16);
                }
            }
        }
    }
    out
}

/// The grid [`simulate_job`] uses for this job (the part's X range plus 3 mm each side).
pub fn sim_grid(job: &Job, mesh: Option<&Mesh>, dx: f64, na: usize) -> Result<sim::Grid, Error> {
    let (lo, hi) = match (mesh, &job.part.profile) {
        (Some(m), _) => {
            let (lo, hi) = m
                .bounds()
                .ok_or_else(|| Error::Invalid("empty mesh".into()))?;
            (lo[0], hi[0])
        }
        (None, Some(p)) => {
            let g = RevolvedProfile::new(p, job.stock.radius);
            (g.x_min(), g.x_max())
        }
        // (Mar's carry-through: the cut piece of the setup before, no model) the stock's own
        (None, None) => (-job.stock.length, 0.0),
    };
    Ok(sim::Grid::new(lo - 3.0, hi + 3.0, dx, na))
}

/// Simulation of a whole job on a radial heightmap, programs in order on the same stock.
#[derive(Debug, Clone)]
pub struct JobSim {
    pub stock: sim::RadialMap,
    pub part: sim::Part,
    /// Stock after each program, with its statistics.
    pub programs: Vec<(String, sim::SimStats, sim::Deviation)>,
    /// Holder and shank collisions per program (G-code lines not filled in).
    pub collisions: Vec<collision::CollisionReport>,
    /// The stock's radii after each program (the page shows each program's result: kept here
    /// rather than simulated a second time)
    pub after: Vec<Vec<f32>>,
}

/// Simulate `tps` (from [`plan_with_mesh`]) on stock of `job.stock.radius`. The grid spans the
/// part's X range plus 3 mm each side with cells `dx` mm long and `na` around. Deviations are
/// measured over the part's X range; `tol` is the gouge threshold (mm).
pub fn simulate_job(
    job: &Job,
    tps: &[Toolpath],
    mesh: Option<&Mesh>,
    dx: f64,
    na: usize,
    tol: f64,
) -> Result<JobSim, Error> {
    if job.stock_box.is_some() {
        return Err(Error::Invalid(
            "the machining simulation doesn't handle 3-axis (box stock) setups yet \
             (the engine's sim3::Heightmap3 does, for tests)"
                .into(),
        ));
    }
    let (x_lo, x_hi, part_fn): (f64, f64, Box<dyn Fn(sim::Grid) -> sim::Part>) =
        match (mesh, &job.part.profile) {
            (Some(m), _) => {
                let (lo, hi) = m
                    .bounds()
                    .ok_or_else(|| Error::Invalid("empty mesh".into()))?;
                (lo[0], hi[0], Box::new(move |g| sim::Part::from_mesh(g, m)))
            }
            (None, Some(p)) => {
                let geom = RevolvedProfile::new(p, job.stock.radius);
                (
                    geom.x_min(),
                    geom.x_max(),
                    Box::new(move |g| sim::Part::from_profile(g, &geom, &p.points)),
                )
            }
            // (Mar's carry-through: the cut piece of the setup before, no model) the stock's
            // own X range, nothing to compare it with (no material left counted, no gouge)
            (None, None) => (
                -job.stock.length,
                0.0,
                Box::new(|g| sim::Part::from_mesh(g, &Mesh::default())),
            ),
        };
    let grid = sim::Grid::new(x_lo - 3.0, x_hi + 3.0, dx, na);
    if grid.len() > sim::MAX_CELLS {
        return Err(Error::Invalid(format!(
            "simulation grid too fine: {} cells (limit {})",
            grid.len(),
            sim::MAX_CELLS
        )));
    }
    let part = part_fn(grid);
    let mut stock = sim::RadialMap::of_job(grid, job);
    let cutters = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let intended = intended_tags(job, tps);
    let bodies = |t: u32| job.tool(t).map(collision::ToolBody::of);
    let bar = (job.stock.radius, -job.stock.length, 0.0);
    let mut programs = Vec::new();
    let mut collisions = Vec::new();
    let mut after = Vec::new();
    // (its work: each move simulated, then each replayed on the 3D stock that confirms rapids)
    let moves: usize = tps
        .iter()
        .map(|tp| sim::segments(std::slice::from_ref(tp)).len())
        .sum();
    let began = work_begin(if job.carried.is_none() {
        2 * moves
    } else {
        moves
    });
    for tp in tps {
        let (st, rep) = collision::simulate_rotary(
            &mut stock,
            tp,
            &cutters,
            &bodies,
            (dx * 2.0).min(0.1),
            bar,
            false,
            job.fixture.as_ref(),
        );
        let dev = sim::deviation_with(&stock, &part, x_lo, x_hi, tol, &|t| intended.contains(&t));
        programs.push((tp.name.clone(), st, dev));
        collisions.push(rep);
        after.push(stock.r.clone());
    }
    // (the radial map keeps only the stock's highest point along each ray from the axis: a
    // rapid beside or under what stands over it, in a pocket an indexed 3+1 roughing cut from
    // the side or under a blade, reads as through stock when it is not; Mar's impeller showed
    // 9 in red. The rapids it flags are confirmed on the 3D stock, a tri-dexel of the bar cut
    // by every move before them, and only those it confirms are counted.)
    if job.carried.is_none() {
        let mut st: Vec<&mut sim::SimStats> = programs.iter_mut().map(|p| &mut p.1).collect();
        confirm_rapids_3d(job, tps, &mut st);
    }
    work_end(began);
    Ok(JobSim {
        stock,
        part,
        programs,
        collisions,
        after,
    })
}

/// Holder and shank collisions of each program ([`collision`]), from a material-removal
/// simulation of its own: rotary stock 0.3 mm by 2° over the whole bar, 3-axis stock at the
/// "coarse" cell size, tool positions 0.4 mm apart. It is skipped when no move brings a
/// shank or holder below the stock's top. Each collision carries its move's G-code line as
/// `machine` posts it. A program with none says how many moves pass within
/// [`collision::NEAR_MISS`] of it (a near miss).
pub fn collision_check(
    job: &Job,
    tps: &[Toolpath],
    machine: &Machine,
) -> Result<Vec<collision::CollisionReport>, Error> {
    phase("holder and shank check");
    let bodies = |t: u32| job.tool(t).map(collision::ToolBody::of);
    let mut out = collision_pass(job, tps, &bodies)?;
    // (near misses: the shank and holder [`collision::NEAR_MISS`] fatter, the holder as much
    // lower. The impeller turned
    // end for end: its roughing shank passed 0.02 mm from the stock and the check said
    // "clear")
    let fat = |t: u32| {
        bodies(t).map(|mut b| {
            b.shank_radius += collision::NEAR_MISS;
            b.holder_radius += collision::NEAR_MISS;
            // (and the holder that much lower: the stock under it as near)
            b.stickout = (b.stickout - collision::NEAR_MISS).max(b.flute_length);
            b
        })
    };
    let near = collision_pass(job, tps, &fat)?;
    for (rep, n) in out.iter_mut().zip(near) {
        if rep.count == 0 && n.count > 0 {
            rep.near = n.count;
            rep.near_more = n.more;
            rep.near_first = n.collisions.first().map(|c| c.move_index);
        }
    }
    for (rep, tp) in out.iter_mut().zip(tps) {
        let (_, _, lines) = post::post_lines(tp, machine);
        collision::attach_lines(rep, &lines);
        if let Some(i) = rep.near_first {
            rep.near_line = lines.get(i).copied();
        }
    }
    Ok(out)
}

/// One pass of [`collision_check`] with the tools' bodies as `bodies` says.
fn collision_pass(
    job: &Job,
    tps: &[Toolpath],
    bodies: &dyn Fn(u32) -> Option<collision::ToolBody>,
) -> Result<Vec<collision::CollisionReport>, Error> {
    let res = sim::Resolution::preset("coarse").expect("built in");
    let cutters = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    // stamps 0.4 mm apart: the cusps left between them are a few hundredths of a mm
    let step = 0.4;
    let mut out = Vec::new();
    // nothing above the flutes ever gets below the stock's top: no simulation needed
    let (segs, top) = match &job.stock_box {
        Some(sb) => (sim3::segments3(tps), sb.max[2]),
        None => (sim::segments(tps), job.stock.radius),
    };
    // (a chuck or a tailstock: checked whatever the tools' bodies)
    let holders = job.fixture.as_ref().is_some_and(|f| {
        job.stock_box.is_none() && (f.chuck(job.stock.length).is_some() || f.tailstock().is_some())
    });
    if !holders && !collision::can_touch(&segs, bodies, top) {
        return Ok(tps
            .iter()
            .map(|tp| collision::CollisionReport {
                program: tp.name.clone(),
                ..Default::default()
            })
            .collect());
    }
    if let Some(sb) = &job.stock_box {
        let mut hm = sim3::Heightmap3::of_job(job, sb, res.cell_for(sb));
        for tp in tps {
            let (_, rep) = collision::simulate_block(
                &mut hm,
                tp,
                &cutters,
                bodies,
                step,
                sb.max[2],
                job.jaws.map(|g| (*sb, sb.min[2] + g)),
                job.fixture.as_ref(),
            );
            out.push(rep);
        }
    } else {
        let len = job.stock.length.max(1.0);
        // 0.3 mm by 2°: holders are tens of mm across, shanks a few
        let grid = sim::Grid::new(-len - 1.0, 3.0, 0.3, 180);
        if grid.len() > sim::MAX_CELLS {
            return Err(Error::Invalid(
                "stock too long for the collision check".into(),
            ));
        }
        let mut stock = sim::RadialMap::of_job(grid, job);
        let bar = (job.stock.radius, -len, 0.0);
        for (k, tp) in tps.iter().enumerate() {
            // the last program's stock is not needed after its last move that can touch
            let last = k + 1 == tps.len();
            let (_, rep) = collision::simulate_rotary(
                &mut stock,
                tp,
                &cutters,
                bodies,
                step,
                bar,
                last,
                job.fixture.as_ref(),
            );
            out.push(rep);
        }
    }
    Ok(out)
}
