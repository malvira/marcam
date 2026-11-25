//! Picking: the features recognised on parts, and what each operation type makes of them.
//! The bracket (tests/fixtures/picking/bracket.stl, see marcam-fixtures `prism::bracket`):
//! 60 x 40 x 12, top at Z0; through holes 5 mm at (40, 8), (52, 8), (40, 32), (52, 32); a
//! blind hole 6 mm at (46, 20) to -6; a pocket X8..28 Y8..32 to -5 with a round island 6 mm
//! at (18, 20) up to -2; a step X56..60 down to -3.

use marcam::pickables::{accepts, apply, pickables, ApplyContext, Frame, Pickable};
use marcam::Mesh;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bracket() -> Mesh {
    Mesh::load(&root().join("tests/fixtures/picking/bracket.stl")).unwrap()
}

fn of<'a>(p: &'a [Pickable], kind: &str) -> Vec<&'a Pickable> {
    p.iter().filter(|x| x.kind == kind).collect()
}

fn f(p: &Pickable, k: &str) -> f64 {
    p.data[k].as_f64().unwrap()
}

fn cx(tool: f64) -> ApplyContext {
    ApplyContext {
        stock_top: Some(0.0),
        tool_diameter: Some(tool),
        model: "part".into(),
        op: None,
    }
}

#[test]
fn the_brackets_holes() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let holes = of(&p, "hole");
    assert_eq!(
        holes.len(),
        5,
        "{:#?}",
        holes.iter().map(|h| &h.label).collect::<Vec<_>>()
    );
    let through: Vec<_> = holes.iter().filter(|h| h.data["through"] == true).collect();
    assert_eq!(through.len(), 4);
    for h in &through {
        assert!((f(h, "diameter") - 5.0).abs() < 0.01, "{}", h.label);
        assert!((f(h, "top") - 0.0).abs() < 1e-6 && (f(h, "bottom") + 12.0).abs() < 1e-6);
    }
    // all four alike: one "similar" key
    assert!(through.iter().all(|h| h.similar == through[0].similar));
    let blind = holes.iter().find(|h| h.data["through"] == false).unwrap();
    assert!((f(blind, "diameter") - 6.0).abs() < 0.01 && (f(blind, "depth") - 6.0).abs() < 1e-6);
    let c = &blind.data["center"];
    assert!(
        (c[0].as_f64().unwrap() - 46.0).abs() < 1e-6
            && (c[1].as_f64().unwrap() - 20.0).abs() < 1e-6
    );
    assert_eq!(blind.faces.len(), 2, "the wall and its floor");
    assert_ne!(blind.similar, through[0].similar);
}

#[test]
fn the_brackets_pocket_faces_and_loops() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let pockets = of(&p, "pocket");
    assert_eq!(
        pockets.len(),
        1,
        "{:#?}",
        pockets.iter().map(|x| &x.label).collect::<Vec<_>>()
    );
    let pk = pockets[0];
    assert!(
        (f(pk, "floor") + 5.0).abs() < 1e-6
            && (f(pk, "top")).abs() < 1e-6
            && (f(pk, "depth") - 5.0).abs() < 1e-6
    );
    assert_eq!(pk.data["islands"].as_array().unwrap().len(), 1);
    assert!(pk.label.contains("20.00 x 24.00"), "{}", pk.label);
    // flat faces: the top, the step, the pocket floor, the island's top (not the blind hole's
    // floor: that belongs to the hole)
    let mut z: Vec<f64> = of(&p, "flat_face").iter().map(|x| f(x, "z")).collect();
    z.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(z, vec![-5.0, -3.0, -2.0, 0.0]);
    // edge loops: the top's outline and its 6 openings (4 holes, the blind hole, the pocket),
    // the step's, the pocket floor's outline and the island opening, the island top's, and
    // the bottom outline
    let loops = of(&p, "edge_loop");
    let outline = loops
        .iter()
        .find(|l| l.data["up"] == false)
        .expect("the bottom outline");
    assert_eq!(outline.data["side"], "outside");
    assert!(
        (f(outline, "length") - 200.0).abs() < 1e-6,
        "60 x 40 footprint: {}",
        outline.label
    );
    let top_openings = loops
        .iter()
        .filter(|l| f(l, "z") == 0.0 && l.data["side"] == "inside")
        .count();
    assert_eq!(top_openings, 6);
    // every region is covered by some pickable
    assert!(of(&p, "surface").iter().all(|s| s.faces.len() == 1));
}

