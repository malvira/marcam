//! A synthetic part whose stock is known exactly, for the exact view's acceptance: a slot, a
//! pocket, a diamond knurl and a 4-axis helix cut into a Ø20 bar, seen at three zooms. For each
//! view: a reference worked out by brute force from the tool's poses (independent of the exact
//! view's pieces and hierarchy), compared with the exact view's own cast on the CPU, and dumped
//! for web/tests/exact_probe.mjs to compare with the GPU's.
//!
//! `cargo run --release -p marcam-core --example exact_synth -- <out dir> [--w 200 --h 120]`

use marcam::dexel::{seg_at, ToolSolid};
use marcam::exact::{sd_tool, Scene};
use marcam::sim::Segment;
use rayon::prelude::*;

const R: f64 = 10.0;
const L: f64 = 40.0;

fn straight(a_deg: f64, from: [f64; 3], to: [f64; 3], tool: u32, op: u16) -> Segment {
    Segment {
        from: Some(from),
        to,
        rapid: false,
        tool: Some(tool),
        op,
        program: 0,
        move_index: 0,
        index_a: Some(a_deg),
        a_from: None,
    }
}

fn rotary(from: [f64; 3], to: [f64; 3], tool: u32, op: u16) -> Segment {
    Segment {
        from: Some(from),
        to,
        rapid: false,
        tool: Some(tool),
        op,
        program: 0,
        move_index: 0,
        index_a: None,
        a_from: None,
    }
}

fn tools(n: u32) -> Option<ToolSolid> {
    match n {
        1 => Some(ToolSolid {
            radius: 1.5,
            ball: false,
            height: 20.0,
            cone: 0.0,
        }),
        2 => Some(ToolSolid {
            radius: 1.5875,
            ball: false,
            height: 20.0,
            cone: 1.0,
        }),
        3 => Some(ToolSolid {
            radius: 1.0,
            ball: true,
            height: 20.0,
            cone: 0.0,
        }),
        _ => None,
    }
}

