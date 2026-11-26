//! What is left on the finished part, in 3D: every program replayed through a tri-dexel stock
//! ([`crate::dexel`]), then each point of the stock's surface measured against the part's
//! mesh. The radial map ([`crate::sim`]) sees only the highest stock along each ray from the
//! axis: a wall radiating from it (an impeller's blade face) can be left 0.5 mm proud without
//! it showing (Mar's impeller-v3). Points a ball of the finishing tool's radius cannot reach
//! (inside corners narrower than it) are counted apart.

use crate::dexel::{replay, Dexel, ToolSolid};
use crate::job::{Job, ToolShape};
use crate::{Mesh, Toolpath};

/// The stock left on the part's surfaces after a job.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Left3d {
    /// Stock surface points measured (on the part's own X length).
    pub points: usize,
    /// How many are more than 0.05, 0.1, 0.2 and 0.5 mm from the part (reachable ones).
    pub over: [usize; 4],
    /// The most left where the tool can reach (mm), and where: (X, radius, part angle deg).
    pub max_left: f64,
    pub max_at: Option<[f64; 3]>,
    /// Points left more than 0.05 mm where a ball of `reach_r` cannot reach (inside corners).
    pub unreachable: usize,
    /// The deepest of those (mm).
    pub max_unreachable: f64,
    /// Up to 20 of the worst reachable points: (left mm, X, radius, part angle deg).
    pub worst: Vec<[f64; 4]>,
    /// Stock surface points more than 0.05 mm inside the part (on the inner side of its
    /// nearest face): the tri-dexel's rays end there where a tool only grazed another family
    /// of them, not stock left (a gouge would show in the gouge check).
    pub inside: usize,
    /// Points left more than 0.05 mm where no tool square to X reaches (the shank above the
    /// ball meets the part: under an overhang): a 4-axis machine cannot finish them.
    pub undercut: usize,
    pub max_undercut: f64,
    #[serde(skip)]
    max_undercut_seen: f64,
    /// Where the deepest of those is: (X, radius, part angle deg).
    pub undercut_at: Option<[f64; 3]>,
    /// Of the reachable points left over 0.05 mm: those a tool over the axis cannot reach (its
    /// shank along the radius meets the part) but one offset in Y can; the most left there.
    pub needs_offset: usize,
    pub max_needs_offset: f64,
    /// The part's mesh as the measure needs it (outward faces): its edges not shared by
    /// two faces (0: closed), its shared edges whose two faces run them the same way (0:
    /// consistently wound), and its volume by the faces' winding (negative: inside out, read
    /// turned the right way round).
    pub mesh: MeshCheck,
}

/// Whether a mesh is closed and its faces wound consistently outward.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct MeshCheck {
    pub open_edges: usize,
    pub flipped_edges: usize,
    pub volume: f64,
}

impl MeshCheck {
    /// Whether "outside" can be told by the nearest face's normal.
    pub fn sound(&self) -> bool {
        self.open_edges == 0 && self.flipped_edges == 0 && self.volume != 0.0
    }
}

