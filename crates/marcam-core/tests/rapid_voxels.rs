//! Rapids replayed against the stock in 3D (voxels), independent of the simulation's radial
//! map: every feed move carves the stock with the tool, every rapid must then pass clear of
//! what is left. (The overhang fixture's indexed 3+1 roughing: 18 rapids came down "just
//! above the level before" onto stock that no level had cut.)

use marcam::job::Job;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn jobs(v: &Value, dir: &Path) -> Vec<(String, Job)> {
    let libs = marcam::profiles::builtin_libraries();
    marcam::project::jobs_from_any_with(&v.to_string(), &libs, &|src| src.load(dir).ok()).unwrap()
}

/// A bar of `radius` from X `-length` to 0 on the A axis, in voxels of `h` mm.
struct Voxels {
    h: f64,
    x0: f64,
    y0: f64,
    nx: usize,
    ny: usize,
    v: Vec<bool>,
}

impl Voxels {
    fn bar(radius: f64, length: f64, h: f64) -> Voxels {
        let (x0, y0) = (-length - 2.0, -radius - 1.0);
        let nx = ((length + 3.0) / h).ceil() as usize;
        let ny = ((2.0 * radius + 2.0) / h).ceil() as usize;
        let mut v = vec![false; nx * ny * ny];
        for i in 0..nx {
            let x = x0 + (i as f64 + 0.5) * h;
            if !(-length..=0.0).contains(&x) {
                continue;
            }
            for j in 0..ny {
                let y = y0 + (j as f64 + 0.5) * h;
                for k in 0..ny {
                    let z = y0 + (k as f64 + 0.5) * h;
                    if y * y + z * z <= radius * radius {
                        v[(i * ny + j) * ny + k] = true;
                    }
                }
            }
        }
        Voxels {
            h,
            x0,
            y0,
            nx,
            ny,
            v,
        }
    }

    /// The voxels inside a flat tool of radius `r` (shrunk by `s`), tip at machine (x, y, z),
    /// `height` up from its tip, the part turned to A `a` degrees (part = Rx(-A) machine):
    /// carved, or counted.
    #[allow(clippy::too_many_arguments)]
    fn visit(
        &mut self,
        r: f64,
        s: f64,
        height: f64,
        x: f64,
        y: f64,
        z: f64,
        a: f64,
        carve: bool,
    ) -> usize {
        let (sn, c) = a.to_radians().sin_cos();
        let rr = r - s;
        // the tool's axis from tip to top, in the part frame
        let ends = [(y, z), (y, z + height)].map(|(my, mz)| (c * my + sn * mz, -sn * my + c * mz));
        let pad = r + self.h;
        let lo = |a: f64, b: f64| a.min(b) - pad;
        let hi = |a: f64, b: f64| a.max(b) + pad;
        let idx =
            |t: f64, o: f64, n: usize| (((t - o) / self.h).floor().max(0.0) as usize).min(n - 1);
        let (i0, i1) = (
            idx(x - pad, self.x0, self.nx),
            idx(x + pad, self.x0, self.nx),
        );
        let (j0, j1) = (
            idx(lo(ends[0].0, ends[1].0), self.y0, self.ny),
            idx(hi(ends[0].0, ends[1].0), self.y0, self.ny),
        );
        let (k0, k1) = (
            idx(lo(ends[0].1, ends[1].1), self.y0, self.ny),
            idx(hi(ends[0].1, ends[1].1), self.y0, self.ny),
        );
        let mut n = 0;
        for i in i0..=i1 {
            let px = self.x0 + (i as f64 + 0.5) * self.h;
            for j in j0..=j1 {
                let py = self.y0 + (j as f64 + 0.5) * self.h;
                for k in k0..=k1 {
                    let at = (i * self.ny + j) * self.ny + k;
                    if !self.v[at] {
                        continue;
                    }
                    let pz = self.y0 + (k as f64 + 0.5) * self.h;
                    // back to the machine frame: m = Rx(A) p
                    let (my, mz) = (c * py - sn * pz, sn * py + c * pz);
                    let dz = mz - z;
                    if dz >= s && dz <= height && (px - x).powi(2) + (my - y).powi(2) <= rr * rr {
                        n += 1;
                        if carve {
                            self.v[at] = false;
                        }
                    }
                }
            }
        }
        n
    }
}

