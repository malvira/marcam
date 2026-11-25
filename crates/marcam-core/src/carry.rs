//! Stock carried from one setup to the next: what the earlier setup's programs left
//! (simulated), turned and moved into the next setup's work coordinates
//! ([`crate::job::Carry`]). A block keeps each cell's top and bottom, so it can be turned over
//! and cut from the other side (a cell cut through has its top at its bottom); a bar keeps its
//! radius at each (X, angle).
//!
//! [`carry_stock`] gives each job that takes its stock from the setup before it that stock
//! ([`Job::carried`]); then everything that starts from the job's stock (planning a rest
//! roughing, adaptive clearing, the simulation) starts from it.

use crate::det::Det;
use crate::job::{Carry, Job, Stock, StockBox};
use crate::sim::{Grid, RadialMap};
use crate::sim3::Heightmap3;
use crate::Mesh;

/// The stock a setup starts with, in its own work coordinates.
#[derive(Debug, Clone)]
pub enum Carried {
    /// A block's cells (centre of cell `(i, j)` at `x0 + (i + ½) cell`, ...): each one's top and
    /// bottom Z.
    Block {
        x0: f64,
        y0: f64,
        cell: f64,
        nx: usize,
        ny: usize,
        top: Vec<f64>,
        bot: Vec<f64>,
    },
    /// A bar: its radius at each (X, part angle) cell.
    Bar(RadialMap),
}

impl PartialEq for Carried {
    fn eq(&self, other: &Carried) -> bool {
        self.key() == other.key()
    }
}

impl Carried {
    /// A hash of the stock (part of a plan's cache key).
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Carried::Block {
                x0,
                y0,
                cell,
                nx,
                ny,
                top,
                bot,
            } => {
                (x0.to_bits(), y0.to_bits(), cell.to_bits(), nx, ny).hash(&mut h);
                for v in top.iter().chain(bot) {
                    v.to_bits().hash(&mut h);
                }
            }
            Carried::Bar(m) => {
                (
                    m.grid.x0.to_bits(),
                    m.grid.dx.to_bits(),
                    m.grid.nx,
                    m.grid.na,
                )
                    .hash(&mut h);
                for v in &m.r {
                    v.to_bits().hash(&mut h);
                }
            }
        }
        h.finish()
    }

    /// The block's cell nearest (x, y), if it is on the block.
    fn cell_at(&self, x: f64, y: f64) -> Option<usize> {
        let Carried::Block {
            x0,
            y0,
            cell,
            nx,
            ny,
            ..
        } = self
        else {
            return None;
        };
        let i = ((x - x0) / cell - 0.5).round();
        let j = ((y - y0) / cell - 0.5).round();
        (i >= 0.0 && j >= 0.0 && (i as usize) < *nx && (j as usize) < *ny)
            .then(|| j as usize * nx + i as usize)
    }

    /// The block as the heightmap of `sb` at `cell` (each cell's top; where there was no
    /// stock, or it was cut through, at the bottom of `sb`).
    pub fn heightmap(&self, sb: &StockBox, cell: f64) -> Option<Heightmap3> {
        let Carried::Block { top, bot, .. } = self else {
            return None;
        };
        let mut hm = Heightmap3::new(sb, cell);
        for j in 0..hm.ny {
            for i in 0..hm.nx {
                let (x, y) = hm.centre(i, j);
                let z = match self.cell_at(x, y) {
                    Some(c) if top[c] > bot[c] => top[c].min(sb.max[2]),
                    _ => sb.min[2],
                };
                hm.z[j * hm.nx + i] = z;
            }
        }
        hm.top = hm.z.iter().copied().fold(sb.min[2], f64::max);
        Some(hm)
    }

    /// The block's bottom under each cell of `hm` (its own bottom where there is none).
    pub fn bottoms(&self, hm: &Heightmap3) -> Vec<f64> {
        let mut out = vec![hm.bottom; hm.nx * hm.ny];
        if let Carried::Block { bot, .. } = self {
            for j in 0..hm.ny {
                for i in 0..hm.nx {
                    let (x, y) = hm.centre(i, j);
                    if let Some(c) = self.cell_at(x, y) {
                        out[j * hm.nx + i] = bot[c];
                    }
                }
            }
        }
        out
    }

    /// The bar on `grid` (where it has none: the stock as it comes).
    pub fn radial(&self, grid: Grid, stock: &Stock) -> Option<RadialMap> {
        let Carried::Bar(m) = self else {
            return None;
        };
        let mut out = RadialMap::of_stock(grid, stock);
        let g = m.grid;
        for i in 0..grid.nx {
            let x = grid.x0 + (i as f64 + 0.5) * grid.dx;
            let si = ((x - g.x0) / g.dx - 0.5).round();
            if si < 0.0 || si as usize >= g.nx {
                continue;
            }
            for j in 0..grid.na {
                let phi = grid.phi(j);
                let sj = (phi / std::f64::consts::TAU * g.na as f64 - 0.5)
                    .round()
                    .rem_euclid(g.na as f64) as usize;
                out.r[i * grid.na + j] = m.r[si as usize * g.na + sj];
            }
        }
        out.top = out.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64;
        Some(out)
    }

    /// The largest radius of a bar.
    pub fn max_radius(&self) -> Option<f64> {
        match self {
            Carried::Bar(m) => Some(m.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64),
            Carried::Block { .. } => None,
        }
    }
}

