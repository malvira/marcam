//! Machine profiles and tool libraries: the versioned JSON formats (see `docs/profiles.md`),
//! the built-in registry (`profiles/`), validation, resolving library tools into jobs,
//! checking jobs against machine limits, and importers for Makera's Fusion 360 files.
//!
//! The kinematics description is general (any number of linear axes and rotary axes with an
//! axis of rotation) so later 3-axis and 5-axis machines fit the same format; today's
//! strategies use one rotary axis about X.

// `!(x > 0.0)` on purpose: it also rejects NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::collapsible_match)]

use crate::job::{Job, Tool, ToolShape};
use crate::machine::{FeedMode, Machine};
use crate::toolpath::{Move, Toolpath};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MACHINE_FORMAT: &str = "marcam-machine";
pub const TOOLS_FORMAT: &str = "marcam-tools";
/// The names these formats had before the project was renamed (4cam -> marcam); still read.
pub const LEGACY_MACHINE_FORMAT: &str = "4cam-machine";
pub const LEGACY_TOOLS_FORMAT: &str = "4cam-tools";
pub const FORMAT_VERSION: u32 = 1;

/// Where a profile's data came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

// ------------------------------------------------------------------ machines

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinearAxis {
    /// Word letter: X, Y, Z (U, V, W ...).
    pub letter: String,
    /// Direction of positive motion of the tool relative to the work, machine frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// mm/min
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_feed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rapid: Option<f64>,
    /// mm/s² (time estimates; default 500)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accel: Option<f64>,
}

