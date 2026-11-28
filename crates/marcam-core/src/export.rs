//! Named NC exports (as Fusion's NC programs): some of a setup's operations, picked from any of
//! its outputs, posted together to a G-code file of their own.
//!
//! The operations' toolpaths are the ones planned for the setup (what was simulated and
//! checked), not planned again: each picked operation's moves are taken as they are, in
//! planning order. Where one does not follow straight on from the operation before it in the
//! export (an operation left out between them, or another output), the machine is brought to
//! the state the operation was planned from: retracted to the safe height, its tool loaded,
//! the Y offset it was planned with, then (at the safe height) over the point it starts from
//! and down to it.

use crate::toolpath::{Move, Toolpath};

/// What the machine is at, as far as the moves so far say.
#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    x: Option<f64>,
    r: Option<f64>,
    a: Option<f64>,
    p3: [Option<f64>; 3],
    pose: Option<([f64; 3], [f64; 3])>,
    mode: Mode,
    tool: Option<(u32, u32)>,
    y_offset: Option<(f64, f64)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Mode {
    #[default]
    None,
    Rotary,
    Three,
    Pose,
}

impl State {
    fn step(&mut self, m: &Move) {
        match m {
            Move::Rapid { x, r, a } => {
                self.x = x.or(self.x);
                self.r = r.or(self.r);
                self.a = a.or(self.a);
                self.mode = Mode::Rotary;
            }
            Move::Plunge { r, .. } => {
                self.r = Some(*r);
                self.mode = Mode::Rotary;
            }
            Move::Cut { x, r, a, .. } => {
                (self.x, self.r, self.a) = (Some(*x), Some(*r), Some(*a));
                self.mode = Mode::Rotary;
            }
            Move::Rapid3 { x, y, z } => {
                for (k, v) in [x, y, z].into_iter().enumerate() {
                    self.p3[k] = v.or(self.p3[k]);
                }
                self.mode = Mode::Three;
            }
            Move::Line3 { x, y, z, .. } => {
                self.p3 = [Some(*x), Some(*y), Some(*z)];
                self.mode = Mode::Three;
            }
            Move::Pose { tip, axis, .. } => {
                self.pose = Some((*tip, *axis));
                self.mode = Mode::Pose;
            }
            Move::ToolChange { tool, rpm } => self.tool = Some((*tool, *rpm)),
            Move::YOffset { y, ball_r } => {
                self.y_offset = (*y != 0.0).then_some((*y, *ball_r));
            }
            Move::Comment { .. } | Move::Operation { .. } => {}
        }
    }
}

/// The program of the operations `picked` (by planning number) of a setup's programs `tps`
/// (as planned, in order), named `name`. None when none of them is in `tps`.
pub fn splice(tps: &[Toolpath], picked: &dyn Fn(u32) -> bool, name: &str) -> Option<Toolpath> {
    let mut moves: Vec<Move> = Vec::new();
    // the export's machine state, and the programs it took operations from
    let mut ex = State::default();
    let mut safe = None::<f64>;
    let mut from: Vec<&Toolpath> = Vec::new();
    for tp in tps {
        let mut st = State::default();
        // (the operation before, in this program, was taken: the next one follows straight on)
        let mut on = false;
        // a program that zeroes A at its start (G92 A0): its angles counted from where the
        // export is when it comes to it
        let mut a_shift = None::<f64>;
        let mut skip = None;
        for (i, m) in tp.moves.iter().enumerate() {
            if skip == Some(i) {
                st.step(m);
                continue;
            }
            if let Move::Operation { id, .. } = m {
                let take = picked(*id);
                if take && !on {
                    if !from.iter().any(|t| std::ptr::eq(*t, tp)) {
                        from.push(tp);
                    }
                    let shift = if tp.reset_a && !moves.is_empty() {
                        *a_shift.get_or_insert(ex.a.unwrap_or(0.0))
                    } else {
                        *a_shift.get_or_insert(0.0)
                    };
                    let mut at = st.clone();
                    at.a = at.a.map(|a| a + shift);
                    moves.push(m.clone());
                    // (an operation that loads its tool: loaded first, then the way in)
                    if let Some(t @ Move::ToolChange { .. }) = tp.moves.get(i + 1) {
                        at.step(t);
                        if at.tool != ex.tool {
                            ex.step(t);
                            moves.push(t.clone());
                        }
                        skip = Some(i + 1);
                    }
                    for t in way_in(&ex, &at, safe.unwrap_or(tp.safe_r), tp.safe_r) {
                        ex.step(&t);
                        moves.push(t);
                    }
                    safe = Some(tp.safe_r);
                    on = true;
                    continue;
                }
                on = take;
            }
            if on {
                let mut m = m.clone();
                if let Some(s) = a_shift.filter(|s| *s != 0.0) {
                    match &mut m {
                        Move::Rapid { a: Some(a), .. } | Move::Cut { a, .. } => *a += s,
                        _ => {}
                    }
                }
                ex.step(&m);
                moves.push(m);
            }
            st.step(m);
        }
    }
    let first = from.first()?;
    let mut notes: Vec<String> = Vec::new();
    for tp in &from {
        for n in &tp.notes {
            if !notes.contains(n) {
                notes.push(n.clone());
            }
        }
    }
    let tool = moves.iter().find_map(|m| match m {
        Move::ToolChange { tool, .. } => Some(*tool),
        _ => None,
    });
    let tool = tool.map(|n| {
        let desc = from
            .iter()
            .chain(tps.iter().collect::<Vec<_>>().iter())
            .find_map(|t| t.tool.as_ref().filter(|(k, _)| *k == n))
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        (n, desc)
    });
    let min = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let max = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    Some(Toolpath {
        name: name.to_string(),
        title: name.to_string(),
        tool,
        notes,
        reset_a: first.reset_a,
        safe_r: from.iter().map(|t| t.safe_r).fold(first.safe_r, f64::max),
        moves,
        origin_comment: first.origin_comment.clone(),
        plunge_limit: from.iter().map(|t| t.plunge_limit).fold(None, min),
        ramp_angle: from.iter().map(|t| t.ramp_angle).fold(None, max),
        post: first.post.clone(),
    })
}

