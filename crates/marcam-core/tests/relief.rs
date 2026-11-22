//! Relief milling: parallel finishing (angle, one-way, scallop target) and 3D roughing, on a
//! plane and on the plaque example built from a grayscale image.

use marcam::dropcutter::Cutter;
use marcam::job::{Raster3d, StockBox};
use marcam::sim3::{deviation3, part_top, simulate3, Heightmap3};
use marcam::strategy::three_axis::{raster3d, raster_stepover, DropCutter3};
use marcam::toolpath::Builder;
use marcam::{cutter_for, limits_for, load_mesh, plan_with_mesh, post, verify_gcode, Job, Move};
use marcam::{Mesh, Toolpath};
use std::path::PathBuf;

fn plane(z: f64) -> Mesh {
    let (a, b, c, d) = (
        [-5.0, -5.0, z],
        [25.0, -5.0, z],
        [25.0, 25.0, z],
        [-5.0, 25.0, z],
    );
    Mesh {
        faces: Vec::new(),
        triangles: vec![[a, b, c], [a, c, d]],
    }
}

fn finish(angle: f64, pattern: &str, direction: &str, scallop: f64) -> Raster3d {
    Raster3d {
        tool: 1,
        index_a: 0.0,
        x_range: [0.0, 12.0],
        y_range: [0.0, 12.0],
        stepover: 1.0,
        sample: 0.2,
        step_down: 5.0,
        floor: -3.0,
        leave_z: 0.0,
        leave_xy: 0.0,
        tolerance: 0.005,
        angle,
        pattern: pattern.into(),
        direction: direction.into(),
        scallop,
        faces: Vec::new(),
        rest: false,
        lead_in: 0.0,
        min_slope: 0.0,
        rest_tool: 0,
        feed: 600.0,
        plunge_feed: 60.0,
        ramp_angle: marcam::job::default_ramp_angle(),
        smoothing: Some(0.0),
    }
}

fn toolpath(b: Builder) -> Toolpath {
    Toolpath {
        name: "t".into(),
        title: "t".into(),
        tool: None,
        notes: vec![],
        reset_a: false,
        safe_r: 3.0,
        moves: b.moves,
        origin_comment: None,
        plunge_limit: None,
        ramp_angle: None,
        post: None,
    }
}

fn block() -> StockBox {
    StockBox {
        min: [-2.0, -2.0, -6.0],
        max: [14.0, 14.0, -1.0],
    }
}

/// Cut a finish over the plane z = -1 (the top of the block) and return the block.
fn cut_plane(op: &Raster3d, cell: f64) -> (Heightmap3, Toolpath) {
    let ball = Cutter::Ball { radius: 1.5875 };
    let dc = DropCutter3::new(&plane(-2.0), ball);
    let mut b = Builder::new();
    b.tool_change(1, 10000);
    b.operation(0, "raster3d");
    raster3d(&mut b, &dc, &block(), 3.0, op).unwrap();
    let tp = toolpath(b);
    let mut hm = Heightmap3::new(&block(), cell);
    let st = simulate3(&mut hm, &tp, &|_| Some(ball), 0.05);
    assert!(st.rapid_hits.is_empty(), "rapids through stock");
    (hm, tp)
}

#[test]
fn scallop_target_sets_the_stepover_and_holds_on_a_plane() {
    let ball = Cutter::Ball { radius: 1.5875 };
    let op = finish(30.0, "zigzag", "climb", 0.01);
    let s = raster_stepover(&op, &ball);
    assert!((s - 2.0 * (0.01f64 * (3.175 - 0.01)).sqrt()).abs() < 1e-12);
    let (hm, _) = cut_plane(&op, 0.05);
    // inside the passes (away from the range ends) the floor is z -2 plus the scallop
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for j in 0..hm.ny {
        for i in 0..hm.nx {
            let (x, y) = hm.centre(i, j);
            if (2.0..10.0).contains(&x) && (2.0..10.0).contains(&y) {
                let z = hm.z[j * hm.nx + i];
                lo = lo.min(z);
                hi = hi.max(z);
            }
        }
    }
    assert!(lo > -2.0 - 1e-6, "cut below the plane: {lo}");
    assert!(
        hi - (-2.0) <= 0.0101,
        "scallop {:.4} over the 0.01 target",
        hi + 2.0
    );
    assert!(
        hi - (-2.0) > 0.006,
        "scallop {:.4}: the stepover is needlessly fine",
        hi + 2.0
    );
}

