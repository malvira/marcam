//! Custom toolpaths: G-code written elsewhere, read, planned, simulated, checked and posted
//! like marcam's own moves; 3-axis on a vise and on the 4th axis; arcs; verbatim posts; and
//! the post read back gives the same path.

use marcam::gcode_in::{read, Frame};
use marcam::job::Job;
use marcam::sim3::{simulate3, Heightmap3};
use marcam::{cutter_for, limits_for, plan, post, simulate_job, verify_gcode};
use serde_json::{json, Value};

fn project(stock: Value, ops: Value, machine: &str) -> Job {
    let n = ops.as_array().unwrap().len();
    let ids: Vec<String> = (0..n).map(|k| format!("op{}", k + 1)).collect();
    let mut ops = ops;
    for (k, o) in ops.as_array_mut().unwrap().iter_mut().enumerate() {
        o["id"] = json!(ids[k]);
    }
    let doc = json!({
        "schema_version": "0.2", "name": "custom test",
        "models": [{"id": "bar", "kind": "profile", "points": [[-40.0, 12.0], [0.0, 12.0]]}],
        "tools": [
            {"number": 1, "description": "flat 3", "kind": "flat", "diameter": 3.0, "flute_length": 12.0},
            {"number": 2, "description": "V-bit 90", "kind": "vbit", "angle": 90.0, "diameter": 3.0}],
        "setups": [{"id": "s", "name": "s", "machine": machine, "stock": stock, "clearance": 3.0,
                    "part": ["bar"], "spindle_rpm": 12000, "operations": ops,
                    "outputs": [{"name": "out", "title": "out", "operations": ids}]}]
    });
    let libs = marcam::profiles::builtin_libraries();
    let mut jobs = marcam::project::jobs_from_any_with(&doc.to_string(), &libs, &|_| None).unwrap();
    jobs.remove(0).1
}

fn vise_stock() -> Value {
    json!({"shape": "box", "min": [0.0, 0.0, -10.0], "max": [40.0, 30.0, 0.0]})
}

fn custom(gcode: &str, verbatim: bool) -> Value {
    json!([{"type": "custom", "tool": 1, "gcode": gcode, "verbatim": verbatim, "feed": 300.0, "plunge_feed": 200.0}])
}

/// The feed moves' end points of a program, as read (rounded to 1 µm).
fn path_of(text: &str, frame: Frame) -> Vec<[i64; 4]> {
    let p = read(text, frame);
    let r = |v: Option<f64>| (v.unwrap_or(0.0) * 1000.0).round() as i64;
    p.steps
        .iter()
        .filter(|s| !s.rapid)
        .map(|s| [r(s.x), r(s.y), r(s.z), r(s.a)])
        .collect()
}

/// A slot 1 mm deep from (5, 5) to (25, 5), then a half circle by G2 to (25, 15).
const VISE: &str = "%\nG21 G90 G17\nG54\nT1 M6\nS12000 M3\nG0 X5 Y5 Z5\nG1 Z-1 F150\nG1 X25 F400\nG2 X25 Y15 I0 J5\nG0 Z5\nM5\nM30\n%\n";

#[test]
fn a_vise_program_is_planned_posted_clean_and_cuts_where_it_says() {
    let job = project(vise_stock(), custom(VISE, false), "makera-carvera-4axis");
    let tps = plan(&job).unwrap();
    let m = marcam::profiles::builtin_machine("makera-carvera-4axis")
        .unwrap()
        .post;
    let text = post(&tps[0], &m);
    let issues = verify_gcode(&text, &limits_for(&tps[0], &m));
    assert!(issues.is_empty(), "{issues:?}");
    // the work offset noted, as a warning in the program's notes
    assert!(
        text.contains("G54 read as the setup's work offset"),
        "{text}"
    );
    // cut where it says: the slot's middle 1 mm down, the stock untouched beside it
    let mut hm = Heightmap3::new(job.stock_box.as_ref().unwrap(), 0.1);
    let cutter = |n: u32| cutter_for(n, job.tool(n)?.shape).ok();
    let st = simulate3(&mut hm, &tps[0], &cutter, 0.05);
    assert!(st.rapid_hits.is_empty());
    let z = |x: f64, y: f64| hm.at(x, y);
    assert!((z(15.0, 5.0) + 1.0).abs() < 0.05, "{}", z(15.0, 5.0));
    assert!(z(15.0, 2.0).abs() < 1e-6);
    // the arc (G2, clockwise seen from above): through (20, 10), not (30, 10)
    assert!((z(20.0, 10.0) + 1.0).abs() < 0.05, "{}", z(20.0, 10.0));
    assert!(z(30.0, 10.0).abs() < 1e-6, "{}", z(30.0, 10.0));
}

