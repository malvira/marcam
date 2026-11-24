//! A custom toolpath ([`crate::job::Custom`]): G-code (or JSON moves) written elsewhere,
//! read by [`crate::gcode_in`] and planned as marcam's own moves, so it is simulated,
//! checked (gouges, holder and fixture collisions, machine limits, feed caps), estimated and
//! carried on like any operation.

use crate::gcode_in::{self, Frame, Program};
use crate::job::Custom;
use crate::toolpath::Builder;

/// The program `o` gives, read for the setup's frame.
pub fn program(o: &Custom, three: bool) -> Program {
    let frame = if three { Frame::Three } else { Frame::Rotary };
    if o.gcode.trim().is_empty() && !o.moves.is_empty() {
        gcode_in::from_json(&o.moves, frame)
    } else {
        gcode_in::read(&o.gcode, frame)
    }
}

/// Plan `o` into `b` (3-axis on a vise, else on the 4th axis), from and back to the safe
/// height/radius `safe`. Its warnings go to `b.warnings`; returns its own lines for a
/// verbatim post (`o.verbatim`), or an error (the program can't be planned as it is).
pub fn plan(
    b: &mut Builder,
    o: &Custom,
    three: bool,
    safe: f64,
) -> Result<Option<Vec<String>>, String> {
    let p = program(o, three);
    if o.gcode.trim().is_empty() && o.moves.is_empty() {
        return Err("a custom toolpath with no G-code (or moves)".into());
    }
    if !p.errors.is_empty() {
        return Err(p.errors.join("; "));
    }
    if p.steps.iter().all(|s| s.rapid) {
        return Err("the program has no feed moves (G1, G2, G3)".into());
    }
    // (they become G-code comments: no parentheses inside them)
    for w in &p.warnings {
        b.warnings.push(w.replace('(', "[").replace(')', "]"));
    }
    if !p.tools.is_empty() && !p.tools.contains(&o.tool) {
        b.warnings.push(format!(
            "the program names {}: planned with the operation's tool, T{}",
            p.tools
                .iter()
                .map(|t| format!("T{t}"))
                .collect::<Vec<_>>()
                .join(", "),
            o.tool
        ));
    }
    // from the safe height (or radius), as every operation starts
    if three {
        b.rapid3(None, None, Some(safe));
    } else {
        b.rapid_r(safe);
    }
    let feed_of = |f: Option<f64>| f.filter(|f| *f > 0.0).unwrap_or(o.feed);
    for s in &p.steps {
        if three {
            if s.rapid {
                b.rapid3(s.x, s.y, s.z);
                continue;
            }
            let (Some(x), Some(y), Some(z)) = (s.x, s.y, s.z) else {
                return Err(format!("line {}: a feed move before X, Y and Z are all known (start with a G0 to a point)", s.line));
            };
            b.line3(x, y, z, [0.0, 0.0, 1.0], feed_of(s.feed));
        } else {
            if s.rapid {
                b.rapid(s.x, s.z, s.a);
                continue;
            }
            let (Some(x), Some(r)) = (s.x.or(b.x), s.z.or(b.r)) else {
                return Err(format!(
                    "line {}: a feed move before X and Z are known (start with a G0 to a point)",
                    s.line
                ));
            };
            b.cut(x, r, s.a.unwrap_or(b.a), feed_of(s.feed), o.plunge_feed);
        }
    }
    // and back to it
    if three {
        b.rapid3(None, None, Some(safe));
    } else {
        b.rapid_r(safe);
    }
    Ok(o.verbatim.then(|| verbatim_lines(o, three)))
}

/// The program's own lines for a verbatim post: as given, less the tape marks (`%`) and the
/// program's end (M2/M30: the post's own programs go on and end it); JSON moves as G-code.
pub fn verbatim_lines(o: &Custom, three: bool) -> Vec<String> {
    let text = if o.gcode.trim().is_empty() {
        let mut t = String::new();
        for m in &o.moves {
            t.push_str(if m.rapid { "G0" } else { "G1" });
            for (c, v) in [('X', m.x), ('Y', m.y), ('Z', m.z), ('A', m.a), ('F', m.f)] {
                if let Some(v) = v {
                    t.push_str(&format!(" {c}{}", (v * 1e4).round() / 1e4));
                }
            }
            t.push('\n');
        }
        let _ = three;
        t
    } else {
        o.gcode.clone()
    };
    text.lines()
        .filter(|l| {
            let u = l.trim().to_ascii_uppercase();
            let bare: String = u
                .split(['(', ';'])
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            !(u == "%" || bare == "M30" || bare == "M2" || bare == "M02")
        })
        .map(str::to_string)
        .collect()
}
