//! Feeds and speeds: spindle speed from the material's surface speed, feed from its chip load
//! per tooth × flutes × rpm, thickened for radial chip thinning where the stepover is less
//! than half the tool (adaptive clearing above all), depths per pass from the material's
//! limits (a slot takes less than a side cut, an adaptive cut can take the flutes' length),
//! all within the machine's spindle range and feed cap. Starting values for carbide tools on
//! hobby and light machines; the results fill a tool's cutting data or an operation's feeds.
//!
//! Feeds are linear (mm/min at the cutting edge): a machine that reads rotary feeds in
//! degrees (the Z1) gets them converted when posting, as with any other feed.

use crate::det::Det;
use serde::{Deserialize, Serialize};

/// What a material takes, for carbide end mills.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MaterialData {
    /// The material (see [`crate::profiles::MATERIALS`]).
    pub name: &'static str,
    /// Surface speed, m/min.
    pub vc: f64,
    /// Chip load per tooth per mm of tool diameter (mm/tooth/mm), within `fz_min..fz_max`.
    pub fz_per_d: f64,
    pub fz_min: f64,
    pub fz_max: f64,
    /// Depth per pass in a full-width slot, × the tool's diameter.
    pub slot_depth: f64,
    /// Plunge feed as a share of the feed.
    pub plunge: f64,
    /// Ramp (and helix) angle, degrees.
    pub ramp: f64,
    /// Energy to cut a cubic millimetre (J/mm³), for the spindle's power.
    pub kc: f64,
}

#[allow(clippy::too_many_arguments)]
const fn m(
    name: &'static str,
    vc: f64,
    fz_per_d: f64,
    fz_max: f64,
    slot_depth: f64,
    plunge: f64,
    ramp: f64,
    kc: f64,
) -> MaterialData {
    MaterialData {
        name,
        vc,
        fz_per_d,
        fz_min: 0.003,
        fz_max,
        slot_depth,
        plunge,
        ramp,
        kc,
    }
}

/// The materials of [`crate::profiles::MATERIALS`], in that order.
pub const MATERIAL_DATA: [MaterialData; 19] = [
    m("aluminium", 200.0, 0.008, 0.10, 0.5, 0.3, 3.0, 0.8),
    m("brass", 150.0, 0.008, 0.10, 0.5, 0.3, 3.0, 1.2),
    m("copper", 120.0, 0.006, 0.08, 0.4, 0.3, 2.0, 1.4),
    m("mild steel", 90.0, 0.004, 0.06, 0.25, 0.2, 2.0, 2.6),
    m("stainless steel", 60.0, 0.003, 0.05, 0.2, 0.2, 1.5, 3.0),
    m("titanium", 45.0, 0.003, 0.05, 0.15, 0.2, 1.5, 3.2),
    m("hardwood", 400.0, 0.012, 0.25, 1.0, 0.5, 5.0, 0.15),
    m("softwood", 500.0, 0.015, 0.30, 1.5, 0.5, 5.0, 0.1),
    m("plywood", 400.0, 0.012, 0.25, 1.0, 0.5, 5.0, 0.15),
    m("mdf", 400.0, 0.014, 0.25, 1.0, 0.5, 5.0, 0.12),
    m("acrylic", 300.0, 0.010, 0.15, 1.0, 0.4, 4.0, 0.3),
    m("polycarbonate", 300.0, 0.010, 0.15, 1.0, 0.4, 4.0, 0.3),
    m("pom", 350.0, 0.012, 0.20, 1.0, 0.5, 5.0, 0.25),
    m("nylon", 300.0, 0.012, 0.20, 1.0, 0.5, 5.0, 0.25),
    m("hdpe", 400.0, 0.014, 0.25, 1.0, 0.5, 5.0, 0.2),
    m("pvc", 300.0, 0.010, 0.15, 1.0, 0.4, 4.0, 0.3),
    m("pcb/fr4", 150.0, 0.004, 0.05, 0.5, 0.3, 3.0, 0.6),
    m("machining wax", 400.0, 0.020, 0.30, 1.5, 0.5, 8.0, 0.05),
    m("foam", 500.0, 0.030, 0.50, 2.0, 0.6, 10.0, 0.02),
];

/// The material's data by name (as [`crate::profiles::material_key`] matches names).
pub fn material(name: &str) -> Option<&'static MaterialData> {
    let k = crate::profiles::material_key(name);
    MATERIAL_DATA
        .iter()
        .find(|m| crate::profiles::material_key(m.name) == k)
}

