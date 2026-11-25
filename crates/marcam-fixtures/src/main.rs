//! Regenerates the part files the examples and tests use. Part generation is a test-fixture
//! concern, not an engine feature: the engine only plans against parts it is given.
//!
//!     cargo run -p marcam-fixtures            (writes the files below)
//!     cargo run -p marcam-fixtures -- --check (fails if a committed file differs)
//!
//! - examples/knob/knob.stl: the knob's revolved profile (720 facets around)
//! - examples/plaque/plaque.png: a grayscale relief (rim, hill, ripples) for the plaque example
//! - examples/plate/dome.stl: a spherical cap (base radius 6, height 4) standing on Z -4.5
//!   at X46 Y20, for the 3-axis plate example
//! - tests/fixtures/picking/bracket.stl: a prismatic bracket with through and blind holes, a
//!   pocket with an island and a step, for the picking tests (see `prism::bracket`)
//! - tests/fixtures/picking/oval_grip.stl: a block with two recessed oval flats (holes in
//!   them) and a textured grip between, for bounded facing and pockets (see `grip`)
//! - tests/fixtures/picking/oval_grip_one_oval.stl: the same with only the left oval, for
//!   replacing a model whose picked pocket is gone

mod grip;
mod prism;
#[cfg(test)]
mod replace_tests;

use marcam::job::Profile;
use marcam::{Job, Mesh};
use std::path::{Path, PathBuf};

/// The knob's revolved profile as a closed binary STL.
pub fn knob_stl(root: &Path) -> Vec<u8> {
    let text = std::fs::read_to_string(root.join("examples/knob/knob.job.json")).expect("knob job");
    let job = Job::from_json(&text).expect("knob job parses");
    Mesh::revolve(job.part.profile.as_ref().expect("profile"), 720).to_stl()
}

/// Spherical cap: base radius 6, height 4 (sphere radius 6.5), axis +Z, base at Z -4.5,
/// centred on X46 Y20.
pub fn dome_mesh() -> Mesh {
    // rings evenly spaced in angle from the base (radius 6) to the apex, so facets stay
    // within 0.001 mm of the sphere
    let (rho, base) = (6.5f64, 2.5f64);
    let top = (6.0f64 / rho).asin();
    let points: Vec<[f64; 2]> = (0..=48)
        .map(|k| {
            let phi = top * (1.0 - k as f64 / 48.0);
            [
                rho * phi.cos() - base,
                if k == 48 { 0.0 } else { rho * phi.sin() },
            ]
        })
        .collect();
    let m = Mesh::revolve(
        &Profile {
            points,
            beyond_front: None,
        },
        180,
    );
    // revolved about X; the cyclic swap (x, y, z) -> (y, z, x) turns X into Z and keeps
    // the winding outward
    Mesh {
        faces: Vec::new(),
        triangles: m
            .triangles
            .iter()
            .map(|t| t.map(|p| [46.0 + p[1], 20.0 + p[2], -4.5 + p[0]]))
            .collect(),
    }
}

/// Grayscale relief for the plaque example (120 x 90 px; on the part 80 x 60 mm): a raised
/// rim, a soft hill on the left and ripples round a point on the right. White is high.
pub fn plaque_png() -> Vec<u8> {
    let (w, h) = (120usize, 90usize);
    let smooth = |e0: f64, e1: f64, x: f64| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut cv = marcam::render::Canvas::new(w, h, [0, 0, 0]);
    for j in 0..h {
        for i in 0..w {
            let (x, y) = (
                (i as f64 + 0.5) * 80.0 / w as f64,
                60.0 - (j as f64 + 0.5) * 60.0 / h as f64,
            );
            let edge = x.min(80.0 - x).min(y).min(60.0 - y);
            let rim = 1.0 - smooth(3.0, 5.0, edge);
            let hill = 0.8 * (-((x - 26.0).powi(2) + (y - 30.0).powi(2)) / (2.0 * 8.0 * 8.0)).exp();
            let r = (x - 56.0).hypot(y - 30.0);
            let ripple = 0.3
                * 0.5
                * (1.0 + (r * std::f64::consts::TAU / 6.0).cos())
                * (1.0 - smooth(10.0, 14.0, r));
            let v = (rim + hill + ripple).clamp(0.0, 1.0);
            let g = (v * 255.0).round() as u8;
            cv.set(i as i64, j as i64, [g, g, g]);
        }
    }
    cv.to_png()
}

fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    vec![
        (root.join("examples/knob/knob.stl"), knob_stl(root)),
        (root.join("examples/plate/dome.stl"), dome_mesh().to_stl()),
        (root.join("examples/plaque/plaque.png"), plaque_png()),
        (
            root.join("tests/fixtures/picking/bracket.stl"),
            prism::bracket().to_stl(),
        ),
        (
            root.join("tests/fixtures/picking/oval_grip.stl"),
            grip::mesh().to_stl(),
        ),
        (
            root.join("tests/fixtures/picking/oval_grip_one_oval.stl"),
            grip::mesh_with(&grip::OVALS[..1]).to_stl(),
        ),
    ]
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let check = std::env::args().any(|a| a == "--check");
    for (path, bytes) in files(&root) {
        if check {
            if std::fs::read(&path).unwrap_or_default() != bytes {
                eprintln!(
                    "{} is out of date: run `cargo run -p marcam-fixtures`",
                    path.display()
                );
                std::process::exit(1);
            }
            println!("{} is up to date", path.display());
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).expect("folder");
            std::fs::write(&path, bytes).expect("write");
            println!("wrote {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_fixtures_match_the_generators() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (path, bytes) in super::files(&root) {
            // (a read that fails says so: on Windows another process holding the file, a
            // test run beside a server or a gate, read as empty was "out of date")
            let cur = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{}: could not read it: {e}", path.display()));
            assert!(cur == bytes, "{} is out of date", path.display());
        }
    }

    #[test]
    fn dome_is_a_closed_cap_on_the_plate() {
        let m = super::dome_mesh();
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for p in m.triangles.iter().flatten() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        assert!(
            (lo[2] + 4.5).abs() < 1e-9 && (hi[2] + 0.5).abs() < 1e-9,
            "{lo:?} {hi:?}"
        );
        assert!((hi[0] - 52.0).abs() < 1e-6 && (lo[1] - 14.0).abs() < 1e-3);
    }
}
