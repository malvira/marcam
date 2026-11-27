//! The exact playback view's pieces and rays on a project (see `marcam::exact`).
//!
//! `cargo run --release -p marcam-core --example exact_view -- <project> [--ppm out.ppm] [--w 320]`
//!
//! For each rotary setup: its moves, the pieces, the hierarchy's size, the time to build it,
//! and a view of the whole job cast on the CPU (rays, point queries per ray, ms); `--ppm`
//! writes that view (shaded, by operation) as an image.

use marcam::exact::Scene;
use marcam::{load_mesh, plan_with_mesh};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

fn main() {
    let libs = marcam::profiles::builtin_libraries();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .map(|i| args[i + 1].clone())
    };
    let path = &args[0];
    let w: usize = opt("--w").map_or(320, |s| s.parse().unwrap());
    let bytes = std::fs::read(path).expect("read");
    let (text, dir) = if marcam::project_file::is_bundle(&bytes) {
        // (a bundle: its files put beside its project, in a folder of their own)
        let (toml, files) = marcam::project_file::read_bundle(&bytes).expect("bundle");
        let dir = std::env::temp_dir().join(format!("exact_view_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, data) in files {
            let f = dir.join(&name);
            if let Some(p) = f.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(f, data).unwrap();
        }
        (toml, dir)
    } else {
        (
            String::from_utf8(bytes).expect("utf-8"),
            Path::new(path).parent().unwrap().to_path_buf(),
        )
    };
    let meshes = |src: &marcam::job::MeshSource| src.load(&dir).ok();
    let jobs = marcam::project::jobs_from_any_with(&text, &libs, &meshes).expect("project");
    for (setup, job) in &jobs {
        if job.stock_box.is_some() || job.carried.is_some() {
            println!("{setup:?}: not a bar (skipped)");
            continue;
        }
        let t = Instant::now();
        let mesh = load_mesh(job, &dir).expect("mesh");
        let tps = plan_with_mesh(job, mesh.as_ref()).expect("plan");
        let plan_ms = t.elapsed().as_millis();
        let segs = marcam::sim::segments(&tps);
        {
            // the moves by kind: straight with A held, turning (radial, rotary), turning poses
            let (mut held, mut rot, mut posed) = (0usize, 0usize, 0usize);
            let mut hist = [0usize; 8];
            let mut total_turn = 0.0f64;
            for s in segs.iter().filter(|s| !s.rapid && s.from.is_some()) {
                let from = s.from.unwrap();
                let turn = match s.index_a {
                    Some(a) => s.a_from.map_or(0.0, |a0| (a - a0).abs()),
                    None => (s.to[2] - from[2]).abs(),
                };
                if turn < 1e-9 {
                    held += 1;
                    continue;
                }
                if s.index_a.is_some() {
                    posed += 1
                } else {
                    rot += 1
                }
                total_turn += turn;
                let b = [0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 45.0]
                    .iter()
                    .position(|&x| turn <= x)
                    .unwrap_or(7);
                hist[b] += 1;
            }
            println!("  held {held}, rotary {rot}, turning poses {posed}; turned {total_turn:.0} deg in all; by degrees <=0.1 .5 1 2 5 10 45 more: {hist:?}");
            if std::env::var("STATS_ONLY").is_ok() {
                continue;
            }
        }
        let t = Instant::now();
        let sc = Scene::of(job.stock.radius, job.stock.length, &segs, &|n| {
            marcam::left3d::solid(job, n)
        });
        let build_ms = t.elapsed().as_millis();
        let cuts = segs.iter().filter(|s| !s.rapid).count();
        println!(
            "{setup:?}: plan {plan_ms} ms, {} segments ({cuts} cutting), {} pieces, {} nodes, built in {build_ms} ms, {:.0} MB",
            segs.len(),
            sc.pieces.len(),
            sc.nodes.len(),
            (sc.pieces.len() * 48 + sc.nodes.len() * 48) as f64 / 1e6
        );
        // a view from the front and above, the whole bar (or `--cam px,py,pz,tx,ty,tz`,
        // `--fov` degrees, as three.js's camera with Z up)
        let (r, l) = (job.stock.radius, job.stock.length);
        let h: usize = opt("--h").map_or(w * 9 / 16, |s| s.parse().unwrap());
        let (eye, target) = match opt("--cam") {
            Some(c) => {
                let v: Vec<f64> = c.split(',').map(|x| x.parse().unwrap()).collect();
                ([v[0], v[1], v[2]], [v[3], v[4], v[5]])
            }
            None => (
                [-l / 2.0, -2.2 * l.max(2.0 * r), 1.2 * l.max(2.0 * r)],
                [-l / 2.0, 0.0, 0.0],
            ),
        };
        let fov: f64 = opt("--fov").map_or(40.0, |s| s.parse().unwrap());
        let unit = |a: [f64; 3]| {
            let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
            [a[0] / n, a[1] / n, a[2] / n]
        };
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let zc = unit([eye[0] - target[0], eye[1] - target[1], eye[2] - target[2]]);
        let right = unit(cross([0.0, 0.0, 1.0], zc));
        let up = cross(zc, right);
        let fwd = [-zc[0], -zc[1], -zc[2]];
        let half = (fov.to_radians() / 2.0).tan();
        let _ = (l, r);
        let t = Instant::now();
        let mut img = vec![0u8; w * h * 3];
        let (mut rays, mut steps, mut most) = (0u64, 0u64, 0u32);
        let mut tests = [0u64; 2];
        let ncut: u32 = opt("--cut").map_or(segs.len() as u32, |s| s.parse().unwrap());
        // per pixel: the operation seen (-1 nothing, 0 the bar's own) and the ray's t
        let mut cut_ref: Vec<f64> = vec![-1.0; w * h * 2];
        for y in 0..h {
            for x in 0..w {
                let u = (2.0 * (x as f64 + 0.5) / w as f64 - 1.0) * half * w as f64 / h as f64;
                let _ = &cut_ref;
                let v = (1.0 - 2.0 * (y as f64 + 0.5) / h as f64) * half;
                let d = unit([
                    fwd[0] + u * right[0] + v * up[0],
                    fwd[1] + u * right[1] + v * up[1],
                    fwd[2] + u * right[2] + v * up[2],
                ]);
                let hit = if args.iter().any(|a| a == "--noref") {
                    None
                } else {
                    sc.cast(eye, d, ncut)
                };
                if let Some(hit) = hit {
                    cut_ref[2 * (y * w + x)] = hit.colour.map_or(0.0, |i| sc.pieces[i].op as f64);
                    cut_ref[2 * (y * w + x) + 1] = hit.t;
                }
                if let Some(hit) = hit {
                    rays += 1;
                    steps += hit.steps as u64;
                    tests[0] += hit.tests[0] as u64;
                    tests[1] += hit.tests[1] as u64;
                    most = most.max(hit.steps);
                    let base = match hit.colour {
                        None => [150.0, 160.0, 175.0],
                        Some(i) => {
                            let op = sc.pieces[i].op as u32;
                            let hue = (op * 97 % 360) as f64;
                            let c = |n: f64| {
                                let k = (n + hue / 30.0) % 12.0;
                                0.5 - 0.5 * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
                            };
                            [255.0 * c(0.0), 255.0 * c(8.0), 255.0 * c(4.0)]
                        }
                    };
                    let light = unit([0.3, -0.6, 0.75]);
                    let n = hit.normal;
                    let k = 0.25
                        + 0.75 * (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
                    for c in 0..3 {
                        img[3 * (y * w + x) + c] = (base[c] * k).min(255.0) as u8;
                    }
                }
            }
        }
        println!(
            "  cast {w}x{h}: {} ms, {rays} rays on the stock, {:.1} point queries a ray (most {most}), {:.1} straight and {:.1} turning pieces looked at a ray",
            t.elapsed().as_millis(),
            steps as f64 / rays.max(1) as f64,
            tests[0] as f64 / rays.max(1) as f64,
            tests[1] as f64 / rays.max(1) as f64
        );
        if let Some(out) = opt("--dump") {
            // the scene as the GPU takes it, the camera, and what the CPU saw at each pixel
            let (pieces, nodes, prims) = sc.packed();
            let tools: Vec<[f64; 3]> = sc
                .tools
                .iter()
                .map(|t| [t.radius, if t.ball { 1.0 } else { 0.0 }, t.cone])
                .collect();
            let j = serde_json::json!({
                "radius": sc.radius, "length": sc.length, "tools": tools, "count": sc.pieces.len(),
                "pieces": pieces, "nodes": nodes, "prims": prims,
                "camera": {"eye": eye, "target": target, "fov": fov}, "w": w, "h": h, "cut": ncut,
                "ref": cut_ref,
            });
            std::fs::write(&out, j.to_string()).unwrap();
            println!("  dumped {out}");
        }
        if let Some(out) = opt("--ppm") {
            let mut f = std::fs::File::create(&out).unwrap();
            write!(f, "P6\n{w} {h}\n255\n").unwrap();
            f.write_all(&img).unwrap();
            println!("  wrote {out}");
        }
    }
}