/// The job: op 1 a slot across the bar, op 2 a pocket (a zigzag), op 3 a diamond knurl (V
/// helices both ways), op 4 a ball's helical groove (4-axis: X and A together).
fn segments() -> Vec<Segment> {
    let mut s = Vec::new();
    // slot: A 0, along Y across the top, 1.5 deep
    s.push(straight(0.0, [-36.0, -12.0, 8.5], [-36.0, 12.0, 8.5], 1, 1));
    // pocket: A 90, passes along Y, 2 deep, x from -30 to -22
    let mut x = -30.0;
    let mut dir = 1.0;
    let mut last: Option<[f64; 3]> = None;
    while x <= -22.0 + 1e-9 {
        let a = [x, -3.0 * dir, 8.0];
        let b = [x, 3.0 * dir, 8.0];
        if let Some(l) = last {
            s.push(straight(90.0, l, a, 1, 2));
        }
        s.push(straight(90.0, a, b, 1, 2));
        last = Some(b);
        x += 1.6;
        dir = -dir;
    }
    // knurl: V-bit tip at radius 9.3, x from -18 to -11 over 360 degrees, 6 starts each hand,
    // 10 degree moves
    for hand in [1.0, -1.0] {
        for k in 0..6 {
            let a0 = k as f64 * 60.0;
            let n = 36;
            for i in 0..n {
                let f0 = i as f64 / n as f64;
                let f1 = (i + 1) as f64 / n as f64;
                s.push(rotary(
                    [-18.0 + 7.0 * f0, 9.3, a0 + hand * 360.0 * f0],
                    [-18.0 + 7.0 * f1, 9.3, a0 + hand * 360.0 * f1],
                    2,
                    3,
                ));
            }
        }
    }
    // helix: ball tip at radius 8.8, x from -8 to -2 over two turns
    let n = 72;
    for i in 0..n {
        let f0 = i as f64 / n as f64;
        let f1 = (i + 1) as f64 / n as f64;
        s.push(rotary(
            [-8.0 + 6.0 * f0, 8.8, 720.0 * f0],
            [-8.0 + 6.0 * f1, 8.8, 720.0 * f1],
            3,
            4,
        ));
    }
    s
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
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    mul(a, 1.0 / dot(a, a).sqrt())
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The reference: a point's signed distance to each move's swept tool, from its poses (every
/// 0.02 mm of any point's travel) and the nearest refined between its neighbours.
/// A move, its tool, its poses' parameters and its box.
type Move = (Segment, ToolSolid, Vec<f64>, [f64; 3], [f64; 3]);

struct Brute {
    segs: Vec<Move>,
}

impl Brute {
    fn new(segs: &[Segment]) -> Brute {
        let mut out = Vec::new();
        for s in segs {
            let t = tools(s.tool.unwrap()).unwrap();
            let from = s.from.unwrap();
            let (p0, _) = seg_at(s, from, 0.0);
            let (p1, _) = seg_at(s, from, 1.0);
            let turn = match s.index_a {
                Some(_) => 0.0,
                None => (s.to[2] - from[2]).to_radians().abs(),
            };
            let travel = dot(sub(p1, p0), sub(p1, p0)).sqrt() + (R + 20.0) * turn;
            let n = ((travel / 0.02).ceil() as usize).max(2);
            let fs: Vec<f64> = (0..=n).map(|i| i as f64 / n as f64).collect();
            // (its box: the poses' tools, grown by a step)
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for &f in &fs {
                let (tip, a) = seg_at(s, from, f);
                for e in [tip, add(tip, mul(a, 20.0))] {
                    for k in 0..3 {
                        lo[k] = lo[k].min(e[k] - t.radius - 0.05);
                        hi[k] = hi[k].max(e[k] + t.radius + 0.05);
                    }
                }
            }
            out.push((*s, t, fs, lo, hi));
        }
        Brute { segs: out }
    }

    /// The least signed distance to any move's tool, and the move's operation.
    fn sd(&self, x: [f64; 3]) -> (f64, u16) {
        let mut best = (f64::INFINITY, 0u16);
        for (s, t, fs, lo, hi) in &self.segs {
            let outside = (0..3)
                .map(|k| (lo[k] - x[k]).max(x[k] - hi[k]).max(0.0))
                .fold(0.0f64, f64::max);
            if outside > best.0 {
                continue;
            }
            let from = s.from.unwrap();
            let d = |f: f64| {
                let (tip, a) = seg_at(s, from, f);
                sd_tool(t, tip, a, 20.0, x)
            };
            let (mut bi, mut bv) = (0, f64::INFINITY);
            for (i, &f) in fs.iter().enumerate() {
                let v = d(f);
                if v < bv {
                    bi = i;
                    bv = v;
                }
            }
            let (mut a, mut b) = (fs[bi.saturating_sub(1)], fs[(bi + 1).min(fs.len() - 1)]);
            for _ in 0..50 {
                let (m1, m2) = (a + (b - a) / 3.0, b - (b - a) / 3.0);
                if d(m1) < d(m2) {
                    b = m2;
                } else {
                    a = m1;
                }
            }
            let v = bv.min(d(0.5 * (a + b)));
            if v < best.0 {
                best = (v, s.op);
            }
        }
        best
    }

    /// Where the ray meets the stock: (t, op: 0 the bar's own), marched through the air (each
    /// step the depth inside a tool: never past its boundary) and refined by bisection.
    fn cast(&self, o: [f64; 3], d: [f64; 3]) -> Option<(f64, u16)> {
        let sc = Scene::new(R, L, vec![], vec![]);
        let (t0, t1, _) = sc.bar_span(o, d)?;
        let mut t = t0.max(0.0);
        let mut steps = 0;
        loop {
            let (v, _) = self.sd(add(o, mul(d, t)));
            if v >= 0.0 {
                break;
            }
            t += (-v).max(2e-5);
            steps += 1;
            if t >= t1 || steps > 100_000 {
                return None;
            }
        }
        if t == t0.max(0.0) {
            return Some((t, 0));
        }
        // (between the last point in the air and this one)
        let (mut a, mut b) = (t - 2e-5_f64.max(0.0), t);
        let (va, _) = self.sd(add(o, mul(d, a)));
        if va >= 0.0 {
            a = t0.max(0.0);
        }
        for _ in 0..40 {
            let m = 0.5 * (a + b);
            if self.sd(add(o, mul(d, m))).0 < 0.0 {
                a = m;
            } else {
                b = m;
            }
        }
        let (_, op) = self.sd(add(o, mul(d, b - 1e-6)));
        Some((b, op))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().cloned().unwrap_or_else(|| ".".into());
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .map(|i| args[i + 1].clone())
    };
    let w: usize = opt("--w").map_or(200, |s| s.parse().unwrap());
    let h: usize = opt("--h").map_or(120, |s| s.parse().unwrap());
    let segs = segments();
    let sc = Scene::of(R, L, &segs, &tools);
    let brute = Brute::new(&segs);
    println!("{} moves, {} pieces", segs.len(), sc.pieces.len());
    // the whole bar; the knurl and the helix nearer (4x); a knurl's flank against the turned
    // bar and the pocket's corner far nearer (25x)
    let views = [
        ("normal", [-20.0, -40.0, 30.0], [-20.0, 0.0, 0.0], 40.0),
        ("close", [-12.0, -14.0, 13.0], [-12.0, 0.0, 6.0], 40.0),
        ("extreme", [-11.4, -2.6, 11.4], [-11.0, 0.0, 9.0], 40.0),
        // (the pocket was cut at A 90: on the bar's +Y side)
        ("pocket", [-20.0, 13.0, 4.0], [-23.0, 8.5, 0.0], 30.0),
    ];
    let only = opt("--views");
    for (name, eye, target, fov) in views {
        if only
            .as_ref()
            .is_some_and(|o| !o.split(',').any(|v| v == name))
        {
            continue;
        }
        let zc = unit(sub(eye, target));
        let right = unit(cross([0.0, 0.0, 1.0], zc));
        let up = cross(zc, right);
        let half = (f64::to_radians(fov) / 2.0).tan();
        let rays: Vec<(usize, [f64; 3])> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let u = (2.0 * (x as f64 + 0.5) / w as f64 - 1.0) * half * w as f64 / h as f64;
                let v = (1.0 - 2.0 * (y as f64 + 0.5) / h as f64) * half;
                (i, unit(add(add(mul(zc, -1.0), mul(right, u)), mul(up, v))))
            })
            .collect();
        let refs: Vec<(f64, f64)> = rays
            .par_iter()
            .map(|&(_, d)| {
                brute
                    .cast(eye, d)
                    .map_or((-1.0, -1.0), |(t, op)| (op as f64, t))
            })
            .collect();
        let mine: Vec<(f64, f64)> = rays
            .par_iter()
            .map(|&(_, d)| {
                sc.cast(eye, d, segs.len() as u32)
                    .map_or((-1.0, -1.0), |hh| {
                        (hh.colour.map_or(0.0, |i| sc.pieces[i].op as f64), hh.t)
                    })
            })
            .collect();
        // the CPU's exact view against the reference: operations differing, beyond a pixel
        let pixel = 2.0 * half / h as f64;
        let (mut diff, mut far, mut deep) = (0, 0, 0);
        let mut worst = 0.0f64;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if mine[i].0 != refs[i].0 {
                    diff += 1;
                    let near = (-1i64..=1).any(|dy| {
                        (-1i64..=1).any(|dx| {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            xx >= 0
                                && yy >= 0
                                && (xx as usize) < w
                                && (yy as usize) < h
                                && refs[yy as usize * w + xx as usize].0 == mine[i].0
                        })
                    });
                    if !near {
                        far += 1;
                    }
                } else if mine[i].0 >= 0.0 {
                    let e = (mine[i].1 - refs[i].1).abs() / (pixel * refs[i].1);
                    worst = worst.max(e);
                    if e > 1.0 {
                        deep += 1;
                    }
                }
            }
        }
        println!("{name}: CPU exact vs reference: {diff} pixels' operation differ, {far} beyond a pixel; depth worst {worst:.3} px, {deep} pixels over 1 px");
        let (pieces, nodes, prims) = sc.packed();
        let tl: Vec<[f64; 3]> = sc
            .tools
            .iter()
            .map(|t| [t.radius, if t.ball { 1.0 } else { 0.0 }, t.cone])
            .collect();
        let r: Vec<f64> = refs.iter().flat_map(|&(op, t)| [op, t]).collect();
        let j = serde_json::json!({
            "radius": R, "length": L, "tools": tl, "count": sc.pieces.len(),
            "pieces": pieces, "nodes": nodes, "prims": prims,
            "camera": {"eye": eye, "target": target, "fov": fov}, "w": w, "h": h, "cut": segs.len(), "ref": r,
        });
        let path = format!("{out}/synth_{name}.json");
        std::fs::write(&path, j.to_string()).unwrap();
        println!("  dumped {path}");
    }
}
