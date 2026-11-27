//! Triangle meshes: STL (binary and ASCII) and 3MF import, binary STL export, axis mapping, and
//! revolving a profile into a closed mesh.

use crate::det::Det;
use crate::job::Profile;
use std::fmt;

pub type Vec3 = [f64; 3];
pub type Triangle = [Vec3; 3];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub triangles: Vec<Triangle>,
    /// The B-rep face each triangle belongs to (a STEP import, 1-based), or empty: the mesh's
    /// faces are then found from its sharp edges ([`crate::geometry::face_regions`]).
    pub faces: Vec<u32>,
}

/// Binary STL files whose facets' attribute words carry their B-rep face (STEP imports,
/// see the `marcam-step` crate) start with this header.
pub const FACES_HEADER: &[u8] = b"marcam STEP faces";

/// The start of several part files given together (a project whose setups have different
/// parts): this line, then for each file its name's length, its name, its length and its
/// bytes (lengths as 4-byte little-endian numbers).
pub const BUNDLE_MAGIC: &[u8] = b"MARCAM-MESHES\n";

fn take<'a>(n: usize, rest: &mut &'a [u8]) -> Result<&'a [u8], MeshError> {
    if rest.len() < n {
        return Err(MeshError::Format("a bundle of part files cut short".into()));
    }
    let (a, b) = rest.split_at(n);
    *rest = b;
    Ok(a)
}

/// One file of a bundle: its name and its bytes.
pub type BundleFile<'a> = (String, &'a [u8]);

/// The files of a bundle ([`BUNDLE_MAGIC`]) as (name, bytes), or None for one plain file.
pub fn bundle(data: &[u8]) -> Result<Option<Vec<BundleFile<'_>>>, MeshError> {
    let Some(mut rest) = data.strip_prefix(BUNDLE_MAGIC) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    while !rest.is_empty() {
        let n = u32::from_le_bytes(take(4, &mut rest)?.try_into().unwrap()) as usize;
        let name = String::from_utf8_lossy(take(n, &mut rest)?).into_owned();
        let m = u32::from_le_bytes(take(4, &mut rest)?.try_into().unwrap()) as usize;
        out.push((name, take(m, &mut rest)?));
    }
    Ok(Some(out))
}

/// The bytes of part file `file`: `data` itself, or that file of a bundle (by name, or by its
/// name without folders).
pub fn bundle_file<'a>(data: &'a [u8], file: &str) -> Result<&'a [u8], MeshError> {
    let Some(files) = bundle(data)? else {
        return Ok(data);
    };
    let base = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
    files
        .iter()
        .find(|(n, _)| n == file)
        .or_else(|| files.iter().find(|(n, _)| base(n) == base(file)))
        .map(|(_, b)| *b)
        .ok_or_else(|| {
            MeshError::Format(format!(
                "no part file {file:?} given (given: {})",
                files
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

#[derive(Debug, Clone, PartialEq)]
pub enum MeshError {
    Io(String),
    Format(String),
}

impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshError::Io(s) => write!(f, "{s}"),
            MeshError::Format(s) => write!(f, "bad mesh file: {s}"),
        }
    }
}

impl std::error::Error for MeshError {}

fn bad(msg: impl Into<String>) -> MeshError {
    MeshError::Format(msg.into())
}

/// Which mesh axis becomes the rotary (X) axis. Cyclic, so handedness is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    #[default]
    X,
    Y,
    Z,
}

/// A STEP file's bytes to a mesh with its B-rep faces.
pub type StepReader = fn(&[u8]) -> Result<Mesh, String>;

/// Reads a STEP file into a mesh with its B-rep faces: set by a program built with STEP
/// support (the `marcam-step` crate's `register`), none otherwise.
static STEP_READER: std::sync::OnceLock<StepReader> = std::sync::OnceLock::new();

/// Install the STEP reader (see `marcam_step::register`). The first one stays.
pub fn set_step_reader(f: StepReader) {
    let _ = STEP_READER.set(f);
}

