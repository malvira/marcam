//! Document model 0.2: a Project holds models, tools and setups; each setup has a machine,
//! stock, holding and an ordered list of operations, grouped into output programs (G-code
//! files). 0.1 jobs upgrade automatically. Today a setup is planned by lowering it to a 0.1
//! [`Job`]; the Z1 knob programs come out byte-identical either way.
//!
//! Operation ids are strings unique within the project; they are what colours, simulation
//! tags and (later) the frontend's operation tree key off.

use crate::det::Det;
use crate::job::{self, Job, MeshSource, Operation, Profile, Stock, Tool};
use crate::profiles::{self, ToolLibrary};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROJECT_SCHEMA: &str = "0.2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub schema_version: String,
    #[serde(default)]
    pub name: String,
    pub models: Vec<Model>,
    /// Tools by number: inline definitions or library references. The number is also the
    /// T number written in the G-code, unless the tool is in `unindexed`.
    pub tools: Vec<ToolEntry>,
    /// Tools (by their number in `tools`) not given a T number yet: the number is only the
    /// project's way to name them. Planned, each is written as the next free T number, with
    /// a warning in the G-code (index it before running).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unindexed: Vec<u32>,
    pub setups: Vec<Setup>,
}

/// Part geometry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: ModelKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelKind {
    /// Revolved profile about the rotary axis.
    Profile(Profile),
    /// Triangle mesh file (STL/3MF), placed on the axis.
    Mesh(MeshSource),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolEntry {
    /// `{"number": 1, "library": "makera-starter", "tool": "..."}`
    Library {
        number: u32,
        library: String,
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// This project's own cutting data for the tool, picked before the library's (see
        /// [`crate::profiles::pick_preset`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cutting: Vec<crate::profiles::Preset>,
    },
    Inline(Tool),
}

impl ToolEntry {
    pub fn number(&self) -> u32 {
        match self {
            ToolEntry::Library { number, .. } => *number,
            ToolEntry::Inline(t) => t.number,
        }
    }
}

/// How the work is held.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Holding {
    /// "rotary_chuck" (today) or later "vise", "fixture".
    pub kind: String,
    #[serde(default)]
    pub tailstock: bool,
    /// A bar's length held in the chuck (mm): the rest sticks out to be cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chuck: Option<f64>,
    /// A vise: the stock's depth gripped by the jaws (mm up from its bottom). None: 3 mm or a
    /// third of a block's height (the smaller), 40% of a bar's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaws: Option<f64>,
    /// A bar in a vise: along "y", or "z" (standing up); None: along X (Mar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub along: Option<String>,
    /// (Mar's Fixture) a vise's jaws: their length along X, their height, the opening
    /// between them (mm; None: the usual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaw_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jaw_height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<f64>,
    /// The chuck: its diameter and its body's length behind the bar (mm; None: not checked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chuck_diameter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chuck_length: Option<f64>,
    /// The tailstock's point (X, mm; None: the bar's front) and how deep its centre goes in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailstock_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centre_depth: Option<f64>,
}

impl Default for Holding {
    fn default() -> Self {
        Holding {
            kind: "rotary_chuck".into(),
            tailstock: false,
            chuck: None,
            jaws: None,
            along: None,
            jaw_width: None,
            jaw_height: None,
            opening: None,
            chuck_diameter: None,
            chuck_length: None,
            tailstock_x: None,
            centre_depth: None,
        }
    }
}

/// A setup's part, stock and carry in its own work coordinates ([`Project::setup_frame`]).
struct SetupFrame {
    part: serde_json::Map<String, Value>,
    stock: Stock,
    stock_box: Option<job::StockBox>,
    stock_axis: Option<[f64; 2]>,
    carry: Option<job::Carry>,
}

fn is_zero3(v: &[f64; 3]) -> bool {
    *v == [0.0; 3]
}

/// Stock shape. A setup with `from_previous` starts from the previous setup's result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum StockDef {
    Cylinder {
        radius: f64,
        length: f64,
    },
    /// The stock left by the previous setup (simulated), or by setup `from` (any setup before
    /// this one), turned (`turn`: flipped over, or end for end) and moved: re-zeroed where it
    /// was (its X and Y lowest corner, and its top for a block, where they were), then by
    /// `offset`. The part turns and moves with it. A bar's `radius` and `length` are that
    /// setup's when zero.
    FromPrevious {
        /// The setup whose stock this one takes (None: the one just before).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// Turned any way first (degrees about X, then Y, then Z): the cut result goes on as a
        /// mesh, sampled again in this setup's kind (Mar). None: the turns below only.
        #[serde(default, skip_serializing_if = "is_zero3")]
        rotate: [f64; 3],
        #[serde(default)]
        radius: f64,
        #[serde(default)]
        length: f64,
        #[serde(default, skip_serializing_if = "job::Turn::is_none")]
        turn: job::Turn,
        #[serde(default, skip_serializing_if = "is_zero3")]
        offset: [f64; 3],
    },
    /// Rectangular block in a vise (3-axis), corners in work coordinates (Z up).
    Box {
        min: [f64; 3],
        max: [f64; 3],
    },
    /// Square bar on the rotary axis: `side` across its flats, one flat at `angle` (0: on
    /// top).
    Square {
        side: f64,
        length: f64,
        #[serde(default)]
        angle: f64,
    },
    /// Rectangular bar on the rotary axis: `width` along Y, `height` along Z, turned by
    /// `angle`.
    Rect {
        width: f64,
        height: f64,
        length: f64,
        #[serde(default)]
        angle: f64,
    },
    /// Hexagonal bar on the rotary axis: `across_flats` wide, one flat at `angle` (0: on
    /// top).
    Hex {
        across_flats: f64,
        length: f64,
        #[serde(default)]
        angle: f64,
    },
}

impl StockDef {
    pub fn cylinder(&self) -> Stock {
        match *self {
            StockDef::Cylinder { radius, length }
            | StockDef::FromPrevious { radius, length, .. } => Stock {
                radius,
                length,
                section: None,
            },
            StockDef::Square {
                side,
                length,
                angle,
            } => Stock::of_section(
                crate::job::Section::Polygon {
                    sides: 4,
                    across_flats: side,
                    angle,
                },
                length,
            ),
            StockDef::Rect {
                width,
                height,
                length,
                angle,
            } => Stock::of_section(
                crate::job::Section::Rect {
                    width,
                    height,
                    angle,
                },
                length,
            ),
            StockDef::Hex {
                across_flats,
                length,
                angle,
            } => Stock::of_section(
                crate::job::Section::Polygon {
                    sides: 6,
                    across_flats,
                    angle,
                },
                length,
            ),
            // rotary fields for a block: the radius that encloses it about X
            StockDef::Box { min, max } => Stock {
                radius: [min[1].abs(), max[1].abs()]
                    .iter()
                    .flat_map(|y| [min[2].abs(), max[2].abs()].map(|z| y.dhypot(z)))
                    .fold(0.0, f64::max),
                length: max[0] - min[0],
                section: None,
            },
        }
    }

    /// Whether it is a bar on the rotary axis (any section) rather than a block in a vise.
    pub fn is_bar(&self) -> bool {
        !matches!(self, StockDef::Box { .. })
    }
}

impl Stock {
    /// A bar of `section`, its radius at the corners.
    pub fn of_section(section: crate::job::Section, length: f64) -> Stock {
        Stock {
            radius: section.corner_radius(),
            length,
            section: Some(section),
        }
    }
}

