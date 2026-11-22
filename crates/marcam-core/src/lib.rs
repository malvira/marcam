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
pub mod dropcutter;
pub mod engrave;
pub mod estimate;
pub mod feeds;
pub mod geom;
pub mod geometry;
pub mod gpu_input;
pub mod job;
pub mod kinematics;
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
    }
}

/// Facets around the axis when a profile is revolved into a finishing mesh.
pub const PROFILE_SEGMENTS: usize = 720;

/// Planning context shared by the programs of a job.
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

/// Operations that work on either setup: flat on a vise, wrapped on a rotary setup.
pub fn is_surface_op(op: &Operation) -> bool {
    matches!(op, Operation::Engrave(_) | Operation::Vcarve(_))
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
            job::StockBox {
                min: [xlo, -rad, -rad],
                max: [xhi, rad, rad],
            }
        })
    });
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
        phase(&format!("planning {} ({})", op_name(op), prog.name));
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
                    // a rest roughing: and the moves before it
                    matches!(op, Operation::Rough3d(r) if r.rest)
                        .then(|| moves_key(&ctx.prior, &b.moves)),
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
            (Operation::HelicalKnurl(o), ToolShape::Vbit { angle, .. }) => {
                // (its radial stock to leave: the body it knurls that much bigger)
                let mut o = o.clone();
                o.body_back[1] += o.leave_radial;
                o.body_front[1] += o.leave_radial;
                strategy::helical_knurl(&mut b, angle, safe_r, &o)
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
            (Operation::RotaryFinish(o), _) => {
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
                let r_min = strategy::rotary_finish_on(
                    &mut b,
                    &dc,
                    sel.as_ref(),
                    mesh.max_radius(),
                    safe_r,
                    o,
                )
                .map_err(|e| Error::Invalid(format!("program {}: {e}", prog.name)))?;
                check_flute(tool, job.stock.radius, r_min)?;
            }
            (Operation::RotaryRough(o), _) => {
                let mesh = ctx.surface(&prog.name)?;
                let c = grown_for(cutter.expect("set above"), o.leave_axial, &prog.name)?;
                let dc = DropCutter::new(mesh, c);
                // a bar carried from an earlier setup: where it is turned down already
                let column = job.carried.as_deref().and_then(|c| match c {
                    carry::Carried::Bar(m) => Some(m),
                    _ => None,
                });
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
                let stock_at = |x: f64| {
                    if !on_bar(x) {
                        return 0.0;
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
                let r_min = strategy::rotary_rough(
                    &mut b,
                    &dc,
                    job.stock.radius,
                    safe_r,
                    o,
                    Some(&stock_at as &dyn Fn(f64) -> f64),
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
        // rotary operations and engravings: smoothed here (3-axis ones above)
        if smooth_tol > 0.0 && !is_three_axis(op) {
            let mut tail = b.moves.split_off(op_start);
            smoothing::smooth(&mut tail, smooth_tol, flat);
            b.moves.extend(tail);
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
        p.operations
            .iter()
            .any(|o| matches!(o, Operation::Rough3d(r) if r.rest || job.carried.is_some()))
    });
    let mut next_op = 0;
    let mut tps: Vec<Toolpath> = Vec::new();
    for p in &job.programs {
        let tp = plan_program(&mut ctx, p, &mut next_op)?;
        if rest {
            ctx.prior.push(tp.moves.clone());
        }
        tps.push(tp);
    }
    cut_off_warnings(job, &mut tps);
    Ok(tps)
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
        let (mut x, mut r, mut a) = (None::<f64>, None::<f64>, 0.0);
        let mut tool: Option<u32> = None;
        for m in &tp.moves {
            match *m {
                Move::ToolChange { tool: t, .. } => {
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
        (None, None) => return Err(Error::Invalid("nothing to compare: no part".into())),
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
            (None, None) => return Err(Error::Invalid("nothing to compare: no part".into())),
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
    for tp in tps {
        let (st, rep) = collision::simulate_rotary(
            &mut stock,
            tp,
            &cutters,
            &bodies,
            (dx * 2.0).min(0.1),
            bar,
            false,
        );
        let dev = sim::deviation_with(&stock, &part, x_lo, x_hi, tol, &|t| intended.contains(&t));
        programs.push((tp.name.clone(), st, dev));
        collisions.push(rep);
    }
    Ok(JobSim {
        stock,
        part,
        programs,
        collisions,
    })
}

/// Holder and shank collisions of each program ([`collision`]), from a material-removal
/// simulation of its own: rotary stock 0.3 mm by 2° over the whole bar, 3-axis stock at the
/// "coarse" cell size, tool positions 0.4 mm apart. It is skipped when no move brings a
/// shank or holder below the stock's top. Each collision carries its move's G-code line as
/// `machine` posts it.
pub fn collision_check(
    job: &Job,
    tps: &[Toolpath],
    machine: &Machine,
) -> Result<Vec<collision::CollisionReport>, Error> {
    phase("holder and shank check");
    let res = sim::Resolution::preset("coarse").expect("built in");
    let cutters = |t: u32| job.tool(t).and_then(|d| cutter_for(t, d.shape).ok());
    let bodies = |t: u32| job.tool(t).map(collision::ToolBody::of);
    // stamps 0.4 mm apart: the cusps left between them are a few hundredths of a mm
    let step = 0.4;
    let mut out = Vec::new();
    // nothing above the flutes ever gets below the stock's top: no simulation needed
    let (segs, top) = match &job.stock_box {
        Some(sb) => (sim3::segments3(tps), sb.max[2]),
        None => (sim::segments(tps), job.stock.radius),
    };
    if !collision::can_touch(&segs, &bodies, top) {
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
                &bodies,
                step,
                sb.max[2],
                job.jaws.map(|g| (*sb, sb.min[2] + g)),
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
            let (_, rep) =
                collision::simulate_rotary(&mut stock, tp, &cutters, &bodies, step, bar, last);
            out.push(rep);
        }
    }
    for (rep, tp) in out.iter_mut().zip(tps) {
        let (_, _, lines) = post::post_lines(tp, machine);
        collision::attach_lines(rep, &lines);
    }
    Ok(out)
}
