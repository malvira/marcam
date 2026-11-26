//! Job description: the JSON input format (schema version "0.1").

use crate::det::Det;
use serde::{Deserialize, Serialize};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: &str = "0.1";

/// A complete CAM job: stock, part, tools and the programs to generate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub schema_version: String,
    /// Machine profile id (see `profiles/`), used when no machine is given explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub stock: Stock,
    /// Box stock for 3-axis (vise) setups, in work coordinates (Z up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock_box: Option<StockBox>,
    /// A bar lying in the vise along X (`stock` its section): its axis at (Y, Z), the block
    /// `stock_box` round it (Z0 at its top). None: the block is the stock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock_axis: Option<[f64; 2]>,
    /// That bar along "y" (its axis at (X, Z) then), or standing up "z" (at (X, Y)); None:
    /// along X.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock_along: Option<String>,
    /// A vise: the depth of the stock its jaws grip (mm up from the bottom of `stock_box`); the
    /// jaws stand beside its Y faces up to there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaws: Option<f64>,
    /// What holds the stock, as drawn and as the tool bodies are checked against (Mar's
    /// Fixture): the vise's jaws' length and opening, or the chuck and the tailstock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<Fixture>,
    /// Radial clearance above the stock for safe rapids (mm).
    #[serde(default = "default_clearance")]
    pub clearance: f64,
    pub part: Part,
    pub tools: Vec<Tool>,
    pub spindle_rpm: u32,
    pub programs: Vec<Program>,
    /// The stock is what the previous setup left (simulated), turned and moved as this says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry: Option<Carry>,
    /// How the G-code is written beyond the machine's post: work offset, header and footer
    /// lines, line numbers, comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostOptions>,
    /// That stock, once worked out ([`crate::carry::carry_stock`]; not saved): what planning
    /// and the simulation start from.
    #[serde(skip)]
    pub carried: Option<std::sync::Arc<crate::carry::Carried>>,
}

/// A setup's own G-code options, on top of the machine's post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PostOptions {
    /// The work offset the program runs in (`G54` .. `G59`, `G59.1` .. `G59.3`, `G54.1 P1`
    /// ..), written after the preamble; None: the controller's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_offset: Option<String>,
    /// Lines written after the preamble (and the work offset), and before the postamble;
    /// `{tool}`, `{rpm}` and `{safe}` filled in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footer: Vec<String>,
    /// Number the lines (N10, N20, ...).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub line_numbers: bool,
    /// Leave out the comments (a controller that dislikes them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_comments: bool,
    /// Where the work zero (X0 Y0 Z0 of the program) is, in the setup's coordinates (a vise
    /// setup's: the block's low X and Y corner and its top when it is not given): the G-code's
    /// numbers are written from there; nothing planned changes (Mar: the WCS).
    #[serde(default, skip_serializing_if = "is_zero3")]
    pub origin: [f64; 3],
}

impl PostOptions {
    /// What is wrong with them, if anything.
    pub fn check(&self) -> Result<(), String> {
        if let Some(w) = &self.work_offset {
            let ok = matches!(
                w.as_str(),
                "G54" | "G55" | "G56" | "G57" | "G58" | "G59" | "G59.1" | "G59.2" | "G59.3"
            ) || w.strip_prefix("G54.1 P").is_some_and(|n| {
                !n.is_empty() && n.len() <= 3 && n.bytes().all(|c| c.is_ascii_digit())
            });
            if !ok {
                return Err(format!(
                    "work offset {w:?}: G54 to G59, G59.1 to G59.3, or G54.1 P1 and up"
                ));
            }
        }
        if self.origin.iter().any(|v| !v.is_finite()) {
            return Err("the work zero must be numbers".into());
        }
        for l in self.header.iter().chain(&self.footer) {
            if l.contains('\n') || l.contains('\r') {
                return Err("a header or footer line holds a line break: one line each".into());
            }
        }
        Ok(())
    }
}

/// Stock carried over from the previous setup: its setup, and how a point there maps to this
/// setup's work coordinates (`rotate` it, `turn` it, then add `shift`). A rotation, or a block
/// going on as a bar (or back), carries the cut result as a mesh ([`crate::carry_mesh`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Carry {
    pub from: String,
    /// Degrees about X, then Y, then Z (before the turn).
    #[serde(default, skip_serializing_if = "is_zero3")]
    pub rotate: [f64; 3],
    #[serde(default, skip_serializing_if = "Turn::is_none")]
    pub turn: Turn,
    #[serde(default)]
    pub shift: [f64; 3],
}

impl Carry {
    /// A point of the previous setup in this one.
    pub fn map(&self, p: [f64; 3]) -> [f64; 3] {
        let p = if self.rotate == [0.0; 3] {
            p
        } else {
            crate::carry_mesh::apply(crate::carry_mesh::rotation(self.rotate), p)
        };
        let q = self.turn.apply(p);
        [
            q[0] + self.shift[0],
            q[1] + self.shift[1],
            q[2] + self.shift[2],
        ]
    }
}

fn default_clearance() -> f64 {
    3.0
}

/// What holds the stock (see [`Job::fixture`]); every size optional (None: the usual).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Fixture {
    /// A vise: its jaws' length along X (mm; None: the block's and 10 mm past each end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaw_width: Option<f64>,
    /// A vise: the jaws' own height (mm, drawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaw_height: Option<f64>,
    /// A vise: between the jaws (mm; None: the block's width, gripped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<f64>,
    /// The 4th axis: the chuck's diameter (mm; present: checked against the tool) and its
    /// body's length behind the bar's back end (mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chuck_diameter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chuck_length: Option<f64>,
    /// The bar's length held in the chuck's jaws (mm, from its back end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grip: Option<f64>,
    /// A tailstock: its centre's rim at X `tailstock_x` (mm; None: the bar's front, X0), its
    /// point `centre_depth` into the bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailstock_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centre_depth: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tailstock: bool,
}

impl Fixture {
    /// The chuck: (x from, x to, radius) for a bar `length` long (its back at -length).
    pub fn chuck(&self, length: f64) -> Option<(f64, f64, f64)> {
        let d = self.chuck_diameter?;
        let grip = self.grip.unwrap_or(0.0).max(0.0);
        Some((
            -length - self.chuck_length.unwrap_or(30.0),
            -length + grip,
            d / 2.0,
        ))
    }

    /// The tailstock: its point's X, the centre's depth (its cone 60° across), the quill's
    /// radius.
    pub fn tailstock(&self) -> Option<(f64, f64, f64)> {
        self.tailstock.then(|| {
            let depth = self.centre_depth.unwrap_or(3.0).max(0.0);
            (self.tailstock_x.unwrap_or(0.0) - depth, depth, 12.0)
        })
    }
}

impl Job {
    /// Parse a job from JSON text.
    /// A 0.1 job from its file's text: JSON, or the same as TOML.
    pub fn from_json(text: &str) -> Result<Job, serde_json::Error> {
        match crate::project_file::to_json_text(text) {
            Ok(t) => serde_json::from_str(&t),
            Err(e) => Err(serde::de::Error::custom(e)),
        }
    }

    /// Radius (Z) at which X and A rapids are safe.
    pub fn safe_r(&self) -> f64 {
        // rounded up to the G-code's micron: a bar's corner radius (a hex's, 12.1243...) would
        // otherwise be written a hair below the safe radius itself
        ((self.stock.radius + self.clearance) * 1000.0 - 1e-6).ceil() / 1000.0
    }

    /// Look up a tool by number.
    pub fn tool(&self, number: u32) -> Option<&Tool> {
        self.tools.iter().find(|t| t.number == number)
    }
}