/// An operation in a setup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectOp {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Display colour for the material it removes (`#rrggbb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Where it may cut (empty = the whole model). See [`crate::geometry::Selection`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometry: Vec<crate::geometry::Selection>,
    /// What was picked for it, by what each feature is (kind, position, size: see
    /// [`crate::replace`]), so it is found again on a changed model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub picks: Vec<serde_json::Value>,
    /// Why it cannot be planned (a picked feature the model no longer has): left out of the
    /// G-code until it is picked again or changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub op: Operation,
}

impl ProjectOp {
    /// Whether it is planned: enabled and not in error.
    pub fn plans(&self) -> bool {
        self.enabled && self.error.is_none()
    }
}

impl Project {
    /// Whether an operation is planned in this project: [`ProjectOp::plans`], and its tool is
    /// one of the project's (an operation whose tool was removed is left out, like one in
    /// error).
    pub fn op_plans(&self, o: &ProjectOp) -> bool {
        o.plans() && self.tools.iter().any(|t| t.number() == o.op.tool())
    }
}

fn yes() -> bool {
    true
}
fn default_clearance() -> f64 {
    3.0
}

/// One G-code file: operations in order, plus header notes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Output {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default)]
    pub reset_a: bool,
    /// Operation ids.
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Setup {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// What kind of setup this is: "rotary" (4th axis, simultaneous: the rotary
    /// operations), "indexed" (rotary 3+1: 3-axis operations at fixed A angles) or "vise"
    /// (3-axis on a block). Absent: a bar takes any operation, a block the 3-axis ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub stock: StockDef,
    /// Stock material (see [`crate::profiles::MATERIALS`]): picks the tools' cutting data
    /// for new operations. Absent: not set (computed defaults).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default = "default_clearance")]
    pub clearance: f64,
    #[serde(default)]
    pub holding: Holding,
    /// Model ids that make up the finished part (a profile and/or a mesh).
    pub part: Vec<String>,
    pub spindle_rpm: u32,
    pub operations: Vec<ProjectOp>,
    pub outputs: Vec<Output>,
    /// Its G-code options: work offset, header and footer lines, line numbers, comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<job::PostOptions>,
    /// The project's tools (by number) this setup lists besides those its operations use (a
    /// tool brought in for it, ready for its operations). Nothing is planned from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<u32>,
}

impl Project {
    /// Read a 0.2 project, or a 0.1 job (upgraded). Returns whether it was upgraded.
    pub fn from_json_any(text: &str) -> Result<(Project, bool), String> {
        // (TOML or JSON: see project_file)
        let text = &*crate::project_file::to_json_text(text)?;
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        match v.get("schema_version").and_then(Value::as_str) {
            Some(job::SCHEMA_VERSION) => {
                let job: Job = serde_json::from_value(v).map_err(|e| format!("job 0.1: {e}"))?;
                Ok((Project::from_job(&job, "knob"), true))
            }
            Some(PROJECT_SCHEMA) => {
                let p: Project =
                    serde_json::from_value(v).map_err(|e| format!("project 0.2: {e}"))?;
                p.validate()?;
                Ok((p, false))
            }
            other => Err(format!(
                "unknown schema_version {other:?} (expected 0.1 or 0.2)"
            )),
        }
    }