impl LinearAxis {
    pub fn travel(&self) -> Option<f64> {
        Some(self.max? - self.min?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotaryAxis {
    /// Word letter: A, B or C.
    pub letter: String,
    /// Axis of rotation (machine frame), e.g. [1, 0, 0] for A about X.
    pub axis: [f64; 3],
    /// What the axis turns: "workpiece" (table/rotary chuck) or "tool" (head).
    #[serde(default = "default_carries")]
    pub carries: String,
    /// Positive rotation: "right" (right-handed about `axis`) or "left".
    #[serde(default = "default_handedness")]
    pub handedness: String,
    /// Turns without limit (wraps); otherwise `min`/`max` apply.
    #[serde(default)]
    pub continuous: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// deg/min
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_feed: Option<f64>,
    /// deg/min (default: max_feed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rapid: Option<f64>,
    /// deg/s² (time estimates; default 3600)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accel: Option<f64>,
}

/// Timing figures for machining-time estimates (see `estimate::Dynamics`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timing {
    /// Junction deviation (mm) of the controller's look-ahead (default 0.02).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub junction_deviation: Option<f64>,
    /// Seconds per tool change, measuring included (default 20 ATC, 60 manual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_change_s: Option<f64>,
    /// Seconds for the spindle to start, beyond any programmed dwell (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_start_s: Option<f64>,
}

fn default_carries() -> String {
    "workpiece".into()
}
fn default_handedness() -> String {
    "right".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Kinematics {
    pub linear: Vec<LinearAxis>,
    #[serde(default)]
    pub rotary: Vec<RotaryAxis>,
    /// Spindle axis with all rotary axes at zero.
    #[serde(default = "default_spindle_axis")]
    pub spindle_axis: [f64; 3],
}

fn default_spindle_axis() -> [f64; 3] {
    [0.0, 0.0, 1.0]
}

impl Kinematics {
    pub fn linear(&self, letter: &str) -> Option<&LinearAxis> {
        self.linear
            .iter()
            .find(|a| a.letter.eq_ignore_ascii_case(letter))
    }
    pub fn rotary(&self, letter: &str) -> Option<&RotaryAxis> {
        self.rotary
            .iter()
            .find(|a| a.letter.eq_ignore_ascii_case(letter))
    }

    /// The rotary axis turning about X (either way, any letter): the one marcam's rotary
    /// toolpaths drive.
    pub fn rotary_about_x(&self) -> Option<&RotaryAxis> {
        self.rotary.iter().find(|a| {
            let n = (a.axis[0].powi(2) + a.axis[1].powi(2) + a.axis[2].powi(2)).sqrt();
            n > 0.0 && (a.axis[0].abs() / n - 1.0).abs() < 1e-6
        })
    }
}

/// Largest work the rotary axis holds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct RotaryEnvelope {
    pub max_diameter: f64,
    pub max_length: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Spindle {
    #[serde(default)]
    pub min_rpm: f64,
    pub max_rpm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolChanger {
    /// "manual" or "atc".
    pub kind: String,
    /// ATC slots (tool numbers 1..=slots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_diameter: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MachineProfile {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    pub kinematics: Kinematics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotary_envelope: Option<RotaryEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle: Option<Spindle>,
    /// Largest programmed feed the controller honours (mm/min).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_feed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_changer: Option<ToolChanger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timing>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// How 2D contours are cut by default: "climb" (None: climb) or "conventional" (less
    /// rigid machines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_direction: Option<String>,
    /// G-code templates.
    pub post: Machine,
}

fn finite(v: f64) -> bool {
    v.is_finite()
}

impl MachineProfile {
    pub fn from_json(text: &str) -> Result<MachineProfile, String> {
        let mut m: MachineProfile =
            serde_json::from_str(text).map_err(|e| format!("machine profile: {e}"))?;
        m.validate()?;
        m.post.axes = m.axis_map()?;
        Ok(m)
    }

    /// How the toolpath's axes become this machine's words (see [`crate::machine::AxisMap`]):
    /// each linear word from the axis that moves the tool along that direction (and its
    /// sign), the rotary word from the rotary axis about X. Its sign flips for a left-handed
    /// axis, an axis pointing along -X, and a head that turns the tool instead of the work.
    pub fn axis_map(&self) -> Result<crate::machine::AxisMap, String> {
        let mut m = crate::machine::AxisMap::default();
        for (k, e) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            .iter()
            .enumerate()
        {
            let along = |d: &[f64; 3]| d[0] * e[0] + d[1] * e[1] + d[2] * e[2];
            if let Some(a) = self
                .kinematics
                .linear
                .iter()
                .find(|a| a.direction.is_some_and(|d| along(&d).abs() > 0.999))
            {
                m.letters[k] = a.letter.clone();
                m.signs[k] = along(&a.direction.expect("found")).signum();
            }
        }
        for a in &self.kinematics.linear {
            if let Some(d) = a.direction {
                let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if n > 0.0 && d.iter().filter(|c| c.abs() / n > 1e-6).count() > 1 {
                    return Err(format!(
                        "machine profile {:?}: linear axis {} moves along {d:?}; marcam posts need axes along X, Y or Z",
                        self.id, a.letter
                    ));
                }
            }
        }
        if let Some(r) = self.kinematics.rotary.iter().find(|r| {
            let n = (r.axis[0].powi(2) + r.axis[1].powi(2) + r.axis[2].powi(2)).sqrt();
            n > 0.0 && (r.axis[0].abs() / n - 1.0).abs() < 1e-6
        }) {
            m.letters[3] = r.letter.clone();
            let mut sign = r.axis[0].signum();
            if r.handedness == "left" {
                sign = -sign;
            }
            if r.carries == "tool" {
                sign = -sign;
            }
            m.signs[3] = sign;
        }
        Ok(m)
    }

    /// Check the profile for mistakes; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        let e = |m: String| Err(format!("machine profile {id:?}: {m}"));
        if self.format != MACHINE_FORMAT && self.format != LEGACY_MACHINE_FORMAT {
            return e(format!(
                "format must be {MACHINE_FORMAT:?}, not {:?}",
                self.format
            ));
        }
        if self.version != FORMAT_VERSION {
            return e(format!(
                "unsupported version {} (this build reads {FORMAT_VERSION})",
                self.version
            ));
        }
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err(format!("machine profile id {id:?} must be non-empty and use only letters, digits, '-', '_', '.'"));
        }
        if self.kinematics.linear.is_empty() {
            return e("kinematics.linear must list at least one axis".into());
        }
        let mut letters: Vec<String> = Vec::new();
        for a in &self.kinematics.linear {
            if let (Some(lo), Some(hi)) = (a.min, a.max) {
                if !(finite(lo) && finite(hi) && lo < hi) {
                    return e(format!(
                        "axis {}: min {lo} must be below max {hi}",
                        a.letter
                    ));
                }
            }
            for f in [a.max_feed, a.max_rapid].into_iter().flatten() {
                if !(f > 0.0) {
                    return e(format!("axis {}: feeds must be > 0", a.letter));
                }
            }
            letters.push(a.letter.to_ascii_uppercase());
        }
        for r in &self.kinematics.rotary {
            let n = (r.axis[0].powi(2) + r.axis[1].powi(2) + r.axis[2].powi(2)).sqrt();
            if !(n > 1e-9) {
                return e(format!(
                    "rotary axis {}: axis of rotation is zero",
                    r.letter
                ));
            }
            if !matches!(r.handedness.as_str(), "right" | "left") {
                return e(format!(
                    "rotary axis {}: handedness must be \"right\" or \"left\"",
                    r.letter
                ));
            }
            if !matches!(r.carries.as_str(), "workpiece" | "tool") {
                return e(format!(
                    "rotary axis {}: carries must be \"workpiece\" or \"tool\"",
                    r.letter
                ));
            }
            if !r.continuous {
                if let (Some(lo), Some(hi)) = (r.min, r.max) {
                    if lo >= hi {
                        return e(format!("rotary axis {}: min must be below max", r.letter));
                    }
                }
            }
            letters.push(r.letter.to_ascii_uppercase());
        }
        let mut sorted = letters.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != letters.len() {
            return e(format!("axis letters repeat: {letters:?}"));
        }
        if let Some(s) = self.spindle {
            if !(s.max_rpm > 0.0 && s.min_rpm >= 0.0 && s.min_rpm <= s.max_rpm) {
                return e("spindle: need 0 <= min_rpm <= max_rpm, max_rpm > 0".into());
            }
        }
        if let Some(tc) = &self.tool_changer {
            if !matches!(tc.kind.as_str(), "manual" | "atc") {
                return e("tool_changer.kind must be \"manual\" or \"atc\"".into());
            }
        }
        if let Some(env) = self.rotary_envelope {
            if !(env.max_diameter > 0.0 && env.max_length > 0.0) {
                return e("rotary_envelope sizes must be > 0".into());
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ tools

/// Cutter geometry in a library (more shapes than the strategies use).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibShape {
    Flat {
        diameter: f64,
    },
    Ball {
        diameter: f64,
    },
    Bull {
        diameter: f64,
        corner_radius: f64,
    },
    Vbit {
        angle: f64,
        diameter: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tip_diameter: Option<f64>,
    },
    TaperedBall {
        tip_diameter: f64,
        taper_angle: f64,
        diameter: f64,
    },
    Drill {
        diameter: f64,
        #[serde(default = "default_point_angle")]
        point_angle: f64,
    },
}

fn default_point_angle() -> f64 {
    118.0
}

impl LibShape {
    pub fn diameter(&self) -> f64 {
        match *self {
            LibShape::Flat { diameter }
            | LibShape::Ball { diameter }
            | LibShape::Bull { diameter, .. }
            | LibShape::Vbit { diameter, .. }
            | LibShape::TaperedBall { diameter, .. }
            | LibShape::Drill { diameter, .. } => diameter,
        }
    }
}

/// Cutting data for a tool: starting speeds, feeds and depths for one stock material and
/// (optionally) one operation type. Picked by [`pick_preset`]; laid over the computed
/// defaults of a new operation by [`crate::ops::apply_cutting`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    /// Stock material (see [`MATERIALS`]; any other name may be used). Empty: any material.
    #[serde(default)]
    pub material: String,
    /// Operation type it is for (`"pocket"`, `"rotary_turning"` ...). Absent: any operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// Spindle speed the feeds are for; the feeds are scaled to the setup's speed (same chip
    /// load). 0: not given.
    #[serde(default)]
    pub rpm: f64,
    /// mm/min (0: not given)
    #[serde(default)]
    pub feed: f64,
    /// mm/min (0: not given)
    #[serde(default)]
    pub plunge: f64,
    /// Depth per pass, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepdown: Option<f64>,
    /// Stepover, mm (operations that take it as a fraction of the tool get it divided by
    /// the diameter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepover: Option<f64>,
    /// Ramp angle, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_angle: Option<f64>,
    /// Stock to leave on the walls (across; on a bar: radial), mm: a roughing preset's (Mar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave_xy: Option<f64>,
    /// Stock to leave on the floors (up; on a bar: axial), mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave_z: Option<f64>,
}

/// Stock materials offered for a setup. Any other name may be typed; cutting data matches
/// materials by name (ignoring case, spaces, `-` and `_`).
pub const MATERIALS: [&str; 19] = [
    "aluminium",
    "brass",
    "copper",
    "mild steel",
    "stainless steel",
    "titanium",
    "hardwood",
    "softwood",
    "plywood",
    "mdf",
    "acrylic",
    "polycarbonate",
    "pom",
    "nylon",
    "hdpe",
    "pvc",
    "pcb/fr4",
    "machining wax",
    "foam",
];

/// A material name for matching: lower case, without spaces, `-` or `_`; "aluminum" is
/// "aluminium".
pub fn material_key(m: &str) -> String {
    let k: String = m
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect();
    if k == "aluminum" {
        "aluminium".into()
    } else {
        k
    }
}

/// The cutting data for operation type `op` in `material` (None: not set) among `presets`,
/// the most specific first: this operation in this material; any operation in this
/// material; this operation in any material; any operation in any material. Among equally
/// specific ones the first wins (put a project's overrides before the library's).
pub fn pick_preset<'a>(
    presets: &[&'a Preset],
    op: &str,
    material: Option<&str>,
) -> Option<&'a Preset> {
    let want = material.map(material_key).filter(|m| !m.is_empty());
    let score = |p: &Preset| -> Option<u8> {
        let op_s = match p.op.as_deref() {
            None | Some("") => 0,
            Some(o) if o == op => 1,
            _ => return None,
        };
        let mat = material_key(&p.material);
        let mat_s = if mat.is_empty() {
            0
        } else if want.as_deref() == Some(mat.as_str()) {
            2
        } else {
            return None;
        };
        Some(mat_s + op_s)
    };
    let mut best: Option<(u8, &'a Preset)> = None;
    for p in presets {
        if let Some(s) = score(p) {
            if best.is_none_or(|(b, _)| s > b) {
                best = Some((s, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LibTool {
    /// Unique within the library.
    pub id: String,
    /// Preferred tool number / ATC slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vendor: String,
    pub shape: LibShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_length: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shank_diameter: Option<f64>,
    /// Length sticking out of the holder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stickout: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl LibTool {
    /// The tool as a job tool with `number`. Bull-nose tools are planned as flat end mills
    /// (conservative: a flat drop-cutter never goes lower than the bull nose could); drills
    /// are not usable by the strategies yet.
    pub fn to_job_tool(&self, number: u32) -> Result<Tool, String> {
        let shape = match self.shape {
            LibShape::Flat { diameter } | LibShape::Bull { diameter, .. } => {
                ToolShape::Flat { diameter }
            }
            LibShape::Ball { diameter } => ToolShape::Ball { diameter },
            LibShape::Vbit {
                angle, diameter, ..
            } => ToolShape::Vbit {
                angle,
                diameter: Some(diameter),
            },
            LibShape::TaperedBall {
                tip_diameter,
                taper_angle,
                diameter,
            } => ToolShape::TaperedBall {
                tip_diameter,
                taper_angle,
                diameter,
            },
            LibShape::Drill { .. } => {
                return Err(format!(
                    "tool {:?} ({}): this shape is not supported by the strategies yet",
                    self.id, self.name
                ))
            }
        };
        Ok(Tool {
            number,
            description: self.name.clone(),
            shape,
            flute_length: self.flute_length,
            shank_diameter: self.shank_diameter,
            stickout: self.stickout,
            holder_diameter: None,
            holder_length: None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLibrary {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    pub tools: Vec<LibTool>,
}

impl ToolLibrary {
    pub fn from_json(text: &str) -> Result<ToolLibrary, String> {
        let l: ToolLibrary =
            serde_json::from_str(text).map_err(|e| format!("tool library: {e}"))?;
        l.validate()?;
        Ok(l)
    }

    pub fn tool(&self, id: &str) -> Option<&LibTool> {
        self.tools.iter().find(|t| t.id == id)
    }

    pub fn validate(&self) -> Result<(), String> {
        let lib = &self.id;
        if self.format != TOOLS_FORMAT && self.format != LEGACY_TOOLS_FORMAT {
            return Err(format!(
                "tool library {lib:?}: format must be {TOOLS_FORMAT:?}, not {:?}",
                self.format
            ));
        }
        if self.version != FORMAT_VERSION {
            return Err(format!(
                "tool library {lib:?}: unsupported version {}",
                self.version
            ));
        }
        if lib.is_empty() {
            return Err("tool library id must not be empty".into());
        }
        let mut ids = std::collections::HashSet::new();
        for t in &self.tools {
            let e = |m: &str| Err(format!("tool library {lib:?}, tool {:?}: {m}", t.id));
            if t.id.is_empty() {
                return Err(format!(
                    "tool library {lib:?}: a tool has an empty id ({:?})",
                    t.name
                ));
            }
            if !ids.insert(t.id.as_str()) {
                return e("id is used twice");
            }
            let d = t.shape.diameter();
            if !(d > 0.0 && d.is_finite()) {
                return e("diameter must be > 0");
            }
            match t.shape {
                LibShape::Vbit {
                    angle,
                    tip_diameter,
                    ..
                } => {
                    if !(angle > 0.0 && angle < 180.0) {
                        return e("V-bit angle must be between 0 and 180 degrees");
                    }
                    if tip_diameter.is_some_and(|tip| !(0.0..d).contains(&tip)) {
                        return e("tip_diameter must be smaller than the diameter");
                    }
                }
                LibShape::Bull { corner_radius, .. } => {
                    if !(corner_radius >= 0.0 && corner_radius <= d / 2.0) {
                        return e("corner_radius must be between 0 and half the diameter");
                    }
                }
                LibShape::TaperedBall {
                    tip_diameter,
                    taper_angle,
                    ..
                } => {
                    if !(tip_diameter > 0.0
                        && tip_diameter <= d
                        && (0.0..90.0).contains(&taper_angle))
                    {
                        return e("tapered ball needs 0 < tip_diameter <= diameter and 0 <= taper_angle < 90");
                    }
                }
                _ => {}
            }
            if t.flute_length.is_some_and(|f| !(f > 0.0)) {
                return e("flute_length must be > 0");
            }
            for p in &t.presets {
                if !(p.rpm >= 0.0 && p.feed >= 0.0 && p.plunge >= 0.0) {
                    return e("preset speeds and feeds must not be negative");
                }
                if p.stepdown.is_some_and(|x| !(x > 0.0)) || p.stepover.is_some_and(|x| !(x > 0.0))
                {
                    return e("preset stepdown and stepover must be > 0");
                }
                if p.ramp_angle.is_some_and(|x| !(0.0..=89.0).contains(&x)) {
                    return e("preset ramp angle must be 0 to 89 degrees");
                }
                if let Some(o) = p.op.as_deref().filter(|o| !o.is_empty()) {
                    if crate::ops::find(o).is_none() {
                        return e(&format!("preset for unknown operation type {o:?}"));
                    }
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ registry

const BUILTIN_MACHINES: [&str; 4] = [
    include_str!("../profiles/makera/makera-z1-4axis.machine.json"),
    include_str!("../profiles/makera/makera-carvera-4axis.machine.json"),
    include_str!("../profiles/makera/makera-carvera-air-4axis.machine.json"),
    include_str!("../profiles/generic/linuxcnc-4axis.machine.json"),
];

const BUILTIN_LIBRARIES: [&str; 1] = [include_str!("../profiles/makera/makera-starter.tools.json")];

/// Machine profiles shipped with marcam.
pub fn builtin_machines() -> Vec<MachineProfile> {
    BUILTIN_MACHINES
        .iter()
        .map(|t| MachineProfile::from_json(t).expect("built-in machine profile is valid"))
        .collect()
}

/// Tool libraries shipped with marcam.
pub fn builtin_libraries() -> Vec<ToolLibrary> {
    BUILTIN_LIBRARIES
        .iter()
        .map(|t| ToolLibrary::from_json(t).expect("built-in tool library is valid"))
        .collect()
}

/// Old names for built-in machines.
pub fn canonical_machine_id(id: &str) -> &str {
    match id {
        "makera-z1" | "z1" => "makera-z1-4axis",
        "linuxcnc" => "linuxcnc-4axis",
        "carvera" => "makera-carvera-4axis",
        "carvera-air" => "makera-carvera-air-4axis",
        other => other,
    }
}

/// Built-in machine profile by id (or old alias).
pub fn builtin_machine(id: &str) -> Option<MachineProfile> {
    let id = canonical_machine_id(id);
    builtin_machines().into_iter().find(|m| m.id == id)
}

// ------------------------------------------------------------------ jobs

/// Replace library references in a job's tools with full tool definitions. A reference is
/// `{"number": 1, "library": "makera-starter", "tool": "spiral-o-metal-3.175x12"}`; an optional
/// `"description"` overrides the library name. Inline tools are left alone.
pub fn resolve_job_tools(job: &mut Value, libraries: &[ToolLibrary]) -> Result<(), String> {
    let Some(tools) = job.get_mut("tools").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for t in tools.iter_mut() {
        let (Some(lib_id), Some(tool_id)) = (
            t.get("library").and_then(Value::as_str),
            t.get("tool").and_then(Value::as_str),
        ) else {
            continue;
        };
        let lib = libraries
            .iter()
            .find(|l| l.id == lib_id)
            .ok_or_else(|| format!("tool library {lib_id:?} is not loaded"))?;
        let lt = lib
            .tool(tool_id)
            .ok_or_else(|| format!("tool library {lib_id:?} has no tool {tool_id:?}"))?;
        let number = t
            .get("number")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .or(lt.number)
            .ok_or_else(|| format!("tool {tool_id:?}: give it a \"number\""))?;
        let mut jt = lt.to_job_tool(number)?;
        if let Some(d) = t.get("description").and_then(Value::as_str) {
            jt.description = d.to_string();
        }
        *t = serde_json::to_value(jt).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Parse job JSON, resolving library tool references first.
pub fn job_from_json(text: &str, libraries: &[ToolLibrary]) -> Result<Job, String> {
    let mut v: Value = serde_json::from_str(text).map_err(|e| format!("job: {e}"))?;
    resolve_job_tools(&mut v, libraries)?;
    serde_json::from_value(v).map_err(|e| format!("job: {e}"))
}

/// A problem found when checking a job against a machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LimitIssue {
    /// "error": the machine cannot do it; "warning": it will run differently than planned.
    pub severity: String,
    pub program: Option<String>,
    pub message: String,
}

/// Check planned toolpaths and their job against the machine's limits.
pub fn check_limits(job: &Job, tps: &[Toolpath], m: &MachineProfile) -> Vec<LimitIssue> {
    let mut out = Vec::new();
    let mut push = |sev: &str, program: Option<&str>, message: String| {
        out.push(LimitIssue {
            severity: sev.into(),
            program: program.map(str::to_string),
            message,
        })
    };
    if let Some(s) = m.spindle {
        let rpm = job.spindle_rpm as f64;
        if rpm > s.max_rpm || rpm < s.min_rpm {
            push(
                "error",
                None,
                format!(
                    "spindle {rpm} rpm is outside {}..{} rpm",
                    s.min_rpm, s.max_rpm
                ),
            );
        }
    }
    // a vise setup (box stock) uses neither the rotary envelope nor the A axis
    let rotary_job = job.stock_box.is_none();
    if let Some(env) = m.rotary_envelope.filter(|_| rotary_job) {
        let d = 2.0 * job.stock.radius;
        if d > env.max_diameter {
            push(
                "error",
                None,
                format!(
                    "stock diameter {d} mm exceeds the rotary axis's {} mm",
                    env.max_diameter
                ),
            );
        }
        if job.stock.length > env.max_length {
            push(
                "error",
                None,
                format!(
                    "stock length {} mm exceeds the rotary axis's {} mm",
                    job.stock.length, env.max_length
                ),
            );
        }
    }
    if !rotary_job {
        // 3-axis programs only need X, Y and Z
    } else if m.kinematics.rotary.is_empty() {
        push(
            "error",
            None,
            "this machine has no rotary axis; marcam's strategies need one about X".into(),
        );
    } else if m.kinematics.rotary_about_x().is_none() {
        push(
            "error",
            None,
            "marcam's rotary strategies need a rotary axis turning about X".into(),
        );
    }
    if let Some(tc) = &m.tool_changer {
        for t in &job.tools {
            let d = match t.shape {
                ToolShape::Flat { diameter }
                | ToolShape::Ball { diameter }
                | ToolShape::TaperedBall { diameter, .. } => Some(diameter),
                ToolShape::Vbit { diameter, .. } => diameter,
            };
            if let (Some(d), Some(max)) = (d, tc.max_tool_diameter) {
                if d > max {
                    push(
                        "error",
                        None,
                        format!(
                            "T{}: {d} mm is wider than the machine takes ({max} mm)",
                            t.number
                        ),
                    );
                }
            }
            if let (true, Some(slots)) = (tc.kind == "atc", tc.slots) {
                if t.number == 0 || t.number > slots {
                    push(
                        "error",
                        None,
                        format!("T{}: the tool changer has slots 1..{slots}", t.number),
                    );
                }
            }
        }
    }
    let x = m.kinematics.linear("X");
    let z = m.kinematics.linear("Z");
    let a = m.kinematics.rotary_about_x();
    for tp in tps {
        let name = Some(tp.name.as_str());
        if job.stock_box.is_some() {
            // 3-axis: spans of X, Y and Z against the travels, feeds against the cap
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            let mut p: [Option<f64>; 3] = [None; 3];
            let mut fmax = 0.0f64;
            for mv in &tp.moves {
                match *mv {
                    Move::Rapid3 { x, y, z } => {
                        for (k, v) in [x, y, z].into_iter().enumerate() {
                            p[k] = v.or(p[k]);
                        }
                    }
                    Move::Line3 { x, y, z, feed, .. } => {
                        p = [Some(x), Some(y), Some(z)];
                        fmax = fmax.max(feed);
                    }
                    _ => continue,
                }
                for k in 0..3 {
                    if let Some(v) = p[k] {
                        lo[k] = lo[k].min(v);
                        hi[k] = hi[k].max(v);
                    }
                }
            }
            for (k, letter) in ["X", "Y", "Z"].into_iter().enumerate() {
                let t = m.kinematics.linear(letter).and_then(LinearAxis::travel);
                if let Some(t) = t.filter(|&t| hi[k] - lo[k] > t) {
                    push(
                        "error",
                        name,
                        format!(
                            "uses {:.1} mm of {letter}; the machine has {t} mm",
                            hi[k] - lo[k]
                        ),
                    );
                }
            }
            if let Some(max) = m.max_feed {
                if fmax > max * 1.001 {
                    push("warning", name, format!("feeds reach F{fmax:.0}; the controller caps feeds at {max:.0} mm/min, so those moves run slower than planned"));
                }
            }
            continue;
        }
        let (mut xlo, mut xhi, mut rhi) = (f64::INFINITY, f64::NEG_INFINITY, 0.0f64);
        let (mut alo, mut ahi) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut fmax = 0.0f64;
        let mut amax_rate = 0.0f64;
        let (mut ylo, mut yhi) = (0.0f64, 0.0f64);
        let (mut px, mut pr, mut pa) = (None::<f64>, None::<f64>, 0.0f64);
        for mv in &tp.moves {
            match *mv {
                Move::Rapid {
                    x: nx,
                    r: nr,
                    a: na,
                } => {
                    px = nx.or(px);
                    pr = nr.or(pr);
                    pa = na.unwrap_or(pa);
                }
                Move::Plunge { r: nr, feed } => {
                    pr = Some(nr);
                    fmax = fmax.max(feed);
                }
                Move::Cut {
                    x: nx,
                    r: nr,
                    a: na,
                    feed,
                    ..
                } => {
                    if let (Some(x0), Some(r0)) = (px, pr) {
                        let len = crate::toolpath::true_length(nx - x0, nr - r0, na - pa, r0, nr);
                        if len > 1e-9 {
                            let dt = len / feed;
                            // what the controller is asked for, as the post writes it
                            let f = match m.post.feed_mode {
                                FeedMode::InverseTime => feed,
                                mode => {
                                    crate::machine::feed_length(
                                        mode,
                                        m.post.surface_radius_pad,
                                        [nx - x0, 0.0, nr - r0, na - pa],
                                        (r0 + nr) / 2.0,
                                    ) / dt
                                }
                            };
                            fmax = fmax.max(f);
                            amax_rate = amax_rate.max((na - pa).abs() / dt);
                        }
                    }
                    px = Some(nx);
                    pr = Some(nr);
                    pa = na;
                }
                // general moves: solved for the machine
                Move::Pose { tip, axis, feed } => {
                    let Ok(na) = crate::kinematics::solve_a(axis, pa) else {
                        continue;
                    };
                    let q = crate::kinematics::to_machine(tip, na);
                    px = Some(q[0]);
                    pr = Some(q[2]);
                    pa = na;
                    ylo = ylo.min(q[1]);
                    yhi = yhi.max(q[1]);
                    if let Some(f) = feed {
                        fmax = fmax.max(f);
                    }
                }
                // indexed 3+1: machine X/Y/Z with A held
                Move::Rapid3 {
                    x: nx,
                    y: ny,
                    z: nz,
                } => {
                    px = nx.or(px);
                    pr = nz.or(pr);
                    if let Some(v) = ny {
                        ylo = ylo.min(v);
                        yhi = yhi.max(v);
                    }
                }
                Move::Line3 {
                    x: nx,
                    y: ny,
                    z: nz,
                    feed,
                    ..
                } => {
                    px = Some(nx);
                    pr = Some(nz);
                    ylo = ylo.min(ny);
                    yhi = yhi.max(ny);
                    fmax = fmax.max(feed);
                }
                _ => continue,
            }
            if let Some(v) = px {
                xlo = xlo.min(v);
                xhi = xhi.max(v);
            }
            if let Some(v) = pr {
                rhi = rhi.max(v);
            }
            alo = alo.min(pa);
            ahi = ahi.max(pa);
        }
        if let Some(t) = x.and_then(LinearAxis::travel) {
            if xhi - xlo > t {
                push(
                    "error",
                    name,
                    format!("uses {:.1} mm of X; the machine has {t} mm", xhi - xlo),
                );
            }
        }
        if let Some(t) = m.kinematics.linear("Y").and_then(LinearAxis::travel) {
            if yhi - ylo > t {
                push(
                    "error",
                    name,
                    format!("uses {:.1} mm of Y; the machine has {t} mm", yhi - ylo),
                );
            }
        } else if yhi - ylo > 1e-9 {
            push(
                "error",
                name,
                "indexed 3-axis operations move Y; this machine has no Y axis".into(),
            );
        }
        if let Some(t) = z.and_then(LinearAxis::travel) {
            if rhi > t {
                push(
                    "error",
                    name,
                    format!(
                        "needs Z {rhi:.1} mm above the rotary axis; the machine has {t} mm of Z"
                    ),
                );
            }
        }
        if let Some(a) = a {
            if !a.continuous {
                if let (Some(lo), Some(hi)) = (a.min, a.max) {
                    if alo < lo || ahi > hi {
                        push("error", name, format!("A goes from {alo:.1} to {ahi:.1} deg; the axis is limited to {lo}..{hi}"));
                    }
                }
            }
            if let Some(rate) = a.max_feed {
                if amax_rate > rate * 1.001 {
                    push("warning", name, format!("A turns at up to {amax_rate:.0} deg/min; the axis tops out at {rate:.0} deg/min, so those moves run slower"));
                }
            }
        }
        if let Some(max) = m.max_feed {
            if m.post.feed_mode != FeedMode::InverseTime && fmax > max * 1.001 {
                push("warning", name, format!("feeds reach F{fmax:.0}; the controller caps feeds at {max:.0} mm/min, so those moves run slower than planned"));
            }
        }
    }
    out
}

// ------------------------------------------------------------------ importers

/// Something read by [`import`].
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Imported {
    Machine(MachineProfile),
    Tools(ToolLibrary),
}

/// Result of an import, with notes about anything skipped or assumed.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub item: Imported,
    pub notes: Vec<String>,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Read a profile or tool library in any supported format:
/// - our own `marcam-machine` / `marcam-tools` JSON;
/// - an older marcam machine file (post templates only, e.g. `machines/*.json`);
/// - a Fusion 360 tool library: `.tools` (zip with `tools.json`) or its JSON;
/// - a Fusion 360 machine definition: `.mch` JSON, or a `.f3d` design that embeds one
///   (as in Makera's Fusion-Profiles.zip).
///
/// `name` (file name or URL) is used for ids and messages.
pub fn import(bytes: &[u8], name: &str) -> Result<Import, String> {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .split('?')
        .next()
        .unwrap_or(name);
    let base = percent_decode(base);
    let base = base.as_str();
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    if crate::archive::is_zip(bytes) {
        let es = crate::archive::entries(bytes)?;
        if let Some((n, _)) = es.iter().find(|(n, _)| n.ends_with(".mch")) {
            let mch = crate::archive::read(bytes, n, &es)?;
            return import(&mch, &format!("{stem}.mch"));
        }
        if let Some((n, _)) = es
            .iter()
            .find(|(n, _)| n.ends_with("tools.json") || n.ends_with(".json"))
        {
            let j = crate::archive::read(bytes, n, &es)?;
            return import(&j, &format!("{stem}.json"));
        }
        return Err(format!(
            "{base}: a zip without a Fusion machine (.mch) or tool library (tools.json)"
        ));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("{base}: not a JSON text or a zip"))?;
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("{base}: not valid JSON: {e}"))?;
    match v.get("format").and_then(Value::as_str) {
        Some(MACHINE_FORMAT | LEGACY_MACHINE_FORMAT) => {
            return Ok(Import {
                item: Imported::Machine(MachineProfile::from_json(text)?),
                notes: vec![],
            })
        }
        Some(TOOLS_FORMAT | LEGACY_TOOLS_FORMAT) => {
            return Ok(Import {
                item: Imported::Tools(ToolLibrary::from_json(text)?),
                notes: vec![],
            })
        }
        _ => {}
    }
    if v.get("data").is_some_and(Value::is_array) {
        return fusion_tools(&v, stem);
    }
    if v.get("kinematics").is_some() && v.get("general").is_some() {
        return fusion_machine(&v, stem);
    }
    if v.get("feed_mode").is_some() && v.get("tool_change").is_some() {
        let post = Machine::from_json(text).map_err(|e| format!("{base}: {e}"))?;
        let id = slug(stem);
        let mut p = MachineProfile {
            format: MACHINE_FORMAT.into(),
            version: FORMAT_VERSION,
            id: if id.is_empty() {
                "imported-machine".into()
            } else {
                id
            },
            name: post.name.clone(),
            vendor: String::new(),
            model: String::new(),
            sources: vec![Source {
                url: name.into(),
                note: "older marcam machine file".into(),
            }],
            kinematics: default_rotary_kinematics(),
            rotary_envelope: None,
            spindle: None,
            max_feed: None,
            tool_changer: None,
            timing: None,
            notes: vec!["Imported from post templates only: no travels or spindle limits.".into()],
            cut_direction: None,
            post,
        };
        p.validate()?;
        p.post.axes = p.axis_map()?;
        return Ok(Import {
            notes: p.notes.clone(),
            item: Imported::Machine(p),
        });
    }
    Err(format!(
        "{base}: not a marcam profile, a Fusion 360 tool library or a Fusion 360 machine"
    ))
}

/// `%20` -> space etc. (file names taken from URLs).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && s.is_char_boundary(i + 3) {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn default_rotary_kinematics() -> Kinematics {
    let ax = |l: &str, d: [f64; 3]| LinearAxis {
        letter: l.into(),
        direction: Some(d),
        min: None,
        max: None,
        max_feed: None,
        max_rapid: None,
        accel: None,
    };
    Kinematics {
        linear: vec![
            ax("X", [1.0, 0.0, 0.0]),
            ax("Y", [0.0, 1.0, 0.0]),
            ax("Z", [0.0, 0.0, 1.0]),
        ],
        rotary: vec![RotaryAxis {
            letter: "A".into(),
            axis: [1.0, 0.0, 0.0],
            carries: "workpiece".into(),
            handedness: "right".into(),
            continuous: true,
            min: None,
            max: None,
            max_feed: None,
            max_rapid: None,
            accel: None,
        }],
        spindle_axis: [0.0, 0.0, 1.0],
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

/// Fusion 360 tool library JSON (`{"data": [...], "version": n}`) -> our library.
fn fusion_tools(v: &Value, stem: &str) -> Result<Import, String> {
    let mut notes = Vec::new();
    let mut tools = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, t) in v["data"].as_array().into_iter().flatten().enumerate() {
        let kind = t.get("type").and_then(Value::as_str).unwrap_or("");
        let name = t
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(kind)
            .to_string();
        let g = &t["geometry"];
        let unit = t
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("millimeters");
        let k = if unit.starts_with("inch") { 25.4 } else { 1.0 };
        let mm = |key: &str| num(g, key).map(|x| x * k);
        let Some(dc) = mm("DC") else {
            notes.push(format!("skipped {name:?}: no cutting diameter (DC)"));
            continue;
        };
        let shape = match kind {
            "flat end mill" | "slot mill" | "face mill" => LibShape::Flat { diameter: dc },
            "ball end mill" | "lollipop mill" => LibShape::Ball { diameter: dc },
            "bull nose end mill" | "radius mill" => LibShape::Bull {
                diameter: dc,
                corner_radius: mm("RE").unwrap_or(0.0).min(dc / 2.0),
            },
            "chamfer mill" | "spot drill" | "engrave" => {
                // Fusion TA is the half angle (taper from the axis)
                let half = num(g, "TA").or_else(|| num(g, "SIG").map(|s| s / 2.0));
                let Some(half) = half.filter(|h| *h > 0.0 && *h < 90.0) else {
                    notes.push(format!("skipped {name:?}: chamfer/V tool without an angle"));
                    continue;
                };
                LibShape::Vbit {
                    angle: 2.0 * half,
                    diameter: dc,
                    tip_diameter: mm("tip-diameter").filter(|d| *d > 0.0),
                }
            }
            "tapered mill" => LibShape::TaperedBall {
                tip_diameter: mm("tip-diameter").unwrap_or(dc),
                taper_angle: num(g, "TA").unwrap_or(0.0),
                diameter: dc,
            },
            "drill" => LibShape::Drill {
                diameter: dc,
                point_angle: num(g, "SIG").unwrap_or(118.0),
            },
            other => {
                notes.push(format!(
                    "skipped {name:?}: Fusion tool type {other:?} is not supported"
                ));
                continue;
            }
        };
        let mut id = t
            .get("product-id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(slug)
            .unwrap_or_else(|| slug(&name));
        if id.is_empty() {
            id = format!("tool-{}", i + 1);
        }
        let base = id.clone();
        let mut n = 2;
        while !seen.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        let presets = t["start-values"]["presets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(Preset {
                    material: p
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_string(),
                    rpm: num(p, "n")?.round(),
                    feed: (num(p, "v_f")? * k).round(),
                    plunge: (num(p, "v_f_plunge").unwrap_or(0.0) * k).round(),
                    stepdown: num(p, "stepdown").map(|x| x * k),
                    stepover: num(p, "stepover").map(|x| x * k),
                    op: None,
                    ramp_angle: None,
                    leave_xy: None,
                    leave_z: None,
                })
            })
            .collect();
        tools.push(LibTool {
            id,
            number: t["post-process"]["number"].as_u64().map(|n| n as u32),
            name,
            vendor: t
                .get("vendor")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            shape,
            flute_length: mm("LCF"),
            overall_length: mm("OAL"),
            shank_diameter: mm("SFDM"),
            stickout: mm("LB"),
            flutes: num(g, "NOF").map(|n| n as u32),
            presets,
            product_url: t
                .get("product-link")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            note: None,
        });
    }
    let numbers: Vec<u32> = tools.iter().filter_map(|t| t.number).collect();
    if numbers.len() > 1 && numbers.iter().all(|&n| n == numbers[0]) {
        notes.push(format!(
            "every tool has number {}; numbers are kept but pick them per job",
            numbers[0]
        ));
    }
    let id = slug(stem);
    let lib = ToolLibrary {
        format: TOOLS_FORMAT.into(),
        version: FORMAT_VERSION,
        id: if id.is_empty() {
            "imported-tools".into()
        } else {
            id
        },
        name: stem.to_string(),
        vendor: String::new(),
        sources: vec![Source {
            url: format!("{stem} (Fusion 360 tool library)"),
            note: format!("{} tools imported", tools.len()),
        }],
        tools,
    };
    lib.validate()?;
    Ok(Import {
        item: Imported::Tools(lib),
        notes,
    })
}

/// Fusion 360 machine definition (`.mch` JSON) -> our machine profile.
fn fusion_machine(v: &Value, stem: &str) -> Result<Import, String> {
    let mut notes = Vec::new();
    let general = &v["general"];
    let vendor = general
        .get("vendor")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = general
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(stem)
        .to_string();
    let kin = v["kinematics"]
        .get("default")
        .or_else(|| v["kinematics"].as_object().and_then(|o| o.values().next()))
        .ok_or("machine definition has no kinematics")?;
    let controller = v["controller"].get("default").unwrap_or(&Value::Null);
    let cparts = &controller["parts"];
    let mut parts = Vec::new();
    fn walk<'a>(p: &'a Value, out: &mut Vec<&'a Value>) {
        for q in p["parts"].as_array().into_iter().flatten() {
            out.push(q);
            walk(q, out);
        }
    }
    walk(kin, &mut parts);
    let mut linear = Vec::new();
    let mut rotary = Vec::new();
    let mut spindle = None;
    let mut max_tool_d = None;
    for p in &parts {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("");
        let letter = p
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        let feed = |k: &str| num(&cparts[id], k).filter(|f| *f > 0.0);
        match p.get("type").and_then(Value::as_str) {
            Some("linear") => {
                let d = p["direction"].as_array().and_then(|a| {
                    Some([
                        a.first()?.as_f64()?,
                        a.get(1)?.as_f64()?,
                        a.get(2)?.as_f64()?,
                    ])
                });
                // Fusion gives the carriage's own direction. An axis that does not carry the
                // head moves the table, so the tool moves the other way relative to the work.
                let tool_side = parts_under_contain(p, "head");
                let d = d.map(|v| if tool_side { v } else { v.map(|c| -c) });
                linear.push(LinearAxis {
                    letter,
                    direction: d,
                    min: num(p, "min"),
                    max: num(p, "max"),
                    max_feed: feed("max_normal_speed"),
                    max_rapid: feed("max_rapid_speed"),
                    accel: None,
                });
            }
            Some("rotary") => {
                let ar = &p["axis_of_rotation"]["direction"];
                let axis = ar.as_array().and_then(|a| {
                    Some([
                        a.first()?.as_f64()?,
                        a.get(1)?.as_f64()?,
                        a.get(2)?.as_f64()?,
                    ])
                });
                let Some(axis) = axis else {
                    notes.push(format!(
                        "rotary axis {letter}: no axis of rotation, skipped"
                    ));
                    continue;
                };
                let (min, max) = (num(p, "min"), num(p, "max"));
                rotary.push(RotaryAxis {
                    letter,
                    axis,
                    carries: if parts_under_contain(p, "head") {
                        "tool".into()
                    } else {
                        "workpiece".into()
                    },
                    handedness: if v["kinematics"]["default"]["conventions"]["rotation"].as_str()
                        == Some("left-handed")
                    {
                        "left".into()
                    } else {
                        "right".into()
                    },
                    continuous: min.is_none() && max.is_none(),
                    min,
                    max,
                    max_feed: feed("max_normal_speed"),
                    max_rapid: feed("max_rapid_speed"),
                    accel: None,
                });
            }
            Some("head") => {
                let s = &p["spindle"];
                if let Some(max) = num(s, "max_speed").filter(|m| *m > 0.0) {
                    spindle = Some(Spindle {
                        min_rpm: num(s, "min_speed").unwrap_or(0.0),
                        max_rpm: max,
                        power_w: num(s, "power").filter(|p| *p > 0.0).map(|kw| kw * 1000.0),
                    });
                }
                max_tool_d = num(&p["tool_station"], "max_tool_diameter").filter(|d| *d > 0.0);
            }
            _ => {}
        }
    }
    linear.sort_by_key(|a| a.letter.clone());
    if linear.iter().all(|a| a.min.is_none() && a.max.is_none()) {
        notes.push("the machine file has no travel limits".into());
    }
    if spindle.is_none() {
        notes.push("the machine file has no spindle speed range".into());
    }
    let tooling = v["tooling"].get("default").unwrap_or(&Value::Null);
    let has_changer = tooling["has_tool_changer"].as_bool().unwrap_or(false);
    let slots = tooling["number_of_tools"].as_u64().map(|n| n as u32);
    let feed_cfg = &v["multiaxis"]["default"]["feedrate"];
    let method = feed_cfg["method"].as_str().unwrap_or("");
    let ftype = feed_cfg["type"].as_str().unwrap_or("");
    let feed_mode = match (method, ftype) {
        (m, _)
            if m.eq_ignore_ascii_case("inverse") || m.to_ascii_lowercase().contains("inverse") =>
        {
            FeedMode::InverseTime
        }
        (_, t) if t.eq_ignore_ascii_case("combination") => FeedMode::DegreesAsLength,
        ("FPM", _) => FeedMode::DegreesAsLength,
        _ => {
            notes.push(format!("multi-axis feed method {method:?}/{ftype:?} not recognised; using degrees counted as mm"));
            FeedMode::DegreesAsLength
        }
    };
    let mut post = if vendor.eq_ignore_ascii_case("makera") {
        builtin_machine("makera-z1-4axis").expect("built in").post
    } else {
        Machine {
            name: String::new(),
            feed_mode,
            surface_radius_pad: 0.0,
            decimals: 3,
            origin_comment: None,
            canned_cycles: false,
            preamble: vec!["G21 G90 G94".into(), "G17".into()],
            reset_rotary: Some("G92 A0".into()),
            tool_change: "M6 T{tool}".into(),
            spindle_on: "M3 S{rpm}".into(),
            dwell: Some("G4 P3".into()),
            postamble: vec!["M5".into(), "G0 Z{safe}".into(), "M30".into()],
            axes: Default::default(),
        }
    };
    post.feed_mode = feed_mode;
    let name = format!("{vendor} {model}").trim().to_string();
    post.name = name.clone();
    let max_feed = num(controller, "max_normal_speed")
        .filter(|f| *f > 0.0)
        .map(|f| (f * 1000.0).round() / 1000.0);
    let id = slug(&name);
    let p = MachineProfile {
        format: MACHINE_FORMAT.into(),
        version: FORMAT_VERSION,
        id: if id.is_empty() {
            "imported-machine".into()
        } else {
            id
        },
        name,
        vendor,
        model,
        sources: vec![Source {
            url: format!("{stem} (Fusion 360 machine definition)"),
            note: String::new(),
        }],
        kinematics: Kinematics {
            linear,
            rotary,
            spindle_axis: [0.0, 0.0, 1.0],
        },
        rotary_envelope: None,
        spindle,
        max_feed,
        tool_changer: Some(ToolChanger {
            kind: if has_changer {
                "atc".into()
            } else {
                "manual".into()
            },
            slots: if has_changer { slots } else { None },
            max_tool_diameter: max_tool_d,
        }),
        timing: None,
        notes: notes.clone(),
        cut_direction: None,
        post,
    };
    let mut p = p;
    p.post.axes = p.axis_map()?;
    p.validate()?;
    Ok(Import {
        item: Imported::Machine(p),
        notes,
    })
}

fn parts_under_contain(p: &Value, ty: &str) -> bool {
    p["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|q| q.get("type").and_then(Value::as_str) == Some(ty) || parts_under_contain(q, ty))
}

/// JSON for listing profiles.
pub fn summary(m: &MachineProfile) -> Value {
    json!({
        "id": m.id, "name": m.name, "vendor": m.vendor, "model": m.model,
        "feed_mode": m.post.feed_mode,
        "rotary": m.kinematics.rotary.iter().map(|r| r.letter.clone()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_load_and_validate() {
        let ms = builtin_machines();
        assert_eq!(ms.len(), 4);
        for id in [
            "makera-z1-4axis",
            "makera-carvera-4axis",
            "makera-carvera-air-4axis",
            "linuxcnc-4axis",
        ] {
            assert!(ms.iter().any(|m| m.id == id), "{id}");
        }
        assert_eq!(builtin_machine("makera-z1").unwrap().id, "makera-z1-4axis");
        let libs = builtin_libraries();
        assert_eq!(libs[0].id, "makera-starter");
        assert!(libs[0].tool("vbit-90-3.175").is_some());
    }

    #[test]
    fn validation_names_the_problem() {
        let z1 = include_str!("../profiles/makera/makera-z1-4axis.machine.json");
        let bad = z1.replace("\"max\": 200,", "\"max\": -50,");
        assert!(MachineProfile::from_json(&bad)
            .unwrap_err()
            .contains("axis X: min -10 must be below max -50"));
        let bad = z1.replace("\"version\": 1", "\"version\": 9");
        assert!(MachineProfile::from_json(&bad)
            .unwrap_err()
            .contains("unsupported version 9"));
        let bad = z1.replace("\"letter\": \"Y\"", "\"letter\": \"X\"");
        assert!(MachineProfile::from_json(&bad)
            .unwrap_err()
            .contains("repeat"));
        let lib = include_str!("../profiles/makera/makera-starter.tools.json");
        let bad = lib.replace("\"angle\": 90", "\"angle\": 190");
        assert!(ToolLibrary::from_json(&bad)
            .unwrap_err()
            .contains("between 0 and 180"));
        let bad = lib.replace("\"id\": \"ball-2x12\"", "\"id\": \"ball-1x4\"");
        assert!(ToolLibrary::from_json(&bad)
            .unwrap_err()
            .contains("used twice"));
    }

    #[test]
    fn job_tools_resolve_from_library() {
        let libs = builtin_libraries();
        let mut v = json!({"tools": [
            {"number": 1, "library": "makera-starter", "tool": "spiral-o-metal-3.175x12"},
            {"number": 4, "library": "makera-starter", "tool": "vbit-90-3.175", "description": "my V"},
            {"number": 5, "description": "inline", "kind": "flat", "diameter": 2.0}
        ]});
        resolve_job_tools(&mut v, &libs).unwrap();
        assert_eq!(v["tools"][0]["kind"], "flat");
        assert_eq!(v["tools"][0]["diameter"], 3.175);
        assert_eq!(v["tools"][0]["flute_length"], 12.0);
        assert_eq!(v["tools"][1]["description"], "my V");
        assert_eq!(v["tools"][1]["diameter"], 3.175);
        assert_eq!(v["tools"][2]["description"], "inline");
        let mut bad =
            json!({"tools": [{"number": 1, "library": "makera-starter", "tool": "nope"}]});
        assert!(resolve_job_tools(&mut bad, &libs)
            .unwrap_err()
            .contains("no tool \"nope\""));
        let mut bad = json!({"tools": [{"number": 1, "library": "x", "tool": "y"}]});
        assert!(resolve_job_tools(&mut bad, &libs)
            .unwrap_err()
            .contains("not loaded"));
        let mut drill =
            json!({"tools": [{"number": 1, "library": "makera-starter", "tool": "drill-3.175"}]});
        assert!(resolve_job_tools(&mut drill, &libs)
            .unwrap_err()
            .contains("not supported"));
    }

    fn fusion_tool(kind: &str, desc: &str, geometry: Value) -> Value {
        json!({"type": kind, "description": desc, "unit": "millimeters", "geometry": geometry,
               "post-process": {"number": 1},
               "start-values": {"presets": [{"name": "Aluminum", "n": 12000.0, "v_f": 500.0, "v_f_plunge": 200.0, "stepdown": 0.5}]}})
    }

    /// Synthetic library in Fusion 360's format (same keys as Makera's published files).
    pub(crate) fn fusion_library() -> Value {
        json!({"version": 36, "data": [
            fusion_tool("flat end mill", "Spiral O Metal 3.175*12mm", json!({"DC": 3.175, "LCF": 12, "OAL": 38, "NOF": 1, "SFDM": 3.175})),
            fusion_tool("ball end mill", "Ball 2*12mm", json!({"DC": 2, "LCF": 12})),
            fusion_tool("chamfer mill", "Engraving 30 deg*.2mm", json!({"DC": 3.175, "TA": 15, "tip-diameter": 0.2, "LCF": 9})),
            fusion_tool("bull nose end mill", "Bull 6 r1", json!({"DC": 6, "RE": 1})),
            fusion_tool("drill", "Drill 3", json!({"DC": 3, "SIG": 118})),
            fusion_tool("thread mill", "Thread M3", json!({"DC": 2.4})),
            {"type": "holder", "description": "holder only", "geometry": {}}
        ]})
    }

    #[test]
    fn fusion_tool_library_imports() {
        let text = fusion_library().to_string();
        let imp = import(
            text.as_bytes(),
            "https://x.test/Tool%20Files/Makera%20Metal%20Tools.tools",
        )
        .unwrap();
        let Imported::Tools(lib) = imp.item else {
            panic!()
        };
        assert_eq!(lib.id, "makera-metal-tools");
        assert_eq!(lib.tools.len(), 5, "{:?}", imp.notes);
        assert_eq!(lib.tools[0].shape, LibShape::Flat { diameter: 3.175 });
        assert_eq!(lib.tools[0].flute_length, Some(12.0));
        assert_eq!(lib.tools[0].presets[0].feed, 500.0);
        assert_eq!(
            lib.tools[2].shape,
            LibShape::Vbit {
                angle: 30.0,
                diameter: 3.175,
                tip_diameter: Some(0.2)
            }
        );
        assert_eq!(
            lib.tools[3].shape,
            LibShape::Bull {
                diameter: 6.0,
                corner_radius: 1.0
            }
        );
        assert!(imp.notes.iter().any(|n| n.contains("thread mill")));
        assert!(imp.notes.iter().any(|n| n.contains("holder only")));
        // the same JSON zipped as a .tools file
        let zip = crate::archive::tests::make_zip(&[("tools.json", text.as_bytes(), true)]);
        let imp2 = import(&zip, "Makera Metal Tools.tools").unwrap();
        assert_eq!(imp2.item, Imported::Tools(lib));
    }

    /// Synthetic machine definition in Fusion 360's `.mch` format (as found in Makera's .f3d).
    pub(crate) fn fusion_machine_json() -> Value {
        json!({
            "general": {"vendor": "Makera", "model": "Test 4 axis"},
            "controller": {"default": {"max_normal_speed": 4000.0000000000005, "parts": {
                "X": {"max_normal_speed": 4000, "max_rapid_speed": 4000},
                "rotary_0": {"max_normal_speed": 21600, "max_rapid_speed": 0}}}},
            "kinematics": {"default": {"conventions": {"rotation": "right-handed"}, "parts": [
                {"id": "Y", "name": "Y", "type": "linear", "direction": [0, -1, 0], "min": -200, "max": 10, "parts": [
                    {"id": "rotary_0", "name": "A", "type": "rotary",
                     "axis_of_rotation": {"direction": [1, 0, 0], "point": [14, 0, 0]},
                     "parts": [{"id": "table", "type": "table"}]}]},
                {"id": "X", "name": "X", "type": "linear", "direction": [1, 0, 0], "min": -10, "max": 200, "parts": [
                    {"id": "Z", "name": "Z", "type": "linear", "direction": [0, 0, 1], "min": 0, "max": 100, "parts": [
                        {"id": "head", "type": "head", "spindle": {"max_speed": 13000, "min_speed": 0, "power": 0.15},
                         "tool_station": {"max_tool_diameter": 8}}]}]}]}},
            "multiaxis": {"default": {"feedrate": {"method": "FPM", "type": "combination"}}},
            "tooling": {"default": {"has_tool_changer": true, "number_of_tools": 6}}
        })
    }

    #[test]
    fn fusion_machine_imports_from_mch_and_f3d() {
        let text = fusion_machine_json().to_string();
        let imp = import(text.as_bytes(), "simulation.mch").unwrap();
        // the table's Y [0, -1, 0] becomes the tool's motion relative to the work: +Y
        let Imported::Machine(m0) = &imp.item else {
            panic!("not a machine")
        };
        let dirs: Vec<_> = m0
            .kinematics
            .linear
            .iter()
            .map(|a| (a.letter.clone(), a.direction))
            .collect();
        assert!(
            dirs.contains(&("Y".to_string(), Some([0.0, 1.0, 0.0]))),
            "{dirs:?}"
        );
        assert!(
            dirs.contains(&("X".to_string(), Some([1.0, 0.0, 0.0]))),
            "{dirs:?}"
        );
        let Imported::Machine(m) = &imp.item else {
            panic!()
        };
        assert_eq!(m.id, "makera-test-4-axis");
        let x = m.kinematics.linear("X").unwrap();
        assert_eq!(
            (x.min, x.max, x.max_feed),
            (Some(-10.0), Some(200.0), Some(4000.0))
        );
        assert_eq!(m.kinematics.linear("Z").unwrap().travel(), Some(100.0));
        let a = m.kinematics.rotary("A").unwrap();
        assert_eq!(a.axis, [1.0, 0.0, 0.0]);
        assert!(a.continuous);
        assert_eq!(a.max_feed, Some(21600.0));
        assert_eq!(m.spindle.unwrap().max_rpm, 13000.0);
        assert_eq!(m.spindle.unwrap().power_w, Some(150.0));
        assert_eq!(m.max_feed, Some(4000.0));
        assert_eq!(m.post.feed_mode, FeedMode::DegreesAsLength);
        let tc = m.tool_changer.as_ref().unwrap();
        assert_eq!(
            (tc.kind.as_str(), tc.slots, tc.max_tool_diameter),
            ("atc", Some(6), Some(8.0))
        );
        // Makera machines get the Makera post templates
        assert!(m.post.tool_change.starts_with("M6 T{tool}"));
        // inside an .f3d (zip, zstd entries in the real files)
        let f3d = crate::archive::tests::make_zip(&[
            ("Manifest.dat", b"x".as_slice(), false),
            (
                "MachineAsset/Machines/simulation.mch",
                text.as_bytes(),
                true,
            ),
        ]);
        assert_eq!(
            import(&f3d, "Makera Test 4 axis.f3d").unwrap().item,
            Imported::Machine(MachineProfile {
                sources: vec![Source {
                    url: "Makera Test 4 axis (Fusion 360 machine definition)".into(),
                    note: String::new()
                }],
                ..m.clone()
            })
        );
    }

    #[test]
    fn profiles_saved_before_the_rename_still_load() {
        let z1 = builtin_machine("makera-z1-4axis").unwrap();
        let mut v = serde_json::to_value(&z1).unwrap();
        v["format"] = json!(LEGACY_MACHINE_FORMAT);
        let old = v.to_string();
        let m = MachineProfile::from_json(&old).unwrap();
        assert_eq!(m.id, z1.id);
        assert!(matches!(
            import(old.as_bytes(), "z1.json").unwrap().item,
            Imported::Machine(_)
        ));
        let lib = &builtin_libraries()[0];
        let mut v = serde_json::to_value(lib).unwrap();
        v["format"] = json!(LEGACY_TOOLS_FORMAT);
        assert!(ToolLibrary::from_json(&v.to_string()).is_ok());
        assert!(matches!(
            import(v.to_string().as_bytes(), "tools.json").unwrap().item,
            Imported::Tools(_)
        ));
    }

    #[test]
    fn limits_catch_out_of_range_jobs() {
        let job = Job::from_json(include_str!("../../../examples/knob/knob.job.json")).unwrap();
        let tps = crate::plan(&job).unwrap();
        let z1 = builtin_machine("makera-z1-4axis").unwrap();
        let issues = check_limits(&job, &tps, &z1);
        // the cut knob G-code asks for more than the Z1's 4000 mm/min at small radii
        assert!(issues.iter().all(|i| i.severity == "warning"), "{issues:?}");
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("caps feeds at 4000")),
            "{issues:?}"
        );
        let mut big = job.clone();
        big.stock.radius = 50.0;
        big.spindle_rpm = 20000;
        let issues = check_limits(&big, &tps, &z1);
        let errs: Vec<&str> = issues
            .iter()
            .filter(|i| i.severity == "error")
            .map(|i| i.message.as_str())
            .collect();
        assert!(errs.iter().any(|m| m.contains("20000 rpm")), "{errs:?}");
        assert!(
            errs.iter().any(|m| m.contains("stock diameter 100")),
            "{errs:?}"
        );
        let carvera = builtin_machine("carvera").unwrap();
        let mut t9 = job.clone();
        t9.tools[0].number = 9;
        let issues = check_limits(&t9, &tps, &carvera);
        assert!(
            issues.iter().any(|i| i.message.contains("slots 1..6")),
            "{issues:?}"
        );
    }
}