/// Whether `data` is a STEP (ISO 10303-21) file.
pub fn is_step(data: &[u8]) -> bool {
    let head = &data[..data.len().min(256)];
    let text = String::from_utf8_lossy(head);
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("ISO-10303-21")
}

impl Mesh {
    /// Load from bytes; the format is detected from the content (3MF zip, binary or ASCII STL,
    /// and STEP where a STEP reader is installed).
    pub fn from_bytes(data: &[u8]) -> Result<Mesh, MeshError> {
        // several files (each setup's part): the first, when nothing says which
        if let Some(first) = bundle(data)?.and_then(|b| b.into_iter().next()) {
            return Mesh::from_bytes(first.1);
        }
        if data.starts_with(b"PK\x03\x04") {
            read_3mf(data)
        } else if is_step(data) {
            match STEP_READER.get() {
                Some(read) => read(data).map_err(MeshError::Format),
                None => Err(bad(
                    "a STEP file: this build reads STL and 3MF (STEP needs the step feature)",
                )),
            }
        } else {
            read_stl(data)
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Mesh, MeshError> {
        let data =
            std::fs::read(path).map_err(|e| MeshError::Io(format!("{}: {e}", path.display())))?;
        Mesh::from_bytes(&data).map_err(|e| match e {
            MeshError::Format(s) => MeshError::Format(format!("{}: {s}", path.display())),
            other => other,
        })
    }

    /// Axis-aligned bounding box `(min, max)`.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut it = self.triangles.iter().flatten();
        let first = *it.next()?;
        Some(it.fold((first, first), |(mut lo, mut hi), p| {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
            (lo, hi)
        }))
    }

    /// Where the part turns about, across X (its Y and Z): for a part round about an axis
    /// along X or the same every 1/k of a turn about it (k from 2 to 24: an impeller's blades),
    /// that axis, through its volume's centroid; else `None` (the box's middle is all there
    /// is). (Mar's 5-blade impeller: centred by its box, its hub 0.5 mm off the axis.) Round
    /// or repeating: its outermost radius round the centroid, a degree at a time, within 3 % of
    /// its largest (round), or the same within that a k-th of a turn on.
    pub fn turning_axis(&self) -> Option<[f64; 2]> {
        // the volume's centroid (a closed mesh: each triangle's tetrahedron with the origin)
        let (mut v, mut cy, mut cz) = (0.0, 0.0, 0.0);
        for t in &self.triangles {
            let [a, b, c] = *t;
            let d = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
            v += d;
            cy += d * (a[1] + b[1] + c[1]);
            cz += d * (a[2] + b[2] + c[2]);
        }
        let (lo, hi) = self.bounds()?;
        let size = (hi[1] - lo[1]).max(hi[2] - lo[2]).max(1e-9);
        if v.abs() < 1e-9 * size.powi(3) {
            return None;
        }
        let (cy, cz) = (cy / (4.0 * v), cz / (4.0 * v));
        if !(cy.is_finite() && cz.is_finite()) {
            return None;
        }
        // its outermost radius round it, a degree at a time
        const BINS: usize = 360;
        let mut outer = vec![f64::NAN; BINS];
        for p in self.triangles.iter().flatten() {
            let (y, z) = (p[1] - cy, p[2] - cz);
            let r = y.hypot(z);
            // (a vertex on the axis, an end's middle, points no way)
            if r < 1e-3 * size {
                continue;
            }
            let k = (((z.atan2(y).to_degrees() + 360.0) % 360.0) as usize).min(BINS - 1);
            if outer[k].is_nan() || outer[k] < r {
                outer[k] = r;
            }
        }
        // (a bin no vertex fell in: the larger of the nearest ones each side)
        if outer.iter().all(|r| r.is_nan()) {
            return None;
        }
        let known = outer.clone();
        let near = |k: usize, step: usize| -> f64 {
            (1..BINS)
                .map(|o| known[(k + step * o) % BINS])
                .find(|r| !r.is_nan())
                .unwrap_or(0.0)
        };
        for k in 0..BINS {
            if known[k].is_nan() {
                outer[k] = near(k, 1).max(near(k, BINS - 1));
            }
        }
        let big = outer.iter().copied().fold(0.0, f64::max);
        let tol = 0.03 * big;
        let small = outer.iter().copied().fold(f64::INFINITY, f64::min);
        if big - small <= tol {
            return Some([cy, cz]);
        }
        // (a k-th of a turn on, the same within 3 %: on average and nearly everywhere; a degree's
        // slack each side, a tip falling either side of a bin's edge)
        for k in 2..=24usize {
            let shift = (BINS as f64 / k as f64).round() as usize;
            let diffs: Vec<f64> = (0..BINS)
                .map(|i| {
                    let j = (i + shift) % BINS;
                    let w = [
                        outer[(j + BINS - 1) % BINS],
                        outer[j],
                        outer[(j + 1) % BINS],
                    ];
                    let (lo, hi) = (
                        w.iter().copied().fold(f64::INFINITY, f64::min),
                        w.iter().copied().fold(0.0, f64::max),
                    );
                    (lo - outer[i]).max(outer[i] - hi).max(0.0)
                })
                .collect();
            let mean = diffs.iter().sum::<f64>() / BINS as f64;
            let mut d = diffs;
            d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if mean <= 0.5 * tol && d[(0.95 * BINS as f64) as usize] <= tol {
                return Some([cy, cz]);
            }
        }
        None
    }

    /// Largest distance of any vertex from the X axis.
    pub fn max_radius(&self) -> f64 {
        self.triangles
            .iter()
            .flatten()
            .map(|p| (p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(0.0, f64::max)
    }

    /// Re-map so `axis` becomes X, then translate.
    pub fn oriented(mut self, axis: Axis, translate: Vec3) -> Mesh {
        for t in &mut self.triangles {
            for p in t.iter_mut() {
                let q = match axis {
                    Axis::X => *p,
                    Axis::Y => [p[1], p[2], p[0]],
                    Axis::Z => [p[2], p[0], p[1]],
                };
                *p = [
                    q[0] + translate[0],
                    q[1] + translate[1],
                    q[2] + translate[2],
                ];
            }
        }
        self
    }

    /// [`Mesh::oriented`] with a half `turn` between the axis mapping and the translation (a
    /// turn that flips the part over keeps its triangles facing out).
    pub fn oriented_turned(self, axis: Axis, turn: crate::job::Turn, translate: Vec3) -> Mesh {
        if turn.is_none() {
            return self.oriented(axis, translate);
        }
        let mut m = self.oriented(axis, [0.0; 3]);
        for t in &mut m.triangles {
            for p in t.iter_mut() {
                let q = turn.apply(*p);
                *p = [
                    q[0] + translate[0],
                    q[1] + translate[1],
                    q[2] + translate[2],
                ];
            }
        }
        m
    }

    /// [`Mesh::oriented_turned`] with a rotation between the axis mapping and the turn:
    /// `rotate` degrees about X, then Y, then Z (right-handed). Quarter turns are exact.
    pub fn oriented_rotated(
        self,
        axis: Axis,
        rotate: Vec3,
        turn: crate::job::Turn,
        translate: Vec3,
    ) -> Mesh {
        if rotate == [0.0; 3] {
            return self.oriented_turned(axis, turn, translate);
        }
        // sine and cosine, exact at quarter turns
        let sc = |deg: f64| -> (f64, f64) {
            let q = deg / 90.0;
            if q.fract() == 0.0 {
                match (q as i64).rem_euclid(4) {
                    0 => (0.0, 1.0),
                    1 => (1.0, 0.0),
                    2 => (0.0, -1.0),
                    _ => (-1.0, 0.0),
                }
            } else {
                deg.to_radians().dsin_cos()
            }
        };
        let (sx, cx) = sc(rotate[0]);
        let (sy, cy) = sc(rotate[1]);
        let (sz, cz) = sc(rotate[2]);
        let r = |p: Vec3| -> Vec3 {
            let p = [p[0], cx * p[1] - sx * p[2], sx * p[1] + cx * p[2]];
            let p = [cy * p[0] + sy * p[2], p[1], -sy * p[0] + cy * p[2]];
            [cz * p[0] - sz * p[1], sz * p[0] + cz * p[1], p[2]]
        };
        let mut m = self.oriented(axis, [0.0; 3]);
        for t in &mut m.triangles {
            for p in t.iter_mut() {
                let q = turn.apply(r(*p));
                *p = [
                    q[0] + translate[0],
                    q[1] + translate[1],
                    q[2] + translate[2],
                ];
            }
        }
        m
    }

    /// The mesh turned by `a_deg` about +X (right-handed): where the part sits in machine
    /// coordinates with the rotary axis at A = `a_deg`.
    pub fn rotated_x(&self, a_deg: f64) -> Mesh {
        let (s, c) = a_deg.to_radians().dsin_cos();
        Mesh {
            triangles: self
                .triangles
                .iter()
                .map(|t| t.map(|p| [p[0], c * p[1] - s * p[2], s * p[1] + c * p[2]]))
                .collect(),
            faces: self.faces.clone(),
        }
    }

    /// Binary STL bytes.
    pub fn to_stl(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(84 + 50 * self.triangles.len());
        let mut header = [0u8; 80];
        // with B-rep faces: each facet's attribute word is its face (at most 65535 faces)
        let labelled = self.faces.len() == self.triangles.len()
            && self.faces.iter().all(|&f| f <= u16::MAX as u32);
        let tag: &[u8] = if labelled {
            FACES_HEADER
        } else {
            b"marcam binary STL"
        };
        header[..tag.len()].copy_from_slice(tag);
        out.extend_from_slice(&header);
        out.extend_from_slice(&(self.triangles.len() as u32).to_le_bytes());
        for (i, t) in self.triangles.iter().enumerate() {
            let n = normal(t);
            for v in std::iter::once(&n).chain(t.iter()) {
                for c in v {
                    out.extend_from_slice(&(*c as f32).to_le_bytes());
                }
            }
            let attr = if labelled { self.faces[i] as u16 } else { 0 };
            out.extend_from_slice(&attr.to_le_bytes());
        }
        out
    }

    /// Revolve a profile about the X axis into a closed mesh with `segments` facets around.
    /// Vertices lie on the true surface (chords are inside it).
    pub fn revolve(profile: &Profile, segments: usize) -> Mesh {
        let segments = segments.max(3);
        let pts = &profile.points;
        let ring = |x: f64, r: f64, j: usize| -> Vec3 {
            let a = std::f64::consts::TAU * (j % segments) as f64 / segments as f64;
            [x, r * a.dcos(), r * a.dsin()]
        };
        let mut tris = Vec::new();
        for w in pts.windows(2) {
            let ([x0, r0], [x1, r1]) = (w[0], w[1]);
            if x0 == x1 && r0 == r1 {
                continue;
            }
            for j in 0..segments {
                let (a, b) = (ring(x0, r0, j), ring(x0, r0, j + 1));
                let (c, d) = (ring(x1, r1, j), ring(x1, r1, j + 1));
                // outward normals: (+angle) x (+x)
                if r0 > 0.0 {
                    tris.push([a, b, c]);
                }
                if r1 > 0.0 {
                    tris.push([b, d, c]);
                }
            }
        }
        // end caps
        if let (Some(&[xb, rb]), Some(&[xf, rf])) = (pts.first(), pts.last()) {
            for j in 0..segments {
                if rb > 0.0 {
                    tris.push([[xb, 0.0, 0.0], ring(xb, rb, j + 1), ring(xb, rb, j)]);
                }
                if rf > 0.0 {
                    tris.push([[xf, 0.0, 0.0], ring(xf, rf, j), ring(xf, rf, j + 1)]);
                }
            }
        }
        Mesh {
            triangles: tris,
            faces: Vec::new(),
        }
    }
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Unit normal (right-hand winding); zero for degenerate triangles.
pub fn normal(t: &Triangle) -> Vec3 {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0; 3]
    }
}

fn read_stl(data: &[u8]) -> Result<Mesh, MeshError> {
    if data.len() >= 84 {
        let n = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
        if 84 + 50 * n == data.len() {
            return Ok(read_binary_stl(data, n));
        }
    }
    let text = std::str::from_utf8(data)
        .map_err(|_| bad("not a binary STL (size mismatch) and not ASCII"))?;
    if !text.trim_start().starts_with("solid") {
        return Err(bad("unrecognised format (expected STL or 3MF)"));
    }
    read_ascii_stl(text)
}

fn read_binary_stl(data: &[u8], n: usize) -> Mesh {
    let f = |o: usize| f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as f64;
    let triangles = (0..n)
        .map(|i| {
            let base = 84 + 50 * i + 12; // skip the stored normal
            let v = |k: usize| {
                let o = base + 12 * k;
                [f(o), f(o + 4), f(o + 8)]
            };
            [v(0), v(1), v(2)]
        })
        .collect();
    // a STEP import's faces, from the facets' attribute words
    let faces = if data.starts_with(FACES_HEADER) {
        (0..n)
            .map(|i| {
                let o = 84 + 50 * i + 48;
                u16::from_le_bytes([data[o], data[o + 1]]) as u32
            })
            .collect()
    } else {
        Vec::new()
    };
    Mesh { triangles, faces }
}

fn read_ascii_stl(text: &str) -> Result<Mesh, MeshError> {
    let mut verts = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        if it.next() == Some("vertex") {
            let mut v = [0.0; 3];
            for c in &mut v {
                *c = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| bad(format!("line {}: bad vertex", ln + 1)))?;
            }
            verts.push(v);
        }
    }
    if verts.len() % 3 != 0 {
        return Err(bad("vertex count is not a multiple of 3"));
    }
    Ok(Mesh {
        triangles: verts.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
        faces: Vec::new(),
    })
}