/// Rectangular block, held in a vise: corners in work coordinates (Z up).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct StockBox {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// Bar stock, held on the rotary axis: round, or (with `section`) square, rectangular or
/// hexagonal, its corners at `radius` (what clearances and roughing start from).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stock {
    pub radius: f64,
    pub length: f64,
    /// Not round: the bar's cross-section about the axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<Section>,
}

/// A bar's cross-section about the rotary axis. Angles are in degrees about +X from +Y
/// towards +Z (the simulation's angle), the axis at the section's centre.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Section {
    /// A regular polygon (4: square, 6: hexagon) `across_flats` wide, one flat facing
    /// `angle` (0: a flat on top, facing +Z).
    Polygon {
        sides: u32,
        across_flats: f64,
        #[serde(default)]
        angle: f64,
    },
    /// A rectangle `width` along Y by `height` along Z, turned by `angle`.
    Rect {
        width: f64,
        height: f64,
        #[serde(default)]
        angle: f64,
    },
}

impl Section {
    /// Distance from the axis to the section's edge at angle `phi` (radians, from +Y towards
    /// +Z).
    pub fn radius_at(&self, phi: f64) -> f64 {
        use std::f64::consts::{FRAC_PI_2, TAU};
        match *self {
            Section::Polygon {
                sides,
                across_flats,
                angle,
            } => {
                let n = sides.max(3) as f64;
                let step = TAU / n;
                // from the first flat's normal (up, turned by `angle`)
                let d = (phi - FRAC_PI_2 - angle.to_radians()).rem_euclid(step);
                let d = if d > step / 2.0 { d - step } else { d };
                across_flats / 2.0 / d.dcos()
            }
            Section::Rect {
                width,
                height,
                angle,
            } => {
                let p = phi - angle.to_radians();
                let (c, s) = (p.dcos().abs(), p.dsin().abs());
                let a = if c > 1e-12 {
                    width / 2.0 / c
                } else {
                    f64::INFINITY
                };
                let b = if s > 1e-12 {
                    height / 2.0 / s
                } else {
                    f64::INFINITY
                };
                a.min(b)
            }
        }
    }

    /// The radius that encloses the section (its corners).
    pub fn corner_radius(&self) -> f64 {
        match *self {
            Section::Polygon {
                sides,
                across_flats,
                ..
            } => across_flats / 2.0 / (std::f64::consts::PI / sides.max(3) as f64).dcos(),
            Section::Rect { width, height, .. } => (width / 2.0).dhypot(height / 2.0),
        }
    }
}

impl Stock {
    /// The bar's outline about its axis, counter-clockwise from +Y: `n` points (y, z).
    pub fn outline(&self, n: usize) -> Vec<[f64; 2]> {
        (0..n)
            .map(|k| {
                let phi = k as f64 * std::f64::consts::TAU / n as f64;
                let r = self.radius_at(phi);
                [r * phi.dcos(), r * phi.dsin()]
            })
            .collect()
    }

    /// The bar lying in a vise along X from 0 to its length: the block round it (its low Y side
    /// at 0, its top at Z0) and where its axis is (Y, Z).
    /// The bar in a vise along `along` ("x" or None, "y", "z": standing up): the block round
    /// it (X and Y from 0, Z0 at its top) and where its axis is across it (along X: (Y, Z);
    /// along Y: (X, Z); standing: (X, Y)). Its section's Y runs along the vise's X when not
    /// along X, its Z up (along Y) or along Y (standing).
    pub fn lying_along(&self, along: Option<&str>) -> Result<(StockBox, [f64; 2]), String> {
        let (sb, axis) = self.lying();
        let (w, h) = (sb.max[1], -sb.min[2]);
        Ok(match along.unwrap_or("x") {
            "x" => (sb, axis),
            "y" => (
                StockBox {
                    min: [0.0, 0.0, -h],
                    max: [w, self.length, 0.0],
                },
                axis,
            ),
            "z" => (
                StockBox {
                    min: [0.0, 0.0, -self.length],
                    max: [w, h, 0.0],
                },
                // (the section's Y along X, its Z along Y: its lowest at Y0)
                [axis[0], h + axis[1]],
            ),
            a => return Err(format!("a bar along {a:?}: use \"x\", \"y\" or \"z\"")),
        })
    }

    pub fn lying(&self) -> (StockBox, [f64; 2]) {
        let pts = self.outline(1440);
        let (mut y0, mut y1, mut z0, mut z1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for [y, z] in &pts {
            y0 = y0.min(*y);
            y1 = y1.max(*y);
            z0 = z0.min(*z);
            z1 = z1.max(*z);
        }
        (
            StockBox {
                min: [0.0, 0.0, z0 - z1],
                max: [self.length, y1 - y0, 0.0],
            },
            [-y0, -z1],
        )
    }

    /// The top of the bar's section at `dy` from its axis (across, along Y); None beside it.
    pub fn top_at(&self, dy: f64, outline: &[[f64; 2]]) -> Option<f64> {
        let mut top: Option<f64> = None;
        for k in 0..outline.len() {
            let [ya, za] = outline[k];
            let [yb, zb] = outline[(k + 1) % outline.len()];
            let (lo, hi) = (ya.min(yb), ya.max(yb));
            if dy < lo || dy > hi {
                continue;
            }
            let z = if (yb - ya).abs() < 1e-12 {
                za.max(zb)
            } else {
                za + (zb - za) * (dy - ya) / (yb - ya)
            };
            top = Some(top.map_or(z, |t: f64| t.max(z)));
        }
        top
    }

    /// Distance from the axis to the bar's surface at angle `phi` (radians).
    pub fn radius_at(&self, phi: f64) -> f64 {
        self.section
            .map_or(self.radius, |s| s.radius_at(phi).min(self.radius))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Part {
    /// Revolved profile (used by `rotary_turning`, and as the finishing surface when no mesh
    /// is given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    /// Triangle mesh of the finished part (STL or 3MF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<MeshSource>,
}

/// A half turn of the part (or stock) between setups: a flip about X or Y (the other side up),
/// or a turn about Z (end for end in the vise). Turns compose: two flips about X and Y are a
/// turn about Z.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    #[default]
    None,
    /// 180 degrees about X: Y and Z change sign.
    X180,
    /// 180 degrees about Y: X and Z change sign.
    Y180,
    /// 180 degrees about Z: X and Y change sign.
    Z180,
}

impl Turn {
    pub fn is_none(&self) -> bool {
        *self == Turn::None
    }

    /// The signs it gives X, Y and Z.
    pub fn signs(self) -> [f64; 3] {
        match self {
            Turn::None => [1.0, 1.0, 1.0],
            Turn::X180 => [1.0, -1.0, -1.0],
            Turn::Y180 => [-1.0, 1.0, -1.0],
            Turn::Z180 => [-1.0, -1.0, 1.0],
        }
    }

    /// The point turned.
    pub fn apply(self, p: [f64; 3]) -> [f64; 3] {
        let s = self.signs();
        [s[0] * p[0], s[1] * p[1], s[2] * p[2]]
    }

    /// `self` after `first`.
    pub fn after(self, first: Turn) -> Turn {
        let (a, b) = (self.signs(), first.signs());
        match [a[0] * b[0], a[1] * b[1], a[2] * b[2]] {
            [x, y, _] if x > 0.0 && y > 0.0 => Turn::None,
            [x, _, _] if x > 0.0 => Turn::X180,
            [_, y, _] if y > 0.0 => Turn::Y180,
            _ => Turn::Z180,
        }
    }

    /// Whether it turns the part over (its top to the bottom).
    pub fn flips(self) -> bool {
        matches!(self, Turn::X180 | Turn::Y180)
    }
}

fn is_zero3(v: &[f64; 3]) -> bool {
    v.iter().all(|x| *x == 0.0)
}

/// Where the part mesh comes from and how it sits on the rotary axis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshSource {
    /// Path, relative to the job file.
    pub file: String,
    /// Mesh axis that becomes the rotary X axis.
    #[serde(default)]
    pub axis: crate::mesh::Axis,
    /// Turned after the axis mapping: degrees about X, then Y, then Z (the part's
    /// orientation in its setup: a quarter turn, laid on a face, any angle).
    #[serde(default, skip_serializing_if = "is_zero3")]
    pub rotate: [f64; 3],
    /// Turned after that (a setup that turns the part over or end for end).
    #[serde(default, skip_serializing_if = "Turn::is_none")]
    pub turn: Turn,
    /// Translation applied after the axis mapping and the turn (mm).
    #[serde(default)]
    pub translate: [f64; 3],
    /// The file is a grayscale image (PNG or PGM) to build a relief from, not a mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<crate::relief::ImageRelief>,
}

impl MeshSource {
    /// The part from the file's bytes, placed as this source says: an STL/3MF mesh, or the
    /// relief built from an image; then the axis mapping and translation.
    pub fn mesh(&self, bytes: &[u8]) -> Result<crate::Mesh, crate::mesh::MeshError> {
        // (several part files given: this one's)
        let bytes = crate::mesh::bundle_file(bytes, &self.file)?;
        let m = match &self.image {
            Some(r) => crate::relief::relief_mesh(&crate::relief::decode_gray(bytes)?, r),
            None => crate::Mesh::from_bytes(bytes)?,
        };
        Ok(m.oriented_rotated(self.axis, self.rotate, self.turn, self.translate))
    }