/// How the tool cuts, by operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engagement {
    /// Full width (contours, engraving, parting, bores).
    Slot,
    /// Clearing with a usual stepover (pockets, facing, roughing).
    Side,
    /// Adaptive clearing: a thin cut, as deep as the flutes allow.
    Adaptive,
    /// Finishing: a light cut on a surface.
    Finish,
    /// Drilling.
    Drill,
}

impl Engagement {
    /// How operation type `op` cuts (`adaptive`: its adaptive option or pattern).
    pub fn of(op: &str, adaptive: bool) -> Engagement {
        if adaptive {
            return Engagement::Adaptive;
        }
        match op {
            "drill" => Engagement::Drill,
            "contour" | "engrave" | "vcarve" | "parting" | "bore" | "flank_chamfer"
            | "helical_knurl" => Engagement::Slot,
            "raster3d" | "waterline" | "rotary_finish" | "rotary_parallel" | "rotary_flowline"
            | "rotary_turning" => Engagement::Finish,
            _ => Engagement::Side,
        }
    }
}

/// The tool, as the calculator needs it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CalcTool {
    Flat {
        diameter: f64,
    },
    Ball {
        diameter: f64,
    },
    /// V-bit (included angle, degrees): it cuts at the diameter its depth gives.
    Vbit {
        diameter: f64,
        angle: f64,
    },
    Drill {
        diameter: f64,
    },
}

impl CalcTool {
    pub fn diameter(&self) -> f64 {
        match *self {
            CalcTool::Flat { diameter }
            | CalcTool::Ball { diameter }
            | CalcTool::Vbit { diameter, .. }
            | CalcTool::Drill { diameter } => diameter,
        }
    }

    /// The calculator's view of a job tool (a tapered ball: a ball of its tip).
    pub fn of(shape: &crate::job::ToolShape) -> CalcTool {
        use crate::job::ToolShape as S;
        match *shape {
            S::Flat { diameter } => CalcTool::Flat { diameter },
            S::Ball { diameter } => CalcTool::Ball { diameter },
            S::Vbit { angle, diameter } => CalcTool::Vbit {
                diameter: diameter.unwrap_or(6.0),
                angle,
            },
            S::TaperedBall { tip_diameter, .. } => CalcTool::Ball {
                diameter: tip_diameter,
            },
        }
    }
}

/// What to calculate for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedsInput {
    pub material: String,
    pub tool: CalcTool,
    /// Flutes (default 2; 1 for a V-bit).
    #[serde(default)]
    pub flutes: Option<u32>,
    /// Flute length, mm (the deepest pass).
    #[serde(default)]
    pub flute_length: Option<f64>,
    /// Operation type (default: a side cut).
    #[serde(default)]
    pub op: Option<String>,
    /// The operation's adaptive option (or pattern).
    #[serde(default)]
    pub adaptive: bool,
    /// The stepover the operation uses (mm); default: the engagement's.
    #[serde(default)]
    pub stepover: Option<f64>,
    /// The depth per pass it uses (mm); default: the material's limit.
    #[serde(default)]
    pub stepdown: Option<f64>,
    /// A spindle speed given (an operation follows its setup's); default: from the surface
    /// speed, within the machine's range.
    #[serde(default)]
    pub rpm: Option<f64>,
    /// The machine's spindle range and feed cap (0: none).
    #[serde(default)]
    pub min_rpm: f64,
    #[serde(default)]
    pub max_rpm: f64,
    #[serde(default)]
    pub feed_cap: f64,
    /// The spindle's power, W (0: not known).
    #[serde(default)]
    pub power_w: f64,
}

/// The calculated values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feeds {
    pub rpm: f64,
    /// mm/min
    pub feed: f64,
    pub plunge: f64,
    /// Depth per pass, mm.
    pub stepdown: f64,
    /// Stepover, mm.
    pub stepover: f64,
    pub ramp_angle: f64,
    /// The chip load per tooth the feed gives (mm), and the thickness of the chip it makes
    /// (thinner than the chip load where the stepover is less than half the tool).
    pub chip_load: f64,
    pub chip_thickness: f64,
    /// Surface speed at the cutting diameter, m/min.
    pub surface_speed: f64,
    /// What limited the values, in words.
    pub notes: Vec<String>,
}

