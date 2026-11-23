//! (Mar's impeller) Roughing on the 4th axis at several A angles in turn (indexed 3+1): the
//! tool upright, Y used, adaptive at each angle from the stock the angles before left.

use marcam::job::Job;
use serde_json::json;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A 4th-axis project round `file` in `dir`: the part placed by `translate`/`rotate`, a bar
/// `radius` x `length`, one 3D roughing at `count` angles (or the layers strategy).
fn project(
    file: &str,
    translate: [f64; 3],
    rotate: [f64; 3],
    radius: f64,
    length: f64,
    op: serde_json::Value,
) -> serde_json::Value {
    json!({
        "schema_version": "0.2", "name": "indexed",
        "models": [{"id": "part", "name": file, "kind": "mesh", "file": file, "axis": "x", "rotate": rotate, "translate": translate}],
        "tools": [{"number": 1, "description": "3.175 flat", "kind": "flat", "diameter": 3.175, "flute_length": 34.0}],
        "setups": [{"id": "s1", "name": "4th", "kind": "rotary", "machine": "makera-z1-4axis", "clearance": 3.0, "part": ["part"], "spindle_rpm": 12000,
            "stock": {"shape": "cylinder", "radius": radius, "length": length},
            "holding": {"kind": "rotary_chuck", "tailstock": false},
            "operations": [op], "outputs": [{"name": "rough", "title": "rough", "operations": ["op1"]}]}]
    })
}

fn job_of(v: &serde_json::Value, dir: &std::path::Path) -> Job {
    let libs = marcam::profiles::builtin_libraries();
    marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(dir).ok())
        .unwrap()
        .remove(0)
        .1
}

#[test]
fn the_impeller_roughed_between_its_blades_at_six_angles() {
    // (local only: Mar's impeller is not in the repository)
    let dir = std::env::var("MARCAM_FIXTURES_LOCAL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root().join("../fixtures-local"));
    if !dir.join("impeller.stl").exists() {
        eprintln!("skipped: no impeller.stl in {}", dir.display());
        return;
    }
    let op = json!({"id": "op1", "type": "rough3d", "tool": 1, "x_range": [-43.0, 0.0], "y_range": [-16.0, 16.0],
        "stepover": 0.8, "sample": 0.3, "step_down": 4.0, "floor": 0.0, "leave_z": 0.15, "leave_xy": 0.15,
        "pattern": "adaptive", "index_count": 6, "feed": 600.0, "plunge_feed": 60.0, "ramp_angle": 3.0});
    let v = project(
        "impeller.stl",
        [-43.0, -24.0, 24.0],
        [0.0, 90.0, 0.0],
        33.0,
        63.0,
        op,
    );
    let job = job_of(&v, &dir);
    let mesh = marcam::load_mesh(&job, &dir).unwrap();
    let t0 = std::time::Instant::now();
    let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
    let m = marcam::profiles::builtin_machine("makera-z1-4axis").unwrap();
    let est = marcam::estimate::estimate_toolpath(&tps[0], &m);
    let gouge = marcam::gouge_check(&job, &tps, mesh.as_ref()).unwrap();
    eprintln!(
        "planned in {:.1} s; {} moves; estimate {:.0} min (feed {:.0} min); gouge {:?}",
        t0.elapsed().as_secs_f64(),
        tps[0].moves.len(),
        est.estimate.total_s / 60.0,
        est.estimate.feed_s / 60.0,
        gouge
            .iter()
            .map(|g| (g.moves_checked, g.max_depth))
            .collect::<Vec<_>>()
    );
    // the tool's clearance above the part (turned to each angle): about the stock to leave
    let mut least = f64::INFINITY;
    let mut a = 0.0;
    let c = marcam::dropcutter::Cutter::Flat {
        radius: 3.175 / 2.0,
    };
    let mut dcs: Vec<(f64, marcam::strategy::three_axis::DropCutter3)> = Vec::new();
    for m in &tps[0].moves {
        if let marcam::Move::Pose {
            tip,
            axis,
            feed: Some(_),
        } = *m
        {
            a = marcam::kinematics::solve_a(axis, a).unwrap();
            let k = (a * 1000.0).round() / 1000.0;
            if !dcs.iter().any(|(q, _)| *q == k) {
                dcs.push((
                    k,
                    marcam::strategy::three_axis::DropCutter3::new(
                        &mesh.as_ref().unwrap().rotated_x(k),
                        c,
                    ),
                ));
            }
            let dc = &dcs.iter().find(|(q, _)| *q == k).unwrap().1;
            let q = marcam::kinematics::to_machine(tip, a);
            least = least.min(q[2] - dc.z_at(q[0], q[1], f64::NEG_INFINITY));
        }
    }
    eprintln!(
        "least clearance over the part: {least:.3} mm; angles {:?}",
        dcs.iter().map(|d| d.0).collect::<Vec<_>>()
    );
    assert!(least > -0.01 && least < 1.0, "{least}");
    assert!(tps[0].moves.len() > 100);
}

/// A stand-in for the impeller: a hub (Ø12) along X with three blades (3 mm thick, out to
/// radius 18), 30 mm long, front at X0; written as a binary STL into `dir`.
fn three_blades(dir: &std::path::Path) {
    let mut tris: Vec<[[f32; 3]; 3]> = Vec::new();
    let mut quad = |a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]| {
        let f = |p: [f64; 3]| [p[0] as f32, p[1] as f32, p[2] as f32];
        tris.push([f(a), f(b), f(c)]);
        tris.push([f(a), f(c), f(d)]);
    };
    // the hub: a 48-sided prism
    let n = 48;
    let rp = |k: usize, r: f64, x: f64| {
        let t = k as f64 / n as f64 * std::f64::consts::TAU;
        [x, r * t.cos(), r * t.sin()]
    };
    for k in 0..n {
        quad(
            rp(k, 6.0, -30.0),
            rp(k + 1, 6.0, -30.0),
            rp(k + 1, 6.0, 0.0),
            rp(k, 6.0, 0.0),
        );
        quad(
            [-30.0, 0.0, 0.0],
            rp(k + 1, 6.0, -30.0),
            rp(k, 6.0, -30.0),
            [-30.0, 0.0, 0.0],
        );
        quad(
            [0.0, 0.0, 0.0],
            rp(k, 6.0, 0.0),
            rp(k + 1, 6.0, 0.0),
            [0.0, 0.0, 0.0],
        );
    }
    // three blades: boxes from the hub out to radius 18, 3 mm thick, X -28..-2
    for b in 0..3 {
        let t = b as f64 / 3.0 * std::f64::consts::TAU;
        let (c, s) = (t.cos(), t.sin());
        let at = |x: f64, r: f64, w: f64| [x, r * c - w * s, r * s + w * c];
        let (x0, x1, r0, r1, w) = (-28.0, -2.0, 5.0, 18.0, 1.5);
        let p = |i: usize| {
            let (x, r, ww) = [
                (x0, r0, -w),
                (x1, r0, -w),
                (x1, r1, -w),
                (x0, r1, -w),
                (x0, r0, w),
                (x1, r0, w),
                (x1, r1, w),
                (x0, r1, w),
            ][i];
            at(x, r, ww)
        };
        for f in [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [2, 3, 7, 6],
            [1, 2, 6, 5],
            [0, 4, 7, 3],
        ] {
            quad(p(f[0]), p(f[1]), p(f[2]), p(f[3]));
        }
    }
    let mut out = vec![0u8; 80];
    out.extend((tris.len() as u32).to_le_bytes());
    for t in &tris {
        out.extend([0u8; 12]);
        for p in t {
            for v in p {
                out.extend(v.to_le_bytes());
            }
        }
        out.extend([0u8; 2]);
    }
    std::fs::write(dir.join("blades.stl"), out).unwrap();
}