#[test]
fn drilling_takes_holes_and_fills_points_depth_and_peck() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let holes = of(&p, "hole");
    let through: Vec<&Pickable> = holes
        .iter()
        .copied()
        .filter(|h| h.data["through"] == true)
        .collect();
    let (v, notes) = apply("drill", &through, &cx(5.0)).unwrap();
    assert_eq!(v["points"].as_array().unwrap().len(), 4);
    assert_eq!(v["top"], 0.0);
    assert_eq!(v["depth"], 12.5, "through: 0.5 past the bottom");
    assert_eq!(v["peck"], 5.0);
    assert!(notes.is_empty(), "{notes:?}");
    // a 6 mm hole with a 5 mm tool: use Bore instead
    let blind: Vec<&Pickable> = holes
        .iter()
        .copied()
        .filter(|h| h.data["through"] == false)
        .collect();
    let (v, notes) = apply("drill", &blind, &cx(5.0)).unwrap();
    assert_eq!(v["depth"], 6.0);
    assert!(notes.iter().any(|n| n.contains("Bore")), "{notes:?}");
    // a pocket is not a drilling target
    let err = apply("drill", &of(&p, "pocket"), &cx(5.0)).unwrap_err();
    assert!(err.contains("does not take a pocket"), "{err}");
}

#[test]
fn pocketing_contouring_and_facing() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let (v, _) = apply("pocket", &of(&p, "pocket"), &cx(3.175)).unwrap();
    assert_eq!(
        v["paths"].as_array().unwrap().len(),
        2,
        "outline and island"
    );
    assert_eq!(v["depth"], 5.0);
    assert_eq!(v["top"], 0.0);
    let outline = of(&p, "edge_loop")
        .into_iter()
        .filter(|l| l.data["up"] == false)
        .collect::<Vec<_>>();
    let (v, _) = apply("contour", &outline, &cx(3.175)).unwrap();
    assert_eq!(v["side"], "outside");
    assert_eq!(v["depth"], 12.0);
    let step = of(&p, "flat_face")
        .into_iter()
        .filter(|x| f(x, "z") == -3.0)
        .collect::<Vec<_>>();
    let (v, _) = apply("face", &step, &cx(6.0)).unwrap();
    assert_eq!(v["depth"], 3.0);
    // finishing takes surfaces and faces as face ids
    let (v, _) = apply("raster3d", &of(&p, "pocket"), &cx(3.0)).unwrap();
    assert!(
        v["faces"].as_array().unwrap().len() >= 5,
        "floor and walls: {v:?}"
    );
    // every operation type takes picks (Mar: picking on all operations), but a custom
    // toolpath: its geometry is its program
    let without: Vec<&str> = marcam::ops::registry()
        .iter()
        .map(|t| t.kind)
        .filter(|k| accepts(k).is_empty() && *k != "custom")
        .collect();
    assert!(without.is_empty(), "types without picking: {without:?}");
    assert!(accepts("no_such_type").is_empty());
}