#[test]
fn reposted_it_reads_back_the_same_path() {
    let job = project(vise_stock(), custom(VISE, false), "makera-carvera-4axis");
    let tps = plan(&job).unwrap();
    let m = marcam::profiles::builtin_machine("makera-carvera-4axis")
        .unwrap()
        .post;
    let text = post(&tps[0], &m);
    // the post's feed moves are the program's (its arc as the reader's segments; the post may
    // fold them back into arcs, which read back as the same circle within its tolerance)
    let want = path_of(VISE, Frame::Three);
    let got = path_of(&text, Frame::Three);
    let close = |a: &[i64; 4], b: &[i64; 4]| a.iter().zip(b).all(|(p, q)| (p - q).abs() <= 3);
    assert!(
        close(want.last().unwrap(), got.last().unwrap()),
        "{:?} vs {:?}",
        want.last(),
        got.last()
    );
    assert!(close(&want[0], &got[0]), "{:?} vs {:?}", want[0], got[0]);
    // every point read back lies on the program's path (the slot, or the arc's circle)
    for p in &got {
        let (x, y, zz) = (p[0] as f64 / 1e3, p[1] as f64 / 1e3, p[2] as f64 / 1e3);
        let on_slot = (y - 5.0).abs() < 0.01 && (5.0 - 0.01..=25.01).contains(&x);
        let on_arc = ((x - 25.0).hypot(y - 10.0) - 5.0).abs() < 0.01;
        let plunge = (x - 5.0).abs() < 0.01 && (y - 5.0).abs() < 0.01;
        assert!((on_slot || on_arc || plunge) && zz <= 5.0, "{p:?}");
    }
}

#[test]
fn verbatim_posts_its_own_lines_and_is_still_simulated() {
    let job = project(vise_stock(), custom(VISE, true), "makera-carvera-4axis");
    let tps = plan(&job).unwrap();
    let m = marcam::profiles::builtin_machine("makera-carvera-4axis")
        .unwrap()
        .post;
    let text = post(&tps[0], &m);
    assert!(text.contains("G2 X25 Y15 I0 J5"), "{text}");
    assert!(text.contains("G1 X25 F400"));
    // its end and tape marks left out (the post ends the program)
    let ends = text.lines().filter(|l| l.trim() == "M30").count();
    assert!(ends <= 1, "{text}");
    assert!(!text.lines().any(|l| l.trim() == "%") || m.preamble.iter().any(|l| l == "%"));
    // simulated all the same
    let mut hm = Heightmap3::new(job.stock_box.as_ref().unwrap(), 0.1);
    let cutter = |n: u32| cutter_for(n, job.tool(n)?.shape).ok();
    simulate3(&mut hm, &tps[0], &cutter, 0.05);
    assert!((hm.at(15.0, 5.0) + 1.0).abs() < 0.05);
}

/// On the 4th axis: down to radius 11 at X-5, a helix two turns long to X-25, out.
const ROTARY: &str = "G21 G90\nT1 M6\nG0 X-5 Z16 A0\nG1 Z11 F100\nG1 X-25 A720 F300\nG0 Z16\nM30\n";