/// A block (its tops `hm`, bottoms `bot`) turned and moved by `carry`.
pub fn turn_block(hm: &Heightmap3, bot: &[f64], carry: &Carry) -> Carried {
    let s = carry.turn.signs();
    let t = carry.shift;
    // the cells' centres turned: the new grid over them, the same cell size
    let xs = [
        s[0] * (hm.x0 + 0.5 * hm.cell) + t[0],
        s[0] * (hm.x0 + (hm.nx as f64 - 0.5) * hm.cell) + t[0],
    ];
    let ys = [
        s[1] * (hm.y0 + 0.5 * hm.cell) + t[1],
        s[1] * (hm.y0 + (hm.ny as f64 - 0.5) * hm.cell) + t[1],
    ];
    let (nx, ny) = (hm.nx, hm.ny);
    let x0 = xs[0].min(xs[1]) - 0.5 * hm.cell;
    let y0 = ys[0].min(ys[1]) - 0.5 * hm.cell;
    let mut top = vec![0.0; nx * ny];
    let mut bt = vec![0.0; nx * ny];
    for j in 0..ny {
        for i in 0..nx {
            // the source cell of the new one (the turns are their own inverses)
            let si = if s[0] < 0.0 { nx - 1 - i } else { i };
            let sj = if s[1] < 0.0 { ny - 1 - j } else { j };
            let c = sj * nx + si;
            let (a, b) = (hm.z[c], bot[c]);
            let (tp, bo) = if s[2] < 0.0 {
                (-b + t[2], -a + t[2])
            } else {
                (a + t[2], b + t[2])
            };
            // cut through: none left
            let (tp, bo) = if a <= b + 1e-9 { (bo, bo) } else { (tp, bo) };
            top[j * nx + i] = tp;
            bt[j * nx + i] = bo;
        }
    }
    Carried::Block {
        x0,
        y0,
        cell: hm.cell,
        nx,
        ny,
        top,
        bot: bt,
    }
}

/// A bar turned and moved by `carry` (a turn about X: the angle half way round; about Y: the
/// angle mirrored and the bar end for end; about Z: both).
pub fn turn_bar(map: &RadialMap, carry: &Carry) -> Carried {
    let s = carry.turn.signs();
    let g = map.grid;
    let x_lo = s[0] * (g.x0 + 0.5 * g.dx) + carry.shift[0];
    let x_hi = s[0] * (g.x0 + (g.nx as f64 - 0.5) * g.dx) + carry.shift[0];
    let grid = Grid {
        x0: x_lo.min(x_hi) - 0.5 * g.dx,
        dx: g.dx,
        nx: g.nx,
        na: g.na,
    };
    let mut out = RadialMap::filled(grid, 0.0);
    for i in 0..g.nx {
        let si = if s[0] < 0.0 { g.nx - 1 - i } else { i };
        for j in 0..g.na {
            // the direction (cos φ, sin φ) in Y/Z turned
            let phi = grid.phi(j);
            let src = (s[2] * phi.dsin()).datan2(s[1] * phi.dcos());
            let sj = (src.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU * g.na as f64
                - 0.5)
                .round()
                .rem_euclid(g.na as f64) as usize;
            out.r[i * g.na + j] = map.r[si * g.na + sj];
        }
    }
    out.top = out.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64;
    Carried::Bar(out)
}

/// The cell the carried stock is simulated at: a fiftieth of a millimetre per 250 mm of the
/// block's larger side (0.05 to 0.25 mm).
pub fn block_cell(sb: &StockBox) -> f64 {
    let span = (sb.max[0] - sb.min[0]).max(sb.max[1] - sb.min[1]);
    (span / 400.0).clamp(0.05, 0.25)
}