// ---------------------------------------------------------------- 3MF

type Affine = [f64; 12]; // 3MF row-major 4x3: p' = [x y z 1] * M

const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn apply(m: &Affine, p: Vec3) -> Vec3 {
    [
        p[0] * m[0] + p[1] * m[3] + p[2] * m[6] + m[9],
        p[0] * m[1] + p[1] * m[4] + p[2] * m[7] + m[10],
        p[0] * m[2] + p[1] * m[5] + p[2] * m[8] + m[11],
    ]
}

/// `a` then `b`.
fn compose(a: &Affine, b: &Affine) -> Affine {
    let mut r = [0.0; 12];
    for row in 0..4 {
        for col in 0..3 {
            let mut s = if row == 3 { b[9 + col] } else { 0.0 };
            for k in 0..3 {
                s += a[row * 3 + k] * b[k * 3 + col];
            }
            r[row * 3 + col] = s;
        }
    }
    r
}

fn parse_transform(s: Option<&str>) -> Result<Affine, MeshError> {
    let Some(s) = s else { return Ok(IDENTITY) };
    let v: Vec<f64> = s
        .split_whitespace()
        .map(|t| t.parse().map_err(|_| bad(format!("bad transform {s:?}"))))
        .collect::<Result<_, _>>()?;
    v.try_into()
        .map_err(|_| bad(format!("transform needs 12 numbers: {s:?}")))
}

