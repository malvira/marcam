//! Operation registry: each operation type describes its parameters (with units, defaults and
//! limits), the tool shapes and geometry it accepts, and the machine abilities it needs. Hosts
//! (CLI, web app) read it to build forms and validate operations; planning dispatches on the
//! same `type` names.

use crate::det::Det;
use crate::job::{Operation, ToolShape};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    /// "number", "integer", "boolean", "vec2" ([a, b]), "rect" ([x0, y0, x1, y1]), "numbers"
    /// (list), "integers", "points" ([[x, y], ...]), "paths" ([[[x, y], ...], ...]),
    /// "string", "choice"
    pub kind: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub unit: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub default: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub choices: &'static [&'static str],
    pub help: &'static str,
    /// Its name in the form (empty: from `name`).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub label: &'static str,
    /// Shown folded away in the form (under More), with the rarely changed settings.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
    /// Left empty on purpose: worked out when planned (rest roughing's max bite, from the
    /// tool).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OperationType {
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// "roughing", "finishing", "feature"
    pub group: &'static str,
    pub params: Vec<ParamSpec>,
    /// Tool kinds it can use ("flat", "ball", "vbit").
    pub tool_shapes: &'static [&'static str],
    /// Geometry it works from: "profile", "mesh", "axial_range", "edge".
    pub geometry: &'static [&'static str],
    /// Machine abilities needed: "rotary_a" (A about X), "xyz" (3-axis moves; on a rotary
    /// setup indexed to a fixed A). None: either setup (flat on a vise, wrapped on the bar).
    pub machine_needs: &'static [&'static str],
    /// Whether it may cut below the part surface on purpose (knurls, chamfers).
    pub shapes_part: bool,
}

const fn p(
    name: &'static str,
    kind: &'static str,
    unit: &'static str,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        kind,
        unit,
        required: true,
        default: Value::Null,
        min: None,
        max: None,
        choices: &[],
        help,
        label: "",
        advanced: false,
        auto: false,
    }
}

impl ParamSpec {
    fn label(mut self, l: &'static str) -> Self {
        self.label = l;
        self
    }
    fn adv(mut self) -> Self {
        self.advanced = true;
        self
    }
    /// Optional, empty: worked out when planned.
    fn auto(mut self) -> Self {
        self.required = false;
        self.default = Value::Null;
        self.auto = true;
        self
    }
    fn opt(mut self, default: Value) -> Self {
        self.required = false;
        self.default = default;
        self
    }
    fn min(mut self, v: f64) -> Self {
        self.min = Some(v);
        self
    }
    fn max(mut self, v: f64) -> Self {
        self.max = Some(v);
        self
    }
    fn choices(mut self, c: &'static [&'static str]) -> Self {
        self.choices = c;
        self
    }
}

fn tool() -> ParamSpec {
    p("tool", "integer", "", "Tool number (T).").min(1.0)
}
fn feed() -> ParamSpec {
    p(
        "feed",
        "number",
        "mm/min",
        "Cutting feed along the surface.",
    )
    .min(0.001)
}
fn plunge() -> ParamSpec {
    p(
        "plunge_feed",
        "number",
        "mm/min",
        "Fastest the tip may descend.",
    )
    .min(0.001)
}

fn smoothing() -> ParamSpec {
    p(
        "smoothing",
        "number",
        "mm",
        "Smoothing tolerance: fewer, smoother moves within it (points dropped, corners rounded, arcs as G2/G3 on a vise). 0: off.",
    )
    .opt(Value::Null)
    .min(0.0)
}

fn ramp() -> ParamSpec {
    p(
        "ramp_angle",
        "number",
        "deg",
        "Descents gentler than this run at the cutting feed; steeper ones (and plunges) are slowed so the tip falls no faster than the plunge feed. 0: slow every descent.",
    )
    .opt(json!(crate::job::default_ramp_angle()))
    .min(0.0)
    .max(89.0)
}

/// Every operation type marcam can plan.
pub fn registry() -> Vec<OperationType> {
    let mut all = types();
    // (Mar) the stock each operation leaves, next to its depth: walls and floors (a drill's
    // bottom, a bore's wall and bottom, a thread's wall, a chamfer's height)
    for t in all.iter_mut() {
        let lv = |n: &'static str, label: &'static str, help: &'static str| {
            p(n, "number", "mm", help).opt(json!(0.0)).label(label)
        };
        let walls = lv(
            "leave_xy",
            "Walls (XY)",
            "Stock to leave on the walls, across (for a finish after it; negative: past them).",
        );
        let floors = lv(
            "leave_z",
            "Floors (Z)",
            "Stock to leave on the floor, up (for a finish after it).",
        );
        let add: Vec<ParamSpec> = match t.kind {
            "face" | "pocket" | "contour" => vec![walls, floors],
            "drill" => vec![lv(
                "leave_z",
                "Bottom",
                "Stock to leave at the bottom of the holes.",
            )],
            "bore" => vec![
                lv(
                    "leave_xy",
                    "Wall",
                    "Stock to leave on the hole's wall (a smaller hole).",
                ),
                lv("leave_z", "Bottom", "Stock to leave at the bottom."),
            ],
            "thread_mill" => vec![lv(
                "leave_xy",
                "Wall (radial)",
                "Stock to leave on the thread's flanks (a shallower thread).",
            )],
            "chamfer" => vec![lv(
                "leave_z",
                "Leave (Z)",
                "The chamfer this much higher (less cut).",
            )],
            "rotary_turning" => vec![
                lv("leave_radial", "Radial", "Stock to leave on the bar, radially (for a finish after it)."),
                lv("leave_axial", "Axial (X)", "Stock to leave on the faces across the bar, along X."),
            ],
            "rotary_rough" | "rotary_finish" | "rotary_parallel" | "rotary_flowline"
            | "rotary_wall" => vec![lv("leave_axial", "Axial (X)", "Stock to leave on the faces across the bar, along X (a flat or ball tool).")],
            "helical_knurl" | "flank_chamfer" => vec![lv("leave_radial", "Radial", "Stock to leave on the bar, radially (a shallower cut).")],
            "parting" => vec![lv("leave_axial", "Axial (X)", "Stock to leave on the part's face (the cut that much further back, the part longer).")],
            _ => vec![],
        };
        if !add.is_empty() {
            let at = t
                .params
                .iter()
                .position(|q| {
                    [
                        "depth",
                        "width",
                        "leave_radial",
                        "rough_depth",
                        "step_down",
                        "depth_per_rev",
                        "passes",
                    ]
                    .contains(&q.name)
                })
                .map_or(t.params.len(), |i| i + 1);
            for (k, q) in add.into_iter().enumerate() {
                t.params.insert(at + k, q);
            }
        }
    }
    // (Mar) in-process stock: the 2D vise operations start at the material left
    for t in all.iter_mut() {
        if ["face", "pocket", "contour", "drill", "bore"].contains(&t.kind) {
            t.params.push(p("in_process", "boolean", "", "Start where the operations before it in the setup (and a stock carried in) left the material: no passes in the air above it. Off: from the stock's top.").opt(json!(true)).adv());
        }
    }
    all
}

