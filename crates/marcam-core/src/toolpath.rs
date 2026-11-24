//! Machine-neutral toolpath IR. Coordinates: `x` along the rotary axis, `r` = radius of the tool
//! tip from the rotation axis (Z on the machine), `a` = rotary angle in degrees.

use serde::{Deserialize, Serialize};

/// One step of a toolpath.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    /// Operator-visible comment.
    Comment { text: String },
    /// Rapid in work XYZ (3-axis and later 5-axis); only the given axes move.
    Rapid3 {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        z: Option<f64>,
    },
    /// Straight cut to the tool tip at (x, y, z) with the tool along `axis` (unit vector;
    /// [0, 0, 1] for 3-axis) at `feed` mm/min.
    Line3 {
        x: f64,
        y: f64,
        z: f64,
        axis: [f64; 3],
        feed: f64,
    },
    /// General move: the tool tip and the tool direction (unit, tip towards spindle) in the
    /// part frame; a rapid without `feed`. The post solves the axes for the machine
    /// (`crate::kinematics`).
    Pose {
        tip: [f64; 3],
        axis: [f64; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feed: Option<f64>,
    },
    /// Start of an operation: the moves until the next marker belong to it. `id` numbers the
    /// job's operations in order (0, 1, ...); `kind` is the operation type. Posts ignore it.
    Operation {
        id: u32,
        kind: String,
        /// Its circles may be written as G2/G3 arcs, within this many mm (boring).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arcs: Option<f64>,
        /// Its moves may be written as one drilling cycle (a machine with canned cycles).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cycle: Option<Box<DrillCycle>>,
        /// A custom toolpath posted verbatim: these lines are written for its moves.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verbatim: Option<Box<Vec<String>>>,
    },
    /// Load a tool and start the spindle.
    ToolChange { tool: u32, rpm: u32 },
    /// Rapid; only the given axes move (machine posts r first, then x, then a).
    Rapid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        a: Option<f64>,
    },
    /// Straight radial feed move at `feed` (mm/min).
    Plunge { r: f64, feed: f64 },
    /// Coordinated cut. `feed` is the surface feed (mm/min) along the true path; the tip never
    /// descends faster than `plunge_cap` (mm/min). A descent gentler than the operation's ramp
    /// angle carries its own feed as the cap (it runs at the cutting feed).
    Cut {
        x: f64,
        r: f64,
        a: f64,
        feed: f64,
        plunge_cap: f64,
    },
}

/// Toolpath of one program (one output file).
impl Move {
    /// Whether every number in it is finite (a NaN or an infinity is a strategy's error).
    pub fn is_finite(&self) -> bool {
        let f = |v: f64| v.is_finite();
        let o = |v: Option<f64>| v.is_none_or(f64::is_finite);
        match self {
            Move::Comment { .. } | Move::ToolChange { .. } => true,
            Move::Operation { arcs, .. } => o(*arcs),
            Move::Rapid3 { x, y, z } => o(*x) && o(*y) && o(*z),
            Move::Line3 {
                x,
                y,
                z,
                axis,
                feed,
            } => [*x, *y, *z, *feed].into_iter().chain(*axis).all(f),
            Move::Pose { tip, axis, feed } => {
                tip.iter().chain(axis).all(|v| v.is_finite()) && o(*feed)
            }
            Move::Rapid { x, r, a } => o(*x) && o(*r) && o(*a),
            Move::Plunge { r, feed } => f(*r) && f(*feed),
            Move::Cut {
                x,
                r,
                a,
                feed,
                plunge_cap,
            } => [*x, *r, *a, *feed].into_iter().all(f) && !plunge_cap.is_nan(),
        }
    }
}

/// Holes drilled as a canned cycle: G81 (no `peck`) or G83, from the R plane `r` to `bottom`
/// at `feed`, returning to the safe height between holes (G98).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrillCycle {
    pub points: Vec<[f64; 2]>,
    pub bottom: f64,
    pub r: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peck: Option<f64>,
    pub feed: f64,
    pub safe: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Toolpath {
    pub name: String,
    pub title: String,
    /// Tool loaded first: (number, description).
    pub tool: Option<(u32, String)>,
    pub notes: Vec<String>,
    pub reset_a: bool,
    /// Safe radius (rotary) or safe Z (3-axis) for rapids.
    pub safe_r: f64,
    pub moves: Vec<Move>,
    /// Replaces the machine's origin comment (3-axis programs state their own zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_comment: Option<String>,
    /// Fastest allowed Z descent (mm/min) for programs whose moves don't carry it
    /// (3-axis): the slowest plunge feed of its operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plunge_limit: Option<f64>,
    /// Descents gentler than this (degrees) may run at the cutting feed: the largest ramp
    /// angle of the program's operations (None: every descent keeps to the plunge feed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_angle: Option<f64>,
    /// The setup's G-code options (work offset, header and footer, line numbers, comments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<crate::job::PostOptions>,
}

/// Whether a descent of `drop` mm over `across` mm of travel (along the surface, not down) is
/// no steeper than the ramp, `ramp_tan` = tan(ramp angle); never for a pure plunge.
pub fn gentle_descent(drop: f64, across: f64, ramp_tan: f64) -> bool {
    ramp_tan > 0.0 && across > 1e-9 && drop <= ramp_tan * across
}