/// A start or empty-element tag with its attributes.
struct Tag<'a> {
    name: &'a str,
    attrs: Vec<(&'a str, &'a str)>,
    closing: bool,
}

impl<'a> Tag<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

/// Minimal XML tag scanner (enough for 3MF core): yields tags, skips text, comments, PIs.
fn tags(xml: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = xml;
    std::iter::from_fn(move || loop {
        let start = rest.find('<')?;
        rest = &rest[start + 1..];
        if rest.starts_with('?') || rest.starts_with('!') {
            let end = if rest.starts_with("!--") {
                rest.find("-->").map(|e| e + 3)
            } else {
                rest.find('>').map(|e| e + 1)
            }?;
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('>')?;
        let body = &rest[..end];
        rest = &rest[end + 1..];
        let closing = body.starts_with('/');
        let body = body.trim_start_matches('/').trim_end_matches('/');
        let (name, mut attr_text) = body
            .split_once(|c: char| c.is_whitespace())
            .unwrap_or((body, ""));
        let name = name.rsplit(':').next().unwrap_or(name);
        let mut attrs = Vec::new();
        while let Some(eq) = attr_text.find('=') {
            let key = attr_text[..eq].trim();
            let after = attr_text[eq + 1..].trim_start();
            let Some(q) = after.chars().next() else { break };
            let after = &after[1..];
            let Some(close) = after.find(q) else { break };
            attrs.push((key.rsplit(':').next().unwrap_or(key), &after[..close]));
            attr_text = &after[close + 1..];
        }
        return Some(Tag {
            name,
            attrs,
            closing,
        });
    })
}

#[derive(Default)]
struct Object {
    verts: Vec<Vec3>,
    tris: Vec<[usize; 3]>,
    components: Vec<(String, Affine)>,
}

fn read_3mf(data: &[u8]) -> Result<Mesh, MeshError> {
    let entries = crate::archive::entries(data).map_err(bad)?;
    let (name, _) = entries
        .iter()
        .find(|(n, _)| n.to_ascii_lowercase().ends_with(".model"))
        .ok_or_else(|| bad("3MF has no .model part"))?;
    let bytes = crate::archive::read(data, name, &entries).map_err(bad)?;
    let xml = String::from_utf8(bytes).map_err(|_| bad(format!("3MF {name}: not UTF-8")))?;
    parse_3mf_model(&xml)
}

fn num(t: &Tag, key: &str) -> Result<f64, MeshError> {
    t.get(key)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| bad(format!("<{}> needs numeric {key}", t.name)))
}

