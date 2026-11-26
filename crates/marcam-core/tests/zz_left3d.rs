//! (ad hoc) the material left on a finished part, in 3D: the stock's surface after every
//! program (a tri-dexel replay), each point's distance to the part's mesh.
use marcam::dexel::{replay, Dexel, ToolSolid};
use std::path::{Path, PathBuf};
fn jobs(v: &serde_json::Value, dir: &Path) -> Vec<(String, marcam::job::Job)> {
    let libs = marcam::profiles::builtin_libraries();
    marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(dir).ok()).unwrap()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
/// Distance from p to triangle (a, b, c) (Ericson's closest point).
fn dist(p: [f64; 3], t: &[[f64; 3]; 3]) -> f64 {
    let (a, b, c) = (t[0], t[1], t[2]);
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    let q = |q: [f64; 3]| {
        let d = sub(p, q);
        dot(d, d).sqrt()
    };
    let lerp = |u: [f64; 3], v: [f64; 3], t: f64| {
        [
            u[0] + (v[0] - u[0]) * t,
            u[1] + (v[1] - u[1]) * t,
            u[2] + (v[2] - u[2]) * t,
        ]
    };
    if d1 <= 0.0 && d2 <= 0.0 {
        return q(a);
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return q(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return q(lerp(a, b, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return q(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return q(lerp(a, c, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return q(lerp(b, c, (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let den = 1.0 / (va + vb + vc);
    let (v, w) = (vb * den, vc * den);
    q([
        a[0] + ab[0] * v + ac[0] * w,
        a[1] + ab[1] * v + ac[1] * w,
        a[2] + ab[2] * v + ac[2] * w,
    ])
}
#[test]
#[ignore]
fn left_in_3d() {
    let dir = PathBuf::from(std::env::var("MARCAM_REPLAY").unwrap());
    let text = std::fs::read_to_string(dir.join("project.toml")).unwrap();
    let (p, _) = marcam::project::Project::from_json_any(&text).unwrap();
    let v = serde_json::to_value(&p).unwrap();
    let js = jobs(&v, &dir);
    let job = &js[0].1;
    let mesh = marcam::load_mesh(job, &dir).unwrap().unwrap();
    let tps = marcam::plan_with_mesh(job, Some(&mesh)).unwrap();
    let mut dx = Dexel::bar(job.stock.radius, job.stock.length, 0.1);
    let tool_for = |t: u32| {
        job.tool(t).map(|d| match d.shape {
            marcam::job::ToolShape::Ball { diameter } => ToolSolid {
                radius: diameter / 2.0,
                ball: true,
                height: 22.0,
                cone: 0.0,
            },
            marcam::job::ToolShape::Flat { diameter } => ToolSolid {
                radius: diameter / 2.0,
                ball: false,
                height: 17.0,
                cone: 0.0,
            },
            _ => ToolSolid {
                radius: 1.5875,
                ball: false,
                height: 17.0,
                cone: 0.0,
            },
        })
    };
    // all but the parting (the part stays on the bar for this)
    let n = tps.len() - 1;
    for tp in &tps[..n] {
        let segs = marcam::sim::segments(std::slice::from_ref(tp));
        replay(&mut dx, &segs, &tool_for, 0.1, 0.15);
    }
    // the part's triangles in 1 mm cells
    let (lo, _) = mesh.bounds().unwrap();
    let cell = 1.0;
    let key = |p: [f64; 3]| {
        [
            ((p[0] - lo[0]) / cell).floor() as i64,
            ((p[1] - lo[1]) / cell).floor() as i64,
            ((p[2] - lo[2]) / cell).floor() as i64,
        ]
    };
    let mut grid: std::collections::HashMap<[i64; 3], Vec<usize>> = Default::default();
    for (i, t) in mesh.triangles.iter().enumerate() {
        let mn = [0, 1, 2].map(|k| t[0][k].min(t[1][k]).min(t[2][k]));
        let mx = [0, 1, 2].map(|k| t[0][k].max(t[1][k]).max(t[2][k]));
        let (a, b) = (key(mn), key(mx));
        for x in a[0]..=b[0] {
            for y in a[1]..=b[1] {
                for z in a[2]..=b[2] {
                    grid.entry([x, y, z]).or_default().push(i);
                }
            }
        }
    }
    let reach = 2i64; // look 2 mm round: farther counts as 2
    let pts = dx.surface_points();
    let x_end = mesh.bounds().unwrap().1[0];
    let mut worst: Vec<(f64, [f64; 3])> = Vec::new();
    let mut over = [0usize; 4];
    for p in pts {
        // only the part's own length (not the bar behind it, left for the parting)
        if p[0] < lo[0] + 0.05 || p[0] > x_end - 0.05 {
            continue;
        }
        let k = key(p);
        let mut best = 2.0f64;
        for x in -reach..=reach {
            for y in -reach..=reach {
                for z in -reach..=reach {
                    if let Some(v) = grid.get(&[k[0] + x, k[1] + y, k[2] + z]) {
                        for &i in v {
                            best = best.min(dist(p, &mesh.triangles[i]));
                        }
                    }
                }
            }
        }
        for (j, t) in [0.05, 0.1, 0.2, 0.5].iter().enumerate() {
            if best > *t {
                over[j] += 1;
            }
        }
        if best > 0.1 {
            worst.push((best, p));
        }
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    eprintln!("surface points over 0.05 / 0.1 / 0.2 / 0.5 mm from the part: {over:?}");
    for (d, p) in worst.iter().take(15) {
        let r = p[1].hypot(p[2]);
        let ang = p[2].atan2(p[1]).to_degrees();
        eprintln!("  {d:.3} mm at X {:.2} r {r:.2} part angle {ang:.1}", p[0]);
    }
    // where: by X band and radius band
    let mut bands: std::collections::BTreeMap<(i64, i64), usize> = Default::default();
    for (_, p) in &worst {
        *bands
            .entry((
                (p[0] / 2.0).floor() as i64 * 2,
                (p[1].hypot(p[2]) / 2.0).floor() as i64 * 2,
            ))
            .or_default() += 1;
    }
    for ((x, r), n) in bands.iter().filter(|b| *b.1 > 50) {
        eprintln!("  X {x}..{} r {r}..{}: {n} points over 0.1", x + 2, r + 2);
    }
}