/// The bar's grid for carrying: over its length (and a little past its front), 0.1 mm and 240
/// cells round.
pub fn bar_grid(stock: &Stock) -> Grid {
    Grid::new(-stock.length - 1.0, 1.0, 0.1, 240)
}

/// Give each of `jobs` (a project's setups in order, up to `upto` when given) that takes its
/// stock from the setup before it ([`Job::carry`]) that stock ([`Job::carried`]): the earlier
/// setups are planned and simulated in turn, each from its own carried stock. `meshes` gives
/// a job's part mesh.
pub fn carry_stock(
    jobs: &mut [(String, Job)],
    meshes: &dyn Fn(&Job) -> Option<Mesh>,
    upto: Option<usize>,
) -> Result<(), String> {
    let last = upto.unwrap_or(jobs.len().saturating_sub(1));
    for k in 0..jobs.len().min(last + 1) {
        let Some(carry) = jobs[k].1.carry.clone() else {
            continue;
        };
        let p = jobs[..k]
            .iter()
            .rposition(|(id, _)| *id == carry.from)
            .ok_or_else(|| {
                format!(
                    "setup {:?}: its stock comes from {:?}, which is not before it",
                    jobs[k].0, carry.from
                )
            })?;
        let prev = jobs[p].1.clone();
        let mesh = meshes(&prev);
        // the same setup before (its stock, part, tools and programs) and the same carry:
        // what it left, worked out before (the web app plans a setup at every edit)
        let key = crate::hash_of(&(
            crate::job_key(&prev, mesh.as_ref()),
            // (and what it goes on as: sampled at this setup's block or bar)
            serde_json::to_string(&(
                &prev.programs,
                &carry,
                &jobs[k].1.stock_box,
                &jobs[k].1.stock,
            ))
            .unwrap_or_default(),
        ));
        if let Some(c) = MEMO.with(|m| {
            m.borrow()
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, c)| c.clone())
        }) {
            set_carried(&mut jobs[k].1, c);
            continue;
        }
        // (what it left, however it goes on: planned and simulated once, then only turned and
        // sampled again as the piece is turned in the next setup; the walkthrough: each turn
        // simulated the whole vise roughing again, 10 s before the piece was drawn)
        let left_key = crate::hash_of(&(
            crate::job_key(&prev, mesh.as_ref()),
            serde_json::to_string(&prev.programs).unwrap_or_default(),
        ));
        let left = match LEFT.with(|m| {
            m.borrow()
                .iter()
                .find(|(k, _)| *k == left_key)
                .map(|(_, l)| l.clone())
        }) {
            Some(l) => l,
            None => {
                let tps = crate::plan_with_mesh(&prev, mesh.as_ref())
                    .map_err(|e| format!("setup {:?} (its stock goes on): {e}", jobs[p].0))?;
                let cutters = |t: u32| {
                    prev.tool(t)
                        .and_then(|d| crate::cutter_for(t, d.shape).ok())
                };
                let l = match prev.stock_box {
                    Some(sb) => {
                        let cell = block_cell(&sb);
                        let mut hm = Heightmap3::of_job(&prev, &sb, cell);
                        let bot = match &prev.carried {
                            Some(c) => c.bottoms(&hm),
                            None => vec![hm.bottom; hm.nx * hm.ny],
                        };
                        for tp in &tps {
                            crate::sim3::simulate3(&mut hm, tp, &cutters, (cell * 2.0).min(0.1));
                        }
                        std::sync::Arc::new(Left::Block(hm, bot))
                    }
                    None => {
                        let grid = bar_grid(&prev.stock);
                        let mut map = RadialMap::of_job(grid, &prev);
                        for tp in &tps {
                            crate::sim::simulate(&mut map, tp, &cutters, 0.2);
                        }
                        std::sync::Arc::new(Left::Bar(map))
                    }
                };
                LEFT.with(|m| {
                    let mut m = m.borrow_mut();
                    if m.len() >= 4 {
                        m.remove(0);
                    }
                    m.push((left_key, l.clone()));
                });
                l
            }
        };
        let carried = match &*left {
            Left::Block(hm, bot) => {
                let (hm, bot) = (hm.clone(), bot.clone());
                if mesh_carry(&carry, true, &jobs[k].1) {
                    let m = crate::carry_mesh::block_mesh(
                        hm.x0, hm.y0, hm.cell, hm.nx, hm.ny, &hm.z, &bot,
                    );
                    from_mesh(&m, &carry, &jobs[k].1)
                } else {
                    turn_block(&hm, &bot, &carry)
                }
            }
            Left::Bar(map) => {
                let map = map.clone();
                if mesh_carry(&carry, false, &jobs[k].1) {
                    from_mesh(&crate::carry_mesh::bar_mesh(&map), &carry, &jobs[k].1)
                } else {
                    turn_bar(&map, &carry)
                }
            }
        };
        let carried = std::sync::Arc::new(carried);
        MEMO.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() >= MEMO_SIZE {
                m.remove(0);
            }
            m.push((key, carried.clone()));
        });
        set_carried(&mut jobs[k].1, carried);
    }
    Ok(())
}