fn idx(t: &Tag, key: &str, n: usize) -> Result<usize, MeshError> {
    let i: usize = t
        .get(key)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| bad(format!("<triangle> needs {key}")))?;
    if i >= n {
        return Err(bad(format!("triangle index {i} out of range")));
    }
    Ok(i)
}

pub(crate) fn parse_3mf_model(xml: &str) -> Result<Mesh, MeshError> {
    use std::collections::HashMap;
    let mut scale = 1.0;
    let mut objects: HashMap<String, Object> = HashMap::new();
    let mut items: Vec<(String, Affine)> = Vec::new();
    let mut current: Option<(String, Object)> = None;
    for t in tags(xml) {
        match (t.name, t.closing) {
            ("model", false) => {
                scale = match t.get("unit").unwrap_or("millimeter") {
                    "micron" => 0.001,
                    "millimeter" => 1.0,
                    "centimeter" => 10.0,
                    "inch" => 25.4,
                    "foot" => 304.8,
                    "meter" => 1000.0,
                    u => return Err(bad(format!("unknown unit {u:?}"))),
                }
            }
            ("object", false) => {
                let id = t.get("id").ok_or_else(|| bad("<object> without id"))?;
                current = Some((id.to_string(), Object::default()));
            }
            ("object", true) => {
                if let Some((id, o)) = current.take() {
                    objects.insert(id, o);
                }
            }
            ("vertex", false) => {
                if let Some((_, o)) = &mut current {
                    o.verts.push([num(&t, "x")?, num(&t, "y")?, num(&t, "z")?]);
                }
            }
            ("triangle", false) => {
                if let Some((_, o)) = &mut current {
                    let n = o.verts.len();
                    o.tris
                        .push([idx(&t, "v1", n)?, idx(&t, "v2", n)?, idx(&t, "v3", n)?]);
                }
            }
            ("component", false) => {
                if let Some((_, o)) = &mut current {
                    let id = t
                        .get("objectid")
                        .ok_or_else(|| bad("<component> without objectid"))?;
                    o.components
                        .push((id.to_string(), parse_transform(t.get("transform"))?));
                }
            }
            ("item", false) => {
                let id = t
                    .get("objectid")
                    .ok_or_else(|| bad("<item> without objectid"))?;
                items.push((id.to_string(), parse_transform(t.get("transform"))?));
            }
            _ => {}
        }
    }
    if items.is_empty() {
        // no build section: take every object as is
        let mut ids: Vec<&String> = objects.keys().collect();
        ids.sort();
        items = ids.into_iter().map(|id| (id.clone(), IDENTITY)).collect();
    }
    let mut mesh = Mesh::default();
    fn emit(
        objects: &std::collections::HashMap<String, Object>,
        id: &str,
        m: &Affine,
        depth: usize,
        out: &mut Mesh,
    ) -> Result<(), MeshError> {
        if depth > 32 {
            return Err(bad("3MF component nesting too deep"));
        }
        let o = objects
            .get(id)
            .ok_or_else(|| bad(format!("unknown object id {id}")))?;
        for t in &o.tris {
            out.triangles.push([
                apply(m, o.verts[t[0]]),
                apply(m, o.verts[t[1]]),
                apply(m, o.verts[t[2]]),
            ]);
        }
        for (cid, cm) in &o.components {
            emit(objects, cid, &compose(cm, m), depth + 1, out)?;
        }
        Ok(())
    }
    for (id, m) in &items {
        emit(&objects, id, m, 0, &mut mesh)?;
    }
    if scale != 1.0 {
        for p in mesh.triangles.iter_mut().flatten() {
            for c in p.iter_mut() {
                *c *= scale;
            }
        }
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    /// A 5-blade impeller's turning axis: through its middle, not its box's (5 blades: the box
    /// lopsided); a round bar's: its own; a lopsided block's: none.
    #[test]
    fn the_axis_a_part_turns_about() {
        use super::Mesh;
        // a prism along X over a 2D outline (closed: its two ends fans from the outline's first
        // point)
        let prism = |pts: &[[f64; 2]], dy: f64, dz: f64| -> Mesh {
            let mut tris = Vec::new();
            let n = pts.len();
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let p = |x: f64, q: [f64; 2]| [x, q[0] + dy, q[1] + dz];
                tris.push([p(0.0, a), p(0.0, b), p(-10.0, b)]);
                tris.push([p(0.0, a), p(-10.0, b), p(-10.0, a)]);
            }
            // (the ends: fans round their middle, a vertex on the axis, as an STL from OpenSCAD
            // has; it fooled the round-or-repeating test, radius 0 a way)
            for i in 0..n {
                let p = |x: f64, q: [f64; 2]| [x, q[0] + dy, q[1] + dz];
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                tris.push([p(0.0, [0.0, 0.0]), p(0.0, b), p(0.0, a)]);
                tris.push([p(-10.0, [0.0, 0.0]), p(-10.0, a), p(-10.0, b)]);
            }
            Mesh {
                triangles: tris,
                faces: Vec::new(),
            }
        };
        // five blades round a hub: a star, its points 8 out, its notches 4
        let star: Vec<[f64; 2]> = (0..10)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / 10.0;
                let r = if i % 2 == 0 { 8.0 } else { 4.0 };
                [r * a.cos(), r * a.sin()]
            })
            .collect();
        let m = prism(&star, 1.5, -0.7);
        let (lo, hi) = m.bounds().unwrap();
        let boxed = [(lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
        let c = m.turning_axis().expect("five-fold");
        assert!(
            (c[0] - 1.5).abs() < 1e-6 && (c[1] + 0.7).abs() < 1e-6,
            "{c:?}"
        );
        assert!((boxed[0] - 1.5).abs() > 0.3, "the box lopsided: {boxed:?}");
        // a round bar: its own axis
        let round: Vec<[f64; 2]> = (0..72)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / 72.0;
                [6.0 * a.cos(), 6.0 * a.sin()]
            })
            .collect();
        let c = prism(&round, -2.0, 3.0).turning_axis().expect("round");
        assert!(
            (c[0] + 2.0).abs() < 1e-6 && (c[1] - 3.0).abs() < 1e-6,
            "{c:?}"
        );
        // a lopsided L: none (its box's middle stays)
        let ell = [
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 2.0],
            [2.0, 2.0],
            [2.0, 8.0],
            [0.0, 8.0],
        ];
        assert_eq!(prism(&ell, 0.0, 0.0).turning_axis(), None);
    }

    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    fn knob_profile() -> Profile {
        Profile {
            points: vec![
                [-21.7, 3.0],
                [-18.0, 3.0],
                [-18.0, 5.0],
                [-15.0, 5.0],
                [-15.0, 10.0],
                [-0.8, 8.106666666666667],
                [0.0, 7.2],
            ],
            beyond_front: Some(7.2),
        }
    }

    /// Signed volume via the divergence theorem (positive for outward winding).
    fn volume(m: &Mesh) -> f64 {
        m.triangles
            .iter()
            .map(|t| {
                let c = cross(t[1], t[2]);
                (t[0][0] * c[0] + t[0][1] * c[1] + t[0][2] * c[2]) / 6.0
            })
            .sum()
    }

    #[test]
    fn revolve_is_closed_and_outward() {
        let m = Mesh::revolve(&knob_profile(), 720);
        // exact volume of the revolved profile (frustums)
        let p = knob_profile().points;
        let exact: f64 = p
            .windows(2)
            .map(|w| {
                let ([x0, r0], [x1, r1]) = (w[0], w[1]);
                std::f64::consts::PI * (x1 - x0) * (r0 * r0 + r0 * r1 + r1 * r1) / 3.0
            })
            .sum();
        let v = volume(&m);
        assert!(v > 0.0);
        assert!((v - exact).abs() / exact < 1e-4, "{v} vs {exact}");
        let (lo, hi) = m.bounds().unwrap();
        assert_eq!(lo[0], -21.7);
        assert_eq!(hi[0], 0.0);
        assert!((m.max_radius() - 10.0).abs() < 1e-12);
    }

    #[test]
    fn binary_stl_round_trip() {
        let m = Mesh::revolve(&knob_profile(), 36);
        let back = Mesh::from_bytes(&m.to_stl()).unwrap();
        assert_eq!(back.triangles.len(), m.triangles.len());
        for (a, b) in m
            .triangles
            .iter()
            .flatten()
            .zip(back.triangles.iter().flatten())
        {
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn ascii_stl() {
        let text = "solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid t\n";
        let m = Mesh::from_bytes(text.as_bytes()).unwrap();
        assert_eq!(
            m.triangles,
            vec![[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]
        );
        assert!(Mesh::from_bytes(b"garbage").is_err());
    }

    #[test]
    fn three_mf_with_components_units_and_transform() {
        let model = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
  </mesh></object>
  <object id="2" type="model"><components>
   <component objectid="1" transform="1 0 0 0 1 0 0 0 1 0 0 5"/>
  </components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 2 0 0"/></build>
</model>"#;
        let buf = crate::archive::tests::make_zip(&[
            ("[Content_Types].xml", b"<Types/>".as_slice(), false),
            ("3D/3dmodel.model", model.as_bytes(), true),
        ]);
        let m = Mesh::from_bytes(&buf).unwrap();
        // translate z+5 then x+2, then cm -> mm
        assert_eq!(
            m.triangles,
            vec![[[20.0, 0.0, 50.0], [30.0, 0.0, 50.0], [20.0, 10.0, 50.0]]]
        );
    }

    #[test]
    fn zip_errors() {
        assert!(Mesh::from_bytes(b"PK truncated").is_err());
        let z = crate::archive::tests::make_zip(&[("a.txt", b"hello".as_slice(), false)]);
        assert!(Mesh::from_bytes(&z)
            .unwrap_err()
            .to_string()
            .contains("no .model"));
    }

    #[test]
    fn orient_z_axis_to_x() {
        let m = Mesh {
            triangles: vec![[[1.0, 2.0, 3.0]; 3]],
            faces: Vec::new(),
        };
        let o = m.oriented(Axis::Z, [-3.0, 0.0, 0.0]);
        assert_eq!(o.triangles[0][0], [0.0, 1.0, 2.0]);
    }
}
