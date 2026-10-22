//! The Insert Part handle: the marker stands a part on a face with no
//! joint, or drops it onto a hole's wall, where OK seats it with its joint.

use serde_json::json;
use solidcraft_engine::geom::Vec3;

use crate::scenario::Harness;

/// An 80 × 50 × 10 plate with a 6.6 mm through hole at (60, 25), handles on.
fn plate() -> Harness {
    let mut h = Harness::new();
    h.app.ui.show_origin = false;
    h.app.run("solid.box", json!({"length": 80, "width": 50, "height": 10})).unwrap();
    h.app.run("solid.hole", json!({"position": [60, 25, 10], "diameter": 6.6})).unwrap();
    h.settle();
    h
}

fn ok(h: &mut Harness) {
    h.step(&json!({"click": {"handle": "dialog:OK"}})).unwrap();
    h.settle();
    assert!(h.app.dialog.is_none(), "OK closed the dialog: {:?}", h.app.dialog.as_ref().map(|d| &d.error));
}

#[test]
fn marker_stands_a_part_on_a_face_with_no_joint() {
    let mut h = plate();
    h.step(&json!({"call": "ui.view", "params": {"view": "top"}})).unwrap();
    h.settle();
    h.step(&json!({"start": "parts.insert"})).unwrap();
    h.settle();
    assert!(h.at(&json!({"handle": "part:rotate"})).is_err(), "a part has no knob");
    h.step(&json!({"drag": [{"handle": "part:position"}, {"world": [20, 15, 10]}], "steps": 12})).unwrap();
    h.settle();
    ok(&mut h);
    let doc = &h.app.session.doc;
    let occ = doc.occurrences.last().expect("the part's occurrence");
    let t = occ.transform[3];
    assert!(Vec3::new(t[0], t[1], t[2]).dist(Vec3::new(20.0, 15.0, 10.0)) < 0.5, "stands where it was dropped: {t:?}");
    assert!(doc.assembly.joints.is_empty(), "no joint for a free place: {:?}", doc.assembly.joints.len());
}

#[test]
fn marker_dropped_on_a_hole_wall_seats_the_part() {
    let mut h = plate();
    h.step(&json!({"call": "ui.view", "params": {"view": "iso"}})).unwrap();
    h.settle();
    h.step(&json!({"start": "parts.insert"})).unwrap();
    h.settle();
    // A point on the hole's wall a little below the rim, on whichever side faces the iso camera:
    // try round the wall until a drop there takes the wall.
    let mut held = None;
    for i in 0..8 {
        let a = std::f64::consts::TAU * f64::from(i) / 8.0;
        let to = [60.0 + 3.3 * a.cos(), 25.0 + 3.3 * a.sin(), 8.0];
        h.step(&json!({"drag": [{"handle": "part:position"}, {"world": to}], "steps": 12})).unwrap();
        h.settle();
        held = h.app.dialog.as_ref().and_then(|d| d.inputs.first()).map(|i| i.items.len());
        if held == Some(1) {
            break;
        }
    }
    assert_eq!(held, Some(1), "the hole's wall became the Hole pick");
    ok(&mut h);
    assert_eq!(h.app.session.doc.assembly.joints.len(), 1, "seated with its joint");
}

/// Only a hole's wall seats a part. A rounded outside corner is curved too, but out along its
/// normal there is open space, not the hole's other side. (In the app, a drag that crossed the
/// sample plate's rounded corner on the way seated the part on a joint about the corner's axis.)
#[test]
fn a_hole_wall_is_told_from_a_rounded_corner() {
    let mut h = plate();
    h.app.run("solid.fillet", json!({"edges": [[0, 0, 5]], "radius": 6})).unwrap();
    h.settle();
    let app = &h.app;
    let wall = |p: Vec3, n: Vec3| super::hole_wall(app, "Body1", p, n);
    // The hole (r 3.3 about (60, 25)): its wall's outward normal points at the axis.
    assert!(wall(Vec3::new(56.7, 25.0, 5.0), Vec3::new(1.0, 0.0, 0.0)), "the hole's wall");
    // The rounded corner (r 6 about (6, 6)): its outward normal points away from the axis.
    let a = 1.25 * std::f64::consts::PI;
    let n = Vec3::new(a.cos(), a.sin(), 0.0);
    assert!(!wall(Vec3::new(6.0, 6.0, 5.0) + n * 6.0, n), "the rounded corner");
}