/// Radial chip thinning: how much thinner than the feed per tooth the chip is when the
/// stepover `ae` is less than half the tool (`d`): 1 at half or more.
pub fn chip_thinning(ae: f64, d: f64) -> f64 {
    if d <= 0.0 || ae >= d / 2.0 {
        return 1.0;
    }
    let ae = ae.max(1e-3 * d);
    (1.0 - (1.0 - 2.0 * ae / d).powi(2)).sqrt()
}

/// Round to `step`.
fn nice(v: f64, step: f64) -> f64 {
    ((v / step).round() * step * 1e6).round() / 1e6
}

/// Feeds and speeds for `input`.
pub fn calculate(input: &FeedsInput) -> Result<Feeds, String> {
    let mat = material(&input.material).ok_or_else(|| {
        format!(
            "no cutting data for {:?}: pick one of {}",
            input.material,
            MATERIAL_DATA.map(|m| m.name).join(", ")
        )
    })?;
    let d = input.tool.diameter();
    if d.is_nan() || d <= 0.0 {
        return Err("the tool needs a diameter".into());
    }
    let op = input.op.as_deref().unwrap_or("pocket");
    let eng = match input.tool {
        CalcTool::Drill { .. } => Engagement::Drill,
        _ => Engagement::of(op, input.adaptive),
    };
    let z = input.flutes.unwrap_or(match input.tool {
        CalcTool::Vbit { .. } => 1,
        _ => 2,
    }) as f64;
    let flute = input.flute_length.unwrap_or(3.0 * d).max(0.1);
    let mut notes = Vec::new();
    // the cut: stepover and depth per pass
    let ae_default = match eng {
        Engagement::Slot | Engagement::Drill => d,
        Engagement::Side => 0.4 * d,
        Engagement::Adaptive => 0.12 * d,
        Engagement::Finish => 0.1 * d,
    };
    let ae = input
        .stepover
        .filter(|&s| s > 0.0)
        .unwrap_or(ae_default)
        .min(d);
    // the depth a pass may take: a slot's limit, more as the cut gets thinner, up to the
    // flutes (an adaptive cut takes up to two diameters)
    let side_scale = if ae >= 0.9 * d {
        1.0
    } else {
        (2.0 - ae / d).min(2.0)
    };
    let mut ap_max = (mat.slot_depth * d * side_scale).min(flute);
    if eng == Engagement::Adaptive {
        ap_max = flute.min(2.0 * d).max(ap_max);
    }
    let ap_default = match eng {
        Engagement::Finish => (0.1 * d).min(ap_max),
        Engagement::Drill => (0.5 * d).min(flute),
        _ => ap_max,
    };
    let mut ap = match input.stepdown.filter(|&s| s > 0.0) {
        Some(s) if s > ap_max + 1e-9 && eng != Engagement::Drill => {
            notes.push(format!(
                "{s} mm a pass is more than {} takes at this stepover ({:.2} mm)",
                mat.name, ap_max
            ));
            s
        }
        Some(s) => s,
        None => ap_default,
    };
    // the diameter cutting: a ball or a V-bit cuts smaller than its diameter at a shallow
    // depth
    let d_cut = match input.tool {
        CalcTool::Ball { .. } if ap < d / 2.0 => {
            (2.0 * (ap * (d - ap)).max(0.0).sqrt()).max(0.2 * d)
        }
        CalcTool::Vbit { angle, .. } => {
            (2.0 * ap * (angle.to_radians() / 2.0).dtan()).clamp(0.2 * d, d)
        }
        _ => d,
    };
    // spindle speed: the surface speed at the cutting diameter, within the machine's range
    let want_rpm = mat.vc * 1000.0 / (std::f64::consts::PI * d_cut);
    let rpm = match input.rpm.filter(|&r| r > 0.0) {
        Some(r) => {
            if r < 0.5 * want_rpm {
                notes.push(format!(
                    "{} wants about {:.0} rpm with this tool; at {r:.0} rpm the surface speed is {:.0} m/min",
                    mat.name,
                    want_rpm,
                    r * std::f64::consts::PI * d_cut / 1000.0
                ));
            }
            r
        }
        None => {
            let mut r = want_rpm;
            if input.max_rpm > 0.0 && r > input.max_rpm {
                notes.push(format!(
                    "the spindle tops out at {:.0} rpm (the surface speed {} wants is {:.0} rpm)",
                    input.max_rpm, mat.name, want_rpm
                ));
                r = input.max_rpm;
            }
            if input.min_rpm > 0.0 && r < input.min_rpm {
                notes.push(format!(
                    "the spindle turns no slower than {:.0} rpm: the chip load is kept, so the feed is higher",
                    input.min_rpm
                ));
                r = input.min_rpm;
            }
            nice(r, 100.0).max(1.0)
        }
    };
    // chip load: the material's for this diameter, thickened so the chip is that thick
    // where the stepover thins it (at most three times)
    let fz = (mat.fz_per_d * d).clamp(mat.fz_min, mat.fz_max);
    let thin = match eng {
        Engagement::Drill => 1.0,
        _ => chip_thinning(ae, d_cut).max(1.0 / 3.0),
    };
    let fz_feed = fz / thin;
    if thin < 0.99 {
        notes.push(format!(
            "chip thinning: at a {:.2} mm stepover the chip is {:.0}% of the feed per tooth, so the feed per tooth is {:.3} mm for a {:.3} mm chip",
            ae,
            100.0 * thin,
            fz_feed,
            fz
        ));
    }
    let (mut feed, mut plunge) = if eng == Engagement::Drill {
        // feed per revolution, about a fiftieth of the diameter (half that in metals)
        let f_rev = fz * 2.0;
        (rpm * f_rev, rpm * f_rev)
    } else {
        let f = rpm * z * fz_feed;
        (f, f * mat.plunge)
    };
    // the spindle's power: a shallower pass where the cut would take more
    if input.power_w > 0.0 && eng != Engagement::Drill {
        let mrr = ae * ap * feed / 60.0; // mm³/s
        let need = mrr * mat.kc;
        let avail = 0.8 * input.power_w;
        if need > avail {
            let k = avail / need;
            ap *= k;
            notes.push(format!(
                "the spindle's {:.0} W: {:.2} mm a pass (the cut would take {:.0} W)",
                input.power_w, ap, need
            ));
        }
    }
    if input.feed_cap > 0.0 && feed > input.feed_cap {
        notes.push(format!(
            "the feed is capped at the machine's {:.0} mm/min ({:.0} would keep the chip load)",
            input.feed_cap, feed
        ));
        let k = input.feed_cap / feed;
        feed = input.feed_cap;
        plunge *= k;
    }
    let chip_load = feed / (rpm * z.max(1.0));
    Ok(Feeds {
        rpm,
        feed: nice(feed.max(1.0), 1.0),
        plunge: nice(plunge.max(1.0), 1.0),
        stepdown: nice(ap.max(0.01), 0.01),
        stepover: nice(ae, 0.01),
        ramp_angle: mat.ramp,
        chip_load: nice(chip_load, 0.0001),
        chip_thickness: nice(chip_load * thin, 0.0001),
        surface_speed: nice(rpm * std::f64::consts::PI * d_cut / 1000.0, 1.0),
        notes,
    })
}