#[test]
fn a_4th_axis_program_cuts_its_helix_and_reads_back() {
    let job = project(
        json!({"shape": "cylinder", "length": 40.0, "radius": 12.0}),
        custom(ROTARY, false),
        "makera-carvera-4axis",
    );
    let tps = plan(&job).unwrap();
    let m = marcam::profiles::builtin_machine("makera-carvera-4axis")
        .unwrap()
        .post;
    let text = post(&tps[0], &m);
    let issues = verify_gcode(&text, &limits_for(&tps[0], &m));
    assert!(issues.is_empty(), "{issues:?}");
    let back = path_of(&text, Frame::Rotary);
    let want = path_of(ROTARY, Frame::Rotary);
    assert_eq!(back.last(), want.last(), "{text}");
    // the simulated bar: cut to radius 11 along the helix, untouched past it
    let sim = simulate_job(&job, &tps, None, 0.1, 240, 0.02).unwrap();
    let g = sim.stock.grid;
    let at = |x: f64| {
        (0..g.na)
            .map(|j| sim.stock.at(((x - g.x0) / g.dx) as usize, j) as f64)
            .fold(f64::INFINITY, f64::min)
    };
    assert!((at(-15.0) - 11.0).abs() < 0.1, "{}", at(-15.0));
    assert!((at(-35.0) - 12.0).abs() < 1e-6, "{}", at(-35.0));
}

#[test]
fn what_it_cannot_plan_is_an_error_and_what_it_cannot_read_a_warning() {
    // off the 4th axis: refused, with its line
    let job = project(
        json!({"shape": "cylinder", "length": 40.0, "radius": 12.0}),
        custom("G0 X0 Y3 Z15\nG1 Z10 F100\n", false),
        "makera-carvera-4axis",
    );
    let e = plan(&job).unwrap_err().to_string();
    assert!(
        e.contains("line 1") && e.contains("off the 4th axis"),
        "{e}"
    );
    // a canned cycle: a warning in the program's notes, the rest planned
    let job = project(
        vise_stock(),
        custom(
            "G0 X5 Y5 Z5\nG81 X5 Y5 Z-3 R1 F100\nG1 Z-1 F100\nG1 X10\n",
            false,
        ),
        "makera-carvera-4axis",
    );
    let tps = plan(&job).unwrap();
    assert!(
        tps[0]
            .notes
            .iter()
            .any(|n| n.contains("line 2: G81 not supported")),
        "{:?}",
        tps[0].notes
    );
    // nothing to cut: an error
    let job = project(
        vise_stock(),
        custom("G0 X5 Y5 Z5\n", false),
        "makera-carvera-4axis",
    );
    assert!(plan(&job)
        .unwrap_err()
        .to_string()
        .contains("no feed moves"));
}

#[test]
fn a_plunge_faster_than_its_plunge_feed_is_reported() {
    let ops =
        json!([{"type": "custom", "tool": 1, "gcode": VISE, "feed": 300.0, "plunge_feed": 100.0}]);
    let job = project(vise_stock(), ops, "makera-carvera-4axis");
    let tps = plan(&job).unwrap();
    let m = marcam::profiles::builtin_machine("makera-carvera-4axis")
        .unwrap()
        .post;
    let issues = verify_gcode(&post(&tps[0], &m), &limits_for(&tps[0], &m));
    assert!(
        issues
            .iter()
            .any(|i| format!("{i:?}").contains("FastDescent")),
        "{issues:?}"
    );
}

#[test]
fn json_moves_plan_like_g_code() {
    let ops = json!([{"type": "custom", "tool": 1, "feed": 300.0, "plunge_feed": 100.0, "moves": [
        {"rapid": true, "x": 5.0, "y": 5.0, "z": 5.0}, {"z": -1.0, "f": 150.0}, {"x": 25.0, "f": 400.0}, {"rapid": true, "z": 5.0}]}]);
    let job = project(vise_stock(), ops, "makera-carvera-4axis");
    let tps = plan(&job).unwrap();
    let mut hm = Heightmap3::new(job.stock_box.as_ref().unwrap(), 0.1);
    let cutter = |n: u32| cutter_for(n, job.tool(n)?.shape).ok();
    simulate3(&mut hm, &tps[0], &cutter, 0.05);
    assert!((hm.at(15.0, 5.0) + 1.0).abs() < 0.05);
}