    /// Upgrade a 0.1 job: one setup holding all its programs as outputs.
    pub fn from_job(job: &Job, name: &str) -> Project {
        let mut models = Vec::new();
        let mut part = Vec::new();
        if let Some(p) = &job.part.profile {
            models.push(Model {
                id: "profile".into(),
                name: "revolved profile".into(),
                kind: ModelKind::Profile(p.clone()),
            });
            part.push("profile".into());
        }
        if let Some(m) = &job.part.mesh {
            models.push(Model {
                id: "mesh".into(),
                name: m.file.clone(),
                kind: ModelKind::Mesh(m.clone()),
            });
            part.push("mesh".into());
        }
        let mut operations = Vec::new();
        let mut outputs = Vec::new();
        for prog in &job.programs {
            let mut ids = Vec::new();
            for op in &prog.operations {
                let id = format!("op{}", operations.len() + 1);
                ids.push(id.clone());
                operations.push(ProjectOp {
                    id,
                    name: String::new(),
                    color: None,
                    enabled: true,
                    geometry: Vec::new(),
                    picks: Vec::new(),
                    error: None,
                    op: op.clone(),
                });
            }
            outputs.push(Output {
                name: prog.name.clone(),
                title: prog.title.clone(),
                notes: prog.notes.clone(),
                reset_a: prog.reset_a,
                operations: ids,
            });
        }
        Project {
            schema_version: PROJECT_SCHEMA.into(),
            name: name.into(),
            models,
            tools: job.tools.iter().cloned().map(ToolEntry::Inline).collect(),
            unindexed: Vec::new(),
            setups: vec![Setup {
                id: "setup1".into(),
                name: "Rotary setup".into(),
                machine: job.machine.clone(),
                kind: None,
                stock: StockDef::Cylinder {
                    radius: job.stock.radius,
                    length: job.stock.length,
                },
                material: None,
                clearance: job.clearance,
                holding: Holding::default(),
                part,
                spindle_rpm: job.spindle_rpm,
                operations,
                outputs,
                post: None,
                tools: Vec::new(),
            }],
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_SCHEMA {
            return Err(format!("project schema_version must be {PROJECT_SCHEMA:?}"));
        }
        let mut ids = std::collections::HashSet::new();
        for m in &self.models {
            if !ids.insert(format!("model:{}", m.id)) {
                return Err(format!("model id {:?} is used twice", m.id));
            }
        }
        let mut numbers = std::collections::HashSet::new();
        for t in &self.tools {
            if !numbers.insert(t.number()) {
                return Err(format!("tool number T{} is used twice", t.number()));
            }
        }
        let mut setups = std::collections::HashSet::new();
        let mut outputs = std::collections::HashSet::new();
        for (k, s) in self.setups.iter().enumerate() {
            if k == 0 && matches!(s.stock, StockDef::FromPrevious { .. }) {
                return Err(format!(
                    "setup {:?}: the first setup cannot take its stock from a previous one",
                    s.id
                ));
            }
            if let StockDef::FromPrevious { from: Some(f), .. } = &s.stock {
                if !self.setups[..k].iter().any(|x| &x.id == f) {
                    return Err(format!(
                        "setup {:?}: its stock comes from {f:?}, which is not a setup before it",
                        s.id
                    ));
                }
            }
            for out in &s.outputs {
                if !outputs.insert(out.name.clone()) {
                    return Err(format!(
                        "output name {:?} is used twice (G-code files would overwrite each other)",
                        out.name
                    ));
                }
            }
            if !setups.insert(&s.id) {
                return Err(format!("setup id {:?} is used twice", s.id));
            }
            for p in &s.part {
                if !self.models.iter().any(|m| &m.id == p) {
                    return Err(format!("setup {:?}: unknown model {p:?}", s.id));
                }
            }
            let mut ops = std::collections::HashSet::new();
            for o in &s.operations {
                if !ids.insert(format!("op:{}", o.id)) || !ops.insert(o.id.as_str()) {
                    return Err(format!("operation id {:?} is used twice", o.id));
                }
                // (an operation whose tool the project does not have is not an invalid project:
                // it is left out of planning, see `op_plans`)
            }
            for out in &s.outputs {
                for id in &out.operations {
                    if !ops.contains(id.as_str()) {
                        return Err(format!("output {:?}: unknown operation {id:?}", out.name));
                    }
                }
            }
        }
        Ok(())
    }

    /// Check every operation against the operation registry (parameters, ranges, tool shapes
    /// for inline tools). Problems are prefixed with the setup and operation id.
    pub fn check_operations(&self) -> Vec<String> {
        let inline: Vec<Tool> = self
            .tools
            .iter()
            .filter_map(|t| match t {
                ToolEntry::Inline(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for s in &self.setups {
            if let Some(k) = s.kind.as_deref() {
                // (stock from the previous setup: a block if that was in a vise)
                let carried = matches!(s.stock, StockDef::FromPrevious { .. });
                let block = match s.stock {
                    StockDef::Box { .. } => true,
                    StockDef::FromPrevious { .. } => {
                        self.setup_frame(s).is_ok_and(|f| f.stock_box.is_some())
                    }
                    _ => false,
                };
                match k {
                    // (a block, or a bar lying in the vise along X)
                    "vise" if carried && !block => out.push(format!(
                        "{}: a vise setup can take the stock of a previous vise setup, not of a rotary one",
                        s.id
                    )),
                    "rotary" | "indexed" if block => {
                        out.push(format!("{}: a {k} setup needs bar stock", s.id))
                    }
                    "vise" | "rotary" | "indexed" => {}
                    _ => out.push(format!(
                        "{}: kind {k:?}: use \"rotary\", \"indexed\" or \"vise\"",
                        s.id
                    )),
                }
            }
            for o in &s.operations {
                let kind = crate::ops::kind_of(&o.op);
                if let (Some(k), Some(t)) = (s.kind.as_deref(), crate::ops::find(kind)) {
                    if !crate::ops::fits_setup(&t, k) && !crate::ops::indexed_on_rotary(&o.op, k) {
                        out.push(format!(
                            "{} / {}: {kind} does not fit a {k} setup",
                            s.id, o.id
                        ));
                    }
                }
                let v = serde_json::to_value(&o.op).unwrap_or(Value::Null);
                let has_inline = inline.iter().any(|t| t.number == o.op.tool());
                for e in crate::ops::check(&v, has_inline.then_some(inline.as_slice())) {
                    out.push(format!("{} / {}: {e}", s.id, o.id));
                }
            }
        }
        out
    }

    /// Defaults for a new operation in a setup: every parameter filled from the setup's
    /// tools, spindle speed, machine limits, part faces and the given geometry selection.
    #[allow(clippy::too_many_arguments)]
    pub fn op_defaults(
        &self,
        kind: &str,
        setup: &str,
        tool: Option<u32>,
        geometry: &[crate::geometry::Selection],
        libraries: &[ToolLibrary],
        machine: Option<&crate::profiles::MachineProfile>,
        meshes: &dyn Fn(&str) -> Option<crate::Mesh>,
    ) -> Result<Value, String> {
        self.op_defaults_with(
            kind, setup, tool, geometry, libraries, machine, meshes, None,
        )
    }

    /// [`Project::op_defaults`], with the stock the setup before left (`carried`, when the
    /// setup takes it: a knurl on a cut piece with no model finds its body there).
    #[allow(clippy::too_many_arguments)]
    pub fn op_defaults_with(
        &self,
        kind: &str,
        setup: &str,
        tool: Option<u32>,
        geometry: &[crate::geometry::Selection],
        libraries: &[ToolLibrary],
        machine: Option<&crate::profiles::MachineProfile>,
        meshes: &dyn Fn(&str) -> Option<crate::Mesh>,
        carried: Option<std::sync::Arc<crate::carry::Carried>>,
    ) -> Result<Value, String> {
        let s = self.setup(setup).ok_or(format!("no setup {setup:?}"))?;
        let mut job = self.setup_job_raw(s, libraries)?;
        if carried.is_some() {
            job.carried = carried;
        }
        // faces of the part: the mesh model if any, else the profile
        let model = s
            .part
            .iter()
            .find(|id| {
                self.models
                    .iter()
                    .any(|m| &m.id == *id && matches!(m.kind, ModelKind::Mesh(_)))
            })
            .or(s.part.first());
        let regions = model.and_then(|m| self.model_regions(m, meshes));
        let radius = tool
            .and_then(|n| job.tool(n))
            .map_or(0.0, |t| match t.shape {
                job::ToolShape::Flat { diameter } | job::ToolShape::Ball { diameter } => {
                    diameter / 2.0
                }
                job::ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0) / 2.0,
                job::ToolShape::TaperedBall { tip_diameter, .. } => tip_diameter / 2.0,
            });
        let boundary = if geometry.is_empty() {
            None
        } else {
            Some(crate::geometry::boundary(
                geometry,
                &|m| self.model_regions(m, meshes),
                radius,
            )?)
        };
        let machine_owned;
        let machine = match machine {
            Some(m) => Some(m),
            None => {
                machine_owned = s.machine.as_deref().and_then(profiles::builtin_machine);
                machine_owned.as_ref()
            }
        };
        let mut v = crate::ops::defaults(
            kind,
            &crate::ops::DefaultsContext {
                job: &job,
                regions: regions.as_deref(),
                machine,
                tool,
                boundary,
                part_xy: model.and_then(|m| {
                    let mesh = match &self.models.iter().find(|x| &x.id == m)?.kind {
                        ModelKind::Mesh(_) => meshes(m)?,
                        ModelKind::Profile(_) => return None,
                    };
                    let (lo, hi) = mesh.bounds()?;
                    Some(([lo[0], hi[0]], [lo[1], hi[1]]))
                }),
                part_z: model.and_then(|m| {
                    let mesh = match &self.models.iter().find(|x| &x.id == m)?.kind {
                        ModelKind::Mesh(_) => meshes(m)?,
                        ModelKind::Profile(_) => return None,
                    };
                    let (lo, hi) = mesh.bounds()?;
                    Some([lo[2], hi[2]])
                }),
            },
        )?;
        // the tool's cutting data for this operation in the setup's material
        let n = v["tool"].as_u64().map_or(0, |n| n as u32);
        if let Some(p) = self.cutting_preset(kind, n, s.material.as_deref(), libraries) {
            let d = job.tool(n).map_or(0.0, |t| match t.shape {
                job::ToolShape::Flat { diameter } | job::ToolShape::Ball { diameter } => diameter,
                job::ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0),
                job::ToolShape::TaperedBall { tip_diameter, .. } => tip_diameter,
            });
            let cap = machine
                .and_then(|m| m.max_feed)
                .map_or(f64::INFINITY, |c| 0.75 * c);
            crate::ops::apply_cutting(&mut v, &p, s.spindle_rpm as f64, d, cap);
        }
        if !geometry.is_empty() {
            v["geometry"] = serde_json::to_value(geometry).map_err(|e| e.to_string())?;
        }
        Ok(v)
    }

    /// The cutting data tool `tool` has for operation type `kind` in `material`, from the
    /// project's own (the tool entry's `cutting`) and its library's, the most specific first
    /// (see [`profiles::pick_preset`]; the project's win a tie). None: an inline tool, or no
    /// data fits (the computed defaults stand).
    pub fn cutting_preset(
        &self,
        kind: &str,
        tool: u32,
        material: Option<&str>,
        libraries: &[ToolLibrary],
    ) -> Option<profiles::Preset> {
        let Some(ToolEntry::Library {
            library,
            tool: id,
            cutting,
            ..
        }) = self.tools.iter().find(|t| t.number() == tool)
        else {
            return None;
        };
        let builtin = profiles::builtin_libraries();
        let lib_tool = libraries
            .iter()
            .chain(builtin.iter())
            .find(|l| &l.id == library)
            .and_then(|l| l.tool(id));
        let all: Vec<&profiles::Preset> = cutting
            .iter()
            .chain(lib_tool.into_iter().flat_map(|t| t.presets.iter()))
            .collect();
        profiles::pick_preset(&all, kind, material).cloned()
    }

    /// Change a setup's stock material (None or blank: not set). In each of its operations,
    /// a cutting parameter (feeds, depth per pass, stepover, ramp angle) that still has the
    /// value the old material gave it takes the new material's; one typed over is kept.
    /// Returns `"<operation id>: <parameter>"` for each value changed.
    pub fn set_material(
        &mut self,
        setup: &str,
        material: Option<String>,
        libraries: &[ToolLibrary],
        machine: Option<&crate::profiles::MachineProfile>,
    ) -> Result<Vec<String>, String> {
        let si = self
            .setups
            .iter()
            .position(|s| s.id == setup)
            .ok_or(format!("no setup {setup:?}"))?;
        let material = material
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let mut next = self.clone();
        next.setups[si].material = material.clone();
        let no_mesh = |_: &str| None;
        let close = |a: &Value, b: &Value| match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
            _ => a == b,
        };
        let mut changed = Vec::new();
        let mut ops = self.setups[si].operations.clone();
        for o in ops.iter_mut() {
            let kind = crate::ops::kind_of(&o.op);
            let tool = Some(o.op.tool());
            let (Ok(old), Ok(new)) = (
                self.op_defaults(kind, setup, tool, &[], libraries, machine, &no_mesh),
                next.op_defaults(kind, setup, tool, &[], libraries, machine, &no_mesh),
            ) else {
                continue;
            };
            let mut v = serde_json::to_value(&o.op).map_err(|e| e.to_string())?;
            let mut any = false;
            for k in crate::ops::CUTTING_KEYS {
                let Some(nv) = new.get(k) else { continue };
                let untouched = match (v.get(k), old.get(k)) {
                    (None, _) => true,
                    (Some(c), Some(ov)) => close(c, ov),
                    (Some(_), None) => false,
                };
                if untouched && v.get(k).is_none_or(|c| !close(c, nv)) {
                    v[k] = nv.clone();
                    changed.push(format!("{}: {k}", o.id));
                    any = true;
                }
            }
            if any {
                o.op = serde_json::from_value(v).map_err(|e| format!("{}: {e}", o.id))?;
            }
        }
        self.setups[si].material = material;
        self.setups[si].operations = ops;
        Ok(changed)
    }

    pub fn setup(&self, id: &str) -> Option<&Setup> {
        self.setups.iter().find(|s| s.id == id)
    }

    /// The 0.1 job that plans a setup (enabled operations only; empty outputs dropped).
    /// Face selections on mesh models need [`Project::setup_job_with`].
    pub fn setup_job(&self, setup: &Setup, libraries: &[ToolLibrary]) -> Result<Job, String> {
        self.setup_job_with(setup, libraries, &|_| None)
    }

    /// Face regions of a model: profiles are revolved (360 facets); meshes come from `meshes`.
    /// Mesh model `model` as setup `setup` holds it: a part carried on from the setup before
    /// (its stock `from_previous`) turned and moved with the stock, as the setup's frame
    /// places it; else the model as written. (Mar: in a carried setup the defaults, the picks
    /// and the picked faces were the part where the first setup had it.)
    pub fn model_source_in(&self, setup: &Setup, model: &str) -> Option<MeshSource> {
        let m = self.models.iter().find(|m| m.id == model)?;
        let ModelKind::Mesh(src) = &m.kind else {
            return None;
        };
        if matches!(setup.stock, StockDef::FromPrevious { .. })
            && setup.part.iter().any(|p| p == model)
        {
            if let Ok(f) = self.setup_frame(setup) {
                if f.carry.is_some() {
                    if let Some(s) = f
                        .part
                        .get("mesh")
                        .and_then(|v| serde_json::from_value::<MeshSource>(v.clone()).ok())
                    {
                        return Some(s);
                    }
                }
            }
        }
        Some(src.clone())
    }

    pub fn model_regions(
        &self,
        model: &str,
        meshes: &dyn Fn(&str) -> Option<crate::Mesh>,
    ) -> Option<Vec<crate::geometry::FaceRegion>> {
        let m = self.models.iter().find(|m| m.id == model)?;
        let mesh = match &m.kind {
            ModelKind::Profile(p) => crate::Mesh::revolve(p, 360),
            ModelKind::Mesh(_) => meshes(model)?,
        };
        Some(crate::geometry::face_regions(&mesh, 30.0).0)
    }

    /// Like [`Project::setup_job`], with mesh models available for face selections.
    pub fn setup_job_with(
        &self,
        setup: &Setup,
        libraries: &[ToolLibrary],
        meshes: &dyn Fn(&str) -> Option<crate::Mesh>,
    ) -> Result<Job, String> {
        let mut job = self.setup_job_raw(setup, libraries)?;
        // apply geometry selections as containment boundaries
        for o in setup
            .operations
            .iter()
            .filter(|o| self.op_plans(o) && !o.geometry.is_empty())
        {
            let radius = job
                .tool(o.op.tool())
                .map(|t| match t.shape {
                    job::ToolShape::Flat { diameter } | job::ToolShape::Ball { diameter } => {
                        diameter / 2.0
                    }
                    job::ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(0.0) / 2.0,
                    job::ToolShape::TaperedBall { tip_diameter, .. } => tip_diameter / 2.0,
                })
                .unwrap_or(0.0);
            let b =
                crate::geometry::boundary(&o.geometry, &|m| self.model_regions(m, meshes), radius)
                    .map_err(|e| format!("operation {:?}: {e}", o.id))?;
            // faces of the part's mesh model become a contact mask (profiles' faces are axial
            // bands, which the X range bounds exactly)
            let mut face_ids: Vec<String> = Vec::new();
            for s in &o.geometry {
                if let crate::geometry::Selection::Faces { model, ids } = s {
                    let is_mesh = self
                        .models
                        .iter()
                        .any(|m| &m.id == model && matches!(m.kind, ModelKind::Mesh(_)));
                    if is_mesh {
                        if !setup.part.contains(model) {
                            return Err(format!(
                                "operation {:?}: faces of {model:?}, which is not this setup's part",
                                o.id
                            ));
                        }
                        for id in ids {
                            if !face_ids.contains(id) {
                                face_ids.push(id.clone());
                            }
                        }
                    }
                }
            }
            let target = job
                .programs
                .iter_mut()
                .flat_map(|p| p.operations.iter_mut())
                .find(|op| **op == o.op)
                .ok_or(format!("operation {:?} not found in the plan", o.id))?;
            let clamp = |r: [f64; 2], b: [f64; 2]| -> [f64; 2] {
                let (lo, hi) = (r[0].min(r[1]).max(b[0]), r[0].max(r[1]).min(b[1]));
                if r[0] <= r[1] {
                    [lo, hi]
                } else {
                    [hi, lo]
                }
            };
            match target {
                Operation::RotaryFinish(f) | Operation::RotaryParallel(f) => {
                    if let Some(x) = b.x {
                        f.x_range = clamp(f.x_range, x);
                    }
                    if let Some(a) = b.a {
                        f.a_range = clamp(f.a_range, a);
                    }
                    f.faces = face_ids;
                }
                Operation::Raster3d(r) | Operation::Waterline(r) => {
                    // only faces: the passes' area comes from the parameters
                    if o.geometry.iter().any(|s| {
                        !matches!(s, crate::geometry::Selection::Faces { .. })
                    }) || face_ids.is_empty()
                    {
                        return Err(format!(
                            "operation {:?}: raster3d takes faces of the part's mesh as its selection",
                            o.id
                        ));
                    }
                    r.faces = face_ids;
                }
                Operation::RotaryRough(r) => {
                    if let Some(x) = b.x {
                        r.x_range = clamp(r.x_range, x);
                    }
                    if b.a.is_some() {
                        return Err(format!("operation {:?}: roughing goes all the way round; an angular range does not apply", o.id));
                    }
                }
                _ => {
                    return Err(format!(
                        "operation {:?} ({}): takes its geometry from its parameters, not from a selection",
                        o.id,
                        crate::ops::kind_of(&o.op)
                    ))
                }
            }
        }
        Ok(job)
    }

    /// Setup `setup`'s part, stock and carry in its own work coordinates: its own, or (stock
    /// from the previous setup) the previous one's turned and moved.
    fn setup_frame(&self, setup: &Setup) -> Result<SetupFrame, String> {
        let k = self.setups.iter().position(|s| s.id == setup.id);
        if let (
            StockDef::FromPrevious {
                from,
                rotate,
                radius,
                length,
                turn,
                offset,
            },
            Some(k),
        ) = (&setup.stock, k)
        {
            if k == 0 {
                return Err(format!(
                    "setup {:?}: the first setup cannot take its stock from a previous one",
                    setup.id
                ));
            }
            // the one just before, or the setup named (any before this one)
            let prev_setup = match from {
                None => &self.setups[k - 1],
                Some(f) => self.setups[..k]
                    .iter()
                    .find(|x| &x.id == f)
                    .ok_or_else(|| {
                        format!(
                        "setup {:?}: its stock comes from {f:?}, which is not a setup before it",
                        setup.id
                    )
                    })?,
            };
            let prev = self.setup_frame(prev_setup)?;
            // turned any way, or a block going on as a bar (or back): the cut result as a mesh
            let to_block = setup.kind.as_deref() == Some("vise") || setup.holding.kind == "vise";
            if *rotate != [0.0; 3] || to_block != prev.stock_box.is_some() {
                return Ok(mesh_carry_frame(
                    prev_setup, &prev, *rotate, *turn, *offset, to_block,
                ));
            }
            let (turn, offset) = (*turn, *offset);
            // the previous stock's box: a block's, or a bar's on the axis (front at X0)
            let pb = prev.stock_box.unwrap_or(job::StockBox {
                min: [-prev.stock.length, -prev.stock.radius, -prev.stock.radius],
                max: [0.0, prev.stock.radius, prev.stock.radius],
            });
            let corners: Vec<[f64; 3]> = (0..8)
                .map(|i| {
                    turn.apply([
                        if i & 1 == 0 { pb.min[0] } else { pb.max[0] },
                        if i & 2 == 0 { pb.min[1] } else { pb.max[1] },
                        if i & 4 == 0 { pb.min[2] } else { pb.max[2] },
                    ])
                })
                .collect();
            let lo = |a: usize| corners.iter().map(|c| c[a]).fold(f64::INFINITY, f64::min);
            let hi = |a: usize| {
                corners
                    .iter()
                    .map(|c| c[a])
                    .fold(f64::NEG_INFINITY, f64::max)
            };
            // re-zeroed: X (and a block's Y) from its lowest corner as before, a block's top
            // where it was; a bar stays on the axis
            let block = prev.stock_box.is_some();
            let s = [
                pb.min[0] - lo(0),
                if block { pb.min[1] - lo(1) } else { 0.0 },
                if block { pb.max[2] - hi(2) } else { 0.0 },
            ];
            let t = [s[0] + offset[0], s[1] + offset[1], s[2] + offset[2]];
            let carry = job::Carry {
                from: prev_setup.id.clone(),
                rotate: [0.0; 3],
                turn,
                shift: t,
            };
            let stock_box = prev.stock_box.map(|_| job::StockBox {
                min: [lo(0) + t[0], lo(1) + t[1], lo(2) + t[2]],
                max: [hi(0) + t[0], hi(1) + t[1], hi(2) + t[2]],
            });
            let stock_axis = prev.stock_axis.map(|[y, z]| {
                let q = carry.map([0.0, y, z]);
                [q[1], q[2]]
            });
            let mut stock = prev.stock.clone();
            if *radius > 0.0 {
                stock.radius = *radius;
            }
            if *length > 0.0 {
                stock.length = *length;
            }
            // the part, turned and moved the same
            let mut part = prev.part.clone();
            if let Some(m) = part.get_mut("mesh") {
                let mut src: job::MeshSource =
                    serde_json::from_value(m.clone()).map_err(|e| e.to_string())?;
                src.turn = turn.after(src.turn);
                src.translate = carry.map(src.translate);
                *m = serde_json::to_value(src).map_err(|e| e.to_string())?;
            }
            if let Some(pr) = part.get_mut("profile") {
                let mut p: job::Profile =
                    serde_json::from_value(pr.clone()).map_err(|e| e.to_string())?;
                let sx = turn.signs()[0];
                p.points = p.points.iter().map(|q| [sx * q[0] + t[0], q[1]]).collect();
                if sx < 0.0 {
                    p.points.reverse();
                    p.beyond_front = None;
                }
                *pr = serde_json::to_value(p).map_err(|e| e.to_string())?;
            }
            return Ok(SetupFrame {
                part,
                stock,
                stock_box,
                stock_axis,
                carry: Some(carry),
            });
        }
        let mut part = serde_json::Map::new();
        for id in &setup.part {
            let m = self
                .models
                .iter()
                .find(|m| &m.id == id)
                .ok_or(format!("unknown model {id:?}"))?;
            match &m.kind {
                ModelKind::Profile(p) => {
                    part.insert(
                        "profile".into(),
                        serde_json::to_value(p).map_err(|e| e.to_string())?,
                    );
                }
                ModelKind::Mesh(src) => {
                    part.insert(
                        "mesh".into(),
                        serde_json::to_value(src).map_err(|e| e.to_string())?,
                    );
                }
            }
        }
        let stock = setup.stock.cylinder();
        let (mut stock_box, mut stock_axis) = (None, None);
        if let StockDef::Box { min, max } = setup.stock {
            stock_box = Some(job::StockBox { min, max });
        } else if setup.stock.is_bar()
            && (setup.kind.as_deref() == Some("vise") || setup.holding.kind == "vise")
        {
            // a bar in the vise (along X, Y, or standing up): the block round it, Z0 at its top
            let (sb, axis) = stock.lying_along(setup.holding.along.as_deref())?;
            stock_box = Some(sb);
            stock_axis = Some(axis);
        }
        Ok(SetupFrame {
            part,
            stock,
            stock_box,
            stock_axis,
            carry: None,
        })
    }

    fn setup_job_raw(&self, setup: &Setup, libraries: &[ToolLibrary]) -> Result<Job, String> {
        let frame = self.setup_frame(setup)?;
        let part = frame.part;
        let programs: Vec<Value> = setup
            .outputs
            .iter()
            .filter_map(|out| {
                let ops: Vec<&Operation> = out
                    .operations
                    .iter()
                    .filter_map(|id| setup.operations.iter().find(|o| &o.id == id))
                    .filter(|o| self.op_plans(o))
                    .map(|o| &o.op)
                    .collect();
                if ops.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "name": out.name, "title": out.title, "notes": out.notes,
                    "reset_a": out.reset_a, "operations": ops,
                }))
            })
            .collect();
        let stock = frame.stock;
        let mut v = serde_json::json!({
            "schema_version": job::SCHEMA_VERSION,
            "stock": stock,
            "clearance": setup.clearance,
            "part": part,
            "tools": self.tools,
            "spindle_rpm": setup.spindle_rpm,
            "programs": programs,
        });
        if let Some(m) = &setup.machine {
            v["machine"] = Value::String(m.clone());
        }
        if let Some(sb) = frame.stock_box {
            v["stock_box"] = serde_json::json!({"min": sb.min, "max": sb.max});
        }
        if let Some(axis) = frame.stock_axis {
            v["stock_axis"] = serde_json::json!(axis);
            if let Some(a) = setup.holding.along.as_deref().filter(|a| *a != "x") {
                v["stock_along"] = serde_json::json!(a);
            }
        }
        if let Some(c) = &frame.carry {
            v["carry"] = serde_json::to_value(c).map_err(|e| e.to_string())?;
        }
        if let Some(p) = &setup.post {
            p.check()
                .map_err(|e| format!("setup {:?}: {e}", setup.id))?;
            v["post"] = serde_json::to_value(p).map_err(|e| e.to_string())?;
        }
        // the vise's jaws: the depth they grip
        if let Some(sb) = v
            .get("stock_box")
            .and_then(|b| serde_json::from_value::<job::StockBox>(b.clone()).ok())
        {
            let h = sb.max[2] - sb.min[2];
            let grip = setup.holding.jaws.unwrap_or(
                if v.get("stock_axis").is_some() && v.get("stock_along").is_none_or(|a| a != "z") {
                    0.4 * h
                } else {
                    (h / 3.0).min(3.0)
                },
            );
            v["jaws"] = serde_json::json!(grip.clamp(0.0, h));
        }
        // what holds it (only what the setup says: a project without it plans as before)
        let hd = &setup.holding;
        let fixture = job::Fixture {
            jaw_width: hd.jaw_width,
            jaw_height: hd.jaw_height,
            opening: hd.opening,
            chuck_diameter: hd.chuck_diameter,
            chuck_length: hd.chuck_length,
            grip: hd.chuck,
            tailstock_x: hd.tailstock_x,
            centre_depth: hd.centre_depth,
            tailstock: hd.tailstock && hd.kind != "vise",
        };
        if fixture != job::Fixture::default() {
            v["fixture"] = serde_json::to_value(&fixture).map_err(|e| e.to_string())?;
        }
        self.number_tools(&mut v);
        profiles::resolve_job_tools(&mut v, libraries)?;
        serde_json::from_value(v).map_err(|e| format!("setup {:?}: {e}", setup.id))
    }

    /// A setup's job (as JSON, before tool libraries are resolved): unindexed tools given the
    /// next free T numbers (their operations follow), and warnings in the G-code of the
    /// programs that use them, and where one tool is under two numbers.
    fn number_tools(&self, v: &mut Value) {
        let used: Vec<u32> = v["programs"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|p| p["operations"].as_array().into_iter().flatten())
            .filter_map(|o| o["tool"].as_u64().map(|n| n as u32))
            .collect();
        let indexed: std::collections::HashSet<u32> = self
            .tools
            .iter()
            .map(ToolEntry::number)
            .filter(|n| !self.unindexed.contains(n))
            .collect();
        let mut given: Vec<(u32, u32)> = Vec::new();
        for &n in &used {
            if !self.unindexed.contains(&n) || given.iter().any(|g| g.0 == n) {
                continue;
            }
            let f = (1..)
                .find(|k| !indexed.contains(k) && !given.iter().any(|g| g.1 == *k))
                .unwrap_or(n);
            given.push((n, f));
        }
        let describe = |n: u32| -> String {
            match self.tools.iter().find(|t| t.number() == n) {
                Some(ToolEntry::Library {
                    library,
                    tool,
                    description,
                    ..
                }) => description
                    .clone()
                    .unwrap_or_else(|| format!("{library}/{tool}")),
                Some(ToolEntry::Inline(t)) => t.description.clone(),
                None => String::new(),
            }
        };
        // the same tool under two numbers (both used)
        let mut twins: Vec<String> = Vec::new();
        for (k, a) in self.tools.iter().enumerate() {
            for b in &self.tools[k + 1..] {
                let same = match (a, b) {
                    (
                        ToolEntry::Library {
                            library: la,
                            tool: ta,
                            ..
                        },
                        ToolEntry::Library {
                            library: lb,
                            tool: tb,
                            ..
                        },
                    ) => la == lb && ta == tb,
                    (ToolEntry::Inline(x), ToolEntry::Inline(y)) => {
                        x.shape == y.shape && x.description == y.description
                    }
                    _ => false,
                };
                if same && used.contains(&a.number()) && used.contains(&b.number()) {
                    twins.push(format!(
                        "(Warning: {} is both T{} and T{}: one tool, two tool numbers)",
                        describe(a.number()),
                        a.number(),
                        b.number()
                    ));
                }
            }
        }
        // renumber: the tools, then the operations
        let to = |n: u32| given.iter().find(|g| g.0 == n).map_or(n, |g| g.1);
        if let Some(ts) = v["tools"].as_array_mut() {
            for t in ts {
                if let Some(n) = t["number"].as_u64() {
                    t["number"] = serde_json::json!(to(n as u32));
                }
            }
        }
        for p in v["programs"].as_array_mut().into_iter().flatten() {
            let mut notes: Vec<String> = Vec::new();
            for o in p["operations"].as_array_mut().into_iter().flatten() {
                if let Some(n) = o["tool"].as_u64().map(|n| n as u32) {
                    if let Some(&(_, f)) = given.iter().find(|g| g.0 == n) {
                        o["tool"] = serde_json::json!(f);
                        let note = format!("(Warning: {} has no tool number in the project: written as T{f}, the next free one. Give it a number (Tools) before running.)", describe(n));
                        if !notes.contains(&note) {
                            notes.push(note);
                        }
                    }
                }
            }
            notes.extend(twins.iter().cloned());
            if !notes.is_empty() {
                let mut all: Vec<Value> = p["notes"].as_array().cloned().unwrap_or_default();
                all.extend(notes.into_iter().map(Value::String));
                p["notes"] = Value::Array(all);
            }
        }
    }

    /// Project operation ids in planning order for a setup (matches `Move::Operation` ids).
    pub fn planned_op_ids(&self, setup: &Setup) -> Vec<String> {
        setup
            .outputs
            .iter()
            .flat_map(|out| out.operations.iter())
            .filter(|id| {
                setup
                    .operations
                    .iter()
                    .any(|o| &o.id == *id && self.op_plans(o))
            })
            .cloned()
            .collect()
    }
}