/// Whether `carry` goes on as a mesh: turned any way, or a block going on as a bar (or back).
fn mesh_carry(carry: &Carry, from_block: bool, to: &Job) -> bool {
    carry.rotate != [0.0; 3] || from_block != to.stock_box.is_some()
}

/// The cut result `m` (in the setup before's coordinates) carried into `to`: turned and moved
/// by `carry`, sampled as `to`'s block (its columns at the block's cell) or bar (its grid).
fn from_mesh(m: &Mesh, carry: &Carry, to: &Job) -> Carried {
    let s = carry.turn.signs();
    let tm = [[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]];
    let r = crate::carry_mesh::mul(tm, crate::carry_mesh::rotation(carry.rotate));
    let m = crate::carry_mesh::transform(m, r, carry.shift);
    let out = sampled(&m, to);
    // (Mar: "the carried over cut stock is still really bad") the solid as it goes on, kept
    // for drawing: the samples are for simulating
    keep_solid(&out, m);
    out
}

/// `m` (in `to`'s coordinates) sampled as `to`'s block or bar.
fn sampled(m: &Mesh, to: &Job) -> Carried {
    match to.stock_box {
        Some(sb) => {
            let (x0, y0, nx, ny, top, bot) = crate::carry_mesh::sample_block(m, block_cell(&sb));
            Carried::Block {
                x0,
                y0,
                cell: block_cell(&sb),
                nx,
                ny,
                top,
                bot,
            }
        }
        None => {
            let mut map = crate::carry_mesh::sample_bar(m, bar_grid(&to.stock));
            map.top = map.r.iter().fold(0.0f32, |a, &b| a.max(b)) as f64;
            Carried::Bar(map)
        }
    }
}

/// What a setup's programs left of its stock, simulated (a block's tops and bottoms, or a
/// bar's radii), before it is turned or moved into the next setup.
enum Left {
    Block(Heightmap3, Vec<f64>),
    Bar(RadialMap),
}

thread_local! {
    /// What the setups before left lately (by the setup and its programs).
    static LEFT: std::cell::RefCell<Vec<(u64, std::sync::Arc<Left>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Carried stocks worked out lately (by the setup before and the carry).
const MEMO_SIZE: usize = 8;
thread_local! {
    /// The solids carried stocks were sampled from (by the stock's key): what is drawn.
    static SOLID: std::cell::RefCell<Vec<(u64, std::sync::Arc<Mesh>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn keep_solid(c: &Carried, m: Mesh) {
    let key = c.key();
    SOLID.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|(k, _)| *k != key);
        if s.len() >= MEMO_SIZE {
            s.remove(0);
        }
        s.push((key, std::sync::Arc::new(m)));
    });
}

/// The closed solid a carried stock was sampled from (turned and moved into its setup), when
/// it was carried as a mesh: what to draw of it (its samples leave gaps where walls run along
/// the rays).
pub fn solid_of(c: &Carried) -> Option<std::sync::Arc<Mesh>> {
    let key = c.key();
    SOLID.with(|s| {
        s.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, m)| m.clone())
    })
}