/// The moves that take the export from `ex` to `st` (the state an operation was planned
/// from): up to the safe height (`safe_was` where it is now, `safe` the operation's), its tool,
/// its Y offset, then (up again after a tool change or at the start) over its start and down.
fn way_in(ex: &State, st: &State, safe_was: f64, safe: f64) -> Vec<Move> {
    let top = safe_was.max(safe);
    let mut out = Vec::new();
    let up = |mode: Mode, out: &mut Vec<Move>| match mode {
        Mode::Three => out.push(Move::Rapid3 {
            x: None,
            y: None,
            z: Some(top),
        }),
        Mode::Rotary | Mode::Pose => out.push(Move::Rapid {
            x: None,
            r: Some(top),
            a: None,
        }),
        Mode::None => {}
    };
    // up, as the export moves now
    let at_top = match ex.mode {
        Mode::Three => ex.p3[2] == Some(top),
        Mode::Rotary => ex.r == Some(top),
        Mode::Pose | Mode::None => false,
    };
    if !at_top {
        up(ex.mode, &mut out);
    }
    let load = st.tool.is_some() && st.tool != ex.tool;
    if load {
        let (tool, rpm) = st.tool.expect("checked");
        out.push(Move::ToolChange { tool, rpm });
    }
    if st.y_offset != ex.y_offset {
        let (y, ball_r) = st
            .y_offset
            .unwrap_or((0.0, ex.y_offset.map_or(0.0, |o| o.1)));
        out.push(Move::YOffset { y, ball_r });
    }
    if st.mode == Mode::None {
        return out;
    }
    // up again (a tool loaded, the start of the file, another kind of moves) before moving over
    let three = |m: Mode| m == Mode::Three;
    if load || ex.mode == Mode::None || three(ex.mode) != three(st.mode) {
        up(st.mode, &mut out);
    }
    // over where it was planned from, and down
    match st.mode {
        Mode::Rotary => {
            if st.x.is_some() || st.a.is_some() {
                out.push(Move::Rapid {
                    x: st.x,
                    r: None,
                    a: st.a,
                });
            }
            if let Some(r) = st.r.filter(|r| *r != top) {
                out.push(Move::Rapid {
                    x: None,
                    r: Some(r),
                    a: None,
                });
            }
        }
        Mode::Three => {
            if st.p3[0].is_some() || st.p3[1].is_some() {
                out.push(Move::Rapid3 {
                    x: st.p3[0],
                    y: st.p3[1],
                    z: None,
                });
            }
            if let Some(z) = st.p3[2].filter(|z| *z != top) {
                out.push(Move::Rapid3 {
                    x: None,
                    y: None,
                    z: Some(z),
                });
            }
        }
        Mode::Pose => {
            let (tip, axis) = st.pose.expect("a pose");
            out.push(Move::Pose {
                tip,
                axis,
                feed: None,
            });
        }
        Mode::None => {}
    }
    out
}

/// The fingerprint of a G-code text (64-bit FNV-1a, hex): an export is in sync while what its
/// operations post now has the fingerprint it was written with.
pub fn fingerprint(gcode: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in gcode.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The planning numbers of `ids` (operation ids) in a setup planned in the order `planned`
/// ([`crate::project::Project::planned_op_ids`]), and the ids that are not planned (disabled,
/// in error, or gone).
pub fn numbers_of(planned: &[String], ids: &[String]) -> (Vec<u32>, Vec<String>) {
    let mut nums = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match planned.iter().position(|p| p == id) {
            Some(k) => nums.push(k as u32),
            None => missing.push(id.clone()),
        }
    }
    (nums, missing)
}
