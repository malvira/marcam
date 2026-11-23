//! STEP (ISO 10303-21: AP203, AP214, AP242) import for marcam, with SolidCraft's kernel
//! (`solidcraft-kernel`, MIT OR Apache-2.0, on SolidCore, Apache-2.0; see docs/THIRD-PARTY.md).
//!
//! A STEP file becomes a [`marcam::Mesh`] in millimetres whose triangles carry their B-rep
//! face (`Mesh::faces`, numbered from 1 across every body in the file's order, the same for the
//! same file), so the engine's face regions are the file's faces. Every body (solids and
//! surface models, placed by the file's assemblies) is meshed within a chord tolerance.
//!
//! - [`import`]: the mesh and what came in (bodies, faces, warnings);
//! - [`to_stl`]: the same as binary STL with the faces in the facets' attribute words (what the
//!   web app and the bindings hand to the engine);
//! - [`register`]: makes `marcam::Mesh::from_bytes` (and so every project and job) read STEP.

use marcam::Mesh;

mod fallback;

/// The default chord tolerance (mm): the farthest a facet stands off the surface.
pub const DEFAULT_CHORD: f64 = 0.01;

/// A STEP file read and meshed.
#[derive(Debug, Clone)]
pub struct StepPart {
    pub mesh: Mesh,
    /// Bodies read (solids and surface models).
    pub bodies: usize,
    /// B-rep faces (numbered 1..=faces in `mesh.faces`).
    pub faces: usize,
    /// Each body's name, in the file's order.
    pub body_names: Vec<String>,
    /// What could not be read, or was read only in part.
    pub warnings: Vec<String>,
}

/// Read a STEP file and mesh every body within `chord` mm.
pub fn import(bytes: &[u8], chord: f64) -> Result<StepPart, String> {
    if !marcam::mesh::is_step(bytes) {
        return Err("not a STEP file (no ISO-10303-21 header)".into());
    }
    let text = String::from_utf8_lossy(bytes);
    let imp = solidcraft_kernel::step_import(&text).map_err(|e| format!("STEP: {e}"))?;
    if imp.bodies.is_empty() {
        let why = imp
            .warnings
            .first()
            .cloned()
            .unwrap_or_else(|| "it has no solids or surfaces".into());
        return Err(format!("STEP: nothing to machine in the file ({why})"));
    }
    let chord = if chord.is_finite() && chord > 0.0 {
        chord
    } else {
        DEFAULT_CHORD
    };
    let tol = solidcore_tess::MeshTolerance::chord(chord);
    // each body as the kernel meshes it; a body it cannot mesh is meshed from the file
    let mut tris: Vec<Vec<[[f64; 3]; 3]>> = Vec::with_capacity(imp.bodies.len());
    let mut tri_faces: Vec<Vec<u32>> = Vec::with_capacity(imp.bodies.len());
    let mut failed: Vec<Option<String>> = Vec::with_capacity(imp.bodies.len());
    for b in &imp.bodies {
        match b.body.mesh(&tol) {
            Ok(m) => {
                tris.push(
                    m.triangles
                        .iter()
                        .map(|t| {
                            t.map(|i| {
                                let v = m.positions[i as usize];
                                [v.x, v.y, v.z]
                            })
                        })
                        .collect(),
                );
                tri_faces.push(m.tri_face);
                failed.push(None);
            }
            Err(e) => {
                tris.push(Vec::new());
                tri_faces.push(Vec::new());
                failed.push(Some(e.to_string()));
            }
        }
    }
    // faces the kernel left out (it could not trim them), meshed from the file instead
    let whole: Vec<bool> = failed.iter().map(Option::is_some).collect();
    let (extra, notes) = fallback::left_out(&text, &imp, &tris, &whole, chord);
    let meshed: std::collections::HashSet<u64> = extra.iter().flatten().map(|f| f.face).collect();
    let mut warnings: Vec<String> = imp
        .warnings
        .iter()
        .filter(|w| !names_only(w, &meshed))
        .cloned()
        .collect();
    let mut mesh = Mesh::default();
    let mut names = Vec::new();
    let mut next_face = 0u32;
    for (bi, (b, (t, more))) in imp.bodies.iter().zip(tris.iter().zip(&extra)).enumerate() {
        let tri_face = &tri_faces[bi];
        if let Some(e) = &failed[bi] {
            if more.is_empty() {
                return Err(format!("STEP: body {:?}: {e}", b.name));
            }
            warnings.push(format!(
                "{}: meshed from the file (the STEP kernel could not mesh it: {e})",
                b.name
            ));
        }
        if let Some(why) = &b.refused {
            if !names_only(why, &meshed) {
                warnings.push(format!("{}: read as surfaces ({why})", b.name));
            }
        }
        let base = next_face;
        for (t, f) in t.iter().zip(tri_face) {
            mesh.triangles.push(*t);
            mesh.faces.push(base + f + 1);
        }
        next_face = base + tri_face.iter().copied().max().map_or(0, |f| f + 1);
        for f in more {
            next_face += 1;
            for t in &f.triangles {
                mesh.triangles.push(*t);
                mesh.faces.push(next_face);
            }
            if failed[bi].is_none() {
                warnings.push(format!(
                    "{}: face #{} meshed from the file (the STEP kernel could not trim it)",
                    b.name, f.face
                ));
            }
        }
        names.push(b.name.clone());
    }
    warnings.extend(notes);
    Ok(StepPart {
        bodies: imp.bodies.len(),
        faces: next_face as usize,
        body_names: names,
        warnings,
        mesh,
    })
}