/// A closed mesh's volume (the signed tetrahedra from the origin).
pub fn mesh_volume(m: &Mesh) -> f64 {
    m.triangles
        .iter()
        .map(|[a, b, c]| {
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum::<f64>()
        .abs()
}

/// A carried stock's volume as sampled (a bar's wedges, a block's columns).
pub fn sampled_volume(c: &Carried) -> f64 {
    match c {
        Carried::Block { cell, top, bot, .. } => top
            .iter()
            .zip(bot)
            .map(|(t, b)| (t - b).max(0.0) * cell * cell)
            .sum(),
        Carried::Bar(m) => {
            let da = std::f64::consts::TAU / m.grid.na as f64;
            m.r.iter()
                .map(|&r| 0.5 * (r as f64).powi(2) * da * m.grid.dx)
                .sum()
        }
    }
}
thread_local! {
    static MEMO: std::cell::RefCell<Vec<(u64, std::sync::Arc<Carried>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn set_carried(job: &mut Job, carried: std::sync::Arc<Carried>) {
    // a bar: planned from its largest radius (no passes in the air above it)
    if let Some(r) = carried.max_radius() {
        if r > 0.0 && r < job.stock.radius {
            job.stock.radius = r;
        }
    }
    job.carried = Some(carried);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Turn;

    fn block() -> Heightmap3 {
        // 10 x 4 mm, 2 mm cells, top Z0, bottom Z-5; one cell cut 2 deep, one cut through
        let sb = StockBox {
            min: [0.0, 0.0, -5.0],
            max: [10.0, 4.0, 0.0],
        };
        let mut hm = Heightmap3::new(&sb, 2.0);
        hm.z[0] = -2.0;
        hm.z[4] = -5.0;
        hm
    }

    #[test]
    fn a_block_turned_over_shows_its_bottom_and_keeps_its_cuts() {
        let hm = block();
        let bot = vec![-5.0; hm.nx * hm.ny];
        // over about Y, re-zeroed: X back over 0..10, its top (the old bottom) at Z0
        let c = Carry {
            from: "s1".into(),
            rotate: [0.0; 3],
            turn: Turn::Y180,
            shift: [10.0, 0.0, -5.0],
        };
        let t = turn_block(&hm, &bot, &c);
        let Carried::Block {
            top, bot: b, nx, ..
        } = &t
        else {
            panic!()
        };
        // cell (0, 0) cut 2 deep was at X 1: now at X 9 (cell 4), its new top the old bottom
        // (Z0), its bottom the cut (Z-3)
        assert_eq!((top[4], b[4]), (0.0, -3.0));
        // the cell cut through (old cell 4, X 9): none left, now at X 1 (cell 0)
        assert!(top[0] <= b[0]);
        // the rest: the old bottom on top, the old top below
        assert_eq!((top[2], b[2]), (0.0, -5.0));
        assert_eq!(*nx, 5);
        // as the next setup's heightmap: the cut-through cell at the bottom
        let sb = StockBox {
            min: [0.0, 0.0, -5.0],
            max: [10.0, 4.0, 0.0],
        };
        let h2 = t.heightmap(&sb, 1.0).unwrap();
        assert_eq!(h2.z[0], -5.0);
        assert_eq!(h2.z[5], 0.0);
        // turned back: as it was
        let back = Carry {
            from: "s2".into(),
            rotate: [0.0; 3],
            turn: Turn::Y180,
            shift: [10.0, 0.0, -5.0],
        };
        let h1 = t.heightmap(&sb, 2.0).unwrap();
        let again = turn_block(&h1, &t.bottoms(&h1), &back);
        let Carried::Block { top, bot, .. } = &again else {
            panic!()
        };
        assert_eq!(top[0], -2.0);
        // (cut through: empty)
        assert!(top[4] <= bot[4]);
    }

    #[test]
    fn a_bar_turned_half_way_round_or_end_for_end() {
        let grid = Grid::new(-10.0, 0.0, 1.0, 8);
        let mut m = RadialMap::filled(grid, 5.0);
        // a flat at the cell nearest +Y (φ 22.5°), 2 mm from the front (x -1.5)
        m.r[8] = 3.0;
        let x = |c: &Carried, i: usize, j: usize| match c {
            Carried::Bar(b) => b.r[i * 8 + j],
            _ => panic!(),
        };
        let about_x = turn_bar(
            &m,
            &Carry {
                from: "a".into(),
                rotate: [0.0; 3],
                turn: Turn::X180,
                shift: [0.0; 3],
            },
        );
        // half way round: φ 202.5° (cell 4), same X
        assert_eq!(x(&about_x, 1, 4), 3.0);
        let about_y = turn_bar(
            &m,
            &Carry {
                from: "a".into(),
                rotate: [0.0; 3],
                turn: Turn::Y180,
                shift: [-10.0, 0.0, 0.0],
            },
        );
        // end for end (cell 1 of 10 becomes cell 8) and mirrored (22.5° -> -22.5°: cell 7)
        assert_eq!(x(&about_y, 8, 7), 3.0);
    }
}
