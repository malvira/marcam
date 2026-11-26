//! Wizards: generators that read a model and a setup and add ordinary, editable operations.
//! The first is the 4th-axis knurl wizard, which rebuilds the knurled-knob program (turn the
//! profile, cut a diamond knurl on a conical body face, chamfer the front edge with the
//! V-bit flank) from the part's faces instead of hand-typed coordinates.

use crate::geometry::FaceRegion;
use crate::job::{FlankChamfer, HelicalKnurl, Operation, RotaryTurning};
use crate::project::{Output, Project, ProjectOp};
use serde::{Deserialize, Serialize};

/// Parameters of the knurl wizard (defaults are the knob that was cut on the Z1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KnurlWizard {
    /// Setup to add to.
    pub setup: String,
    /// Profile or mesh model to read faces from.
    pub model: String,
    /// The conical (or cylindrical) body face to knurl, by face id; None = the largest cone
    /// or cylinder.
    pub body_face: Option<String>,
    pub flat_tool: u32,
    pub vbit_tool: u32,
    pub count: u32,
    pub helix_angle: f64,
    /// "diamond" or "straight" (see [`HelicalKnurl::pattern`]).
    pub pattern: String,
    pub fill: f64,
    pub passes: Vec<f64>,
    pub knurl_feed: f64,
    pub plunge_feed: f64,
    /// Turning (rough + finish the profile) before knurling.
    pub turn: bool,
    pub rough_depth: f64,
    pub rough_step: f64,
    pub rough_feed: f64,
    pub finish_allowance: f64,
    pub finish_step: f64,
    pub finish_feed: f64,
    /// Chamfer the front edge with the V-bit flank.
    pub chamfer: bool,
    pub chamfer_tip_offset: f64,
    pub chamfer_passes: Vec<f64>,
    /// Output (G-code file) names.
    pub turn_output: String,
    pub knurl_output: String,
}

impl Default for KnurlWizard {
    fn default() -> Self {
        KnurlWizard {
            setup: "setup1".into(),
            model: "profile".into(),
            body_face: None,
            flat_tool: 1,
            vbit_tool: 2,
            count: 30,
            helix_angle: 30.0,
            pattern: "diamond".into(),
            fill: 0.95,
            passes: vec![0.6, 1.0],
            knurl_feed: 200.0,
            plunge_feed: 60.0,
            turn: true,
            rough_depth: 0.4,
            rough_step: 1.0,
            rough_feed: 300.0,
            finish_allowance: 0.15,
            finish_step: 0.4,
            finish_feed: 250.0,
            chamfer: true,
            chamfer_tip_offset: 0.5,
            chamfer_passes: vec![0.2, 0.0],
            turn_output: "knob_1_profile".into(),
            knurl_output: "knob_2_knurl".into(),
        }
    }
}

/// Snap to 1e-6 mm so derived coordinates come out as the round numbers they are.
fn snap(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// What the wizard added, for the host to show.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WizardResult {
    pub operations: Vec<String>,
    pub outputs: Vec<String>,
    pub notes: Vec<String>,
}