    /// [`MeshSource::mesh`] from the file, relative to `dir`.
    pub fn load(&self, dir: &std::path::Path) -> Result<crate::Mesh, crate::mesh::MeshError> {
        let path = dir.join(&self.file);
        let data = std::fs::read(&path)
            .map_err(|e| crate::mesh::MeshError::Io(format!("{}: {e}", path.display())))?;
        self.mesh(&data).map_err(|e| match e {
            crate::mesh::MeshError::Format(s) => {
                crate::mesh::MeshError::Format(format!("{}: {s}", path.display()))
            }
            other => other,
        })
    }
}

/// Revolved profile: `[x, r]` points in ascending x. Two consecutive points with the same x
/// form a step; at the step itself the +x side wins. Read from points written front to back
/// (descending x) too: they are turned round. Points must be finite, with radii not below 0,
/// and x must not go back and forth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "ProfileText")]
pub struct Profile {
    pub points: Vec<[f64; 2]>,
    /// Radius left past the last point (toward +x). Defaults to the last point's radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beyond_front: Option<f64>,
}

/// A profile as written, before [`Profile::normalized`].
#[derive(Deserialize)]
struct ProfileText {
    points: Vec<[f64; 2]>,
    #[serde(default)]
    beyond_front: Option<f64>,
}

impl TryFrom<ProfileText> for Profile {
    type Error = String;
    fn try_from(t: ProfileText) -> Result<Profile, String> {
        Profile {
            points: t.points,
            beyond_front: t.beyond_front,
        }
        .normalized()
    }
}

impl Profile {
    /// The profile in ascending x (points written front to back turned round), or why it
    /// cannot be one: a point not finite or with a negative radius, or x going back and
    /// forth.
    pub fn normalized(mut self) -> Result<Profile, String> {
        if let Some((i, p)) = self
            .points
            .iter()
            .enumerate()
            .find(|(_, p)| !(p[0].is_finite() && p[1].is_finite() && p[1] >= 0.0))
        {
            return Err(format!(
                "profile point {} [{}, {}]: x and the radius must be numbers, the radius 0 or more",
                i + 1,
                p[0],
                p[1]
            ));
        }
        if self
            .beyond_front
            .is_some_and(|r| !(r.is_finite() && r >= 0.0))
        {
            return Err("profile beyond_front: a radius of 0 or more".into());
        }
        if let (Some(a), Some(b)) = (self.points.first(), self.points.last()) {
            if a[0] > b[0] {
                self.points.reverse();
            }
        }
        if let Some(i) = self.points.windows(2).position(|w| w[1][0] < w[0][0]) {
            return Err(format!(
                "profile points {} and {}: x goes back (from {} to {}); a revolved profile's points run one way along the axis",
                i + 1,
                i + 2,
                self.points[i][0],
                self.points[i + 1][0]
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tool {
    pub number: u32,
    pub description: String,
    #[serde(flatten)]
    pub shape: ToolShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length: Option<f64>,
    /// Shank diameter above the flutes (default: the cutting diameter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shank_diameter: Option<f64>,
    /// Length from the tip to the holder (default: flute length + 5 mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stickout: Option<f64>,
    /// Holder (collet nut) diameter (default 20 mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_diameter: Option<f64>,
    /// Holder length above the stickout (default 30 mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_length: Option<f64>,
}

impl Tool {
    /// A tool with only its number, description and shape given (everything else default).
    pub fn new(number: u32, description: &str, shape: ToolShape) -> Tool {
        Tool {
            number,
            description: description.into(),
            shape,
            flute_length: None,
            shank_diameter: None,
            stickout: None,
            holder_diameter: None,
            holder_length: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolShape {
    /// Flat endmill.
    Flat { diameter: f64 },
    /// Ball endmill.
    Ball { diameter: f64 },
    /// V-bit with the given included angle (degrees). `diameter` (largest cutting diameter)
    /// is needed for mesh finishing.
    Vbit {
        angle: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diameter: Option<f64>,
    },
    /// Tapered ball end mill: a ball of `tip_diameter` blending into a cone whose sides are
    /// `taper_angle` (deg) from the axis, up to `diameter`. A taper of 0 is a ball end mill
    /// of the tip's size.
    TaperedBall {
        tip_diameter: f64,
        taper_angle: f64,
        diameter: f64,
    },
}

/// One output file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Program {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Re-zero A at the start of the program.
    #[serde(default)]
    pub reset_a: bool,
    pub operations: Vec<Operation>,
}

/// A waterline's fields: a [`Raster3d`]'s without the pass direction, pattern and spacing
/// (it cuts levels, `step_down` apart).
fn waterline_fields<S: serde::Serializer>(r: &Raster3d, s: S) -> Result<S::Ok, S::Error> {
    let mut v = serde_json::to_value(r).map_err(serde::ser::Error::custom)?;
    if let Some(m) = v.as_object_mut() {
        for k in ["angle", "pattern", "stepover", "scallop"] {
            m.remove(k);
        }
    }
    v.serialize(s)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    RotaryTurning(RotaryTurning),
    HelicalKnurl(HelicalKnurl),
    FlankChamfer(FlankChamfer),
    RotaryFinish(RotaryFinish),
    /// Rotary parallel (simultaneous 4-axis finishing): lines along X, rings round the axis,
    /// or a spiral, the A axis moving with X and the radius.
    RotaryParallel(RotaryFinish),
    /// Rotary flowline (morph): passes following the channels between the part's walls,
    /// A and X together.
    RotaryFlowline(RotaryFlowline),
    /// Rotary wall finishing: waterlines on the walls radiating from the axis, A and X
    /// together along their twist.
    RotaryWall(RotaryWall),
    /// Rotary pocket roughing: a pocket round the bar cleared level by level in radius,
    /// adaptively (a steady load), A and X together.
    RotaryPocket(RotaryPocket),
    /// Rotary deburring: the part's sharp edges chamfered with a V-bit along them, A and X
    /// together.
    RotaryDeburr(RotaryDeburr),
    RotaryRough(RotaryRough),
    Face(Face),
    Pocket(Pocket),
    Drill(Drill),
    Bore(Bore),
    Contour(Contour),
    Raster3d(Raster3d),
    /// 3D roughing: the parallel raster with a flat (or ball) tool in step-down levels,
    /// leaving stock for the finish.
    Rough3d(Raster3d),
    /// 3D waterline finishing: the part's steep walls cut round at constant Z levels, where
    /// a parallel finish's passes run down them too far apart.
    #[serde(serialize_with = "waterline_fields")]
    Waterline(Raster3d),
    Engrave(Engrave),
    Vcarve(Vcarve),
    Parting(Parting),
    /// 3-axis: chamfer or deburr the top edges of outlines and holes with a V-bit.
    Chamfer(Chamfer),
    /// 3-axis: mill threads in holes or on bosses, on a helix.
    ThreadMill(ThreadMill),
    /// A toolpath written elsewhere (G-code or JSON moves: an AI through MCP, a person's
    /// script), planned, simulated, checked and posted like marcam's own.
    Custom(Custom),
}

/// A custom toolpath (Mar: "full custom toolpaths, 3 axis and 4 axis ... via MCP where AI
/// would upload a custom generated gcode"): `gcode` (or `moves`, JSON) in the setup's frame,
/// read by [`crate::gcode_in`]: on a vise X Y Z (the tool tip, the setup's WCS); on the 4th
/// axis X along the bar, Z the radius, A degrees. Posted through the machine's post, or with
/// `verbatim` its own lines as given (still simulated and checked).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Custom {
    pub tool: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gcode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<crate::gcode_in::JsonMove>,
    /// Post its own lines as given, not re-posted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub verbatim: bool,
    /// The feed of feed moves before the program's first F (mm/min).
    pub feed: f64,
    /// The fastest descent (mm/min) on the 4th axis (where the posts slow plunges).
    pub plunge_feed: f64,
}

/// 3-axis: chamfer (or deburr) the top edges of closed outlines (`side`: "outside" for the
/// part's outline or a boss, "inside" for a pocket's edge) and of round holes, `width` wide,
/// with a V-bit (see [`crate::strategy::edges`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chamfer {
    pub tool: u32,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Rotary setups: the A angle (deg) the part is indexed to (indexed 3+1).
    #[serde(default)]
    pub index_a: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<[f64; 2]>>,
    #[serde(default = "default_outside")]
    pub side: String,
    /// Round holes: [x, y, diameter].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<[f64; 3]>,
    /// The chamfer's width on the top face (mm).
    pub width: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// How much deeper the tip goes than the chamfer needs, so the cone cuts, not the point.
    #[serde(default = "default_tip_clearance")]
    pub tip_clearance: f64,
    #[serde(default = "default_direction")]
    pub direction: String,
    pub feed: f64,
    pub plunge_feed: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

fn default_outside() -> String {
    "outside".into()
}

fn default_tip_clearance() -> f64 {
    0.2
}

/// 3-axis: mill a thread (`diameter` its nominal, `pitch`, `depth` long from the top) in a
/// hole (`internal`) or on a boss at each of `points`, with a thread mill (a V-bit or flat
/// tool; its radius at the tip cuts the profile) on a helix, in radial `passes` (see
/// [`crate::strategy::edges`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadMill {
    pub tool: u32,
    /// Stock to leave on the walls (mm, across): for a finish after it (Mar). Negative: cut
    /// past them.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    #[serde(default)]
    pub index_a: f64,
    pub points: Vec<[f64; 2]>,
    pub diameter: f64,
    pub pitch: f64,
    pub depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    #[serde(default = "default_true")]
    pub internal: bool,
    /// "right" or "left".
    #[serde(default = "default_right")]
    pub hand: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    /// The thread's radial depth; None: ISO metric's for the pitch (0.541 P inside, 0.613 P
    /// outside).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_depth: Option<f64>,
    #[serde(default = "default_one")]
    pub passes: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub spring_passes: u32,
    pub feed: f64,
    pub plunge_feed: f64,
}

fn default_right() -> String {
    "right".into()
}

fn default_one() -> u32 {
    1
}

/// One of several features a facing, pocket or contour works on, each in turn (the ones
/// picked: the ovals of a part): its outline(s) and depth (and top, side for a contour).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Region {
    /// Closed outlines, even-odd (facing, pocket).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<[f64; 2]>>,
    /// A closed outline (contour).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<[f64; 2]>,
    /// "outside", "inside" or "on" (contour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    pub depth: f64,
}

impl Region {
    /// The middle of its outline(s): where the tool goes to it.
    pub fn centre(&self) -> [f64; 2] {
        let pts: Vec<[f64; 2]> = self
            .paths
            .first()
            .cloned()
            .unwrap_or_else(|| self.path.clone());
        if pts.is_empty() {
            return [0.0, 0.0];
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in &pts {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]
    }
}

/// The order to work `regions` in from `start`: nearest next (short travels between them).
pub fn region_order(regions: &[Region], start: [f64; 2]) -> Vec<usize> {
    let mut left: Vec<usize> = (0..regions.len()).collect();
    let mut at = start;
    let mut out = Vec::new();
    while !left.is_empty() {
        let (k, _) = left
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let c = regions[i].centre();
                (k, (c[0] - at[0]).dhypot(c[1] - at[1]))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let i = left.remove(k);
        at = regions[i].centre();
        out.push(i);
    }
    out
}

/// 3-axis: face the top of the stock down by `depth`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Face {
    pub tool: u32,
    /// Stock to leave on the walls (mm, across): for a finish after it (Mar). Negative: cut
    /// past them.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Start from the stock as the operations before it in the setup (and a stock carried
    /// in) left it: no pass wholly above the material in the tool's way (Mar). Off: from the
    /// stock's top, as planned before.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub in_process: bool,
    /// Rotary setups: the A angle (deg) the part is indexed to before this 3-axis
    /// operation (indexed 3+1). Vise setups: must be 0.
    #[serde(default)]
    pub index_a: f64,
    pub depth: f64,
    pub step_down: f64,
    /// Stepover as a fraction of the tool diameter.
    pub stepover: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Where to face (even-odd outlines: a picked face's outline and its openings); the tool
    /// stays inside, a radius in from the outline. Empty: the whole stock top.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<[f64; 2]>>,
    /// Several faces, each faced in turn to its own depth (then `paths`/`depth` are unused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<Region>,
    /// (Mar: "a way to clear all the remaining stock") face everything of the stock at its
    /// depth, out to the stock's edges (its corners too), around what of the part stands above
    /// that level (checked against the part, never into it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_to_stock: bool,
    /// (Mar: "a boundary offset in only one direction") the boundary offset grows the picked
    /// outline outward only, not into its openings (what of the part stands inside them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outward_only: bool,
    /// Grow the faced area this far past its boundary (mm; the picked faces' outlines, or the
    /// stock's), or shrink it (negative).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub boundary_offset: f64,
}

/// 3-axis: rectangular pocket `[x0, y0, x1, y1]`, `depth` below `top` (default: stock top).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pocket {
    pub tool: u32,
    /// Stock to leave on the walls (mm, across): for a finish after it (Mar). Negative: cut
    /// past them.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Start from the stock as the operations before it in the setup (and a stock carried
    /// in) left it: no pass wholly above the material in the tool's way (Mar). Off: from the
    /// stock's top, as planned before.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub in_process: bool,
    /// Rotary setups: the A angle (deg) the part is indexed to before this 3-axis
    /// operation (indexed 3+1). Vise setups: must be 0.
    #[serde(default)]
    pub index_a: f64,
    /// Rectangle `[x0, y0, x1, y1]`, used when `paths` is empty.
    #[serde(default)]
    pub rect: [f64; 4],
    /// Closed outlines (even-odd: an outline and the islands inside it, any shape), cleared
    /// by offsets instead of the rectangle when given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Vec<[f64; 2]>>,
    pub depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    pub step_down: f64,
    /// Fraction of the tool diameter.
    pub stepover: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Ramp in (a rectangle): down to each level along the first pass, back and forth, at
    /// this angle (degrees below the horizontal) instead of plunging; 0: plunge.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub ramp: f64,
    /// Adaptive clearing: the tool cuts at a steady load (the stepover's width) from a helix
    /// out to the walls, instead of the offset loops or the zigzag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adaptive: bool,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
    /// Several pockets, each cleared in turn (then `paths`/`rect`/`top`/`depth` are unused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<Region>,
}

/// 3-axis: holes at `points`, pecking `peck` at a time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Drill {
    pub tool: u32,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Start from the stock as the operations before it in the setup (and a stock carried
    /// in) left it: no pass wholly above the material in the tool's way (Mar). Off: from the
    /// stock's top, as planned before.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub in_process: bool,
    /// Written as a canned cycle (G81, or G83 with pecks) on a machine whose post has them
    /// (a vise setup); else the moves as planned.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub canned: bool,
    /// Rotary setups: the A angle (deg) the part is indexed to before this 3-axis
    /// operation (indexed 3+1). Vise setups: must be 0.
    #[serde(default)]
    pub index_a: f64,
    pub points: Vec<[f64; 2]>,
    pub depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    pub peck: f64,
    /// Clearance above the top for retracts between pecks.
    pub retract: f64,
    pub feed: f64,
}

/// 3-axis: bore round holes at `points` to `diameter` with an end mill smaller than them:
/// helical entry, roughing rings out to the finishing allowance, the finish at the size and
/// the spring passes; G2/G3 arcs (`arcs`) or straight segments within `tolerance`. See
/// [`crate::strategy::bore`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bore {
    pub tool: u32,
    /// Stock to leave on the walls (mm, across): for a finish after it (Mar). Negative: cut
    /// past them.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Start from the stock as the operations before it in the setup (and a stock carried
    /// in) left it: no pass wholly above the material in the tool's way (Mar). Off: from the
    /// stock's top, as planned before.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub in_process: bool,
    /// Rotary setups: the A angle (deg) the part is indexed to (indexed 3+1, a radial hole).
    #[serde(default)]
    pub index_a: f64,
    pub points: Vec<[f64; 2]>,
    pub diameter: f64,
    pub depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// Descent per turn of the helix (mm).
    #[serde(default = "default_helix_pitch")]
    pub helix_pitch: f64,
    /// Roughing rings at most this far apart, as a fraction of the tool diameter.
    #[serde(default = "default_bore_stepover")]
    pub stepover: f64,
    /// Radial stock the roughing leaves for the finish (mm).
    #[serde(default = "default_bore_allowance")]
    pub finish_allowance: f64,
    /// Finishing passes repeated at the size (for the tool's deflection).
    #[serde(default = "default_spring_passes")]
    pub spring_passes: u32,
    /// "climb" (counter-clockwise inside the hole, G3) or "conventional" (G2).
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Write the circles as G2/G3 arcs (else straight segments).
    #[serde(default = "default_true")]
    pub arcs: bool,
    /// Largest distance between the segments and the circle (mm).
    #[serde(default = "default_bore_tolerance")]
    pub tolerance: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
}

fn default_helix_pitch() -> f64 {
    0.5
}
fn default_bore_stepover() -> f64 {
    0.4
}
fn default_bore_allowance() -> f64 {
    0.1
}
fn default_spring_passes() -> u32 {
    1
}
fn default_bore_tolerance() -> f64 {
    0.002
}

/// 3-axis: follow a closed 2D path, outside / inside / on it, in step-down passes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contour {
    pub tool: u32,
    /// Stock to leave on the walls (mm, across): for a finish after it (Mar). Negative: cut
    /// past them.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    /// Stock to leave on the floor (mm, up): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Start from the stock as the operations before it in the setup (and a stock carried
    /// in) left it: no pass wholly above the material in the tool's way (Mar). Off: from the
    /// stock's top, as planned before.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub in_process: bool,
    /// Rotary setups: the A angle (deg) the part is indexed to before this 3-axis
    /// operation (indexed 3+1). Vise setups: must be 0.
    #[serde(default)]
    pub index_a: f64,
    pub path: Vec<[f64; 2]>,
    /// "outside", "inside" or "on".
    pub side: String,
    pub depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    pub step_down: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
    /// Several outlines, each followed in turn (then `path`/`side`/`top`/`depth` are unused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<Region>,
    /// "climb" or "conventional" (spindle clockwise); None: climb. New operations take the
    /// machine profile's `cut_direction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Tabs left holding the part (0: none): this many, evenly round the outline.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub tabs: u32,
    /// Tabs placed by hand: where each one is, as a fraction (0 to 1) of the way round the
    /// outline as written (`path`, from its first point), so it stays put when the operation
    /// is planned again, its direction changes or the model is replaced. With `tabs` both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_at: Vec<f64>,
    /// Each tab's length along the outline (mm, the material left).
    #[serde(default = "default_tab_width")]
    pub tab_width: f64,
    /// Each tab's height above the bottom of the cut (mm).
    #[serde(default = "default_tab_height")]
    pub tab_height: f64,
    /// Lead-in: a quarter arc of this radius (mm) from the waste side onto the contour,
    /// tangent to it, instead of plunging on it; 0: none.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub lead_in: f64,
    /// Lead-out: a quarter arc of this radius (mm) off the contour to the waste side; 0: none.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub lead_out: f64,
    /// Move the contour this far out from the outline (mm; in, when negative), beyond the
    /// tool's radius: a hole cut bigger, an outline left bigger. The tabs move with it.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub boundary_offset: f64,
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn default_tab_width() -> f64 {
    4.0
}
fn default_tab_height() -> f64 {
    1.0
}

/// 3-axis: drop-cutter raster over the part mesh (passes along X, stepping in Y), cut in
/// step-down levels; `floor` is where there is no part.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Raster3d {
    pub tool: u32,
    /// Rotary setups: the A angle (deg) the part is indexed to before this 3-axis
    /// operation (indexed 3+1). Vise setups: must be 0.
    #[serde(default)]
    pub index_a: f64,
    pub x_range: [f64; 2],
    pub y_range: [f64; 2],
    /// (A waterline has none: its levels are `step_down` apart.)
    #[serde(default)]
    pub stepover: f64,
    pub sample: f64,
    pub step_down: f64,
    pub floor: f64,
    /// Stock to leave on the surface, up (mm; read from `leave` as before).
    #[serde(default, alias = "leave", skip_serializing_if = "is_zero_f64")]
    pub leave_z: f64,
    /// Stock to leave across (mm): the tool kept this much further from the walls (flat and
    /// ball tools).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_xy: f64,
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// Pass direction, degrees from +X (0: passes along X, stepping in Y).
    #[serde(default)]
    pub angle: f64,
    /// "zigzag" (back and forth, linked at depth) or "one_way" (every pass the same way,
    /// re-entering from above the stock).
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// One-way passes: "climb" or "conventional" (M3 / clockwise spindle).
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Target scallop height (mm). When > 0 it sets the stepover from the tool shape
    /// (ball or V-bit); `stepover` is used otherwise.
    #[serde(default)]
    pub scallop: f64,
    /// Face regions of the part mesh the tool may touch (ids from
    /// [`crate::geometry::face_regions`] at 30°, filled from a face selection): passes run
    /// only where the tool rests on them, following the whole part. Empty: the whole part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<String>,
    /// Rest roughing (3D roughing on a vise): cut only where the operations before it in
    /// the setup left material, simulated (a smaller tool after a larger one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rest: bool,
    /// Rest finishing (a parallel finish): only where the earlier finish's tool, this number
    /// (larger), could not reach: where this tool goes deeper than it would (corners,
    /// fillets); 0: everywhere.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub rest_tool: u32,
    /// Waterline: only where the surface is at least this steep (degrees from the
    /// horizontal); 0: everywhere.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub min_slope: f64,
    /// Lead-in (finishing): each pass comes onto the surface from this high above it (mm),
    /// down along the pass at the ramp angle, instead of plunging onto it; 0: plunge.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub lead_in: f64,
    /// (Mar's impeller) On the 4th axis: rough at each of these A angles in turn (indexed
    /// 3+1, the tool upright, Y used), adaptive, from the stock the angles before it left.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<f64>,
    /// On the 4th axis: that many angles, equally apart, the first where the part is lowest
    /// (between blades); used when `indexes` is empty.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub index_count: u32,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Follow paths and single-line text at `depth` below the surface, in `step_down` passes:
/// on a vise at Z `top`, or wrapped round the rotary axis at radius `top` (path Y = arc
/// length, centred on the top of the bar at A0).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Engrave {
    pub tool: u32,
    /// Polylines (closed when the last point repeats the first).
    #[serde(default)]
    pub paths: Vec<Vec<[f64; 2]>>,
    /// Text in the built-in single-stroke font (A-Z, 0-9, - . , / : ! ? + = ( ) ').
    #[serde(default)]
    pub text: String,
    /// Cap height of the text (mm).
    #[serde(default = "default_text_height")]
    pub text_height: f64,
    /// Start of the text's first baseline.
    #[serde(default)]
    pub text_at: [f64; 2],
    /// Turn the text by this many degrees about `text_at` (90: it runs along +Y, i.e.
    /// round the bar when wrapped).
    #[serde(default)]
    pub text_rotate: f64,
    pub depth: f64,
    /// Surface: Z on a vise (default: the stock top), radius on a rotary setup (default:
    /// the stock radius).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    pub step_down: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

fn default_text_weight() -> f64 {
    crate::engrave::OUTLINE_WEIGHT
}

fn default_text_height() -> f64 {
    5.0
}

/// V-carve closed paths (even-odd fill) with a V-bit: depth from the bit's angle and the
/// distance to the outline, up to `max_depth`; flat or wrapped as [`Engrave`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Vcarve {
    pub tool: u32,
    /// Closed outlines (even-odd).
    #[serde(default)]
    pub paths: Vec<Vec<[f64; 2]>>,
    /// Text in outline letters (marcam's own font, see [`crate::engrave::outline_text`]),
    /// carved with the paths.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Cap height of the text (mm).
    #[serde(default = "default_text_height")]
    pub text_height: f64,
    /// Start of the text's first baseline.
    #[serde(default)]
    pub text_at: [f64; 2],
    /// Turn the text by this many degrees about `text_at`.
    #[serde(default)]
    pub text_rotate: f64,
    /// Letter weight: the pen's width as a fraction of the cap height.
    #[serde(default = "default_text_weight")]
    pub text_weight: f64,
    pub max_depth: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// Distance between rows (mm).
    pub stepover: f64,
    /// Point spacing along a row (mm).
    pub sample: f64,
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

fn default_pattern() -> String {
    "zigzag".into()
}

fn default_direction() -> String {
    "climb".into()
}

impl Operation {
    /// A angle a 3-axis operation is indexed to (None for rotary operations).
    pub fn index_a(&self) -> Option<f64> {
        match self {
            Operation::Face(o) => Some(o.index_a),
            Operation::Pocket(o) => Some(o.index_a),
            Operation::Drill(o) => Some(o.index_a),
            Operation::Bore(o) => Some(o.index_a),
            Operation::Contour(o) => Some(o.index_a),
            Operation::Raster3d(o) | Operation::Rough3d(o) | Operation::Waterline(o) => {
                Some(o.index_a)
            }
            Operation::Chamfer(o) => Some(o.index_a),
            Operation::ThreadMill(o) => Some(o.index_a),
            _ => None,
        }
    }

    /// The ramp angle of an operation that descends along its path (None: plunges only).
    pub fn ramp_angle(&self) -> Option<f64> {
        match self {
            Operation::Raster3d(o) | Operation::Rough3d(o) | Operation::Waterline(o) => {
                Some(o.ramp_angle)
            }
            Operation::Engrave(o) => Some(o.ramp_angle),
            Operation::Vcarve(o) => Some(o.ramp_angle),
            Operation::RotaryTurning(o) => Some(o.ramp_angle),
            Operation::HelicalKnurl(o) => Some(o.ramp_angle),
            Operation::FlankChamfer(o) => Some(o.ramp_angle),
            Operation::RotaryFinish(o) | Operation::RotaryParallel(o) => Some(o.ramp_angle),
            Operation::RotaryFlowline(o) => Some(o.ramp_angle),
            Operation::RotaryWall(o) => Some(o.ramp_angle),
            Operation::RotaryPocket(o) => Some(o.ramp_angle),
            Operation::RotaryDeburr(o) => Some(o.ramp_angle),
            Operation::RotaryRough(o) => Some(o.ramp_angle),
            Operation::Bore(o) => Some(o.ramp_angle),
            Operation::Parting(o) => Some(o.ramp_angle),
            _ => None,
        }
    }

    /// The smoothing tolerance (mm) this operation is planned with: its own, else its type's
    /// default (0.005 for finishing, 0.01 for roughing; 0 for operations without it).
    pub fn smoothing(&self) -> f64 {
        let (own, finishing) = match self {
            Operation::RotaryTurning(o) => (o.smoothing, false),
            Operation::HelicalKnurl(o) => (o.smoothing, true),
            Operation::FlankChamfer(o) => (o.smoothing, true),
            Operation::RotaryFinish(o) | Operation::RotaryParallel(o) => (o.smoothing, true),
            Operation::RotaryFlowline(o) => (o.smoothing, true),
            Operation::RotaryWall(o) => (o.smoothing, true),
            Operation::RotaryPocket(o) => (o.smoothing, false),
            Operation::RotaryDeburr(o) => (o.smoothing, true),
            Operation::RotaryRough(o) => (o.smoothing, false),
            Operation::Raster3d(o) => (o.smoothing, true),
            Operation::Rough3d(o) => (o.smoothing, false),
            Operation::Waterline(o) => (o.smoothing, true),
            Operation::Engrave(o) => (o.smoothing, true),
            Operation::Vcarve(o) => (o.smoothing, true),
            Operation::Contour(o) => (o.smoothing, true),
            Operation::Chamfer(o) => (o.smoothing, true),
            Operation::Pocket(o) => (o.smoothing, false),
            Operation::Parting(o) => (o.smoothing, false),
            _ => return 0.0,
        };
        own.unwrap_or(if finishing { 0.005 } else { 0.01 }).max(0.0)
    }

    pub fn tool(&self) -> u32 {
        match self {
            Operation::RotaryFinish(o) | Operation::RotaryParallel(o) => o.tool,
            Operation::RotaryFlowline(o) => o.tool,
            Operation::RotaryWall(o) => o.tool,
            Operation::RotaryPocket(o) => o.tool,
            Operation::RotaryDeburr(o) => o.tool,
            Operation::RotaryRough(o) => o.tool,
            Operation::Face(o) => o.tool,
            Operation::Pocket(o) => o.tool,
            Operation::Drill(o) => o.tool,
            Operation::Bore(o) => o.tool,
            Operation::Contour(o) => o.tool,
            Operation::Raster3d(o) | Operation::Rough3d(o) | Operation::Waterline(o) => o.tool,
            Operation::Engrave(o) => o.tool,
            Operation::Vcarve(o) => o.tool,
            Operation::RotaryTurning(o) => o.tool,
            Operation::HelicalKnurl(o) => o.tool,
            Operation::FlankChamfer(o) => o.tool,
            Operation::Parting(o) => o.tool,
            Operation::Chamfer(o) => o.tool,
            Operation::ThreadMill(o) => o.tool,
            Operation::Custom(o) => o.tool,
        }
    }
}

/// Rough + finish the revolved profile with a flat endmill moving helically along X.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryTurning {
    pub tool: u32,
    /// Stock to leave on the bar, radially (mm): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// Stock to leave on the faces across the bar, along X (mm): for a finish after it.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    pub rough_depth: f64,
    pub rough_step: f64,
    pub rough_feed: f64,
    pub finish_allowance: f64,
    pub finish_step: f64,
    pub finish_feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    #[serde(default = "default_segment_deg")]
    pub segment_deg: f64,
    #[serde(default = "default_grid")]
    pub grid: f64,
    #[serde(default = "default_footprint_step")]
    pub footprint_step: f64,
    /// Turn only this X span of the profile (picked bands); None: all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_range: Option<[f64; 2]>,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Diamond knurl: families of helical V grooves over a conical body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelicalKnurl {
    pub tool: u32,
    /// Stock to leave on the bar, radially (mm): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    pub body_back: [f64; 2],
    pub body_front: [f64; 2],
    pub count: u32,
    pub helix_angle: f64,
    /// "diamond": the grooves along helices, `families` of them crossing (two opposite:
    /// diamonds); "straight": grooves along X, parallel to the axis (the helix angle and the
    /// families unused). (Mar: "a selection box as a parameter for diamond knurl or straight
    /// knurls")
    #[serde(default = "default_knurl_pattern", skip_serializing_if = "is_diamond")]
    pub pattern: String,
    #[serde(default = "default_fill")]
    pub fill: f64,
    pub passes: Vec<f64>,
    #[serde(default = "default_families")]
    pub families: Vec<i32>,
    #[serde(default = "default_runout")]
    pub runout: f64,
    #[serde(default = "default_samples")]
    pub samples: usize,
    #[serde(default = "default_approach_clearance")]
    pub approach_clearance: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Chamfer an edge with the flank of a V-bit while the part rotates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlankChamfer {
    pub tool: u32,
    /// Stock to leave on the bar, radially (mm): for a finish after it (Mar).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// `[x, r]` of the chamfer's outer corner on the finished part.
    pub edge: [f64; 2],
    /// Axial distance from the edge to the V-bit tip.
    pub tip_offset: f64,
    /// Extra radial stock per pass (last pass normally 0).
    pub passes: Vec<f64>,
    #[serde(default = "default_revolutions")]
    pub revolutions: f64,
    #[serde(default = "default_segment_deg")]
    pub segment_deg: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Parting (rotary): a groove cut radially into the turning bar at `x` with a flat end mill or
/// a V-bit, in a helical infeed (the radius falls as A turns), its depth per turn and its feed
/// tapering towards the axis, to a full cut-off (leaving a sliver to break, if asked) or down
/// to a radius. A groove wider than the tool is cut in steps along X; the part-side edge can
/// be chamfered first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Parting {
    pub tool: u32,
    /// Stock to leave on the faces across the bar, along X (mm): for a finish after it.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    /// Axial position of the groove's part-side edge: the part is on the +X side (towards the
    /// front), the groove runs from here towards -X.
    pub x: f64,
    /// Groove width along X (mm); 0 or less than the tool: the tool's width.
    #[serde(default)]
    pub width: f64,
    /// X step between the passes of a wider groove, as a fraction of the tool width.
    #[serde(default = "default_parting_step")]
    pub step_x: f64,
    /// Radius of the material at the groove (the part's, or the stock's when absent): the
    /// infeed starts just above it, the chamfer is measured from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// Radius to stop at; 0: a full cut-off.
    #[serde(default)]
    pub stop_radius: f64,
    /// Full cut-off: leave a core of this radius (mm) holding the part, to break off by hand;
    /// 0: right through.
    #[serde(default)]
    pub sliver: f64,
    /// Radial depth per turn at the outside (mm).
    pub depth_per_rev: f64,
    /// Depth per turn and feed at the axis, as a fraction of their values at the outside
    /// (they fall linearly with the radius, so the shrinking core is cut gently).
    #[serde(default = "default_parting_taper")]
    pub taper: f64,
    /// Chamfer the part-side edge by this much (mm along X) before parting; 0: none. A V-bit
    /// cuts it with its flank, a flat end mill in small steps.
    #[serde(default)]
    pub chamfer: f64,
    /// Angular length of each move (degrees).
    #[serde(default = "default_segment_deg")]
    pub segment_deg: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// See [`FlankChamfer::ramp_angle`].
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

fn default_parting_step() -> f64 {
    0.8
}
fn default_parting_taper() -> f64 {
    0.4
}

/// Raster direction for [`RotaryFinish`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishPattern {
    /// Passes along X at constant A, stepping over in A (zig-zag, linked along A).
    Along,
    /// One helix around the axis, advancing `stepover_mm` in X per revolution.
    Around,
    /// Rotary parallel's "line": as [`FinishPattern::Along`].
    Line,
    /// Rotary parallel's "circular": rings round the axis at fixed X, `stepover_mm` apart
    /// in X (a full turn each, or across `a_range` and back).
    Circular,
    /// Rotary parallel's "spiral": as [`FinishPattern::Around`] (a full turn at each end).
    Spiral,
}

/// Finish the part mesh with a radial drop-cutter (flat, ball or V-bit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryFinish {
    pub tool: u32,
    /// Stock to leave on the faces across the bar, along X (mm): for a finish after it.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    pub pattern: FinishPattern,
    /// Face regions of the part mesh the tool may touch (ids from
    /// [`crate::geometry::face_regions`] at 30°, filled from a face selection): passes run
    /// only where the tool rests on them, following the whole part. Empty: the whole part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<String>,

    /// Axial extent `[start, end]` of the tool tip.
    pub x_range: [f64; 2],
    /// Angular extent (degrees) for `along`; a full turn by default.
    #[serde(default = "default_a_range")]
    pub a_range: [f64; 2],
    /// `along`: angular stepover (degrees). Alternatively give `stepover_mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepover_deg: Option<f64>,
    /// `along`: stepover measured at the part's largest radius; `around`: X advance per turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepover_mm: Option<f64>,
    /// Sample spacing along X for `along` passes (mm).
    #[serde(default = "default_sample_mm")]
    pub sample_mm: f64,
    /// Sample spacing around the axis (degrees) for `around` and for links.
    #[serde(default = "default_sample_deg")]
    pub sample_deg: f64,
    /// Radial stock to leave (mm; read from `leave` as before).
    #[serde(default, alias = "leave", skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// Largest allowed dip of a straight move below the surface (mm); moves are split until
    /// they stay within it.
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// The tip never goes below this radius (mm, >= 0).
    #[serde(default)]
    pub min_radius: f64,
    /// `around`: add a full turn at the foot of every wall, which a helix would otherwise
    /// only touch once per turn.
    #[serde(default = "default_true")]
    pub clean_walls: bool,
    /// Lead-in: a pass comes onto the surface turning, from this high above it (mm), not a
    /// radial plunge onto it; 0: plunge.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub lead_in: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
    /// (Rotary parallel) the tool this far off the axis in Y (mm, a ball, at most its
    /// radius): the ball cuts off its tip, where it turns at speed; 0: over the axis.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub y_offset: f64,
}

/// Rotary flowline (morph): finish the channels between the part's walls (an impeller's
/// blades) with passes that follow them, A and X together, a stepover apart across each
/// channel's floor, one after another without leaving it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryFlowline {
    pub tool: u32,
    /// Stock to leave on the faces across the bar, along X (mm).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    /// Axial extent `[start, end]` of the tool tip.
    pub x_range: [f64; 2],
    /// Distance between passes across a channel's floor (mm, at its widest).
    pub stepover_mm: f64,
    /// Point spacing along X (mm).
    #[serde(default = "default_flow_sample")]
    pub sample_mm: f64,
    /// A wall: where the surface stands more than this above the lowest at that X (mm); the
    /// channels are what is lower.
    #[serde(default = "default_wall_height")]
    pub wall_height: f64,
    /// What a pass spans: a channel's floor between its walls, or the whole sector from one
    /// wall's top to the next (the passes climbing the walls along their twist).
    #[serde(default)]
    pub span: FlowSpan,
    /// Radial stock to leave (mm).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// Largest allowed dip of a straight move below the surface (mm).
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// The tip never goes below this radius (mm, >= 0).
    #[serde(default)]
    pub min_radius: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Rotary wall finishing: finish the walls that radiate from the axis (an impeller's blade
/// faces) with waterlines on them, `step_down` apart in radius, A and X together along their
/// twist, both faces of every wall.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryWall {
    pub tool: u32,
    /// Stock to leave on the faces across the bar, along X (mm).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    /// Axial extent `[start, end]` of the tool tip.
    pub x_range: [f64; 2],
    /// Radial distance between waterlines (mm).
    pub step_down: f64,
    /// The tool upright this far off the axis in Y (mm; the faces either side of each wall
    /// with the offset to one side, then the other): its shank leans off the radius, clear of
    /// a wall leaning over the face it cuts. 0: over the axis.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub y_offset: f64,
    /// Point spacing along X (mm).
    #[serde(default = "default_flow_sample")]
    pub sample_mm: f64,
    /// Radial stock to leave (mm).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// Largest allowed dip of a straight move below the surface (mm).
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// The tip never goes below this radius (mm, >= 0).
    #[serde(default)]
    pub min_radius: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Rotary pocket roughing (4-axis simultaneous): a pocket round the bar (between an
/// impeller's blades, a recess in a cylinder) within `x_range` and `a_range`, cleared level by
/// level `step_down` apart in radius, each level adaptively (constant engagement: the tool
/// takes the stepover's width as it goes, never a full-width slot but where it comes in, down
/// a helix where it has room). `rest`: from the stock as the operations before left it (in
/// 3D), not the whole bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryPocket {
    pub tool: u32,
    /// Axial extent `[start, end]` of the tool tip.
    pub x_range: [f64; 2],
    /// Angular extent `[from, to]` (degrees) round the axis.
    #[serde(default = "full_turn")]
    pub a_range: [f64; 2],
    /// Radial depth per level (mm).
    pub step_down: f64,
    /// The width of material the tool takes, as a fraction of its diameter.
    #[serde(default = "default_pocket_stepover")]
    pub stepover: f64,
    /// Radial stock to leave on the part (mm).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// The tip never goes below this radius (mm, >= 0).
    #[serde(default)]
    pub min_radius: f64,
    /// Down a helix where there is room (else a plunge).
    #[serde(default = "default_true")]
    pub helix: bool,
    /// From the stock as the operations before left it (in 3D).
    #[serde(default)]
    pub rest: bool,
    pub feed: f64,
    pub plunge_feed: f64,
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

/// Rotary deburring (4-axis simultaneous): the part's sharp convex edges within `x_range` that
/// a tool over the axis reaches (one face looking out from it: a blade's or a fin's top edges,
/// a groove's lips), each followed by a V-bit over the axis through it, its tip `depth` below
/// the edge: a chamfer `depth` down the wall and `depth` times the tan of the V's half angle
/// across the top (a 90° bit: 45°, `depth` each way).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryDeburr {
    pub tool: u32,
    /// Axial extent `[start, end]` of the edges.
    pub x_range: [f64; 2],
    /// How far below the edge the tip goes (mm).
    #[serde(default = "default_deburr_depth")]
    pub depth: f64,
    /// An edge: where the faces turn by this much or more (degrees).
    #[serde(default = "default_deburr_angle")]
    pub min_angle: f64,
    /// Point spacing along the edges (mm).
    #[serde(default = "default_deburr_sample")]
    pub sample_mm: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
}

fn default_deburr_depth() -> f64 {
    0.3
}

fn default_deburr_angle() -> f64 {
    45.0
}

fn default_deburr_sample() -> f64 {
    0.2
}

fn full_turn() -> [f64; 2] {
    [0.0, 360.0]
}

fn default_pocket_stepover() -> f64 {
    0.15
}

/// What a rotary flowline's pass spans.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlowSpan {
    /// A channel's floor, between the feet of its walls.
    #[default]
    Floor,
    /// From one wall's top to the next: over the walls too.
    Sector,
}

fn default_flow_sample() -> f64 {
    0.25
}

fn default_wall_height() -> f64 {
    1.0
}

/// Descents at up to 30 degrees run at the cutting feed (Mar, 2026-11-17).
pub fn default_ramp_angle() -> f64 {
    30.0
}

fn default_true() -> bool {
    true
}

/// Rough the stock down to a mesh (or revolved profile) in radial levels, each a helix along X.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryRough {
    pub tool: u32,
    /// Stock to leave on the faces across the bar, along X (mm): for a finish after it.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub leave_axial: f64,
    /// Axial extent of the tool tip.
    pub x_range: [f64; 2],
    /// Radial depth per level (mm).
    pub step_down: f64,
    /// X advance per turn (mm).
    pub step_x: f64,
    /// Radial stock left on the part for finishing (mm).
    #[serde(default, alias = "allowance", skip_serializing_if = "is_zero_f64")]
    pub leave_radial: f64,
    /// Points per turn are spaced this far apart (degrees).
    #[serde(default = "default_rough_sample_deg")]
    pub sample_deg: f64,
    /// Largest dip of a straight move below the allowance surface (mm).
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// The tip never goes below this radius (mm, >= 0).
    #[serde(default)]
    pub min_radius: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Descents gentler than this (degrees below the horizontal, along the path) run at
    /// the cutting feed; steeper ones are slowed so the tip falls no faster than
    /// `plunge_feed`. 0: every descent is slowed.
    #[serde(default = "default_ramp_angle")]
    pub ramp_angle: f64,
    /// Smoothing tolerance (mm): fewer, smoother moves within it (see
    /// [`crate::smoothing`]); None: the type's default; 0: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
    /// Rest roughing on the 4th axis: only where the operations before it in the setup left
    /// material (simulated), angle by angle; round the rest at the air feed (Mar's impeller:
    /// the hub under the blades, which no index of the roughing reached).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rest: bool,
    /// Rest roughing on the 3D stock: the most a pass may take per mm of travel (mm3/mm). A
    /// level's pass that would take more goes in two, half a step down each; where the
    /// second still would, slower in proportion (to 0.6 of the feed). None: 1.5 times the
    /// tool's normal bite (buried to its centre: a ball's half disc, a flat's radius by its
    /// width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bite: Option<f64>,
}

fn default_rough_sample_deg() -> f64 {
    6.0
}

fn default_a_range() -> [f64; 2] {
    [0.0, 360.0]
}
fn default_tolerance() -> f64 {
    0.005
}
fn default_sample_mm() -> f64 {
    0.1
}
fn default_sample_deg() -> f64 {
    2.0
}
fn default_segment_deg() -> f64 {
    6.0
}
fn default_grid() -> f64 {
    0.02
}
fn default_footprint_step() -> f64 {
    0.01
}
fn default_fill() -> f64 {
    1.0
}
fn default_knurl_pattern() -> String {
    "diamond".into()
}
fn is_diamond(p: &str) -> bool {
    p == "diamond"
}
fn default_families() -> Vec<i32> {
    vec![1, -1]
}
fn default_runout() -> f64 {
    0.6
}
fn default_samples() -> usize {
    120
}
fn default_approach_clearance() -> f64 {
    0.5
}
fn default_revolutions() -> f64 {
    2.0
}