/// The calculated values as a cutting-data row for `material` and operation type `op`.
pub fn preset(f: &Feeds, material: &str, op: Option<&str>) -> crate::profiles::Preset {
    crate::profiles::Preset {
        material: material.into(),
        op: op.map(str::to_string),
        rpm: f.rpm,
        feed: f.feed,
        plunge: f.plunge,
        stepdown: Some(f.stepdown),
        stepover: Some(f.stepover),
        ramp_angle: Some(f.ramp_angle),
        leave_xy: None,
        leave_z: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::det::Det;

    fn input(material: &str, d: f64, op: &str) -> FeedsInput {
        FeedsInput {
            material: material.into(),
            tool: CalcTool::Flat { diameter: d },
            flutes: Some(2),
            flute_length: Some(12.0),
            op: Some(op.into()),
            adaptive: false,
            stepover: None,
            stepdown: None,
            rpm: None,
            min_rpm: 0.0,
            max_rpm: 15000.0,
            feed_cap: 4000.0,
            power_w: 0.0,
        }
    }

    #[test]
    fn every_material_offered_has_data() {
        for m in crate::profiles::MATERIALS {
            assert!(material(m).is_some(), "{m}");
        }
        assert_eq!(material("Aluminum").unwrap().name, "aluminium");
    }

    #[test]
    fn chip_load_times_flutes_times_rpm() {
        // aluminium, 3.175 mm two-flute: 200 m/min wants 20 000 rpm, the spindle gives 15 000
        let f = calculate(&input("aluminium", 3.175, "pocket")).unwrap();
        assert_eq!(f.rpm, 15000.0);
        assert!(
            f.notes.iter().any(|n| n.contains("tops out")),
            "{:?}",
            f.notes
        );
        // 0.0254 mm a tooth at a 0.4 D stepover (thinned a little: 98%)
        let fz = 0.008 * 3.175;
        let want = 15000.0 * 2.0 * fz / chip_thinning(0.4 * 3.175, 3.175);
        assert!((f.feed - want).abs() < 1.0, "{} vs {want}", f.feed);
        assert!((f.chip_thickness - fz).abs() < 1e-3);
        assert!(f.plunge < f.feed);
        assert!(f.stepdown <= 12.0);
    }

    #[test]
    fn a_thin_stepover_feeds_faster_for_the_same_chip() {
        let side = calculate(&input("aluminium", 6.0, "pocket")).unwrap();
        let mut a = input("aluminium", 6.0, "pocket");
        a.adaptive = true;
        a.feed_cap = 0.0;
        let ad = calculate(&a).unwrap();
        // 0.12 D: the chip is 65% of the feed per tooth
        assert!((chip_thinning(0.72, 6.0) - 0.65).abs() < 0.01);
        assert!(ad.feed > 1.4 * side.feed, "{} vs {}", ad.feed, side.feed);
        assert!((ad.chip_thickness - side.chip_thickness).abs() < 2e-3);
        // and deeper: up to the flutes (12 mm, two diameters)
        assert!(
            ad.stepdown > 2.0 * side.stepdown,
            "{} vs {}",
            ad.stepdown,
            side.stepdown
        );
        assert!(ad.stepdown <= 12.0);
    }

    #[test]
    fn a_slot_is_shallower_than_a_side_cut_and_steel_than_aluminium() {
        let slot = calculate(&input("aluminium", 6.0, "contour")).unwrap();
        let side = calculate(&input("aluminium", 6.0, "pocket")).unwrap();
        let steel = calculate(&input("mild steel", 6.0, "contour")).unwrap();
        assert!((slot.stepdown - 3.0).abs() < 1e-9, "{}", slot.stepdown);
        assert!(side.stepdown > slot.stepdown);
        assert!(steel.stepdown < slot.stepdown);
        assert!(steel.rpm < slot.rpm && steel.feed < slot.feed);
    }

    #[test]
    fn the_machine_caps_the_feed_and_the_power_the_depth() {
        let mut i = input("hardwood", 6.0, "pocket");
        i.feed_cap = 1000.0;
        let f = calculate(&i).unwrap();
        assert_eq!(f.feed, 1000.0);
        assert!(f.notes.iter().any(|n| n.contains("capped")));
        let mut p = input("aluminium", 6.0, "pocket");
        p.power_w = 100.0;
        let full = calculate(&input("aluminium", 6.0, "pocket")).unwrap();
        let weak = calculate(&p).unwrap();
        assert!(weak.stepdown < full.stepdown);
        let mrr = weak.stepover * weak.stepdown * weak.feed / 60.0;
        assert!(mrr * 0.8 <= 0.8 * 100.0 + 1.0, "{mrr}");
    }

    #[test]
    fn a_given_spindle_speed_keeps_the_chip_load() {
        let mut i = input("aluminium", 3.175, "pocket");
        i.rpm = Some(10000.0);
        let f = calculate(&i).unwrap();
        assert_eq!(f.rpm, 10000.0);
        let g = calculate(&input("aluminium", 3.175, "pocket")).unwrap();
        assert!((f.feed / g.feed - 10000.0 / 15000.0).abs() < 0.01);
    }

    #[test]
    fn a_ball_finishing_shallow_spins_faster() {
        let mut i = input("aluminium", 6.0, "raster3d");
        i.tool = CalcTool::Ball { diameter: 6.0 };
        i.max_rpm = 0.0;
        i.stepdown = Some(0.3);
        let f = calculate(&i).unwrap();
        // cutting at 2 sqrt(0.3 * 5.7) = 2.6 mm
        assert!((f.surface_speed - 200.0).abs() < 2.0, "{}", f.surface_speed);
        assert!(f.rpm > 20000.0);
    }

    #[test]
    fn unknown_materials_are_refused() {
        assert!(calculate(&input("unobtainium", 3.0, "pocket")).is_err());
    }
}