#[test]
fn passes_follow_the_angle_and_one_way_climb_runs_against_u() {
    // one-way climb at 30 deg: every cutting pass runs along -(cos 30, sin 30)
    let (_, tp) = cut_plane(&finish(30.0, "one_way", "climb", 0.01), 0.2);
    let u = [30f64.to_radians().cos(), 30f64.to_radians().sin()];
    let mut p: Option<[f64; 3]> = None;
    let (mut along, mut against) = (0, 0);
    for m in &tp.moves {
        if let Move::Line3 { x, y, z, .. } = *m {
            if let Some(q) = p {
                let (dx, dy) = (x - q[0], y - q[1]);
                let len = dx.hypot(dy);
                if len > 0.1 && (z - q[2]).abs() < 1e-9 {
                    let c = (dx * u[0] + dy * u[1]) / len;
                    if c > 0.999 {
                        along += 1;
                    } else if c < -0.999 {
                        against += 1;
                    }
                }
            }
            p = Some([x, y, z]);
        } else if let Move::Rapid3 { x, y, z } = *m {
            let q = p.unwrap_or([0.0; 3]);
            p = Some([x.unwrap_or(q[0]), y.unwrap_or(q[1]), z.unwrap_or(q[2])]);
        }
    }
    assert!(
        along == 0 && against > 100,
        "along {along}, against {against}"
    );
    // conventional runs the other way
    let (_, tp) = cut_plane(&finish(30.0, "one_way", "conventional", 0.01), 0.2);
    let firsts: Vec<_> = tp
        .moves
        .iter()
        .filter_map(|m| match *m {
            Move::Line3 { x, y, .. } => Some((x, y)),
            _ => None,
        })
        .take(3)
        .collect();
    let (dx, dy) = (firsts[2].0 - firsts[1].0, firsts[2].1 - firsts[1].1);
    assert!(dx * u[0] + dy * u[1] > 0.0, "{firsts:?}");
}

fn plaque() -> (Job, Mesh) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plaque");
    let text = std::fs::read_to_string(dir.join("plaque.marcam.toml")).unwrap();
    let libs = marcam::profiles::builtin_libraries();
    let mut jobs =
        marcam::project::jobs_from_any_with(&text, &libs, &|src| src.load(&dir).ok()).unwrap();
    let job = jobs.remove(0).1;
    let mesh = load_mesh(&job, &dir).unwrap().expect("relief mesh");
    (job, mesh)
}

#[test]
fn plaque_from_an_image_roughs_then_finishes_to_the_relief() {
    let (job, mesh) = plaque();
    let (lo, hi) = mesh.bounds().unwrap();
    assert!(
        (lo[2] + 4.0).abs() < 0.02 && (hi[2] + 1.0).abs() < 0.02,
        "{lo:?} {hi:?}"
    );
    let tps = plan_with_mesh(&job, Some(&mesh)).unwrap();
    let profile = marcam::profiles::builtin_machine("makera-carvera-4axis").unwrap();
    for tp in &tps {
        let text = post(tp, &profile.post);
        let issues = verify_gcode(&text, &limits_for(tp, &profile.post));
        assert!(
            issues.is_empty(),
            "{}: {:?}",
            tp.name,
            &issues[..issues.len().min(3)]
        );
    }
    assert!(marcam::profiles::check_limits(&job, &tps, &profile).is_empty());

    let mut hm = Heightmap3::new(job.stock_box.as_ref().unwrap(), 0.25);
    let part = part_top(&hm, &mesh);
    let cutter = |n: u32| {
        let t = job.tools.iter().find(|t| t.number == n)?;
        cutter_for(n, t.shape).ok()
    };
    // roughing leaves stock everywhere over the relief (0.25, a little less between the
    // flat tool's sample points), never cuts into it
    let st = simulate3(&mut hm, &tps[0], &cutter, 0.1);
    assert!(st.rapid_hits.is_empty());
    let d = deviation3(&hm, &part, 0.02);
    assert_eq!(d.gouge_cells, 0, "{d:?}");
    let min_left = part
        .iter()
        .enumerate()
        .filter_map(|(k, p)| p.map(|p| hm.z[k] - p))
        .fold(f64::INFINITY, f64::min);
    assert!(min_left > 0.2, "roughing left only {min_left:.3}");
    // the finish takes it to the relief: no gouge beyond the facets, little left
    let st = simulate3(&mut hm, &tps[1], &cutter, 0.1);
    assert!(st.rapid_hits.is_empty());
    let d = deviation3(&hm, &part, 0.02);
    assert_eq!(d.gouge_cells, 0, "{d:?}");
    assert!(d.mean_left < 0.05, "{d:?}");
    eprintln!(
        "plaque: {} cells, max left {:.3} (in concave corners the ball cannot reach), mean {:.4}, deepest {:.4}",
        d.cells, d.max_left, d.mean_left, d.max_gouge
    );
}