/// Check `mesh` (vertices matched within a micron).
pub fn check_mesh(mesh: &Mesh) -> MeshCheck {
    let key = |p: [f64; 3]| [0, 1, 2].map(|k| (p[k] * 1000.0).round() as i64);
    let mut edges: std::collections::HashMap<([i64; 3], [i64; 3]), (i32, i32)> = Default::default();
    let mut volume = 0.0;
    for t in &mesh.triangles {
        volume += dot(
            t[0],
            [
                t[1][1] * t[2][2] - t[1][2] * t[2][1],
                t[1][2] * t[2][0] - t[1][0] * t[2][2],
                t[1][0] * t[2][1] - t[1][1] * t[2][0],
            ],
        ) / 6.0;
        for k in 0..3 {
            let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
            if a == b {
                continue;
            }
            // (counted by the edge's ends in order: forward and backward)
            let e = if a < b { (a, b) } else { (b, a) };
            let c = edges.entry(e).or_insert((0, 0));
            if a < b {
                c.0 += 1;
            } else {
                c.1 += 1;
            }
        }
    }
    let (mut open, mut flipped) = (0, 0);
    for &(f, r) in edges.values() {
        if f + r != 2 {
            open += 1;
        } else if f != 1 {
            flipped += 1;
        }
    }
    MeshCheck {
        open_edges: open,
        flipped_edges: flipped,
        volume,
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The point of triangle `t` nearest `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest(p: [f64; 3], t: &[[f64; 3]; 3]) -> [f64; 3] {
    let (a, b, c) = (t[0], t[1], t[2]);
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    let lerp = |u: [f64; 3], v: [f64; 3], s: f64| {
        [
            u[0] + (v[0] - u[0]) * s,
            u[1] + (v[1] - u[1]) * s,
            u[2] + (v[2] - u[2]) * s,
        ]
    };
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return lerp(a, b, d1 / (d1 - d3));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return lerp(a, c, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return lerp(b, c, (d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let den = 1.0 / (va + vb + vc);
    let (v, w) = (vb * den, vc * den);
    [
        a[0] + ab[0] * v + ac[0] * w,
        a[1] + ab[1] * v + ac[1] * w,
        a[2] + ab[2] * v + ac[2] * w,
    ]
}

/// The part's triangles in cubes of `cell` mm, for nearest-point queries out to `reach`.
struct Near<'a> {
    mesh: &'a Mesh,
    lo: [f64; 3],
    cell: f64,
    reach: i64,
    cells: std::collections::HashMap<[i64; 3], Vec<u32>>,
}

impl<'a> Near<'a> {
    fn new(mesh: &'a Mesh, cell: f64, reach_mm: f64) -> Near<'a> {
        let (lo, _) = mesh.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
        let mut n = Near {
            mesh,
            lo,
            cell,
            reach: (reach_mm / cell).ceil() as i64,
            cells: Default::default(),
        };
        for (i, t) in mesh.triangles.iter().enumerate() {
            let mn = [0, 1, 2].map(|k| t[0][k].min(t[1][k]).min(t[2][k]));
            let mx = [0, 1, 2].map(|k| t[0][k].max(t[1][k]).max(t[2][k]));
            let (a, b) = (n.key(mn), n.key(mx));
            for x in a[0]..=b[0] {
                for y in a[1]..=b[1] {
                    for z in a[2]..=b[2] {
                        n.cells.entry([x, y, z]).or_default().push(i as u32);
                    }
                }
            }
        }
        n
    }

    fn key(&self, p: [f64; 3]) -> [i64; 3] {
        [0, 1, 2].map(|k| ((p[k] - self.lo[k]) / self.cell).floor() as i64)
    }

    /// The nearest point of the part to `p` within the reach, its distance and the outward
    /// normal there (None: nothing within it).
    fn nearest(&self, p: [f64; 3]) -> Option<(f64, [f64; 3], [f64; 3])> {
        let k = self.key(p);
        let mut best: Option<(f64, [f64; 3], usize)> = None;
        let r = self.reach;
        for x in -r..=r {
            for y in -r..=r {
                for z in -r..=r {
                    let Some(v) = self.cells.get(&[k[0] + x, k[1] + y, k[2] + z]) else {
                        continue;
                    };
                    for &i in v {
                        let q = closest(p, &self.mesh.triangles[i as usize]);
                        let d = dot(sub(p, q), sub(p, q)).sqrt();
                        if best.is_none_or(|b| d < b.0) {
                            best = Some((d, q, i as usize));
                        }
                    }
                }
            }
        }
        best.map(|(d, q, i)| {
            let t = &self.mesh.triangles[i];
            let (u, v) = (sub(t[1], t[0]), sub(t[2], t[0]));
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let l = dot(n, n).sqrt().max(1e-12);
            (d, q, [n[0] / l, n[1] / l, n[2] / l])
        })
    }
}

/// The tool's solid for the replay: a ball's or a flat's, as long as its flutes (or 20 mm).
pub fn solid(job: &Job, t: u32) -> Option<ToolSolid> {
    let d = job.tool(t)?;
    let height = d.flute_length.unwrap_or(20.0).max(5.0);
    Some(match d.shape {
        ToolShape::Ball { diameter } => ToolSolid {
            radius: diameter / 2.0,
            ball: true,
            height,
        },
        ToolShape::Flat { diameter } => ToolSolid {
            radius: diameter / 2.0,
            ball: false,
            height,
        },
        ToolShape::TaperedBall { diameter, .. } => ToolSolid {
            radius: diameter / 2.0,
            ball: true,
            height,
        },
        ToolShape::Vbit { diameter, .. } => ToolSolid {
            radius: diameter.unwrap_or(3.175) / 2.0,
            ball: false,
            height,
        },
    })
}

/// The simulated stock in 3D, for the view: the surface of a tri-dexel of the bar (`h` mm
/// apart) every program cut, and at each of its vertices what is left over the part (mm; 0 on
/// the part or inside it, NaN past the part's ends or with no part). Not a radial map: a
/// blade over a hollow stays a blade and the hollow stays hollow (Mar's impeller: the radial
/// map's one radius a ray filled under its leaning blades and stepped along their edges).
pub struct StockSurface {
    pub mesh: crate::dexel::SurfaceMesh,
    pub left: Vec<f32>,
    pub h: f64,
}

/// See [`StockSurface`].
pub fn stock_surface(job: &Job, tps: &[Toolpath], mesh: Option<&Mesh>, h: f64) -> StockSurface {
    let mut dx = Dexel::bar(job.stock.radius, job.stock.length, h);
    // (its work: each move replayed, then the surface made and measured, a quarter as much
    // each)
    let segs: Vec<_> = tps
        .iter()
        .map(|tp| crate::sim::segments(std::slice::from_ref(tp)))
        .collect();
    let moves: usize = segs.iter().map(Vec::len).sum();
    let began = crate::work_begin(moves + moves / 2);
    for sg in &segs {
        replay(&mut dx, sg, &|t| solid(job, t), h, 0.15);
    }
    let surf = dx.surface_mesh_working(moves / 4);
    let mut left = vec![f32::NAN; surf.positions.len() / 3];
    let per = (left.len() / (moves / 4).max(1)).max(1);
    if let Some(m) = mesh.filter(|m| !m.triangles.is_empty()) {
        let near = Near::new(m, 1.0, 2.0);
        let out_sign = if check_mesh(m).volume < 0.0 {
            -1.0
        } else {
            1.0
        };
        let (lo, hi) = m.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
        for (v, l) in left.iter_mut().enumerate() {
            if v % per == 0 {
                crate::work(1);
            }
            let p = [0, 1, 2].map(|k| surf.positions[3 * v + k] as f64);
            if p[0] < lo[0] - h || p[0] > hi[0] + h {
                continue;
            }
            *l = match near.nearest(p) {
                Some((d, q, n)) => {
                    let n = n.map(|c| c * out_sign);
                    if dot(sub(p, q), n) <= 0.0 {
                        0.0
                    } else {
                        d as f32
                    }
                }
                None => 2.0,
            };
        }
    }
    crate::work_end(began);
    StockSurface {
        mesh: surf,
        left,
        h,
    }
}

/// Replay `tps` (a rotary job on its bar) through a tri-dexel of `h` mm and measure what is
/// left on `mesh`'s surfaces. `reach_r`: the finishing ball's radius, for the inside corners
/// it cannot reach.
pub fn left_in_3d(job: &Job, tps: &[Toolpath], mesh: &Mesh, h: f64, reach_r: f64) -> Left3d {
    let mut dx = Dexel::bar(job.stock.radius, job.stock.length, h);
    for tp in tps {
        let segs = crate::sim::segments(std::slice::from_ref(tp));
        replay(&mut dx, &segs, &|t| solid(job, t), h, 0.15);
    }
    let near = Near::new(mesh, 1.0, 2.0);
    let check = check_mesh(mesh);
    // (a mesh wound inside out: its normals read the other way)
    let out_sign = if check.volume < 0.0 { -1.0 } else { 1.0 };
    let (lo, hi) = mesh.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    let mut out = Left3d {
        points: 0,
        over: [0; 4],
        max_left: 0.0,
        max_at: None,
        unreachable: 0,
        max_unreachable: 0.0,
        worst: Vec::new(),
        inside: 0,
        undercut: 0,
        max_undercut: 0.0,
        max_undercut_seen: 0.0,
        undercut_at: None,
        needs_offset: 0,
        max_needs_offset: 0.0,
        mesh: check,
    };
    let mut worst: Vec<[f64; 4]> = Vec::new();
    for p in dx.surface_points() {
        // (the part's own length: not the bar behind it, nor the face the parting leaves)
        if p[0] < lo[0] + 0.05 || p[0] > hi[0] - 0.05 {
            continue;
        }
        out.points += 1;
        let (d, q, n) = match near.nearest(p) {
            Some(v) => v,
            None => (2.0, p, [0.0; 3]),
        };
        if d <= 0.05 {
            continue;
        }
        // (inside the part: on the inner side of its nearest face)
        let n = [n[0] * out_sign, n[1] * out_sign, n[2] * out_sign];
        if dot(sub(p, q), n) <= 0.0 {
            out.inside += 1;
            continue;
        }
        // (an inside corner narrower than the ball: a ball touching the part at the nearest
        // point would be in the part elsewhere)
        let c = [
            q[0] + n[0] * reach_r,
            q[1] + n[1] * reach_r,
            q[2] + n[2] * reach_r,
        ];
        let blocked = near
            .nearest(c)
            .is_some_and(|(dc, _, _)| dc < reach_r - 0.02);
        if blocked && d < reach_r {
            out.unreachable += 1;
            out.max_unreachable = out.max_unreachable.max(d);
            continue;
        }
        // (a face a 4-axis machine's tool cannot come at: its axis is square to X, and along
        // every such axis near the face's normal the shank above the ball meets the part:
        // under a blade leaning over)
        let (py, pz) = (n[1], n[2]);
        let pl = py.hypot(pz);
        if pl > 0.2 {
            let base = pz.atan2(py);
            let clear = |dir: [f64; 3]| {
                (1..=30).all(|k| {
                    let s = k as f64 * 0.5;
                    let at = [c[0], c[1] + dir[1] * s, c[2] + dir[2] * s];
                    near.nearest(at)
                        .is_none_or(|(dc, _, _)| dc >= reach_r - 0.02)
                })
            };
            let open = [-40.0f64, -20.0, 0.0, 20.0, 40.0].iter().any(|dg| {
                let t = base + dg.to_radians();
                clear([0.0, t.cos(), t.sin()])
            });
            // (over the axis its shank runs along the radius through the ball's centre:
            // blocked there but open off it, only a tool offset in Y reaches)
            let cr = c[1].hypot(c[2]).max(1e-9);
            if open && !clear([0.0, c[1] / cr, c[2] / cr]) {
                out.needs_offset += 1;
                out.max_needs_offset = out.max_needs_offset.max(d);
            }
            if !open {
                out.undercut += 1;
                out.max_undercut = out.max_undercut.max(d);
                if d > out.max_undercut_seen {
                    out.max_undercut_seen = d;
                    out.undercut_at = Some([p[0], p[1].hypot(p[2]), p[2].atan2(p[1]).to_degrees()]);
                }
                continue;
            }
        }
        for (j, t) in [0.05, 0.1, 0.2, 0.5].iter().enumerate() {
            if d > *t {
                out.over[j] += 1;
            }
        }
        let at = [p[0], p[1].hypot(p[2]), p[2].atan2(p[1]).to_degrees()];
        if d > out.max_left {
            out.max_left = d;
            out.max_at = Some(at);
        }
        worst.push([d, at[0], at[1], at[2]]);
    }
    worst.sort_by(|a, b| b[0].total_cmp(&a[0]));
    worst.truncate(20);
    out.worst = worst;
    out
}