#[test]
fn three_blades_roughed_between_them_at_three_angles_far_quicker_than_in_layers() {
    let dir = std::env::temp_dir().join(format!("marcam-blades-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    three_blades(&dir);
    let m = marcam::profiles::builtin_machine("makera-z1-4axis").unwrap();
    let plan = |op: serde_json::Value| {
        let v = project("blades.stl", [0.0; 3], [0.0; 3], 20.0, 40.0, op);
        let job = job_of(&v, &dir);
        let mesh = marcam::load_mesh(&job, &dir).unwrap();
        let tps = marcam::plan_with_mesh(&job, mesh.as_ref()).unwrap();
        let t = marcam::estimate::estimate_toolpath(&tps[0], &m)
            .estimate
            .total_s;
        (job, mesh, tps, t)
    };
    let (job, mesh, tps, t_idx) = plan(
        json!({"id": "op1", "type": "rough3d", "tool": 1, "x_range": [-30.0, 0.0], "y_range": [-12.0, 12.0],
        "stepover": 0.8, "sample": 0.3, "step_down": 4.0, "floor": 0.0, "leave_z": 0.2, "leave_xy": 0.2,
        "pattern": "adaptive", "index_count": 3, "feed": 600.0, "plunge_feed": 60.0, "ramp_angle": 3.0}),
    );
    let gouge = marcam::gouge_check(&job, &tps, mesh.as_ref()).unwrap();
    assert!(
        gouge[0].moves_checked > 100 && gouge[0].max_depth < 0.01,
        "{gouge:?}"
    );
    // three angles, between the blades (each blade at 0, 120, 240 round the part: the tool,
    // upright, over a gap's middle at A 30, 150, 270)
    let mut angles: Vec<f64> = Vec::new();
    let mut a = 0.0;
    for mv in &tps[0].moves {
        if let marcam::Move::Pose { axis, .. } = *mv {
            a = marcam::kinematics::solve_a(axis, a).unwrap();
            let k = (a.rem_euclid(360.0) * 10.0).round() / 10.0;
            if !angles.contains(&k) {
                angles.push(k);
            }
        }
    }
    angles.sort_by(f64::total_cmp);
    assert!(
        angles.len() == 3
            && angles
                .iter()
                .all(|g| ((g - 30.0) / 120.0 - ((g - 30.0) / 120.0).round()).abs() * 120.0 < 6.0),
        "{angles:?}"
    );
    // the layers strategy round the whole bar, for the same part
    let (_, _, _, t_layers) = plan(
        json!({"id": "op1", "type": "rotary_rough", "tool": 1, "x_range": [-30.0, 0.0],
        "step_down": 0.5, "step_x": 0.95, "leave_radial": 0.2, "sample_deg": 6.0, "tolerance": 0.005, "min_radius": 0.0,
        "feed": 600.0, "plunge_feed": 60.0, "ramp_angle": 30.0, "smoothing": 0.01}),
    );
    eprintln!(
        "indexed {:.0} min, layers {:.0} min; angles {angles:?}",
        t_idx / 60.0,
        t_layers / 60.0
    );
    assert!(t_idx < t_layers, "{t_idx} vs {t_layers}");
    let _ = std::fs::remove_dir_all(&dir);
}