impl KnurlWizard {
    /// Add the operations and outputs to the project.
    pub fn run(
        &self,
        p: &mut Project,
        meshes: &dyn Fn(&str) -> Option<crate::Mesh>,
    ) -> Result<WizardResult, String> {
        let regions = p.model_regions(&self.model, meshes).ok_or(format!(
            "knurl wizard: no model {:?} (or its mesh is not loaded)",
            self.model
        ))?;
        let body = match &self.body_face {
            Some(id) => regions.iter().find(|r| &r.id == id).ok_or(format!(
                "knurl wizard: model {:?} has no face {id:?}",
                self.model
            ))?,
            None => regions
                .iter()
                .filter(|r| r.shape == "cone" || r.shape == "cylinder")
                .max_by(|a, b| a.area.total_cmp(&b.area))
                .ok_or("knurl wizard: the model has no conical or cylindrical face")?,
        };
        if body.shape != "cone" && body.shape != "cylinder" {
            return Err(format!(
                "knurl wizard: face {} is a {}, not a cone or cylinder",
                body.id, body.shape
            ));
        }
        if body.a.is_some() {
            return Err(format!(
                "knurl wizard: face {} does not go all the way round",
                body.id
            ));
        }
        let mut notes = Vec::new();
        // body cone as a line r(x) through its back (larger radius at lower X here) and front
        let (x0, x1) = (body.x[0], body.x[1]);
        let (r0, r1) = radius_at_ends(body, &regions);
        let front = regions
            .iter()
            .map(|r| r.x[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let r_at = |x: f64| r0 + (r1 - r0) * (x - x0) / (x1 - x0);
        // the knurl runs over the body cone extended to the front face (a chamfer there is
        // cut afterwards)
        let body_back = [snap(x0), snap(r0)];
        let body_front = [snap(front), snap(r_at(front))];
        if (front - x1).abs() > 1e-9 {
            notes.push(format!(
                "knurl body extended from X{:.3} to the front face at X{:.3} (r {:.3})",
                x1, front, body_front[1]
            ));
        }
        let setup = p
            .setups
            .iter_mut()
            .find(|s| s.id == self.setup)
            .ok_or(format!("knurl wizard: no setup {:?}", self.setup))?;
        let mut next = setup.operations.len() + 1;
        let mut new_id = || loop {
            let id = format!("op{next}");
            next += 1;
            if !setup_has(&setup.operations, &id) {
                return id;
            }
        };
        let mut ops = Vec::new();
        let mut turn_ids = Vec::new();
        if self.turn {
            let id = new_id();
            turn_ids.push(id.clone());
            ops.push(ProjectOp {
                id,
                name: "Turn the profile".into(),
                color: None,
                enabled: true,
                geometry: vec![],
                picks: Vec::new(),
                error: None,
                op: Operation::RotaryTurning(RotaryTurning {
                    leave_radial: 0.0,
                    leave_axial: 0.0,
                    tool: self.flat_tool,
                    rough_depth: self.rough_depth,
                    rough_step: self.rough_step,
                    rough_feed: self.rough_feed,
                    finish_allowance: self.finish_allowance,
                    finish_step: self.finish_step,
                    finish_feed: self.finish_feed,
                    plunge_feed: self.plunge_feed,
                    ramp_angle: crate::job::default_ramp_angle(),
                    smoothing: Some(0.0),
                    x_range: None,
                    segment_deg: 6.0,
                    grid: 0.02,
                    footprint_step: 0.01,
                }),
            });
        }
        let mut knurl_ids = Vec::new();
        let id = new_id();
        knurl_ids.push(id.clone());
        ops.push(ProjectOp {
            id,
            name: "Diamond knurl".into(),
            color: None,
            enabled: true,
            geometry: vec![],
            picks: Vec::new(),
            error: None,
            op: Operation::HelicalKnurl(HelicalKnurl {
                leave_radial: 0.0,
                tool: self.vbit_tool,
                body_back,
                body_front,
                count: self.count,
                helix_angle: self.helix_angle,
                pattern: self.pattern.clone(),
                fill: self.fill,
                passes: self.passes.clone(),
                families: vec![1, -1],
                runout: 0.6,
                samples: 120,
                approach_clearance: 0.5,
                feed: self.knurl_feed,
                plunge_feed: self.plunge_feed,
                ramp_angle: crate::job::default_ramp_angle(),
                smoothing: Some(0.0),
            }),
        });
        if self.chamfer {
            // the front edge: outer corner of the front face
            let face = regions
                .iter()
                .filter(|r| r.shape == "plane" && (r.x[0] - front).abs() < 1e-9)
                .max_by(|a, b| a.area.total_cmp(&b.area))
                .ok_or("knurl wizard: no front face to chamfer")?;
            let id = new_id();
            knurl_ids.push(id.clone());
            ops.push(ProjectOp {
                id,
                name: "Front chamfer (V-bit flank)".into(),
                color: None,
                enabled: true,
                geometry: vec![],
                picks: Vec::new(),
                error: None,
                op: Operation::FlankChamfer(FlankChamfer {
                    leave_radial: 0.0,
                    tool: self.vbit_tool,
                    edge: [snap(front), snap(face.r[1])],
                    tip_offset: self.chamfer_tip_offset,
                    passes: self.chamfer_passes.clone(),
                    revolutions: 2.0,
                    segment_deg: 6.0,
                    feed: self.knurl_feed,
                    plunge_feed: self.plunge_feed,
                    ramp_angle: crate::job::default_ramp_angle(),
                    smoothing: Some(0.0),
                }),
            });
        }
        let mut outputs = Vec::new();
        if !turn_ids.is_empty() {
            outputs.push(Output {
                name: self.turn_output.clone(),
                title: "Knob radial profile - rough + finish".into(),
                notes: vec![],
                reset_a: false,
                operations: turn_ids,
            });
        }
        outputs.push(Output {
            name: self.knurl_output.clone(),
            title: "Knob diamond knurl".into(),
            notes: vec![
                "Pattern is rotationally symmetric: A start position does not matter".into(),
            ],
            reset_a: true,
            operations: knurl_ids,
        });
        let res = WizardResult {
            operations: ops.iter().map(|o| o.id.clone()).collect(),
            outputs: outputs.iter().map(|o| o.name.clone()).collect(),
            notes,
        };
        setup.operations.extend(ops);
        setup.outputs.extend(outputs);
        p.validate()?;
        Ok(res)
    }
}

fn setup_has(ops: &[ProjectOp], id: &str) -> bool {
    ops.iter().any(|o| o.id == id)
}

/// Radii at the body face's two ends. A cone's radius changes monotonically along X; which end
/// is larger comes from the neighbouring faces (the face that meets it at that X).
pub(crate) fn radius_at_ends(body: &FaceRegion, regions: &[FaceRegion]) -> (f64, f64) {
    if body.shape == "cylinder" {
        return (body.r[0], body.r[1]);
    }
    // the shoulder plane at the back edge reaches the body's larger radius if the body
    // shrinks toward +X (as on the knob)
    let back_shoulder = regions.iter().find(|r| {
        r.shape == "plane" && (r.x[0] - body.x[0]).abs() < 1e-6 && (r.r[1] - body.r[1]).abs() < 1e-6
    });
    if back_shoulder.is_some() {
        (body.r[1], body.r[0])
    } else {
        (body.r[0], body.r[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOB: &str = include_str!("../../../examples/knob/knob.job.json");

    #[test]
    fn knurl_wizard_rebuilds_the_knob_program_byte_for_byte() {
        // the knob project without its operations: models, tools, one setup
        let (mut p, _) = Project::from_json_any(KNOB).unwrap();
        let reference = p.clone();
        p.setups[0].operations.clear();
        p.setups[0].outputs.clear();
        let res = KnurlWizard::default().run(&mut p, &|_| None).unwrap();
        assert_eq!(res.operations, vec!["op1", "op2", "op3"]);
        assert!(res.notes[0].contains("extended"), "{:?}", res.notes);
        // same operations as the hand-written job (names aside)
        let strip = |p: &Project| {
            p.setups[0]
                .operations
                .iter()
                .map(|o| o.op.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(&p), strip(&reference));
        assert_eq!(p.setups[0].outputs, reference.setups[0].outputs);
        // and the posted G-code is the cut G-code
        let job = p.setup_job(&p.setups[0], &[]).unwrap();
        let m = crate::Machine::makera_z1();
        for tp in crate::plan(&job).unwrap() {
            let golden = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("../../tests/golden/{}.nc", tp.name)),
            )
            .unwrap()
            .replace("\r\n", "\n");
            assert!(crate::post(&tp, &m) == golden, "{} differs", tp.name);
        }
    }

    #[test]
    fn knurl_wizard_options_and_errors() {
        let (mut p, _) = Project::from_json_any(KNOB).unwrap();
        p.setups[0].operations.clear();
        p.setups[0].outputs.clear();
        let w = KnurlWizard {
            turn: false,
            chamfer: false,
            count: 24,
            ..KnurlWizard::default()
        };
        let res = w.run(&mut p, &|_| None).unwrap();
        assert_eq!(res.operations.len(), 1);
        let Operation::HelicalKnurl(k) = &p.setups[0].operations[0].op else {
            panic!()
        };
        assert_eq!(k.count, 24);
        // running again adds more ops with fresh ids and needs fresh output names
        let again = KnurlWizard {
            turn: false,
            chamfer: false,
            ..KnurlWizard::default()
        }
        .run(&mut p, &|_| None);
        assert!(again.unwrap_err().contains("used twice"));
        let mut q = Project::from_json_any(KNOB).unwrap().0;
        let bad = KnurlWizard {
            body_face: Some("nope".into()),
            ..KnurlWizard::default()
        };
        assert!(bad.run(&mut q, &|_| None).unwrap_err().contains("no face"));
        let plane = q
            .model_regions("profile", &|_| None)
            .unwrap()
            .into_iter()
            .find(|r| r.shape == "plane")
            .unwrap();
        let bad = KnurlWizard {
            body_face: Some(plane.id),
            ..KnurlWizard::default()
        };
        assert!(bad
            .run(&mut q, &|_| None)
            .unwrap_err()
            .contains("not a cone"));
    }
}