#[test]
fn indexed_the_part_turned_half_round_shows_its_underside() {
    // turned 180 degrees about X: the bottom is now on top, the holes still go through
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 180.0,
            sides: false,
        },
    );
    assert_eq!(
        of(&p, "hole")
            .iter()
            .filter(|h| h.data["through"] == true)
            .count(),
        4
    );
    let top = of(&p, "flat_face")
        .iter()
        .map(|x| f(x, "z"))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (top - 12.0).abs() < 1e-6,
        "the old bottom (Z-12) is now the top at +12: {top}"
    );
    // the same face ids whatever the index
    let p0 = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let ids = |p: &[Pickable]| {
        let mut v: Vec<String> = p.iter().flat_map(|x| x.faces.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(ids(&p), ids(&p0));
}

#[test]
fn rotary_bands_on_the_knob() {
    let knob = Mesh::load(&root().join("examples/knob/knob.stl")).unwrap();
    let p = pickables(&knob, Frame::Rotary);
    let cyl = of(&p, "cylindrical_band");
    assert!(!cyl.is_empty(), "the boss is a cylinder");
    assert!(
        cyl.iter()
            .any(|c| (c.data["r"][1].as_f64().unwrap() - 5.0).abs() < 0.01),
        "{:?}",
        cyl.iter().map(|c| &c.label).collect::<Vec<_>>()
    );
    assert!(
        !of(&p, "revolved_band").is_empty(),
        "the taper, chamfer and shoulders"
    );
    let (v, _) = apply("rotary_finish", &cyl, &cx(3.175)).unwrap();
    assert_eq!(v["faces"].as_array().unwrap().len(), cyl.len());
    let (v, _) = apply("rotary_rough", &cyl[..1], &cx(3.175)).unwrap();
    assert!(v["x_range"].is_array());
    assert!(apply("drill", &cyl, &cx(3.175)).is_err());
}

#[test]
fn every_rotary_operation_takes_a_pick() {
    let knob = Mesh::load(&root().join("examples/knob/knob.stl")).unwrap();
    let p = pickables(&knob, Frame::Rotary);
    let c = cx(3.175);
    // the taper (a cone, R10 at the back to R8 at the front): the knurl's body
    let taper = of(&p, "revolved_band")
        .into_iter()
        .find(|b| b.data["shape"] == "cone" && b.data["r"][1].as_f64().unwrap() > 9.9)
        .expect("the taper");
    let (v, _) = apply("helical_knurl", &[taper], &c).unwrap();
    let (back, front) = (&v["body_back"], &v["body_front"]);
    assert!(
        back[0].as_f64().unwrap() < front[0].as_f64().unwrap(),
        "{v:?}"
    );
    assert!(
        (back[1].as_f64().unwrap() - 10.0).abs() < 0.05
            // the taper ends where the front chamfer begins (X-0.8): R 8 + 2 x 0.8 / 15
            && (front[1].as_f64().unwrap() - (8.0 + 1.6 / 15.0)).abs() < 0.01,
        "{v:?}"
    );
    // rotary turning: the span of the bands picked
    let (v, _) = apply("rotary_turning", &[taper], &c).unwrap();
    assert_eq!(v["x_range"], taper.data["x"]);
    // the chamfer: a round edge (the front face meets the chamfer at X0)
    let edges = of(&p, "circular_edge");
    assert!(!edges.is_empty());
    let front = edges
        .iter()
        .find(|e| f(e, "x").abs() < 1e-6)
        .expect("an edge at the front face");
    let (v, _) = apply("flank_chamfer", &[front], &c).unwrap();
    assert_eq!(v["edge"][0], 0.0);
    // wrapped engraving on a cylinder: the bar's radius is the surface, the text centred
    let boss = of(&p, "cylindrical_band")[0];
    let mut c2 = cx(1.0);
    c2.op = Some(serde_json::json!({"text": "AB", "text_height": 2.0}));
    let (v, _) = apply("engrave", &[boss], &c2).unwrap();
    assert!((v["top"].as_f64().unwrap() - boss.data["r"][1].as_f64().unwrap()).abs() < 1e-6);
    let xm = (boss.data["x"][0].as_f64().unwrap() + boss.data["x"][1].as_f64().unwrap()) / 2.0;
    assert!(
        v["text_at"][0].as_f64().unwrap() < xm && v["text_at"][1] == -1.0,
        "{v:?}"
    );
}

#[test]
fn engraving_on_a_flat_face_is_centred_on_it() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let step = of(&p, "flat_face")
        .into_iter()
        .filter(|x| f(x, "z") == -3.0)
        .collect::<Vec<_>>();
    let mut c = cx(1.0);
    c.op = Some(serde_json::json!({"text": "I", "text_height": 2.0}));
    let (v, _) = apply("vcarve", &step, &c).unwrap();
    assert_eq!(v["top"], -3.0);
    // the step is X56..60 Y0..40: centred at (58, 20)
    let at = &v["text_at"];
    assert!(
        (at[0].as_f64().unwrap() - 58.0).abs() < 1.0
            && (at[1].as_f64().unwrap() - 19.0).abs() < 1e-6,
        "{v:?}"
    );
}