fn types() -> Vec<OperationType> {
    vec![
        OperationType {
            kind: "rotary_turning",
            title: "Rotary turning",
            description: "Rough the bar to a revolved profile in radial levels, then finish it with one helical pass (flat end mill).",
            group: "roughing",
            params: vec![
                tool(),
                p("rough_depth", "number", "mm", "Radial depth per roughing level.").min(0.01),
                p("rough_step", "number", "mm/rev", "X advance per turn while roughing.").min(0.01),
                p("rough_feed", "number", "mm/min", "Roughing feed.").min(0.001),
                p("finish_allowance", "number", "mm", "Radial stock left by roughing.").min(0.0),
                p("finish_step", "number", "mm/rev", "X advance per turn while finishing.").min(0.01),
                p("finish_feed", "number", "mm/min", "Finishing feed.").min(0.001),
                plunge(),
                ramp(),
                smoothing(),
                p("segment_deg", "number", "deg", "Angular length of each rotary move.").opt(json!(6.0)).min(0.1).max(90.0),
                p("grid", "number", "mm", "X spacing of the tool-radius table.").opt(json!(0.02)).min(0.001),
                p("footprint_step", "number", "mm", "Sampling of the flat tool's footprint.").opt(json!(0.01)).min(0.001),
                p("x_range", "vec2", "mm", "Turn only this X span of the profile (e.g. picked bands); empty: all of it.").opt(Value::Null),
            ],
            tool_shapes: &["flat"],
            geometry: &["profile"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_rough",
            title: "Rotary roughing (mesh)",
            description: "Rough the bar to the part (mesh or profile) plus an allowance, in radial levels, each a helix along X.",
            group: "roughing",
            params: vec![
                tool(),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [start, end].").min(-10000.0),
                p("step_down", "number", "mm", "Radial depth per level.").min(0.01),
                p("step_x", "number", "mm/rev", "X advance per turn.").min(0.01),
                p("leave_radial", "number", "mm", "Stock to leave on the bar, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("sample_deg", "number", "deg", "Point spacing around the axis.").opt(json!(6.0)).min(0.1),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the allowance surface.").opt(json!(0.005)).min(0.0001),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                p("rest", "boolean", "", "Rest roughing: only where the operations before it in the setup left material (simulated), e.g. a ball under blades no index of the roughing reached; round the rest at the air feed.").opt(json!(false)),
                p("max_bite", "number", "mm3/mm", "Rest roughing: the most a pass may take per mm of travel (the simulated stock says how much it would). A level's pass that would take more goes in two, half a step down each, and slower where still over (to 0.6 of the feed). Empty: 1.5 times the tool's normal bite (buried to its centre).").auto().min(0.1).adv(),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_finish",
            title: "Rotary finishing (mesh)",
            description: "Finish the part with a radial drop-cutter: rasters along X at constant A, or a helix around X.",
            group: "finishing",
            params: vec![
                tool(),
                p("pattern", "choice", "", "Rasters along X, or one helix around X.").choices(&["along", "around"]),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [start, end]."),
                p("a_range", "vec2", "deg", "Angular extent for 'along'.").opt(json!([0.0, 360.0])),
                p("stepover_deg", "number", "deg", "'along': angular stepover.").opt(Value::Null).min(0.01),
                p("stepover_mm", "number", "mm", "'along': stepover at the largest radius; 'around': X advance per turn.").opt(Value::Null).min(0.001),
                p("sample_mm", "number", "mm", "Point spacing along X ('along').").opt(json!(0.1)).min(0.001),
                p("sample_deg", "number", "deg", "Point spacing around X ('around' and links).").opt(json!(2.0)).min(0.01),
                p("leave_radial", "number", "mm", "Stock to leave on the bar, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the surface.").opt(json!(0.005)).min(0.0001),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                p("clean_walls", "boolean", "", "'around': a full turn at the foot of each wall.").opt(json!(true)),
                p("lead_in", "number", "mm", "Lead-in: each pass comes onto the surface turning, from this high above it (at 15 degrees), instead of a radial plunge (0: plunge).").opt(json!(0.0)).min(0.0),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_parallel",
            title: "Rotary parallel (4-axis simultaneous)",
            description: "Finish the part turning while it cuts: lines along X at angles a stepover apart, rings round the axis at X positions a stepover apart (a full turn, or an A range back and forth), or one spiral with a full turn at each end.",
            group: "finishing",
            params: vec![
                tool(),
                p("pattern", "choice", "", "Line: passes along X, stepping round. Circular: rings round the axis, stepping along X. Spiral: one helix along X, a full turn at each end.").choices(&["line", "circular", "spiral"]).label("Style"),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [front, back]."),
                p("a_range", "vec2", "deg", "Angular extent: a full turn, or a range ('line', 'circular').").opt(json!([0.0, 360.0])),
                p("stepover_deg", "number", "deg", "'line': angular stepover.").opt(Value::Null).min(0.01),
                p("stepover_mm", "number", "mm", "'line': stepover at the largest radius; 'circular': X between rings; 'spiral': X advance per turn.").opt(Value::Null).min(0.001),
                p("sample_mm", "number", "mm", "Point spacing along X.").opt(json!(0.1)).min(0.001),
                p("sample_deg", "number", "deg", "Point spacing round X.").opt(json!(2.0)).min(0.01),
                p("leave_radial", "number", "mm", "Stock to leave on the bar, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the surface.").opt(json!(0.005)).min(0.0001),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                p("y_offset", "number", "mm", "The tool this far off the axis in Y (a ball, at most its radius): the ball cuts off its tip, where its edge turns at speed, not at its centre, where it does not move. 0: over the axis.").opt(json!(0.0)).label("Y offset"),
                p("clean_walls", "boolean", "", "'spiral': a full turn at the foot of each wall.").opt(json!(true)),
                p("lead_in", "number", "mm", "Lead-in: each pass comes onto the surface turning, from this high above it (at 15 degrees), instead of a radial plunge (0: plunge).").opt(json!(0.0)).min(0.0),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_flowline",
            title: "Rotary flowline (4-axis simultaneous)",
            description: "Finish the channels between the part's walls (an impeller's blades) with passes that follow them, A and X together: each keeps its place across the channel, from one wall's line to the other's, a stepover apart across the floor, one after another without leaving the channel.",
            group: "finishing",
            params: vec![
                tool(),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [front, back]."),
                p("stepover_mm", "number", "mm", "Distance between passes across a channel's floor (at its widest).").min(0.001),
                p("wall_height", "number", "mm", "A wall: where the surface stands more than this above the lowest at that X; the channels are what is lower.").opt(json!(1.0)).min(0.01),
                p("span", "choice", "", "Floor: each channel's floor, between its walls. Sector: from one wall's top to the next, the passes climbing the walls along their twist.").choices(&["floor", "sector"]).opt(json!("floor")),
                p("sample_mm", "number", "mm", "Point spacing along X.").opt(json!(0.25)).min(0.001),
                p("leave_radial", "number", "mm", "Stock to leave on the bar, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the surface.").opt(json!(0.005)).min(0.0001),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_wall",
            title: "Rotary wall finishing (4-axis simultaneous)",
            description: "Finish the walls that radiate from the axis (an impeller's blade faces), which a ball dropped toward the axis only grazes: waterlines on them a step apart in radius, A and X together along their twist, both faces of every wall, top down.",
            group: "finishing",
            params: vec![
                tool(),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [front, back]."),
                p("step_down", "number", "mm", "Radial distance between waterlines (a 3.175 ball at 0.3: a scallop of 0.007 mm on the wall).").min(0.01),
                p("y_offset", "number", "mm", "The tool upright this far off the axis in Y, to one side for each wall's one face and to the other for its other face: its shank leans off the radius, clear of a wall leaning over the face it cuts (an impeller's blades near the hub). 0: over the axis.").opt(json!(0.0)).min(0.0).label("Y offset"),
                p("sample_mm", "number", "mm", "Point spacing along X.").opt(json!(0.25)).min(0.001),
                p("leave_radial", "number", "mm", "Stock to leave on the bar, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the surface.").opt(json!(0.005)).min(0.0001),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_pocket",
            title: "Rotary pocket (4-axis simultaneous)",
            description: "Rough a pocket round the bar (between an impeller's blades, a recess in a cylinder) level by level in radius, each level cleared adaptively: the tool takes the stepover's width as it goes (a steady load, never a full-width slot but where it comes in, down a helix where it has room), X and A together.",
            group: "roughing",
            params: vec![
                tool(),
                p("x_range", "vec2", "mm", "Axial extent of the tool tip [front, back]."),
                p("a_range", "vec2", "deg", "The angles round the axis [from, to] (0 to 360: all round).").opt(json!([0.0, 360.0])).label("A range"),
                p("step_down", "number", "mm", "Radial depth per level.").min(0.01),
                p("stepover", "number", "", "The width of material the tool takes, a fraction of its diameter (0.1 to 0.2: a steady, light load).").opt(json!(0.15)).min(0.02).max(1.0),
                p("leave_radial", "number", "mm", "Stock to leave on the part, radially (for a finish after it).").opt(json!(0.0)).label("Radial"),
                p("min_radius", "number", "mm", "The tip never goes below this radius.").opt(json!(0.0)).min(0.0),
                p("helix", "bool", "", "Down a helix where there is room (else a plunge).").opt(json!(true)),
                p("rest", "bool", "", "From the stock as the operations before left it (in 3D), not the whole bar.").opt(json!(false)).label("Rest"),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "ball"],
            geometry: &["mesh", "profile", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: false,
        },
        OperationType {
            kind: "rotary_deburr",
            title: "Rotary deburr (4-axis simultaneous)",
            description: "Chamfer the part's sharp edges that a tool over the axis reaches (a blade's or a fin's top edges, a groove's lips) with a V-bit following each edge, X and A together, its tip a depth below the edge: a 90° bit makes a 45° chamfer that deep each way.",
            group: "feature",
            params: vec![
                tool(),
                p("x_range", "vec2", "mm", "Axial extent of the edges [front, back]."),
                p("depth", "number", "mm", "How far below the edge the tip goes: the chamfer down the wall (across the top, times the tan of the V's half angle).").opt(json!(0.3)).min(0.01),
                p("min_angle", "number", "deg", "An edge: where the faces turn by this much or more.").opt(json!(45.0)).min(5.0).max(175.0).label("Edge angle"),
                p("sample_mm", "number", "mm", "Point spacing along the edges.").opt(json!(0.2)).min(0.02),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["vbit"],
            geometry: &["mesh", "axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: true,
        },
        OperationType {
            kind: "helical_knurl",
            title: "Knurl (diamond or straight)",
            description: "V grooves over a conical body: diamond, families of helical grooves crossing (two opposite families make diamonds); or straight, grooves along the axis.",
            group: "feature",
            params: vec![
                tool(),
                p("body_back", "vec2", "mm", "[x, r] of the body's back edge."),
                p("body_front", "vec2", "mm", "[x, r] of the body's front edge."),
                p("count", "integer", "", "Grooves per family (diamonds around).").min(1.0),
                p("pattern", "choice", "", "Diamond: helical grooves crossing (the helix angle and families below); Straight: grooves along the axis.").choices(&["diamond", "straight"]).opt(json!("diamond")).label("Pattern"),
                p("helix_angle", "number", "deg", "Groove angle from the X axis (diamond).").min(0.0).max(89.0),
                p("fill", "number", "", "1 = full-point pyramids; less leaves flats on the peaks.").opt(json!(1.0)).min(0.0).max(1.0),
                p("passes", "numbers", "", "Fraction of the groove depth per pass, e.g. [0.6, 1.0]."),
                p("families", "integers", "", "Helix directions: [1, -1] for diamonds.").opt(json!([1, -1])),
                p("runout", "number", "mm", "How far grooves run past both ends.").opt(json!(0.6)).min(0.0),
                p("samples", "integer", "", "Points along each groove.").opt(json!(120)).min(2.0),
                p("approach_clearance", "number", "mm", "Rapid down to this far above the body.").opt(json!(0.5)).min(0.0),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["vbit"],
            geometry: &["axial_range"],
            machine_needs: &["rotary_a"],
            shapes_part: true,
        },
        OperationType {
            kind: "flank_chamfer",
            title: "V-bit flank chamfer",
            description: "Chamfer an edge with the flank of a V-bit while the part turns.",
            group: "feature",
            params: vec![
                tool(),
                p("edge", "vec2", "mm", "[x, r] of the chamfer's outer corner."),
                p("tip_offset", "number", "mm", "Axial distance from the edge to the V-bit tip."),
                p("passes", "numbers", "mm", "Extra radial stock per pass (last normally 0)."),
                p("revolutions", "number", "", "Turns per pass.").opt(json!(2.0)).min(0.1),
                p("segment_deg", "number", "deg", "Angular length of each move.").opt(json!(6.0)).min(0.1),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["vbit"],
            geometry: &["edge"],
            machine_needs: &["rotary_a"],
            shapes_part: true,
        },
        OperationType {
            kind: "parting",
            title: "Parting (cut-off)",
            description: "Cut the part off the bar (or groove it down to a radius): the tool feeds in helically as the part turns, slowing towards the axis.",
            group: "feature",
            params: vec![
                tool(),
                p("x", "number", "mm", "Where the groove's part-side edge is: the part is on the +X side, the groove runs towards -X."),
                p("width", "number", "mm", "Groove width; 0 (or less than the tool): the tool's width. Wider grooves are cut in steps along X.").opt(json!(0.0)).min(0.0),
                p("step_x", "number", "× tool Ø", "X step between the passes of a wider groove, as a fraction of the tool width.").opt(json!(0.8)).min(0.05).max(1.0),
                p("top", "number", "mm", "Radius of the material at the groove (where the infeed starts, what the chamfer is measured from). Empty: the stock's.").opt(Value::Null).min(0.0),
                p("stop_radius", "number", "mm", "Stop at this radius (a groove); 0: cut the part off.").opt(json!(0.0)).min(0.0),
                p("sliver", "number", "mm", "Cutting off: leave a core of this radius holding the part, to break by hand; 0: right through.").opt(json!(0.0)).min(0.0),
                p("depth_per_rev", "number", "mm", "Radial depth per turn at the outside.").min(0.005),
                p("taper", "number", "", "Depth per turn and feed at the axis, as a fraction of their values at the outside (they fall with the radius).").opt(json!(0.4)).min(0.05).max(1.0),
                p("chamfer", "number", "mm", "Chamfer the part's edge by this much along X first (a V-bit with its flank, a flat end mill in small steps); 0: none.").opt(json!(0.0)).min(0.0),
                p("segment_deg", "number", "deg", "Angular length of each move.").opt(json!(6.0)).min(0.1),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["flat", "vbit"],
            geometry: &["edge"],
            machine_needs: &["rotary_a"],
            shapes_part: true,
        },
        OperationType {
            kind: "face",
            title: "Facing (3-axis)",
            description: "Face the top of the block down by a depth, in zig-zag passes along X.",
            group: "roughing",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("depth", "number", "mm", "How much to take off the top.").min(0.0),
                p("step_down", "number", "mm", "Depth per pass.").min(0.01),
                p("stepover", "number", "× tool Ø", "Stepover as a fraction of the tool diameter (0.65: 65% of it).").min(0.05).max(1.0),
                p("paths", "paths", "mm", "Where to face: closed outlines, even-odd (a picked flat face's outline and its openings); the tool stays a radius inside. Empty: the whole stock top.").opt(json!([])),
                p("regions", "regions", "", "Several picked features worked in turn, each with its own outline and depth (set by picking; empty: the fields above).").opt(json!([])),
                p("boundary_offset", "number", "mm", "Grow the faced area this far past its boundary (the picked faces' outlines, or the stock's); negative: shrink it.").opt(json!(0.0)),
                p("outward_only", "boolean", "", "The boundary offset grows the picked outline outward only, not into its openings (where the part stands inside them).").opt(json!(false)),
                p("clear_to_stock", "boolean", "", "Face everything of the stock at this depth, out to its edges (its corners too), round what of the part stands above it.").opt(json!(false)),
                feed(),
                plunge(),
            ],
            tool_shapes: &["flat"],
            geometry: &["stock", "curve"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "pocket",
            title: "Pocket (3-axis)",
            description: "Clear a rectangle (zig-zag, then a lap of the walls) or any closed outline with islands (offset loops from the middle out, the last one along the walls) to a depth, in levels.",
            group: "roughing",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("rect", "rect", "mm", "[x0, y0, x1, y1] of a rectangular pocket (used when there are no paths).").opt(Value::Null),
                p("paths", "paths", "mm", "Closed outlines, even-odd (an outline and its islands), e.g. imported from an SVG; when given, the pocket follows them instead of the rectangle.").opt(json!([])),
                p("regions", "regions", "", "Several picked features worked in turn, each with its own outline and depth (set by picking; empty: the fields above).").opt(json!([])),
                p("depth", "number", "mm", "Depth below the top.").min(0.01),
                p("top", "number", "mm", "Top of the pocket (default: stock top).").opt(Value::Null),
                p("step_down", "number", "mm", "Depth per level.").min(0.01),
                p("stepover", "number", "", "Stepover as a fraction of the tool diameter (shaped pockets: at most 0.7, so the loops' corners leave nothing).").min(0.05).max(1.0),
                p("ramp", "number", "deg", "A rectangle: down to each level back and forth along the first pass at this angle below the horizontal, instead of plunging (0: plunge).").opt(json!(0.0)).min(0.0).max(45.0),
                p("adaptive", "boolean", "", "Adaptive clearing: down a helix (at the ramp's angle) and out to the walls at a steady load, the stepover's width (e.g. 0.1 to 0.2 of the tool, with a deeper step-down), then a lap of the walls.").opt(json!(false)),
                feed(),
                plunge(),
                smoothing(),
            ],
            tool_shapes: &["flat"],
            geometry: &["rectangle", "curve"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "drill",
            title: "Drilling / pecking (3-axis)",
            description: "Peck holes at points with an end mill.",
            group: "feature",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("points", "points", "mm", "Hole centres as [[x, y], ...]."),
                p("depth", "number", "mm", "Hole depth below the top.").min(0.01),
                p("top", "number", "mm", "Top (default: stock top).").opt(Value::Null),
                p("peck", "number", "mm", "Depth per peck.").min(0.01),
                p("retract", "number", "mm", "Clearance above the top between pecks.").min(0.0),
                p("canned", "boolean", "", "Write the holes as a drilling canned cycle (G81, or G83 with pecks; R at the retract height) on a machine whose post has them, in a vise setup; else as the moves.").opt(json!(false)),
                feed(),
            ],
            tool_shapes: &["flat"],
            geometry: &["points"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "chamfer",
            title: "Chamfer / deburr (3-axis)",
            description: "Chamfer the top edges of outlines and holes with a V-bit, a width wide (a small width deburrs): the cone cuts the edge, its point clear of it.",
            group: "feature",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("paths", "paths", "mm", "Closed outlines whose top edge is chamfered.").opt(json!([])),
                p("side", "choice", "", "Outside: the part's outline or a boss (the material inside the outline); inside: a pocket's edge.").choices(&["outside", "inside"]).opt(json!("outside")),
                p("holes", "points3", "mm", "Round holes to chamfer: [[x, y, diameter], ...].").opt(json!([])),
                p("width", "number", "mm", "The chamfer's width on the top face (0.2 to 0.5: a deburr).").min(0.01),
                p("top", "number", "mm", "Top of the edges (default: stock top).").opt(Value::Null),
                p("tip_clearance", "number", "mm", "How much deeper the V-bit's point goes than the chamfer needs (the cone cuts, not the point).").opt(json!(0.2)).min(0.0),
                p("direction", "choice", "", "Climb or conventional milling, spindle clockwise.").choices(&["climb", "conventional"]).opt(json!("climb")),
                feed(),
                plunge(),
                smoothing(),
            ],
            tool_shapes: &["vbit"],
            geometry: &["paths", "points"],
            machine_needs: &["xyz"],
            shapes_part: true,
        },
        OperationType {
            kind: "thread_mill",
            title: "Thread milling (3-axis)",
            description: "Mill threads in holes or on bosses with a thread mill (a 60° single-form one as a V-bit with its diameter, or a flat tool) on a helix, a pitch a turn, in radial passes: right or left hand, climb or conventional.",
            group: "feature",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("points", "points", "mm", "The threads' centres as [[x, y], ...]."),
                p("diameter", "number", "mm", "The thread's nominal (major) diameter, e.g. 6 for M6.").min(0.1),
                p("pitch", "number", "mm", "Thread pitch, e.g. 1.0 for M6.").min(0.05),
                p("depth", "number", "mm", "Thread length below the top.").min(0.01),
                p("top", "number", "mm", "Top (default: stock top).").opt(Value::Null),
                p("internal", "boolean", "", "A thread in a hole (else on a boss).").opt(json!(true)),
                p("hand", "choice", "", "Right- or left-hand thread.").choices(&["right", "left"]).opt(json!("right")),
                p("direction", "choice", "", "Climb or conventional milling, spindle clockwise.").choices(&["climb", "conventional"]).opt(json!("climb")),
                p("thread_depth", "number", "mm", "The thread's radial depth (default: ISO metric's for the pitch).").opt(Value::Null).min(0.0),
                p("passes", "integer", "", "Radial passes to the depth.").opt(json!(1)).min(1.0),
                p("spring_passes", "integer", "", "Passes again at the final size.").opt(json!(0)).min(0.0),
                feed(),
                plunge(),
            ],
            tool_shapes: &["vbit", "flat"],
            geometry: &["points"],
            machine_needs: &["xyz"],
            shapes_part: true,
        },
        OperationType {
            kind: "bore",
            title: "Boring (3-axis)",
            description: "Bore round holes bigger than the end mill: helical entry, roughing rings, the finish at the size and spring passes, as G2/G3 arcs or segments.",
            group: "feature",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1, a radial hole); 0 on a vise.").opt(json!(0.0)),
                p("points", "points", "mm", "Hole centres as [[x, y], ...]."),
                p("diameter", "number", "mm", "Hole diameter (bigger than the tool).").min(0.01),
                p("depth", "number", "mm", "Hole depth below the top (through holes: a little past the bottom).").min(0.01),
                p("top", "number", "mm", "Top (default: stock top).").opt(Value::Null),
                p("helix_pitch", "number", "mm", "Descent per turn of the helix.").opt(json!(0.5)).min(0.01),
                p("stepover", "number", "", "Roughing rings at most this far apart, as a fraction of the tool diameter.").opt(json!(0.4)).min(0.05).max(1.0),
                p("finish_allowance", "number", "mm", "Radial stock the roughing leaves for the finish.").opt(json!(0.1)).min(0.0),
                p("spring_passes", "integer", "", "Finishing passes repeated at the size.").opt(json!(1)).min(0.0).max(5.0),
                p("direction", "choice", "", "Climb (counter-clockwise inside the hole, G3) or conventional (G2).").opt(json!("climb")).choices(&["climb", "conventional"]),
                p("arcs", "boolean", "", "Write the circles as G2/G3 arcs (else straight segments).").opt(json!(true)),
                p("tolerance", "number", "mm", "Largest distance between the segments and the circle.").opt(json!(0.002)).min(0.0001),
                feed(),
                plunge(),
                ramp(),
            ],
            tool_shapes: &["flat"],
            geometry: &["points"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "contour",
            title: "2D contour (3-axis)",
            description: "Follow a closed outline outside, inside or on it, in step-down passes.",
            group: "finishing",
            params: vec![
                tool(),
                p("index_a", "number", "deg", "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.").opt(json!(0.0)),
                p("path", "points", "mm", "Closed outline as [[x, y], ...] (3 or more)."),
                p("side", "choice", "", "Which side of the outline the tool runs on.").choices(&["outside", "inside", "on"]),
                p("regions", "regions", "", "Several picked features worked in turn, each with its own outline and depth (set by picking; empty: the fields above).").opt(json!([])),
                p("depth", "number", "mm", "Depth below the top.").min(0.01),
                p("top", "number", "mm", "Top (default: stock top).").opt(Value::Null),
                p("step_down", "number", "mm", "Depth per pass.").min(0.01),
                p("direction", "choice", "", "Climb (the usual on a rigid machine) or conventional milling, spindle clockwise; new operations take the machine's preference.").choices(&["climb", "conventional"]).opt(json!("climb")),
                p("tabs", "integer", "", "Tabs left to hold the part (0: none), evenly round the outline.").opt(json!(0)).min(0.0).max(64.0),
                p("tab_at", "numbers", "", "Tabs placed by hand (a click on the outline in the 3D view): each a fraction of the way round the outline as written, so it stays put when replanned or the model is replaced.").opt(json!([])),
                p("tab_width", "number", "mm", "Length of each tab along the outline.").opt(json!(4.0)).min(0.1),
                p("tab_height", "number", "mm", "Height of each tab above the bottom of the cut.").opt(json!(1.0)).min(0.05),
                p("lead_in", "number", "mm", "Lead-in: a quarter arc of this radius from the waste side onto the contour, tangent to it, instead of plunging on it (0: none; new operations: the tool's radius).").opt(json!(0.0)).min(0.0),
                p("lead_out", "number", "mm", "Lead-out: a quarter arc of this radius off the contour to the waste side (0: none).").opt(json!(0.0)).min(0.0),
                p("boundary_offset", "number", "mm", "Move the contour this far out from the outline (negative: in), beyond the tool's radius: a hole cut bigger, an outline left bigger. The tabs move with it.").opt(json!(0.0)),
                feed(),
                plunge(),
                smoothing(),
            ],
            tool_shapes: &["flat"],
            geometry: &["curve"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "raster3d",
            title: "3D parallel finishing",
            description: "Drop-cutter passes over the part mesh (or an image relief) at an angle, zigzag or one-way, stepover from a scallop target; in step-down levels if the stock is above it.",
            group: "finishing",
            params: {
                let mut ps = raster_params();
                let at = ps.iter().position(|p| p.name == "feed").unwrap_or(ps.len());
                ps.insert(at, p("lead_in", "number", "mm", "Lead-in: each pass comes onto the surface from this high above it, down along the pass at the ramp angle, instead of plunging onto it (0: plunge).").opt(json!(0.0)).min(0.0));
                ps.insert(at, p("rest_tool", "integer", "", "Rest finishing: the number of the earlier finish's (larger) tool; the passes cut only where it could not reach (corners, fillets); 0: everywhere.").opt(json!(0)).min(0.0));
                ps
            },
            tool_shapes: &["ball", "flat", "vbit", "tapered_ball"],
            geometry: &["mesh"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "waterline",
            title: "3D waterline finishing",
            description: "Round the part's steep walls at constant heights, level by level down the part (where a parallel finish's passes run down a wall too far apart); shallow stretches are left to the parallel finish.",
            group: "finishing",
            params: {
                let mut ps: Vec<ParamSpec> = raster_params()
                    .into_iter()
                    .filter(|p| !["angle", "pattern", "scallop", "stepover"].contains(&p.name))
                    .collect();
                let at = ps.iter().position(|p| p.name == "feed").unwrap_or(ps.len());
                ps.insert(at, p("min_slope", "number", "deg", "Only where the surface is at least this steep (from the horizontal); 0: everywhere.").opt(json!(30.0)).min(0.0).max(90.0));
                ps
            },
            tool_shapes: &["ball", "flat", "tapered_ball"],
            geometry: &["mesh"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "rough3d",
            title: "3D roughing",
            description: "Clears the stock down to the part mesh (or an image relief) in step-down levels with parallel passes, leaving stock for the finish; as rest roughing, only what the operations before it left.",
            group: "roughing",
            params: {
                let mut ps = raster_params();
                // adaptive: a steady load, from the outside in or a helix out
                if let Some(pp) = ps.iter_mut().find(|p| p.name == "pattern") {
                    *pp = p("pattern", "choice", "", "Back and forth linked at depth, every pass the same way, or adaptive: a steady load (the stepover's width, e.g. 0.1 to 0.2 of the tool with a deep step-down) following the material's edge.").choices(&["zigzag", "one_way", "adaptive"]);
                }
                // before the feeds
                let at = ps.iter().position(|p| p.name == "feed").unwrap_or(ps.len());
                ps.insert(at, p("rest", "boolean", "", "Rest roughing (vise): cut only where the operations before it in the setup left material, e.g. a smaller tool after a larger one.").opt(json!(false)));
                // (Mar's impeller) on the 4th axis: several angles in turn, indexed 3+1
                let at = ps.iter().position(|p| p.name == "feed").unwrap_or(ps.len());
                ps.insert(at, p("indexes", "numbers", "deg", "4th axis: rough at each of these A angles in turn (indexed 3+1: the tool upright, Y used), adaptive, from the stock the angles before left; empty: one angle (index_a).").opt(json!([])));
                ps.insert(at + 1, p("index_count", "integer", "", "4th axis, no indexes given: that many angles equally apart, the first where the part is lowest (between its blades); 0: none.").opt(json!(0)).min(0.0));
                ps
            },
            tool_shapes: &["flat", "ball", "tapered_ball"],
            geometry: &["mesh"],
            machine_needs: &["xyz"],
            shapes_part: false,
        },
        OperationType {
            kind: "engrave",
            title: "Engrave paths / text",
            description: "Follow paths (e.g. from an SVG) and single-line text at a depth, in passes; flat on a vise, wrapped round the bar on a rotary setup.",
            group: "feature",
            params: vec![
                tool(),
                p("text", "string", "", "Text in the built-in single-stroke font (A-Z, 0-9, - . , / : ! ? + = ( ) ').").opt(json!("")),
                p("text_height", "number", "mm", "Cap height of the text.").opt(json!(5.0)).min(0.1),
                p("text_at", "vec2", "mm", "Start of the text's baseline (wrapped: X, and arc length from the top of the bar)."),
                p("text_rotate", "number", "deg", "Turn the text (90: it runs round the bar when wrapped).").opt(json!(0.0)),
                p("paths", "paths", "mm", "Polylines to follow, e.g. imported from an SVG.").opt(json!([])),
                p("depth", "number", "mm", "Depth below the surface.").min(0.001),
                p("top", "number", "mm", "Surface: Z on a vise (default: stock top), radius on a rotary setup (default: stock radius).").opt(Value::Null),
                p("step_down", "number", "mm", "Depth per pass.").min(0.001),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["vbit", "ball", "flat", "tapered_ball"],
            geometry: &["curve"],
            machine_needs: &[],
            shapes_part: true,
        },
        OperationType {
            kind: "vcarve",
            title: "V-carve",
            description: "Carve closed shapes with a V-bit, the depth set by the bit's angle and the distance to the outline (up to a maximum); flat or wrapped.",
            group: "feature",
            params: vec![
                tool(),
                p("paths", "paths", "mm", "Closed outlines (even-odd fill), e.g. imported from an SVG.").opt(json!([])),
                p("text", "string", "", "Text in outline letters (marcam's own font: A-Z, 0-9, - . , / : ! ? + = ( ) '), carved with the paths.").opt(json!("")),
                p("text_height", "number", "mm", "Cap height of the text.").opt(json!(5.0)).min(0.1),
                p("text_at", "vec2", "mm", "Start of the text's baseline.").opt(json!([0.0, 0.0])),
                p("text_rotate", "number", "deg", "Turn the text.").opt(json!(0.0)),
                p("text_weight", "number", "", "Letter weight: pen width as a fraction of the cap height.").opt(json!(crate::engrave::OUTLINE_WEIGHT)).min(0.01).max(0.45),
                p("max_depth", "number", "mm", "Deepest cut (wide areas get a flat floor).").min(0.01),
                p("top", "number", "mm", "Surface: Z on a vise (default: stock top), radius on a rotary setup (default: stock radius).").opt(Value::Null),
                p("stepover", "number", "mm", "Distance between rows.").min(0.005),
                p("sample", "number", "mm", "Point spacing along a row.").min(0.005),
                p("tolerance", "number", "mm", "Largest dip of a straight move below the carved surface.").opt(json!(0.005)).min(0.0001),
                feed(),
                plunge(),
                ramp(),
                smoothing(),
            ],
            tool_shapes: &["vbit"],
            geometry: &["curve"],
            machine_needs: &[],
            shapes_part: true,
        },
        OperationType {
            kind: "custom",
            title: "Custom toolpath (G-code)",
            description: "A toolpath written elsewhere (an AI through MCP, a script, another CAM): G-code or JSON moves in the setup's frame (a vise: X Y Z, the tool tip; the 4th axis: X along the bar, Z the radius, A degrees), planned, simulated and checked like the others; posted through the machine's post, or verbatim.",
            group: "feature",
            params: vec![
                tool(),
                p("gcode", "gcode", "", "The program: G0 G1 G2 G3 (G17/G18/G19, I J K or R), G90/G91, G20/G21, F S A T M3/M5/M6, G54..G59 (the setup's WCS). What it can't read is listed with its line numbers.").opt(json!("")),
                p("moves", "moves", "", "Instead of G-code: JSON moves [{rapid?, x?, y?, z?, a?, f?}], absolute, mm and degrees.").opt(json!([])).adv(),
                p("verbatim", "boolean", "", "Post the program's own lines as given (still simulated and checked), not re-posted through the machine's post.").opt(json!(false)),
                p("feed", "number", "mm/min", "Feed of feed moves before the program's first F.").min(0.1),
                p("plunge_feed", "number", "mm/min", "On the 4th axis: the fastest descent towards the axis.").min(0.1),
            ],
            tool_shapes: &["flat", "ball", "vbit", "tapered_ball"],
            geometry: &[],
            machine_needs: &[],
            shapes_part: false,
        },
    ]
}

/// Whether an operation type fits a setup kind: "rotary" takes the operations that turn
/// the part (and engrave/V-carve, wrapped), "indexed" the 3-axis ones (at a fixed A),
/// "vise" everything that needs no rotary axis. Unknown kinds take everything.
pub fn fits_setup(t: &OperationType, setup_kind: &str) -> bool {
    match setup_kind {
        "rotary" => !t.machine_needs.contains(&"xyz"),
        "indexed" => t.machine_needs.contains(&"xyz"),
        "vise" => !t.machine_needs.contains(&"rotary_a"),
        _ => true,
    }
}

/// (Mar's impeller) 3D roughing at several A angles in turn (indexed 3+1, `indexes` or
/// `index_count`) fits a 4th-axis setup too: the part is turned to each angle on the axis.
pub fn indexed_on_rotary(op: &crate::job::Operation, setup_kind: &str) -> bool {
    setup_kind == "rotary"
        && matches!(op, crate::job::Operation::Rough3d(r) if !r.indexes.is_empty() || r.index_count > 0)
}

pub fn find(kind: &str) -> Option<OperationType> {
    registry().into_iter().find(|t| t.kind == kind)
}

fn tool_kind(s: &ToolShape) -> &'static str {
    match s {
        ToolShape::Flat { .. } => "flat",
        ToolShape::Ball { .. } => "ball",
        ToolShape::Vbit { .. } => "vbit",
        ToolShape::TaperedBall { .. } => "tapered_ball",
    }
}

/// Check an operation's JSON against its type: unknown or missing parameters, types, ranges,
/// and (if `tools` is given) the tool's shape. Returns readable problems (empty = fine).
pub fn check(op: &Value, tools: Option<&[crate::job::Tool]>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(kind) = op.get("type").and_then(Value::as_str) else {
        return vec!["operation has no \"type\"".into()];
    };
    let Some(t) = find(kind) else {
        return vec![format!("unknown operation type {kind:?}")];
    };
    let obj = op.as_object().cloned().unwrap_or_default();
    const COMMON: [&str; 7] = ["type", "id", "name", "color", "enabled", "picks", "error"];
    for k in obj.keys() {
        if !COMMON.contains(&k.as_str()) && !t.params.iter().any(|p| p.name == k) {
            out.push(format!("{kind}: unknown parameter {k:?}"));
        }
    }
    for p in &t.params {
        let v = obj.get(p.name);
        let Some(v) = v.filter(|v| !v.is_null()) else {
            if p.required {
                out.push(format!("{kind}: missing {} ({})", p.name, p.help));
            }
            continue;
        };
        let range = |x: f64, out: &mut Vec<String>| {
            if p.min.is_some_and(|m| x < m) || p.max.is_some_and(|m| x > m) {
                out.push(format!(
                    "{kind}: {} = {x} is outside {}..{}",
                    p.name,
                    p.min.map_or("".into(), |m| m.to_string()),
                    p.max.map_or("".into(), |m| m.to_string())
                ));
            }
        };
        let ok = match p.kind {
            "number" => v.as_f64().map(|x| range(x, &mut out)).is_some(),
            "integer" => v.as_i64().map(|x| range(x as f64, &mut out)).is_some(),
            "boolean" => v.is_boolean(),
            "vec2" => v
                .as_array()
                .is_some_and(|a| a.len() == 2 && a.iter().all(Value::is_number)),
            "rect" => v.as_array().is_some_and(|a| {
                let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                // all zeros: no rectangle (a pocket given by its paths)
                n.len() == 4
                    && a.len() == 4
                    && (n[2] > n[0] && n[3] > n[1] || n.iter().all(|&x| x == 0.0))
            }),
            "string" => v.is_string(),
            "paths" => v.as_array().is_some_and(|a| {
                a.iter().all(|path| {
                    path.as_array().is_some_and(|q| {
                        q.len() >= 2
                            && q.iter().all(|pt| {
                                pt.as_array()
                                    .is_some_and(|c| c.len() == 2 && c.iter().all(Value::is_number))
                            })
                    })
                }) && (p.name != "paths" || !a.is_empty() || !p.required)
            }),
            "points" => v.as_array().is_some_and(|a| {
                let pairs = a.iter().all(|q| {
                    q.as_array()
                        .is_some_and(|q| q.len() == 2 && q.iter().all(Value::is_number))
                });
                pairs && a.len() >= if p.name == "path" { 3 } else { 1 }
            }),
            "points3" => v.as_array().is_some_and(|a| {
                a.iter().all(|q| {
                    q.as_array()
                        .is_some_and(|q| q.len() == 3 && q.iter().all(Value::is_number))
                })
            }),
            // (an optional list may be empty: none placed)
            "numbers" => v
                .as_array()
                .is_some_and(|a| (!a.is_empty() || !p.required) && a.iter().all(Value::is_number)),
            "integers" => v
                .as_array()
                .is_some_and(|a| !a.is_empty() && a.iter().all(|x| x.is_i64())),
            "choice" => v.as_str().is_some_and(|s| p.choices.contains(&s)),
            "regions" => v.as_array().is_some_and(|a| {
                a.iter()
                    .all(|r| r.get("depth").is_some_and(Value::is_number))
            }),
            _ => true,
        };
        if !ok {
            out.push(format!(
                "{kind}: {} should be {}",
                p.name,
                match p.kind {
                    "vec2" => "[a, b]".to_string(),
                    "rect" => "[x0, y0, x1, y1] with x1 > x0 and y1 > y0".to_string(),
                    "points" if p.name == "path" =>
                        "[[x, y], ...] with 3 or more points".to_string(),
                    "points" => "[[x, y], ...]".to_string(),
                    "points3" => "[[x, y, diameter], ...]".to_string(),
                    "paths" => "[[[x, y], ...], ...] (polylines of 2 or more points)".to_string(),
                    "numbers" | "integers" => format!("a list of {}", p.kind),
                    "choice" => format!("one of {:?}", p.choices),
                    k => format!("a {k}"),
                }
            ));
        }
    }
    if let (Some(tools), Some(n)) = (tools, obj.get("tool").and_then(Value::as_u64)) {
        match tools.iter().find(|x| x.number as u64 == n) {
            None => out.push(format!("{kind}: tool T{n} is not defined")),
            Some(tl) if !t.tool_shapes.contains(&tool_kind(&tl.shape)) => out.push(format!(
                "{kind}: T{n} is a {} tool; this operation takes {}",
                tool_kind(&tl.shape),
                t.tool_shapes.join(" or ")
            )),
            _ => {}
        }
    }
    out
}

/// The `type` name of a parsed operation.
pub fn kind_of(op: &Operation) -> &'static str {
    match op {
        Operation::RotaryTurning(_) => "rotary_turning",
        Operation::HelicalKnurl(_) => "helical_knurl",
        Operation::FlankChamfer(_) => "flank_chamfer",
        Operation::Parting(_) => "parting",
        Operation::RotaryFinish(_) => "rotary_finish",
        Operation::RotaryParallel(_) => "rotary_parallel",
        Operation::RotaryFlowline(_) => "rotary_flowline",
        Operation::RotaryWall(_) => "rotary_wall",
        Operation::RotaryPocket(_) => "rotary_pocket",
        Operation::RotaryDeburr(_) => "rotary_deburr",
        Operation::RotaryRough(_) => "rotary_rough",
        Operation::Face(_) => "face",
        Operation::Pocket(_) => "pocket",
        Operation::Drill(_) => "drill",
        Operation::Bore(_) => "bore",
        Operation::Chamfer(_) => "chamfer",
        Operation::ThreadMill(_) => "thread_mill",
        Operation::Contour(_) => "contour",
        Operation::Raster3d(_) => "raster3d",
        Operation::Waterline(_) => "waterline",
        Operation::Rough3d(_) => "rough3d",
        Operation::Engrave(_) => "engrave",
        Operation::Vcarve(_) => "vcarve",
        Operation::Custom(_) => "custom",
    }
}

// ------------------------------------------------------------------ defaults

/// What defaults are derived from.
pub struct DefaultsContext<'a> {
    /// The setup, lowered to a job (stock, tools, spindle, part).
    pub job: &'a crate::job::Job,
    /// The part's face regions (from the profile or mesh), if known.
    pub regions: Option<&'a [crate::geometry::FaceRegion]>,
    /// Machine limits (feed cap, spindle range).
    pub machine: Option<&'a crate::profiles::MachineProfile>,
    /// Chosen tool number (None: the first tool whose shape the operation accepts).
    pub tool: Option<u32>,
    /// Containment boundary from the operation's geometry selection.
    pub boundary: Option<crate::geometry::Boundary>,
    /// XY extent of the part mesh (3-axis setups), for raster ranges.
    pub part_xy: Option<([f64; 2], [f64; 2])>,
    /// Z extent of the part mesh (3-axis setups), for the raster floor.
    pub part_z: Option<[f64; 2]>,
}

/// Round to a "nice" step (0.05 mm, 5 mm/min ...).
fn nice(v: f64, step: f64) -> f64 {
    ((v / step).round() * step * 1e6).round() / 1e6
}

/// The parameters cutting data sets (see [`apply_cutting`]).
pub const CUTTING_KEYS: [&str; 12] = [
    "feed",
    "rough_feed",
    "finish_feed",
    "plunge_feed",
    "step_down",
    "rough_depth",
    "depth_per_rev",
    "stepover",
    "stepover_mm",
    "step_x",
    "rough_step",
    "ramp_angle",
];

/// Lay a tool's cutting data over an operation's computed defaults `v` (from [`defaults`]):
/// feeds (scaled from the preset's spindle speed to `spindle_rpm`, so the chip load is the
/// same, and capped at `feed_cap`), plunge, depth per pass, stepover (in mm, or as a
/// fraction of the tool diameter `tool_d` where the operation takes one) and ramp angle,
/// each where the operation has that parameter and the preset gives it.
pub fn apply_cutting(
    v: &mut Value,
    p: &crate::profiles::Preset,
    spindle_rpm: f64,
    tool_d: f64,
    feed_cap: f64,
) {
    let Some(kind) = v.get("type").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(t) = find(&kind) else { return };
    let has = |k: &str| t.params.iter().any(|x| x.name == k);
    let unit = |k: &str| t.params.iter().find(|x| x.name == k).map_or("", |x| x.unit);
    let scale = if p.rpm > 0.0 && spindle_rpm > 0.0 {
        spindle_rpm / p.rpm
    } else {
        1.0
    };
    let feed_of = |f: f64| nice((f * scale).min(feed_cap).max(1.0), 1.0);
    let Some(m) = v.as_object_mut() else { return };
    let mut set = |k: &str, x: f64| {
        if has(k) {
            m.insert(k.into(), json!(x));
        }
    };
    if p.feed > 0.0 {
        if kind != "drill" {
            set("feed", feed_of(p.feed));
        }
        set("rough_feed", feed_of(p.feed));
        set("finish_feed", feed_of(0.85 * p.feed));
    }
    if p.plunge > 0.0 {
        set("plunge_feed", feed_of(p.plunge));
        if kind == "drill" {
            set("feed", feed_of(p.plunge));
        }
    }
    if let Some(d) = p.stepdown {
        set("step_down", nice(d, 0.001));
        set("rough_depth", nice(d, 0.001));
        set("depth_per_rev", nice(d, 0.001));
    }
    if let Some(s) = p.stepover {
        for k in ["stepover", "stepover_mm", "step_x", "rough_step"] {
            if unit(k).starts_with("mm") {
                set(k, nice(s, 0.001));
            } else if tool_d > 0.0 {
                set(k, nice((s / tool_d).clamp(0.05, 1.0), 0.01));
            }
        }
    }
    if let Some(a) = p.ramp_angle {
        set("ramp_angle", a);
    }
    // (a roughing preset's stock to leave: walls / floors, on a bar radial / axial)
    if let Some(l) = p.leave_xy {
        set("leave_xy", nice(l, 0.001));
        set("leave_radial", nice(l, 0.001));
    }
    if let Some(l) = p.leave_z {
        set("leave_z", nice(l, 0.001));
        set("leave_axial", nice(l, 0.001));
    }
}
/// The lowest X a cut on a bar in the 4th axis's chuck may reach: 2 mm off the jaws' face
/// (the bar's back end plus what the jaws grip, whatever their size). Below it: the jaws.
pub fn chuck_clear(job: &crate::job::Job) -> f64 {
    let grip = job
        .fixture
        .as_ref()
        .and_then(|f| f.grip)
        .unwrap_or(0.0)
        .max(0.0);
    -job.stock.length + grip + 2.0
}

/// A complete, valid operation of `kind` with every parameter filled in from the context:
/// tool size (depths, stepovers), spindle speed and the machine's feed cap (feeds), and the
/// part / selected geometry (ranges, knurl body, chamfer edge). Every host gets the same
/// defaults from here.
/// (Mar's carry-through: a cut piece carried onto the 4th axis, no model) its longest round
/// stretch along X: `(x0, x1, radius)`, where every angle's radius is within 0.3 mm of the
/// others and of the stretch's (a piece cut in the vise comes as columns, a staircase round
/// its outline), the radius the outermost.
fn carried_body(job: &crate::job::Job) -> Option<(f64, f64, f64)> {
    let crate::carry::Carried::Bar(m) = job.carried.as_deref()? else {
        return None;
    };
    let g = m.grid;
    let mut best: Option<(f64, f64, f64)> = None;
    let mut run: Option<(usize, f32)> = None;
    let close = |run: Option<(usize, f32)>, end: usize, best: &mut Option<(f64, f64, f64)>| {
        if let Some((i0, r)) = run {
            let (a, b) = (g.x(i0) - 0.5 * g.dx, g.x(end - 1) + 0.5 * g.dx);
            if best.is_none_or(|(x0, x1, _)| b - a > x1 - x0) {
                *best = Some((a, b, r as f64));
            }
        }
    };
    for i in 0..g.nx {
        let row = &m.r[i * g.na..(i + 1) * g.na];
        let (lo, hi) = row
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &v| {
                (a.min(v), b.max(v))
            });
        let round = hi > 0.5 && hi - lo < 0.3;
        match run {
            Some((_, r)) if round && (hi - r).abs() < 0.3 => {}
            _ => {
                close(run, i, &mut best);
                run = round.then_some((i, hi));
            }
        }
    }
    close(run, g.nx, &mut best);
    // (a stretch of a millimetre or more)
    best.filter(|(a, b, _)| b - a >= 1.0)
}

pub fn defaults(kind: &str, cx: &DefaultsContext) -> Result<Value, String> {
    let t = find(kind).ok_or(format!("unknown operation type {kind:?}"))?;
    let job = cx.job;
    // tool: the one asked for, else the first the operation accepts
    let tool = match cx.tool {
        Some(n) => job.tool(n).ok_or(format!("tool T{n} is not defined"))?,
        None => job
            .tools
            .iter()
            .find(|x| t.tool_shapes.contains(&tool_kind(&x.shape)))
            .ok_or(format!(
                "{kind} needs a {} tool; the job has none",
                t.tool_shapes.join(" or ")
            ))?,
    };
    let shape = tool_kind(&tool.shape);
    let d = match tool.shape {
        ToolShape::Flat { diameter } | ToolShape::Ball { diameter } => diameter,
        ToolShape::Vbit { diameter, .. } => diameter.unwrap_or(3.175),
        // the tip does the cutting that depths and stepovers are sized for
        ToolShape::TaperedBall { tip_diameter, .. } => tip_diameter,
    };
    // feeds: 0.025 mm per tooth (single flute) at the job's rpm, capped by the machine
    let cap = cx.machine.and_then(|m| m.max_feed).unwrap_or(f64::INFINITY);
    let rpm = job.spindle_rpm as f64;
    let feed = nice((0.025 * rpm).clamp(50.0, 0.75 * cap), 5.0);
    let plunge = nice((feed / 5.0).max(10.0), 5.0);
    // where: the selection's X range, else the part's
    let part_x = || -> Option<[f64; 2]> {
        if let Some(r) = cx.regions {
            let lo = r.iter().map(|f| f.x[0]).fold(f64::INFINITY, f64::min);
            let hi = r.iter().map(|f| f.x[1]).fold(f64::NEG_INFINITY, f64::max);
            if lo.is_finite() {
                return Some([lo, hi]);
            }
        }
        let p = job.part.profile.as_ref()?;
        Some([p.points.first()?[0], p.points.last()?[0]])
    };
    let x_range = cx
        .boundary
        .and_then(|b| b.x)
        .or_else(part_x)
        .unwrap_or([-10.0, 0.0]);
    let x_range = [nice(x_range[0], 0.001), nice(x_range[1], 0.001)];
    let a_range = cx.boundary.and_then(|b| b.a).unwrap_or([0.0, 360.0]);
    let mut v = serde_json::Map::new();
    v.insert("type".into(), json!(kind));
    v.insert("tool".into(), json!(tool.number));
    let depth = nice((0.125 * d).max(0.05), 0.05);
    let step = nice((0.3 * d).max(0.1), 0.05);
    let fine = nice((0.125 * d).max(0.05), 0.05);
    match kind {
        "rotary_turning" => {
            // the whole part (picking bands narrows it)
            v.insert("x_range".into(), json!(x_range));
            v.insert("rough_depth".into(), json!(depth));
            v.insert("rough_step".into(), json!(step));
            v.insert("rough_feed".into(), json!(feed));
            v.insert("finish_allowance".into(), json!(0.15));
            v.insert("finish_step".into(), json!(fine));
            v.insert("finish_feed".into(), json!(nice(0.85 * feed, 5.0)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "rotary_rough" => {
            v.insert("x_range".into(), json!(x_range));
            v.insert("step_down".into(), json!(depth));
            v.insert("step_x".into(), json!(step));
            v.insert("leave_radial".into(), json!(0.15));
            v.insert("feed".into(), json!(feed));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "rotary_finish" => {
            let along = shape != "flat";
            v.insert(
                "pattern".into(),
                json!(if along { "along" } else { "around" }),
            );
            v.insert(
                "x_range".into(),
                json!(if along {
                    x_range
                } else {
                    [x_range[1], x_range[0]]
                }),
            );
            v.insert("a_range".into(), json!(a_range));
            if along {
                v.insert("stepover_deg".into(), json!(3.0));
            } else {
                v.insert("stepover_mm".into(), json!(fine));
            }
            v.insert("feed".into(), json!(nice(1.25 * feed, 5.0).min(0.75 * cap)));
            v.insert("plunge_feed".into(), json!(plunge));
            // onto each pass turning, from a little above the surface
            v.insert("lead_in".into(), json!(0.3));
        }
        "rotary_parallel" => {
            // (a spiral: the A axis moving with X all the way, a full turn at each end)
            v.insert("pattern".into(), json!("spiral"));
            v.insert("x_range".into(), json!([x_range[1], x_range[0]]));
            v.insert("a_range".into(), json!(a_range));
            v.insert("stepover_mm".into(), json!(fine));
            v.insert("feed".into(), json!(nice(1.25 * feed, 5.0).min(0.75 * cap)));
            v.insert("plunge_feed".into(), json!(plunge));
            v.insert("lead_in".into(), json!(0.3));
        }
        "rotary_deburr" => {
            v.insert("x_range".into(), json!(x_range));
            v.insert("feed".into(), json!(nice(0.6 * feed, 5.0)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "rotary_pocket" => {
            v.insert("x_range".into(), json!(x_range));
            v.insert("step_down".into(), json!(depth));
            v.insert("leave_radial".into(), json!(0.15));
            v.insert("feed".into(), json!(feed));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "rotary_wall" => {
            v.insert("x_range".into(), json!(x_range));
            v.insert("step_down".into(), json!(0.3));
            v.insert("feed".into(), json!(nice(1.25 * feed, 5.0).min(0.75 * cap)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "rotary_flowline" => {
            v.insert("x_range".into(), json!(x_range));
            v.insert("stepover_mm".into(), json!(fine));
            v.insert("feed".into(), json!(nice(1.25 * feed, 5.0).min(0.75 * cap)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "helical_knurl" => {
            // the part's biggest cone or cylinder; (Mar's carry-through: a cut piece carried in,
            // no model) else the carried piece's longest round stretch
            let (x0, x1, r0, r1, front) = match cx.regions {
                Some(regions) => {
                    let body = regions
                        .iter()
                        .filter(|r| r.shape == "cone" || r.shape == "cylinder")
                        .max_by(|a, b| a.area.total_cmp(&b.area))
                        .ok_or("the part has no conical or cylindrical body to knurl")?;
                    let (r0, r1) = crate::wizards::radius_at_ends(body, regions);
                    let front = regions
                        .iter()
                        .map(|r| r.x[1])
                        .fold(f64::NEG_INFINITY, f64::max);
                    // (clear of the chuck's jaws too: Mar's carried part, its body the bar the
                    // chuck holds)
                    let clear = chuck_clear(job);
                    let (x0, x1) = (body.x[0], body.x[1]);
                    if x0 < clear && x1 - clear >= 1.0 {
                        let r = r0 + (r1 - r0) * (clear - x0) / (x1 - x0);
                        (clear, x1, r, r1, front)
                    } else {
                        (x0, x1, r0, r1, front)
                    }
                }
                None => {
                    let (mut a, b, r) = carried_body(job)
                        .ok_or("helical_knurl defaults need the part's faces (or a round piece carried in)")?;
                    // (clear of the chuck's jaws, where the piece is held: 2 mm off their face)
                    a = a.max(chuck_clear(job));
                    if b - a < 1.0 {
                        return Err(
                            "the piece carried in has no round stretch clear of the chuck to knurl"
                                .into(),
                        );
                    }
                    (a, b, r, r, b)
                }
            };
            let r_at = |x: f64| r0 + (r1 - r0) * (x - x0) / (x1 - x0);
            let r_mean = 0.5 * (r0 + r_at(front));
            // about 2 mm between grooves around the mean circumference
            let count = ((std::f64::consts::TAU * r_mean / 2.0).round() as u32).max(4);
            v.insert("body_back".into(), json!([nice(x0, 1e-6), nice(r0, 1e-6)]));
            v.insert(
                "body_front".into(),
                json!([nice(front, 1e-6), nice(r_at(front), 1e-6)]),
            );
            v.insert("count".into(), json!(count));
            v.insert("helix_angle".into(), json!(30.0));
            v.insert("fill".into(), json!(0.95));
            v.insert("passes".into(), json!([0.6, 1.0]));
            v.insert("feed".into(), json!(nice(feed * 2.0 / 3.0, 5.0)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "parting" => {
            // at the back of the part, the full depth (a cut-off), gently: a tenth of the tool
            // a turn
            let back = part_x().map_or(x_range[0], |r| r[0]);
            v.insert("x".into(), json!(nice(back, 0.001)));
            // behind the part the bar is whole; as deep as the tool's flutes reach (a cut-off
            // when they reach the axis, else a groove that deep)
            let top = job.stock.radius;
            v.insert("top".into(), json!(nice(top, 0.001)));
            let reach = tool.flute_length.map_or(f64::INFINITY, |f| f - 1.0);
            let stop = if reach > top {
                0.0
            } else {
                nice(top - reach + 0.1, 0.05)
            };
            v.insert("stop_radius".into(), json!(stop));
            v.insert(
                "depth_per_rev".into(),
                json!(nice((0.1 * d).max(0.05), 0.01)),
            );
            v.insert("feed".into(), json!(nice(feed * 0.6, 5.0)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "flank_chamfer" => {
            let regions = cx
                .regions
                .ok_or("flank_chamfer defaults need the part's faces")?;
            let front = regions
                .iter()
                .map(|r| r.x[1])
                .fold(f64::NEG_INFINITY, f64::max);
            let face = regions
                .iter()
                .filter(|r| r.shape == "plane" && (r.x[0] - front).abs() < 1e-9)
                .max_by(|a, b| a.area.total_cmp(&b.area))
                .ok_or("the part has no front face to chamfer")?;
            v.insert(
                "edge".into(),
                json!([nice(front, 1e-6), nice(face.r[1], 1e-6)]),
            );
            v.insert("tip_offset".into(), json!(0.5));
            v.insert("passes".into(), json!([0.2, 0.0]));
            v.insert("feed".into(), json!(nice(feed * 2.0 / 3.0, 5.0)));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "engrave" | "vcarve" => {
            // the surface: the vise block's top, or round the bar at the part's largest
            // radius over the area (the stock radius without a profile)
            let (cx, cy, span, top) = match job.stock_box {
                Some(sb) => (
                    (sb.min[0] + sb.max[0]) / 2.0,
                    (sb.min[1] + sb.max[1]) / 2.0,
                    [sb.max[0] - sb.min[0], sb.max[1] - sb.min[1]],
                    sb.max[2],
                ),
                None => {
                    let rad = job
                        .part
                        .profile
                        .as_ref()
                        .map(|p| {
                            let g = crate::geom::RevolvedProfile::new(p, job.stock.radius);
                            (0..=20)
                                .map(|k| {
                                    g.radius(
                                        x_range[0] + (x_range[1] - x_range[0]) * k as f64 / 20.0,
                                    )
                                })
                                .fold(0.0, f64::max)
                        })
                        .filter(|r| *r > 0.0)
                        .unwrap_or(job.stock.radius);
                    let circ = std::f64::consts::TAU * rad;
                    (
                        (x_range[0] + x_range[1]) / 2.0,
                        0.0,
                        [x_range[1] - x_range[0], circ / 2.0],
                        rad,
                    )
                }
            };
            v.insert("top".into(), json!(nice(top, 0.001)));
            if kind == "engrave" {
                let h = nice((span[1] / 4.0).clamp(1.0, 5.0), 0.5);
                let text = "MARCAM";
                let width = (5.0 * text.len() as f64 - 1.0) * h / 6.0;
                v.insert("text".into(), json!(text));
                v.insert("text_height".into(), json!(h));
                v.insert(
                    "text_at".into(),
                    json!([nice(cx - width / 2.0, 0.01), nice(cy - h / 2.0, 0.01)]),
                );
                v.insert("text_rotate".into(), json!(0.0));
                v.insert("paths".into(), json!([]));
                let dep = match tool.shape {
                    ToolShape::Vbit { .. } => 0.3,
                    _ => nice((0.1 * d).max(0.05), 0.05),
                };
                v.insert("depth".into(), json!(dep));
                v.insert("step_down".into(), json!(nice(dep / 2.0, 0.01)));
            } else {
                let (hw, hh) = ((span[0] / 3.0).min(10.0), (span[1] / 6.0).min(4.0));
                let (x0, y0, x1, y1) = (
                    nice(cx - hw, 0.01),
                    nice(cy - hh, 0.01),
                    nice(cx + hw, 0.01),
                    nice(cy + hh, 0.01),
                );
                v.insert(
                    "paths".into(),
                    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]]),
                );
                v.insert("max_depth".into(), json!(1.0));
                v.insert("stepover".into(), json!(0.1));
                v.insert("sample".into(), json!(0.1));
                v.insert("tolerance".into(), json!(0.005));
            }
            v.insert("feed".into(), json!(feed));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "custom" => {
            // (its program comes from elsewhere: empty until given)
            v.insert("gcode".into(), json!(""));
            v.insert("moves".into(), json!([]));
            v.insert("verbatim".into(), json!(false));
            v.insert("feed".into(), json!(feed));
            v.insert("plunge_feed".into(), json!(plunge));
        }
        "face" | "pocket" | "drill" | "bore" | "contour" | "raster3d" | "rough3d" | "waterline"
        | "chamfer" | "thread_mill" => {
            // the vise block, or around the bar over the part (indexed 3+1 on a rotary setup)
            let rad = job.stock.radius;
            let sb = job.stock_box.unwrap_or(crate::job::StockBox {
                min: [x_range[0], -rad, -rad],
                max: [x_range[1], rad, rad],
            });
            v.insert("index_a".into(), json!(0.0));
            let (w, h, hz) = (
                sb.max[0] - sb.min[0],
                sb.max[1] - sb.min[1],
                sb.max[2] - sb.min[2],
            );
            let (mx, my) = ((sb.min[0] + sb.max[0]) / 2.0, (sb.min[1] + sb.max[1]) / 2.0);
            if matches!(
                kind,
                "pocket" | "drill" | "bore" | "contour" | "chamfer" | "thread_mill"
            ) {
                v.insert("top".into(), json!(sb.max[2]));
            }
            match kind {
                "face" => {
                    v.insert("depth".into(), json!(0.5f64.min(hz)));
                    v.insert("step_down".into(), json!(depth.max(0.25)));
                    v.insert("stepover".into(), json!(0.6));
                }
                "pocket" => {
                    v.insert(
                        "rect".into(),
                        json!([
                            nice(mx - w / 4.0, 0.01),
                            nice(my - h / 4.0, 0.01),
                            nice(mx + w / 4.0, 0.01),
                            nice(my + h / 4.0, 0.01)
                        ]),
                    );
                    v.insert("depth".into(), json!(nice((0.3 * hz).max(0.5), 0.05)));
                    v.insert("step_down".into(), json!(depth));
                    v.insert("stepover".into(), json!(0.5));
                    // ramped in, not plunged
                    v.insert("ramp".into(), json!(3.0));
                }
                "drill" => {
                    v.insert("points".into(), json!([[nice(mx, 0.01), nice(my, 0.01)]]));
                    v.insert("depth".into(), json!(nice((0.5 * hz).max(0.5), 0.05)));
                    v.insert("peck".into(), json!(nice(d.max(0.5), 0.05)));
                    v.insert("retract".into(), json!(1.0));
                    v.insert("feed".into(), json!(plunge));
                }
                "chamfer" => {
                    // the block's outline, a 0.5 mm chamfer
                    let i = 2.0f64.min(w / 4.0).min(h / 4.0);
                    let (x0, y0, x1, y1) =
                        (sb.min[0] + i, sb.min[1] + i, sb.max[0] - i, sb.max[1] - i);
                    v.insert(
                        "paths".into(),
                        json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]]),
                    );
                    v.insert("side".into(), json!("outside"));
                    v.insert("holes".into(), json!([]));
                    v.insert("width".into(), json!(0.5));
                    v.insert("tip_clearance".into(), json!(0.2));
                    let dir = cx
                        .machine
                        .and_then(|m| m.cut_direction.clone())
                        .unwrap_or_else(|| "climb".into());
                    v.insert("direction".into(), json!(dir));
                }
                "thread_mill" => {
                    // an M6 x 1 hole in the middle, a third of the block deep
                    v.insert("points".into(), json!([[nice(mx, 0.01), nice(my, 0.01)]]));
                    v.insert("diameter".into(), json!(6.0));
                    v.insert("pitch".into(), json!(1.0));
                    v.insert("depth".into(), json!(nice((hz / 3.0).max(1.0), 0.5)));
                    v.insert("internal".into(), json!(true));
                    v.insert("hand".into(), json!("right"));
                    v.insert("direction".into(), json!("climb"));
                    // ISO metric inside a nut: 5/8 of the thread's height
                    v.insert("thread_depth".into(), json!(0.54));
                    v.insert("passes".into(), json!(2));
                    v.insert("spring_passes".into(), json!(0));
                }
                "bore" => {
                    // a hole twice the tool's size, half the block deep
                    v.insert("points".into(), json!([[nice(mx, 0.01), nice(my, 0.01)]]));
                    v.insert("diameter".into(), json!(nice(2.0 * d, 0.1)));
                    v.insert("depth".into(), json!(nice((0.5 * hz).max(0.5), 0.05)));
                    v.insert("helix_pitch".into(), json!(nice((0.15 * d).max(0.1), 0.05)));
                }
                "contour" => {
                    let i = 2.0f64.min(w / 4.0).min(h / 4.0);
                    let (x0, y0, x1, y1) =
                        (sb.min[0] + i, sb.min[1] + i, sb.max[0] - i, sb.max[1] - i);
                    v.insert(
                        "path".into(),
                        json!([[x0, y0], [x1, y0], [x1, y1], [x0, y1]]),
                    );
                    v.insert("side".into(), json!("outside"));
                    v.insert("depth".into(), json!(nice((0.3 * hz).max(0.5), 0.05)));
                    v.insert("step_down".into(), json!(depth));
                    // the machine's preference (climb unless its profile says otherwise)
                    let dir = cx
                        .machine
                        .and_then(|m| m.cut_direction.clone())
                        .unwrap_or_else(|| "climb".into());
                    v.insert("direction".into(), json!(dir));
                    // in and out along arcs the tool's radius
                    v.insert("lead_in".into(), json!(nice(d / 2.0, 0.05)));
                    v.insert("lead_out".into(), json!(nice(d / 2.0, 0.05)));
                }
                _ => {
                    // over the part mesh, or the block
                    let (xr, yr) = cx
                        .part_xy
                        .unwrap_or(([sb.min[0], sb.max[0]], [sb.min[1], sb.max[1]]));
                    let zb = sb.min[2];
                    v.insert(
                        "x_range".into(),
                        json!([nice(xr[0], 0.01), nice(xr[1], 0.01)]),
                    );
                    v.insert(
                        "y_range".into(),
                        json!([nice(yr[0], 0.01), nice(yr[1], 0.01)]),
                    );
                    v.insert("angle".into(), json!(0.0));
                    v.insert("pattern".into(), json!("zigzag"));
                    v.insert("direction".into(), json!("climb"));
                    v.insert("sample".into(), json!(nice((0.1 * d).max(0.05), 0.01)));
                    v.insert("tolerance".into(), json!(0.005));
                    if kind == "waterline" {
                        // levels a fifth of the tool apart, sampled finely, on the walls
                        v.insert("step_down".into(), json!(nice((0.2 * d).max(0.05), 0.05)));
                        v.insert("sample".into(), json!(nice((0.05 * d).max(0.05), 0.01)));
                        v.insert("leave_z".into(), json!(0.0));
                        v.insert("min_slope".into(), json!(30.0));
                        // levels, not passes: no pass direction, pattern or spacing
                        for k in ["angle", "pattern", "stepover", "scallop"] {
                            v.remove(k);
                        }
                    } else if kind == "rough3d" {
                        // flat-end clearing: wide passes, deep levels, stock left for the finish
                        v.insert("scallop".into(), json!(0.0));
                        v.insert("stepover".into(), json!(nice(0.4 * d, 0.05)));
                        v.insert("step_down".into(), json!(nice((0.5 * d).max(0.1), 0.05)));
                        v.insert("leave_z".into(), json!(0.25));
                    } else {
                        // onto each pass from a little above the surface, not a plunge
                        v.insert("lead_in".into(), json!(nice((0.25 * d).max(0.2), 0.05)));
                        // finishing: a ball or V-bit works to a scallop target (the stepover
                        // shown is what it gives), a flat tool to 15% of its diameter
                        // a 0.02 mm cusp: about a sixth of a 3 mm ball's diameter apart
                        let scallop = if matches!(shape, "ball" | "vbit" | "tapered_ball") {
                            0.02
                        } else {
                            0.0
                        };
                        let so = match tool.shape {
                            ToolShape::Ball { diameter } if scallop > 0.0 => {
                                2.0 * (scallop * (diameter - scallop)).sqrt()
                            }
                            ToolShape::Vbit { angle, .. } if scallop > 0.0 => {
                                2.0 * scallop * (angle / 2.0).to_radians().dtan()
                            }
                            ToolShape::TaperedBall { .. } if scallop > 0.0 => {
                                crate::cutter_for(tool.number, tool.shape)
                                    .ok()
                                    .and_then(|c| c.scallop_width(scallop))
                                    .unwrap_or(0.15 * d)
                            }
                            _ => 0.15 * d,
                        };
                        v.insert("scallop".into(), json!(scallop));
                        v.insert("stepover".into(), json!(nice(so.max(0.01), 0.001)));
                        v.insert("step_down".into(), json!(depth));
                        v.insert("leave_z".into(), json!(0.0));
                    }
                    // down to the bottom of the part (a relief's black level), else half the block
                    let floor = match cx.part_z {
                        Some([lo, _]) if job.stock_box.is_some() => lo.max(zb),
                        _ => zb.max(sb.max[2] - hz / 2.0),
                    };
                    v.insert("floor".into(), json!(nice(floor, 0.01)));
                    // after a 3D roughing the finish takes the whole depth in one level; without
                    // one it also clears the stock, in levels of half the tool's diameter (one
                    // full-depth level would bury the tool in the stock)
                    let roughed = job
                        .programs
                        .iter()
                        .flat_map(|p| &p.operations)
                        .any(|o| matches!(o, Operation::Rough3d(_)));
                    if kind == "raster3d" && !roughed {
                        v.insert("step_down".into(), json!(nice((0.5 * d).max(0.1), 0.05)));
                    }
                    if kind == "raster3d" && roughed {
                        v.insert(
                            "step_down".into(),
                            json!(nice((sb.max[2] - floor).max(0.01), 0.01)),
                        );
                    }
                }
            }
            if !v.contains_key("feed") {
                v.insert("feed".into(), json!(feed));
            }
            if kind != "drill" {
                v.insert("plunge_feed".into(), json!(plunge));
            }
        }
        _ => return Err(format!("no defaults for {kind:?}")),
    }
    // smoothing: finishing 0.005 mm, roughing 0.01
    if t.params.iter().any(|p| p.name == "smoothing") && !v.contains_key("smoothing") {
        let finishing = !matches!(
            kind,
            "rotary_turning" | "rotary_rough" | "rough3d" | "pocket"
        );
        v.insert(
            "smoothing".into(),
            json!(if finishing { 0.005 } else { 0.01 }),
        );
    }
    // every other parameter: its registry default
    for p in &t.params {
        if !v.contains_key(p.name) && !p.default.is_null() {
            v.insert(p.name.into(), p.default.clone());
        }
    }
    let out = Value::Object(v);
    let problems = check(&out, Some(&job.tools));
    if !problems.is_empty() {
        return Err(format!(
            "defaults for {kind} are not valid: {}",
            problems.join("; ")
        ));
    }
    Ok(out)
}

/// Parameters shared by the parallel finishing and roughing rasters.
fn raster_params() -> Vec<ParamSpec> {
    vec![
        tool(),
        p(
            "index_a",
            "number",
            "deg",
            "Rotary setups: the A angle the part is turned to first (indexed 3+1); 0 on a vise.",
        )
        .opt(json!(0.0)),
        p("x_range", "vec2", "mm", "X extent of the tool centre."),
        p("y_range", "vec2", "mm", "Y extent of the tool centre."),
        p(
            "angle",
            "number",
            "deg",
            "Pass direction from +X (0: passes along X, stepping in Y).",
        )
        .opt(json!(0.0)),
        p(
            "pattern",
            "choice",
            "",
            "Back and forth linked at depth, or every pass the same way.",
        )
        .choices(&["zigzag", "one_way"]),
        p(
            "direction",
            "choice",
            "",
            "One-way passes: climb or conventional (clockwise spindle).",
        )
        .choices(&["climb", "conventional"]),
        p(
            "scallop",
            "number",
            "mm",
            "Target scallop height; > 0 sets the stepover for ball and V-bit tools.",
        )
        .opt(json!(0.0))
        .min(0.0),
        p(
            "stepover",
            "number",
            "mm",
            "Distance between passes (when no scallop target applies).",
        )
        .min(0.01),
        p("sample", "number", "mm", "Point spacing along a pass.").min(0.005),
        p(
            "step_down",
            "number",
            "mm",
            "Depth per level (roughing to the surface).",
        )
        .min(0.01),
        p(
            "floor",
            "number",
            "mm",
            "Lowest Z (where there is no part).",
        )
        .min(-10000.0),
        p(
            "leave_z",
            "number",
            "mm",
            "Stock to leave on the surface, up (for a finish after it).",
        )
        .opt(json!(0.0))
        .label("Floors (Z)"),
        p(
            "leave_xy",
            "number",
            "mm",
            "Stock to leave across, on the walls (a flat or ball tool).",
        )
        .opt(json!(0.0))
        .label("Walls (XY)"),
        p(
            "tolerance",
            "number",
            "mm",
            "Largest dip of a straight move below the surface.",
        )
        .opt(json!(0.005))
        .min(0.0001),
        feed(),
        plunge(),
        ramp(),
        smoothing(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;
    use crate::job::Job;

    fn sample(t: &OperationType) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("type".into(), json!(t.kind));
        for p in &t.params {
            let v = if !p.default.is_null() {
                p.default.clone()
            } else {
                match p.kind {
                    "integer" => json!(p.min.unwrap_or(1.0).max(1.0) as i64),
                    "vec2" => json!([-5.0, 0.0]),
                    "rect" => json!([5.0, 5.0, 25.0, 35.0]),
                    "points" => json!([[2.0, 2.0], [58.0, 2.0], [58.0, 38.0], [2.0, 38.0]]),
                    "paths" => json!([[
                        [-15.0, -3.0],
                        [-5.0, -3.0],
                        [-5.0, 3.0],
                        [-15.0, 3.0],
                        [-15.0, -3.0]
                    ]]),
                    "string" => json!("4CAM"),
                    "numbers" => json!([1.0]),
                    "points3" => json!([[10.0, 10.0, 6.0]]),
                    "integers" => json!([1]),
                    "choice" => json!(p.choices[0]),
                    "boolean" => json!(true),
                    _ => json!(p.min.unwrap_or(1.0).max(0.5)),
                }
            };
            m.insert(p.name.into(), v);
        }
        if t.kind == "rotary_finish" || t.kind == "rotary_parallel" {
            m.insert("stepover_mm".into(), json!(0.5));
        }
        Value::Object(m)
    }

    #[test]
    fn every_operation_type_is_registered_and_its_spec_matches_the_engine() {
        let reg = registry();
        let kinds: Vec<&str> = reg.iter().map(|t| t.kind).collect();
        let job = Job::from_json(include_str!("../../../examples/knob/knob.job.json")).unwrap();
        for prog in &job.programs {
            for op in &prog.operations {
                assert!(kinds.contains(&kind_of(op)));
            }
        }
        for t in &reg {
            // a sample built from the spec parses as that operation ...
            let s = sample(t);
            let op: Operation =
                serde_json::from_value(s.clone()).unwrap_or_else(|e| panic!("{}: {e}", t.kind));
            assert_eq!(kind_of(&op), t.kind);
            assert!(
                check(&s, None).is_empty(),
                "{}: {:?}",
                t.kind,
                check(&s, None)
            );
            // ... and every field the engine writes is in the spec
            let back = serde_json::to_value(&op).unwrap();
            for k in back.as_object().unwrap().keys() {
                assert!(
                    k == "type" || t.params.iter().any(|p| p.name == k),
                    "{}: {k} missing from spec",
                    t.kind
                );
            }
        }
    }

    #[test]
    fn defaults_for_every_type_are_complete_valid_and_plan_on_the_knob() {
        let job = Job::from_json(include_str!("../../../examples/knob/knob.job.json")).unwrap();
        let mesh = crate::Mesh::revolve(job.part.profile.as_ref().unwrap(), 360);
        let (regions, _) = crate::geometry::face_regions(&mesh, 30.0);
        let z1 = crate::profiles::builtin_machine("makera-z1-4axis").unwrap();
        // tools on hand for every shape: the knob's flat T1 and V-bit T2, plus a ball T3
        let mut job = job;
        job.tools.push(crate::job::Tool {
            flute_length: Some(12.0),
            ..crate::job::Tool::new(3, "ball 3.175", ToolShape::Ball { diameter: 3.175 })
        });
        let mut boxed = job.clone();
        boxed.stock_box = Some(crate::job::StockBox {
            min: [0.0, 0.0, -10.0],
            max: [60.0, 40.0, 0.0],
        });
        let dome =
            crate::Mesh::from_bytes(include_bytes!("../../../examples/plate/dome.stl")).unwrap();
        for t in registry() {
            let three = t.machine_needs.contains(&"xyz");
            let job = if three { &boxed } else { &job };
            let cx = DefaultsContext {
                job,
                regions: Some(&regions),
                machine: Some(&z1),
                tool: None,
                boundary: None,
                part_xy: None,
                part_z: None,
            };
            let v = defaults(t.kind, &cx).unwrap_or_else(|e| panic!("{}: {e}", t.kind));
            // every parameter present (none blank)
            for p in &t.params {
                let has = v.get(p.name).is_some_and(|x| !x.is_null());
                let either = matches!(p.name, "stepover_deg" | "stepover_mm") // one of the two
                    // (empty: worked out when planned)
                    || p.auto;
                assert!(has || either, "{}: {} has no default", t.kind, p.name);
            }
            assert!(
                check(&v, Some(&job.tools)).is_empty(),
                "{}: {:?}",
                t.kind,
                check(&v, Some(&job.tools))
            );
            // feeds respect the machine's cap
            for k in ["feed", "rough_feed", "finish_feed"] {
                if let Some(f) = v.get(k).and_then(Value::as_f64) {
                    assert!(f <= 0.75 * 4000.0, "{}: {k} {f}", t.kind);
                }
            }
            // and it plans on the knob setup (a custom toolpath once given its program: a
            // helix on the bar, or a slot on the vise)
            let mut v = v.clone();
            if t.kind == "custom" {
                v["gcode"] = json!(if three {
                    "G0 X10 Y10 Z5
G1 Z-0.5 F60
G1 X20 F300
G1 Y15
G1 X10
G1 Y10
G0 Z5
"
                } else {
                    "G0 X-5 Z16 A0
G1 Z11.8 F60
G1 X-15 A720 F300
G1 X-16 A900
G0 Z16
"
                });
            }
            let op: Operation = serde_json::from_value(v.clone()).unwrap();
            let mut one = job.clone();
            one.programs = vec![crate::job::Program {
                name: "t".into(),
                title: "t".into(),
                notes: vec![],
                reset_a: false,
                operations: vec![op],
            }];
            let tps = crate::plan_with_mesh(&one, three.then_some(&dome))
                .unwrap_or_else(|e| panic!("{}: {e}", t.kind));
            // (a custom toolpath: the few moves of the program given above)
            // (rotary flowline and wall finishing: the knob has no channel between walls, no
            // wall rising from the axis, only their warnings)
            let least = match t.kind {
                "custom" => 4,
                "rotary_flowline" | "rotary_wall" => 0,
                _ => 10,
            };
            assert!(tps[0].moves.len() > least, "{}", t.kind);
        }
        // defaults follow the context: the knob's own values come out for the knob
        let cx = DefaultsContext {
            job: &job,
            regions: Some(&regions),
            machine: Some(&z1),
            tool: None,
            boundary: None,
            part_xy: None,
            part_z: None,
        };
        let turn = defaults("rotary_turning", &cx).unwrap();
        assert_eq!(
            (
                turn["rough_depth"].as_f64(),
                turn["rough_step"].as_f64(),
                turn["rough_feed"].as_f64(),
                turn["plunge_feed"].as_f64()
            ),
            (Some(0.4), Some(0.95), Some(300.0), Some(60.0))
        );
        let knurl = defaults("helical_knurl", &cx).unwrap();
        assert_eq!(knurl["body_back"], json!([-15.0, 10.0]));
        assert_eq!(knurl["body_front"], json!([0.0, 8.0]));
        assert_eq!(knurl["count"], json!(28));
        let chamfer = defaults("flank_chamfer", &cx).unwrap();
        assert_eq!(chamfer["edge"], json!([0.0, 7.2]));
        // a ball tool finishes along X, a flat one around; a selection sets the range
        let b = crate::geometry::Boundary {
            x: Some([-15.0, -1.0]),
            a: None,
        };
        let ball = defaults(
            "rotary_finish",
            &DefaultsContext {
                tool: Some(3),
                boundary: Some(b),
                ..cx
            },
        )
        .unwrap();
        assert_eq!(
            (ball["pattern"].as_str(), ball["x_range"].clone()),
            (Some("along"), json!([-15.0, -1.0]))
        );
        let flat = defaults(
            "rotary_finish",
            &DefaultsContext {
                tool: Some(1),
                ..cx
            },
        )
        .unwrap();
        assert_eq!(flat["pattern"], "around");
        // a slow machine caps the feeds
        let mut slow = z1.clone();
        slow.max_feed = Some(200.0);
        let capped = defaults(
            "rotary_turning",
            &DefaultsContext {
                machine: Some(&slow),
                ..cx
            },
        )
        .unwrap();
        assert!(capped["rough_feed"].as_f64().unwrap() <= 150.0);
        assert!(defaults(
            "helical_knurl",
            &DefaultsContext {
                tool: Some(1),
                ..cx
            }
        )
        .is_err());
    }

    #[test]
    fn check_reports_problems() {
        let job = Job::from_json(include_str!("../../../examples/knob/knob.job.json")).unwrap();
        let knurl = serde_json::to_value(&job.programs[1].operations[0]).unwrap();
        assert!(check(&knurl, Some(&job.tools)).is_empty());
        let mut bad = knurl.clone();
        bad["tool"] = json!(1);
        bad["fill"] = json!(1.5);
        bad["passes"] = json!("x");
        bad["bogus"] = json!(1);
        bad.as_object_mut().unwrap().remove("count");
        let errs = check(&bad, Some(&job.tools)).join("\n");
        for want in [
            "T1 is a flat tool",
            "fill = 1.5 is outside",
            "passes should be",
            "unknown parameter \"bogus\"",
            "missing count",
        ] {
            assert!(errs.contains(want), "{want}: {errs}");
        }
        assert!(check(&json!({"type": "nope"}), None)[0].contains("unknown operation type"));
    }
}