/// True length of a move at the mean radius (minimum 0.5 mm so moves near the axis still count).
pub fn true_length(dx: f64, dr: f64, da: f64, r_from: f64, r_to: f64) -> f64 {
    let r = crate::numeric::py_max(0.5 * (r_to + r_from), 0.5);
    let arc = r * crate::numeric::rad(da);
    (dx * dx + dr * dr + arc * arc).sqrt()
}

/// Builds a toolpath while tracking the machine state.
#[derive(Debug, Clone)]
pub struct Builder {
    pub moves: Vec<Move>,
    pub x: Option<f64>,
    pub r: Option<f64>,
    pub a: f64,
    /// 3-axis position (tool tip, work XYZ).
    pub p3: Option<[f64; 3]>,
    /// tan(ramp angle) of the operation being planned: gentler descents run at the cutting
    /// feed (0: every descent keeps to the plunge feed).
    pub ramp_tan: f64,
    /// What the operation being planned warns about (the program's notes get them).
    pub warnings: Vec<String>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            moves: Vec::new(),
            x: None,
            r: None,
            a: 0.0,
            p3: None,
            ramp_tan: 0.0,
            warnings: Vec::new(),
        }
    }

    /// Re-zero the A axis (the programs start with A = 0).
    pub fn reset_a(&mut self) {
        self.a = 0.0;
    }

    pub fn comment(&mut self, text: impl Into<String>) {
        self.moves.push(Move::Comment { text: text.into() });
    }

    pub fn operation(&mut self, id: u32, kind: &str) {
        self.moves.push(Move::Operation {
            id,
            kind: kind.to_string(),
            arcs: None,
            cycle: None,
            verbatim: None,
        });
    }

    pub fn tool_change(&mut self, tool: u32, rpm: u32) {
        self.moves.push(Move::ToolChange { tool, rpm });
    }

    pub fn rapid(&mut self, x: Option<f64>, r: Option<f64>, a: Option<f64>) {
        if x.is_some() {
            self.x = x;
        }
        if r.is_some() {
            self.r = r;
        }
        if let Some(a) = a {
            self.a = a;
        }
        if crate::budget::keep_move() {
            self.moves.push(Move::Rapid { x, r, a });
        }
    }

    pub fn rapid_r(&mut self, r: f64) {
        self.rapid(None, Some(r), None);
    }

    pub fn rapid_x(&mut self, x: f64) {
        self.rapid(Some(x), None, None);
    }

    /// 3-axis rapid; the planner orders the axes (up first, then across, then down).
    pub fn rapid3(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>) {
        let p = self.p3.get_or_insert([0.0, 0.0, 0.0]);
        if let Some(v) = x {
            p[0] = v;
        }
        if let Some(v) = y {
            p[1] = v;
        }
        if let Some(v) = z {
            p[2] = v;
        }
        if crate::budget::keep_move() {
            self.moves.push(Move::Rapid3 { x, y, z });
        }
    }

    /// 3-axis feed move; dropped when shorter than 1e-6 mm.
    pub fn line3(&mut self, x: f64, y: f64, z: f64, axis: [f64; 3], feed: f64) {
        if let Some(p) = self.p3 {
            let l = ((x - p[0]).powi(2) + (y - p[1]).powi(2) + (z - p[2]).powi(2)).sqrt();
            if l < 1e-6 {
                return;
            }
        }
        self.p3 = Some([x, y, z]);
        // (past the planning budget the moves are not kept: see crate::budget)
        if !crate::budget::keep_move() {
            return;
        }
        self.moves.push(Move::Line3 {
            x,
            y,
            z,
            axis,
            feed,
        });
    }

    pub fn plunge(&mut self, r: f64, feed: f64) {
        self.r = Some(r);
        if crate::budget::keep_move() {
            self.moves.push(Move::Plunge { r, feed });
        }
    }

    /// Feed move; dropped when its true length is below 1e-6 mm.
    pub fn cut(&mut self, x: f64, r: f64, a: f64, feed: f64, plunge_cap: f64) {
        let (x0, r0) = (
            self.x.expect("cut before X is known"),
            self.r.expect("cut before Z is known"),
        );
        if true_length(x - x0, r - r0, a - self.a, r0, r) < 1e-6 {
            return;
        }
        // across: the travel along the surface (X and the arc at the mean radius)
        let across = true_length(x - x0, 0.0, a - self.a, r0, r);
        let plunge_cap = if r < r0 && gentle_descent(r0 - r, across, self.ramp_tan) {
            feed
        } else {
            plunge_cap
        };
        if crate::budget::keep_move() {
            self.moves.push(Move::Cut {
                x,
                r,
                a,
                feed,
                plunge_cap,
            });
        }
        self.x = Some(x);
        self.r = Some(r);
        self.a = a;
    }
}

/// Serialize toolpaths as pretty JSON (used by `marcam toolpath`).
pub fn to_json(tps: &[Toolpath]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(tps)
}