#[test]
fn image_wrapped_round_the_bar_roughs_and_finishes_on_the_rotary() {
    use marcam::job::{Operation, Program};
    use marcam::ops::{defaults, DefaultsContext};
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plaque");
    let doc = serde_json::json!({
        "schema_version": "0.2", "name": "wrapped relief",
        "models": [{"id": "wrap", "kind": "mesh", "file": "plaque.png", "translate": [-45.0, 0.0, 0.0],
                    "image": {"width": 40.0, "height": 30.0, "depth": 1.5, "base": 10.0, "wrap": true}}],
        "tools": [
            {"number": 1, "description": "flat 3.175", "kind": "flat", "diameter": 3.175, "flute_length": 12.0},
            {"number": 3, "description": "ball 3.175", "kind": "ball", "diameter": 3.175, "flute_length": 12.0}],
        "setups": [{"id": "s", "name": "bar", "machine": "makera-z1-4axis",
                    "stock": {"shape": "cylinder", "radius": 12.0, "length": 60.0}, "clearance": 3.0,
                    "part": ["wrap"], "spindle_rpm": 12000, "operations": [], "outputs": []}]
    });
    let libs = marcam::profiles::builtin_libraries();
    let mut jobs =
        marcam::project::jobs_from_any_with(&doc.to_string(), &libs, &|s| s.load(&dir).ok())
            .unwrap();
    let mut job = jobs.remove(0).1;
    let mesh = load_mesh(&job, &dir).unwrap().unwrap();
    let (lo, hi) = mesh.bounds().unwrap();
    assert!(
        (lo[0] + 44.83).abs() < 0.01 && (hi[0] + 5.17).abs() < 0.01,
        "{lo:?} {hi:?}"
    );
    let z1 = marcam::profiles::builtin_machine("makera-z1-4axis").unwrap();
    let op = |kind: &str, tool: u32| -> Operation {
        let v = defaults(
            kind,
            &DefaultsContext {
                job: &job,
                regions: None,
                machine: Some(&z1),
                tool: Some(tool),
                boundary: Some(marcam::geometry::Boundary {
                    x: Some([-44.0, -6.0]),
                    a: None,
                }),
                part_xy: None,
                part_z: None,
            },
        )
        .unwrap();
        serde_json::from_value(v).unwrap()
    };
    let (rough, fin) = (op("rotary_rough", 1), op("rotary_finish", 3));
    job.programs = vec![
        Program {
            name: "rough".into(),
            title: "rough".into(),
            notes: vec![],
            reset_a: false,
            operations: vec![rough],
        },
        Program {
            name: "finish".into(),
            title: "finish".into(),
            notes: vec![],
            reset_a: true,
            operations: vec![fin],
        },
    ];
    let tps = plan_with_mesh(&job, Some(&mesh)).unwrap();
    for tp in &tps {
        let text = post(tp, &z1.post);
        let issues = verify_gcode(&text, &limits_for(tp, &z1.post));
        if let Some(i) = issues.first() {
            let l: Vec<&str> = text.lines().collect();
            eprintln!(
                "{}",
                l[i.line - 4..i.line + 1].join(
                    "
"
                )
            );
        }
        assert!(
            issues.is_empty(),
            "{}: {} issues, {:?}",
            tp.name,
            issues.len(),
            &issues[..issues.len().min(4)]
        );
    }
    let res = marcam::simulate_job(&job, &tps, Some(&mesh), 0.1, 720, 0.02).unwrap();
    let (_, st, dev) = &res.programs[1];
    assert!(st.rapid_hits.is_empty());
    assert_eq!(dev.gouge_cells, 0, "{dev:?}");
    assert!(dev.mean_left < 0.08, "{dev:?}");
    eprintln!(
        "wrapped relief: max left {:.3}, mean {:.4}, deepest {:.4}",
        dev.max_left, dev.mean_left, dev.max_gouge
    );
}