/// Whether a warning is about faces in `meshed` only (each face it names is one of them).
fn names_only(w: &str, meshed: &std::collections::HashSet<u64>) -> bool {
    let mut any = false;
    let mut rest = w;
    while let Some(at) = rest.find("face #") {
        let tail = &rest[at + 6..];
        let n = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        match tail[..n].parse::<u64>() {
            Ok(id) if meshed.contains(&id) => any = true,
            _ => return false,
        }
        rest = &tail[n..];
    }
    any
}

/// [`import`] as binary STL, each facet's attribute word its face (see
/// `marcam::mesh::FACES_HEADER`).
pub fn to_stl(bytes: &[u8], chord: f64) -> Result<Vec<u8>, String> {
    let part = import(bytes, chord)?;
    if part.faces > u16::MAX as usize {
        return Err(format!("STEP: {} faces, more than 65535", part.faces));
    }
    Ok(part.mesh.to_stl())
}

fn read_for_engine(bytes: &[u8]) -> Result<Mesh, String> {
    import(bytes, DEFAULT_CHORD).map(|p| p.mesh)
}

/// Let `marcam::Mesh::from_bytes` read STEP files (meshed at [`DEFAULT_CHORD`]).
pub fn register() {
    marcam::mesh::set_step_reader(read_for_engine);
}

// ------------------------------------------------------------------ WebAssembly
// The web app's STEP module: `ms_alloc` a buffer, write the file into it, `ms_import` it;
// the result is JSON at `ms_result_ptr` / `ms_result_len` and the STL at `ms_stl_ptr` /
// `ms_stl_len` (until the next call).
#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;

    thread_local! {
        static RESULT: RefCell<(Vec<u8>, Vec<u8>)> = const { RefCell::new((Vec::new(), Vec::new())) };
    }

    #[no_mangle]
    pub extern "C" fn ms_alloc(len: usize) -> *mut u8 {
        let mut v = vec![0u8; len];
        let p = v.as_mut_ptr();
        std::mem::forget(v);
        p
    }

    /// # Safety
    /// `p` and `len` from one [`ms_alloc`].
    #[no_mangle]
    pub unsafe extern "C" fn ms_free(p: *mut u8, len: usize) {
        // SAFETY: allocated by ms_alloc with this length.
        drop(unsafe { Vec::from_raw_parts(p, len, len) });
    }

    /// # Safety
    /// `p` and `len` from [`ms_alloc`], holding the file.
    #[no_mangle]
    pub unsafe extern "C" fn ms_import(p: *const u8, len: usize, chord: f64) -> u32 {
        // SAFETY: the caller wrote `len` bytes there.
        let bytes = unsafe { std::slice::from_raw_parts(p, len) };
        let (json, stl) = match super::import(bytes, chord) {
            Ok(part) if part.faces <= u16::MAX as usize => {
                let stl = part.mesh.to_stl();
                let j = serde_json::json!({
                    "bodies": part.bodies, "faces": part.faces,
                    "triangles": part.mesh.triangles.len(),
                    "names": part.body_names, "warnings": part.warnings
                });
                (j, stl)
            }
            Ok(part) => (
                serde_json::json!({"error": format!("STEP: {} faces, more than 65535", part.faces)}),
                Vec::new(),
            ),
            Err(e) => (serde_json::json!({ "error": e }), Vec::new()),
        };
        let ok = json.get("error").is_none();
        RESULT.with(|r| *r.borrow_mut() = (json.to_string().into_bytes(), stl));
        ok as u32
    }

    #[no_mangle]
    pub extern "C" fn ms_result_ptr() -> *const u8 {
        RESULT.with(|r| r.borrow().0.as_ptr())
    }
    #[no_mangle]
    pub extern "C" fn ms_result_len() -> usize {
        RESULT.with(|r| r.borrow().0.len())
    }
    #[no_mangle]
    pub extern "C" fn ms_stl_ptr() -> *const u8 {
        RESULT.with(|r| r.borrow().1.as_ptr())
    }
    #[no_mangle]
    pub extern "C" fn ms_stl_len() -> usize {
        RESULT.with(|r| r.borrow().1.len())
    }
}