#[test]
fn indexed_a_side_face_sets_the_index_that_turns_it_up() {
    // the bracket on a 4th axis (indexed): its underside, long sides and step are side faces
    let m = bracket();
    let p = pickables(
        &m,
        Frame::Xyz {
            index_a: 0.0,
            sides: true,
        },
    );
    let sides = of(&p, "side_face");
    let under = sides
        .iter()
        .find(|s| (f(s, "index_a").abs() - 180.0).abs() < 1e-6)
        .expect("the underside, at A180");
    assert!(
        (f(under, "distance") - 12.0).abs() < 1e-6,
        "{}",
        under.label
    );
    assert!(
        sides.iter().any(|s| (f(s, "index_a") - 90.0).abs() < 1e-6)
            && sides.iter().any(|s| (f(s, "index_a") + 90.0).abs() < 1e-6)
    );
    // at that index the face is up: a flat face, with the same id
    let turned = pickables(
        &m,
        Frame::Xyz {
            index_a: f(under, "index_a"),
            sides: false,
        },
    );
    assert!(turned.iter().any(|x| x.kind == "flat_face"
        && x.faces == under.faces
        && (f(x, "z") - 12.0).abs() < 1e-6));
    // and a quarter turn (where a wrong sign would show): every side face turned up by its
    // index is a flat face there
    for side in &sides {
        let up = pickables(
            &m,
            Frame::Xyz {
                index_a: f(side, "index_a"),
                sides: false,
            },
        );
        assert!(
            up.iter()
                .any(|x| x.kind == "flat_face" && x.faces == side.faces),
            "{} is not up at its index",
            side.label
        );
    }
    // facing it: the index, and the depth from the bar's radius down to it
    let c = ApplyContext {
        stock_top: Some(15.0),
        tool_diameter: Some(6.0),
        model: "part".into(),
        op: None,
    };
    let (v, _) = apply("face", &[under], &c).unwrap();
    assert_eq!(v["index_a"], f(under, "index_a"));
    assert_eq!(v["depth"], 3.0);
}

#[test]
fn threads_and_chamfers_take_the_brackets_holes() {
    let p = pickables(
        &bracket(),
        Frame::Xyz {
            index_a: 0.0,
            sides: false,
        },
    );
    let through: Vec<&Pickable> = of(&p, "hole")
        .into_iter()
        .filter(|h| h.data["through"] == true)
        .collect();
    // its 5 mm through holes: M6 x 1's tap drill, the whole hole
    let (v, notes) = apply("thread_mill", &through, &cx(4.0)).unwrap();
    assert_eq!(v["points"].as_array().unwrap().len(), 4);
    assert_eq!(
        (v["diameter"].as_f64(), v["pitch"].as_f64()),
        (Some(6.0), Some(1.0))
    );
    assert_eq!(v["depth"], 12.0);
    assert!(notes.iter().any(|n| n.contains("M6")), "{notes:?}");
    // a chamfer round the holes and a pocket's edge
    let mut picks = through.clone();
    let pocket = of(&p, "pocket");
    picks.push(pocket[0]);
    let (c, _) = apply("chamfer", &picks, &cx(6.0)).unwrap();
    assert_eq!(c["holes"].as_array().unwrap().len(), 4);
    assert_eq!(c["holes"][0][2], 5.0);
    assert_eq!(c["side"], "inside");
    assert_eq!(c["paths"].as_array().unwrap().len(), 1);
    assert_eq!(
        accepts("chamfer"),
        &["hole", "pocket", "outline", "edge_loop"]
    );
}