/// Result of simulating a whole project: setups in order on one simulated bar.
#[derive(Debug, Clone)]
pub struct ProjectSim {
    /// Stock after each setup: `(setup id, stock)`.
    pub after: Vec<(String, crate::sim::RadialMap)>,
    pub part: crate::sim::Part,
    /// Rapids that went through stock, per setup.
    pub rapid_hits: Vec<(String, usize)>,
}

/// The frame of a setup whose stock is the cut result of `prev_setup` (frame `prev`) as a mesh:
/// turned by `rotate`, then `turn`, then moved by `offset`. A block (`to_block`): re-zeroed as a
/// block carried on is (X and Y from its lowest corner where it was, its top where it was; a
/// bar's at X0 Y0 and its top at Z0); a bar: its front at X0, centred on the axis, its radius
/// to the box's corners. The part (a mesh) turns and moves with it; a revolved profile, which
/// stays only about the axis, does not go on (a setup with no model: its stock-only
/// operations, facing, still work).
fn mesh_carry_frame(
    prev_setup: &Setup,
    prev: &SetupFrame,
    rotate: [f64; 3],
    turn: job::Turn,
    offset: [f64; 3],
    to_block: bool,
) -> SetupFrame {
    use crate::carry_mesh::{apply, euler, mul, rotation};
    let s = turn.signs();
    let tm = [[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]];
    let r = mul(tm, rotation(rotate));
    let pb = prev.stock_box.unwrap_or(job::StockBox {
        min: [-prev.stock.length, -prev.stock.radius, -prev.stock.radius],
        max: [0.0, prev.stock.radius, prev.stock.radius],
    });
    let corners: Vec<[f64; 3]> = (0..8)
        .map(|i| {
            apply(
                r,
                [
                    if i & 1 == 0 { pb.min[0] } else { pb.max[0] },
                    if i & 2 == 0 { pb.min[1] } else { pb.max[1] },
                    if i & 4 == 0 { pb.min[2] } else { pb.max[2] },
                ],
            )
        })
        .collect();
    let lo = |a: usize| corners.iter().map(|c| c[a]).fold(f64::INFINITY, f64::min);
    let hi = |a: usize| {
        corners
            .iter()
            .map(|c| c[a])
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let z = if to_block {
        if prev.stock_box.is_some() {
            [pb.min[0] - lo(0), pb.min[1] - lo(1), pb.max[2] - hi(2)]
        } else {
            [-lo(0), -lo(1), -hi(2)]
        }
    } else {
        [-hi(0), -0.5 * (lo(1) + hi(1)), -0.5 * (lo(2) + hi(2))]
    };
    let t = [z[0] + offset[0], z[1] + offset[1], z[2] + offset[2]];
    let deg = euler(r);
    let carry = job::Carry {
        from: prev_setup.id.clone(),
        rotate: deg,
        turn: job::Turn::None,
        shift: t,
    };
    let (stock_box, mut stock) = if to_block {
        (
            Some(job::StockBox {
                min: [lo(0) + t[0], lo(1) + t[1], lo(2) + t[2]],
                max: [hi(0) + t[0], hi(1) + t[1], hi(2) + t[2]],
            }),
            prev.stock.clone(),
        )
    } else {
        let (dy, dz) = (0.5 * (hi(1) - lo(1)), 0.5 * (hi(2) - lo(2)));
        (
            None,
            job::Stock {
                // (moved across the axis: its corners that much farther out)
                radius: (dy + offset[1].abs()).hypot(dz + offset[2].abs()),
                length: hi(0) - lo(0),
                section: None,
            },
        )
    };
    if !to_block && stock.length <= 0.0 {
        stock.length = 1.0;
    }
    // the part: a mesh turned and moved the same (its own turn and rotation folded in)
    let mut part = serde_json::Map::new();
    if let Some(m) = prev.part.get("mesh") {
        if let Ok(mut src) = serde_json::from_value::<job::MeshSource>(m.clone()) {
            let ss = src.turn.signs();
            let ts = [[ss[0], 0.0, 0.0], [0.0, ss[1], 0.0], [0.0, 0.0, ss[2]]];
            src.rotate = euler(mul(r, mul(ts, rotation(src.rotate))));
            src.turn = job::Turn::None;
            let q = apply(r, src.translate);
            src.translate = [q[0] + t[0], q[1] + t[1], q[2] + t[2]];
            if let Ok(v) = serde_json::to_value(src) {
                part.insert("mesh".into(), v);
            }
        }
    }
    SetupFrame {
        part,
        stock,
        stock_box,
        stock_axis: None,
        carry: Some(carry),
    }
}

/// Simulate every setup in order. A setup whose stock is `from_previous` starts from the
/// previous setup's result (same work origin); otherwise it starts from fresh bar stock. The
/// grid covers the first setup's part (as [`crate::simulate_job`] does).
pub fn simulate_project(
    p: &Project,
    jobs: &[(String, Job)],
    mesh_for: &dyn Fn(&Job) -> Option<crate::Mesh>,
    dx: f64,
    na: usize,
) -> Result<ProjectSim, String> {
    let (_, first) = jobs.first().ok_or("the project has no setups")?;
    let mesh0 = mesh_for(first);
    let base =
        crate::simulate_job(first, &[], mesh0.as_ref(), dx, na, 0.02).map_err(|e| e.to_string())?;
    let grid = base.stock.grid;
    let mut stock: Option<crate::sim::RadialMap> = None;
    let mut after: Vec<(String, crate::sim::RadialMap)> = Vec::new();
    let mut rapid_hits = Vec::new();
    for (id, job) in jobs {
        let setup = p.setup(id).ok_or(format!("unknown setup {id:?}"))?;
        let start = match (&setup.stock, &stock) {
            // (from a setup named: what that one left)
            (StockDef::FromPrevious { from: Some(f), .. }, _) => after
                .iter()
                .find(|(x, _)| x == f)
                .map(|(_, s)| s.clone())
                .ok_or(format!(
                    "setup {id:?}: its stock comes from {f:?}, not simulated before it"
                ))?,
            (StockDef::FromPrevious { .. }, Some(prev)) => prev.clone(),
            (StockDef::FromPrevious { .. }, None) => {
                return Err(format!(
                    "setup {id:?}: stock from_previous, but it is the first setup"
                ))
            }
            (
                s @ (StockDef::Cylinder { .. }
                | StockDef::Square { .. }
                | StockDef::Rect { .. }
                | StockDef::Hex { .. }),
                _,
            ) => crate::sim::RadialMap::of_stock(grid, &s.cylinder()),
            (StockDef::Box { .. }, _) => {
                return Err(format!(
                    "setup {id:?}: 3-axis (box) setups are not simulated on the rotary heightmap"
                ))
            }
        };
        let mesh = mesh_for(job);
        let tps = crate::plan_with_mesh(job, mesh.as_ref()).map_err(|e| e.to_string())?;
        let cutters = |t: u32| job.tool(t).and_then(|d| crate::cutter_for(t, d.shape).ok());
        let mut s = start;
        let mut hits = 0;
        for tp in &tps {
            hits += crate::sim::simulate(&mut s, tp, &cutters, (dx * 2.0).min(0.1))
                .rapid_hits
                .len();
        }
        rapid_hits.push((id.clone(), hits));
        after.push((id.clone(), s.clone()));
        stock = Some(s);
    }
    Ok(ProjectSim {
        after,
        part: base.part,
        rapid_hits,
    })
}

/// Jobs to plan from a 0.1 job or 0.2 project text: one per setup, as `(setup id, job)`.
pub fn jobs_from_any(text: &str, libraries: &[ToolLibrary]) -> Result<Vec<(String, Job)>, String> {
    jobs_from_any_with(text, libraries, &|_| None)
}

/// Like [`jobs_from_any`], with mesh models available (for face selections).
pub fn jobs_from_any_with(
    text: &str,
    libraries: &[ToolLibrary],
    meshes: &dyn Fn(&MeshSource) -> Option<crate::Mesh>,
) -> Result<Vec<(String, Job)>, String> {
    let text = &*crate::project_file::to_json_text(text)?;
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    if v.get("schema_version").and_then(Value::as_str) == Some(job::SCHEMA_VERSION) {
        return Ok(vec![(
            "setup1".into(),
            profiles::job_from_json(text, libraries)?,
        )]);
    }
    let (p, _) = Project::from_json_any(text)?;
    p.setups
        .iter()
        .map(|s| {
            // (a carried part's faces where the setup holds it)
            let by_model = |id: &str| meshes(&p.model_source_in(s, id)?);
            Ok((s.id.clone(), p.setup_job_with(s, libraries, &by_model)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    const KNOB: &str = include_str!("../../../examples/knob/knob.job.json");

    #[test]
    fn upgrade_round_trip_and_lowering() {
        let (p, upgraded) = Project::from_json_any(KNOB).unwrap();
        assert!(upgraded);
        assert_eq!(p.setups.len(), 1);
        let s = &p.setups[0];
        assert_eq!(s.operations.len(), 3);
        assert_eq!(s.outputs[1].operations, vec!["op2", "op3"]);
        // 0.2 JSON round-trips
        let text = serde_json::to_string_pretty(&p).unwrap();
        let (back, up2) = Project::from_json_any(&text).unwrap();
        assert!(!up2);
        assert_eq!(back, p);
        // lowering gives back the original job
        let job = p.setup_job(s, &[]).unwrap();
        let orig = Job::from_json(KNOB).unwrap();
        assert_eq!(job, orig);
        assert_eq!(p.planned_op_ids(s), vec!["op1", "op2", "op3"]);
    }

    #[test]
    fn disabled_operations_and_empty_outputs_drop_out() {
        let (mut p, _) = Project::from_json_any(KNOB).unwrap();
        p.setups[0].operations[1].enabled = false; // knurl
        let job = p.setup_job(&p.setups[0], &[]).unwrap();
        assert_eq!(job.programs[1].operations.len(), 1);
        assert_eq!(p.planned_op_ids(&p.setups[0]), vec!["op1", "op3"]);
        p.setups[0].operations[2].enabled = false;
        let job = p.setup_job(&p.setups[0], &[]).unwrap();
        assert_eq!(job.programs.len(), 1);
    }

    #[test]
    fn validation_errors() {
        let (p, _) = Project::from_json_any(KNOB).unwrap();
        let mut bad = p.clone();
        bad.setups[0].outputs[0].operations.push("nope".into());
        assert!(bad
            .validate()
            .unwrap_err()
            .contains("unknown operation \"nope\""));
        let mut bad = p.clone();
        bad.setups[0].operations[1].id = "op1".into();
        assert!(bad.validate().unwrap_err().contains("used twice"));
        let mut bad = p.clone();
        bad.setups[0].part.push("ghost".into());
        assert!(bad.validate().unwrap_err().contains("unknown model"));
        assert!(Project::from_json_any("{\"schema_version\": \"9\"}").is_err());
    }

    /// The knob as two setups: turning on fresh bar, then knurl + chamfer on the turned stock.
    pub(crate) fn two_setups() -> Project {
        let (mut p, _) = Project::from_json_any(KNOB).unwrap();
        let mut s2 = p.setups[0].clone();
        let s1 = &mut p.setups[0];
        s1.operations.truncate(1);
        s1.outputs.truncate(1);
        s2.id = "setup2".into();
        s2.name = "Knurl (re-chucked)".into();
        s2.stock = StockDef::FromPrevious {
            from: None,
            rotate: [0.0; 3],
            radius: 12.5,
            length: 100.0,
            turn: job::Turn::None,
            offset: [0.0; 3],
        };
        s2.operations.remove(0);
        s2.outputs.remove(0);
        p.setups.push(s2);
        p
    }

    #[test]
    fn two_setups_plan_like_one_and_carry_the_stock() {
        let p = two_setups();
        p.validate().unwrap();
        let text = serde_json::to_string(&p).unwrap();
        let jobs = jobs_from_any(&text, &[]).unwrap();
        assert_eq!(jobs.len(), 2);
        // each setup posts exactly the program it had in the single-setup job
        let one = Job::from_json(KNOB).unwrap();
        let m = crate::Machine::makera_z1();
        let single: Vec<String> = crate::plan(&one)
            .unwrap()
            .iter()
            .map(|t| crate::post(t, &m))
            .collect();
        let split: Vec<String> = jobs
            .iter()
            .flat_map(|(_, j)| crate::plan(j).unwrap())
            .map(|t| crate::post(&t, &m))
            .collect();
        assert!(single == split, "G-code differs when split into setups");
        // the second setup cuts the stock the first one left
        let carried = simulate_project(&p, &jobs, &|_| None, 0.2, 90).unwrap();
        let (one_p, _) = Project::from_json_any(KNOB).unwrap();
        let one_jobs = jobs_from_any(KNOB, &[]).unwrap();
        let whole = simulate_project(&one_p, &one_jobs, &|_| None, 0.2, 90).unwrap();
        assert_eq!(
            carried.after[1].1.r, whole.after[0].1.r,
            "carried stock differs from one setup"
        );
        // ... and from fresh bar it would not match
        let mut fresh = p.clone();
        fresh.setups[1].stock = StockDef::Cylinder {
            radius: 12.5,
            length: 100.0,
        };
        let f = simulate_project(&fresh, &jobs, &|_| None, 0.2, 90).unwrap();
        assert_ne!(f.after[1].1.r, whole.after[0].1.r);
        assert_eq!(
            carried.rapid_hits,
            vec![("setup1".into(), 0), ("setup2".into(), 0)]
        );
        // a first setup cannot carry stock; output names must be unique
        let mut bad = p.clone();
        bad.setups.swap(0, 1);
        assert!(bad.validate().unwrap_err().contains("first setup"));
        let mut bad = p.clone();
        bad.setups[1].outputs[0].name = "knob_1_profile".into();
        assert!(bad.validate().unwrap_err().contains("used twice"));
    }

    #[test]
    fn face_selection_limits_where_the_tool_goes() {
        let text = include_str!("../../../examples/knob/knob_finish.job.json");
        let (mut p, _) = Project::from_json_any(text).unwrap();
        // finish only the taper of the revolved profile (the mesh is the same shape)
        let regions = p.model_regions("profile", &|_| None).unwrap();
        let taper = regions
            .iter()
            .find(|r| r.shape == "cone" && (r.x[0] + 15.0).abs() < 1e-6)
            .unwrap();
        let s = &mut p.setups[0];
        let finish = s.operations.iter_mut().find(|o| o.id == "op2").unwrap();
        finish.geometry = vec![crate::geometry::Selection::Faces {
            model: "profile".into(),
            ids: vec![taper.id.clone()],
        }];
        let text = serde_json::to_string(&p).unwrap();
        let (p, _) = Project::from_json_any(&text).unwrap();
        let job = p.setup_job(&p.setups[0], &[]).unwrap();
        let Operation::RotaryFinish(f) = &job.programs[1].operations[0] else {
            panic!()
        };
        // ball 3.175: the taper's X extent grown by the tool radius, inside the op's own
        // range [-21.0, 0.6]; the planned cuts stay inside it
        // plan against the revolved profile (the mesh file is the same shape)
        let mut no_mesh = job.clone();
        no_mesh.part.mesh = None;
        let tps = crate::plan(&no_mesh).unwrap();
        let (lo, hi) = tps[1]
            .moves
            .iter()
            .filter_map(|m| match m {
                crate::Move::Cut { x, .. } => Some(*x),
                _ => None,
            })
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                (a.min(x), b.max(x))
            });
        assert!(lo >= -16.5876 && hi <= 0.6001, "cuts span {lo}..{hi}");
        assert!(
            (f.x_range[0] - (-15.0 - 1.5875)).abs() < 1e-9 && (f.x_range[1] - 0.6).abs() < 1e-9,
            "{:?}",
            f.x_range
        );
        // an axial range on the knurl is refused: it takes its geometry from its parameters
        let (mut q, _) = Project::from_json_any(KNOB).unwrap();
        q.setups[0].operations[1].geometry =
            vec![crate::geometry::Selection::AxialRange { x: [-5.0, 0.0] }];
        assert!(q
            .setup_job(&q.setups[0], &[])
            .unwrap_err()
            .contains("takes its geometry from its parameters"));
    }

    #[test]
    fn library_tools_resolve_when_lowering() {
        let (mut p, _) = Project::from_json_any(KNOB).unwrap();
        p.tools[0] = ToolEntry::Library {
            number: 1,
            library: "makera-starter".into(),
            tool: "spiral-o-metal-3.175x12".into(),
            description: None,
            cutting: vec![],
        };
        let text = serde_json::to_string(&p).unwrap();
        let (p2, _) = Project::from_json_any(&text).unwrap();
        assert!(matches!(p2.tools[0], ToolEntry::Library { .. }));
        let job = p2
            .setup_job(&p2.setups[0], &profiles::builtin_libraries())
            .unwrap();
        assert_eq!(
            job.tools[0].description,
            "Spiral O Metal 3.175 x 12 mm (single flute)"
        );
    }
}