/// Rapid moves (their G-code line) that pass through the stock left by the cuts before them.
fn rapids_through_stock(
    gcode: &str,
    radius: f64,
    length: f64,
    tool_r: f64,
    flutes: f64,
) -> Vec<String> {
    let mut vx = Voxels::bar(radius, length, 0.25);
    let mut p = [0.0, 0.0, 30.0, 0.0];
    let mut hits = Vec::new();
    for line in gcode.lines() {
        let code = line.split('(').next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        let mut q = p;
        let mut any = false;
        for w in code.split_whitespace() {
            let (k, v) = w.split_at(1);
            let Ok(v) = v.parse::<f64>() else { continue };
            let i = match k {
                "X" => 0,
                "Y" => 1,
                "Z" => 2,
                "A" => 3,
                _ => continue,
            };
            q[i] = v;
            any = true;
        }
        if !any {
            continue;
        }
        let rapid = code.starts_with("G0 ") || code == "G0" || code.starts_with("G00");
        let feed = code.starts_with("G1 ") || code.starts_with("G01");
        if rapid || feed {
            let arc = (q[3] - p[3]).to_radians().abs() * radius;
            let len = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2))
                .sqrt()
                .max(arc);
            let n = ((len / if rapid { 0.2 } else { 0.3 }).ceil() as usize).max(1);
            let mut worst = 0;
            for t in 1..=n {
                let f = t as f64 / n as f64;
                let at = [0, 1, 2, 3].map(|i| p[i] + (q[i] - p[i]) * f);
                if rapid {
                    worst =
                        worst.max(vx.visit(tool_r, 0.15, 30.0, at[0], at[1], at[2], at[3], false));
                } else {
                    vx.visit(tool_r, 0.0, flutes, at[0], at[1], at[2], at[3], true);
                }
            }
            if rapid && worst > 0 {
                hits.push(format!("{code} ({worst} voxels)"));
            }
        }
        p = q;
    }
    hits
}

#[test]
fn indexed_roughing_round_the_overhang_fixture_rapids_only_through_air() {
    let dir = root().join("tests/fixtures");
    let v = json!({
        "schema_version": "0.2",
        "name": "overhang",
        "models": [{"id": "part", "name": "overhang_ledge.stl", "kind": "mesh", "file": "overhang_ledge.stl", "axis": "x", "translate": [-20.0, 0.0, 0.0]}],
        "tools": [{"number": 1, "library": "makera-starter", "tool": "spiral-o-metal-3.175x12"}],
        "setups": [{
            "id": "setup1", "name": "Setup 1", "machine": "makera-z1-4axis", "kind": "rotary",
            "clearance": 3.0, "part": ["part"], "spindle_rpm": 12000,
            "stock": {"shape": "cylinder", "radius": 13.0, "length": 35.0},
            "holding": {"kind": "rotary_chuck", "tailstock": false},
            "operations": [{"id": "op1", "type": "rough3d", "tool": 1, "index_a": 0.0, "index_count": 6,
                "x_range": [-20.0, 0.0], "y_range": [-12.0, 12.0], "stepover": 1.25, "sample": 0.32,
                "step_down": 1.6, "floor": 0.0, "leave_z": 0.25, "tolerance": 0.005, "angle": 0.0,
                "pattern": "adaptive", "direction": "climb", "scallop": 0.0, "feed": 300.0,
                "plunge_feed": 60.0, "ramp_angle": 30.0, "smoothing": 0.01}],
            "outputs": [{"name": "rough", "title": "rough", "operations": ["op1"]}]
        }]
    });
    let js = jobs(&v, &dir);
    let job = &js[0].1;
    let mesh = marcam::load_mesh(job, &dir).unwrap();
    let tps = marcam::plan_with_mesh(job, mesh.as_ref()).unwrap();
    let g = marcam::post(&tps[0], &marcam::Machine::makera_z1());
    let hits = rapids_through_stock(&g, 13.0, 35.0, 3.175 / 2.0, 12.0);
    assert!(
        hits.is_empty(),
        "{} rapid(s) through the stock: {:?}",
        hits.len(),
        &hits[..hits.len().min(5)]
    );
    // (and the replay sees a rapid through the stock when there is one)
    let bad = "G0 X-10 Y0 Z20 A0\nG0 Z5\n";
    assert_eq!(
        rapids_through_stock(bad, 13.0, 35.0, 3.175 / 2.0, 12.0).len(),
        1
    );
}
