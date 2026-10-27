//! The primitives' handle: the marker places a new primitive on a
//! face while its dialog is open, and moves one from Edit Feature.

use serde_json::json;
use solidcraft_engine::Sel;
use solidcraft_engine::doc::FeatureKind;
use solidcraft_engine::geom::Vec3;

use crate::dialogs::Kind;
use crate::scenario::Harness;
use crate::selection::PLANAR_FACES;

/// An 80 × 50 × 10 plate seen from the top, with the handles on.
fn plate() -> Harness {
    let mut h = Harness::new();
    h.app.set_origin_shown(false);
    h.app.preferences.placement_handles = true;
    h.app.run("solid.box", json!({"length": 80, "width": 50, "height": 10})).unwrap();
    h.step(&json!({"call": "ui.view", "params": {"view": "top"}})).unwrap();
    h.settle();
    h
}

fn drag_ok(h: &mut Harness, to: [f64; 3]) {
    assert!(h.at(&json!({"handle": "primitive:position"})).is_ok(), "the marker is published");
    assert!(h.at(&json!({"handle": "primitive:rotate"})).is_err(), "a primitive has no knob");
    h.step(&json!({"drag": [{"handle": "primitive:position"}, {"world": to}], "steps": 12})).unwrap();
    h.settle();
    h.step(&json!({"click": {"handle": "dialog:OK"}})).unwrap();
    h.settle();
    assert!(h.app.dialog.is_none(), "OK closed the dialog");
}

fn value(h: &Harness, e: &str) -> f64 {
    h.app.session.doc.eval(e, solidcraft_engine::doc::expr::Kind::Length).unwrap()
}

#[test]
fn marker_stands_a_new_cylinder_on_the_face_it_is_dropped_on() {
    let mut h = plate();
    h.step(&json!({"start": "solid.cylinder"})).unwrap();
    h.settle();
    // Nothing was pre-selected: the marker starts where the command would put it, the origin.
    drag_ok(&mut h, [40.0, 25.0, 10.0]);
    let f = h.app.session.doc.features.iter().find(|f| matches!(f.kind, FeatureKind::Cylinder { .. })).expect("a Cylinder was made");
    let FeatureKind::Cylinder { base, axis, .. } = &f.kind else { panic!("a Cylinder: {:?}", f.kind) };
    let b = Vec3::new(value(&h, &base[0]), value(&h, &base[1]), value(&h, &base[2]));
    assert!(b.dist(Vec3::new(40.0, 25.0, 10.0)) < 0.5, "stands where it was dropped: {b:?}");
    assert!(axis.0.dist(Vec3::Z) < 1e-9, "along the face normal: {:?}", axis.0);
}

#[test]
fn marker_centres_a_new_box_on_the_face() {
    let mut h = plate();
    h.step(&json!({"start": "solid.box"})).unwrap();
    h.settle();
    drag_ok(&mut h, [40.0, 25.0, 10.0]);
    let f = h.app.session.doc.features.iter().filter(|f| matches!(f.kind, FeatureKind::Box { .. })).nth(1).expect("a second Box was made");
    let FeatureKind::Box { corner, .. } = &f.kind else { panic!("a Box: {:?}", f.kind) };
    let c = Vec3::new(value(&h, &corner[0]), value(&h, &corner[1]), value(&h, &corner[2]));
    // 20 × 20 × 20 by default, centred over the drop and standing on the top.
    assert!(c.dist(Vec3::new(30.0, 15.0, 10.0)) < 0.5, "the corner: {c:?}");
}

#[test]
fn marker_moves_a_sphere_from_edit_feature() {
    let mut h = plate();
    let made = h.app.run("solid.sphere", json!({"radius": 4, "center": [15, 15, 10], "operation": "join"})).unwrap();
    let name = made["name"].as_str().unwrap().to_string();
    h.settle();
    h.step(&json!({"call": "ui.editFeature", "params": {"feature": name}})).unwrap();
    h.settle();
    let at = h.at(&json!({"handle": "primitive:position"})).unwrap();
    let expect = h.at(&json!({"world": [15, 15, 10]})).unwrap();
    assert!((at[0] - expect[0]).hypot(at[1] - expect[1]) < 2.0, "the marker starts at the stored centre: {at:?} {expect:?}");
    drag_ok(&mut h, [60.0, 35.0, 10.0]);
    let f = h.app.session.doc.find_feature(&name).expect("the Sphere is still there");
    let FeatureKind::Sphere { center, .. } = &f.kind else { panic!("a Sphere: {:?}", f.kind) };
    let c = Vec3::new(value(&h, &center[0]), value(&h, &center[1]), value(&h, &center[2]));
    assert!(c.dist(Vec3::new(60.0, 35.0, 10.0)) < 0.5, "moved: {c:?}");
    h.step(&json!({"expect": {"errors": 0, "bodies": 1}})).unwrap();
}

#[test]
fn marker_starts_at_the_bottom_middle_of_a_box_placed_by_its_centre() {
    let mut h = plate();
    let made = h.app.run("solid.box", json!({"length": 10, "width": 6, "height": 8, "center": [20, 15, 14], "operation": "join"})).unwrap();
    let name = made["name"].as_str().unwrap().to_string();
    h.settle();
    h.step(&json!({"call": "ui.editFeature", "params": {"feature": name}})).unwrap();
    h.settle();
    let at = h.at(&json!({"handle": "primitive:position"})).unwrap();
    let expect = h.at(&json!({"world": [20, 15, 10]})).unwrap();
    assert!((at[0] - expect[0]).hypot(at[1] - expect[1]) < 2.0, "the marker stands under the centre, on the bottom: {at:?} {expect:?}");
}

#[test]
fn a_dialog_with_a_face_input_for_its_place_is_moved_through_that_input() {
    // A Cylinder that stands where its Face input is picked (reference edges, #6380) works its
    // place out from that input every frame, so the marker moves the input, not the place.
    let mut h = plate();
    h.step(&json!({"start": "solid.cylinder"})).unwrap();
    h.settle();
    {
        let d = h.app.dialog.as_mut().expect("a dialog");
        d.inputs.insert(0, crate::selection::SelInput::new("Face", PLANAR_FACES | crate::selection::FACES | crate::selection::VERTICES, false));
        d.active = 0;
    }
    h.frames(2);
    h.step(&json!({"drag": [{"handle": "primitive:position"}, {"world": [40.0, 25.0, 10.0]}], "steps": 12})).unwrap();
    h.settle();
    let d = h.app.dialog.as_ref().expect("the dialog is still open");
    match d.inputs[0].items.as_slice() {
        [Sel::Face { point, .. }] => assert!(point.dist(Vec3::new(40.0, 25.0, 10.0)) < 0.5, "the face place: {point:?}"),
        other => panic!("one face place in the input, not {other:?}"),
    }
    let Kind::Primitive { at, .. } = &d.kind else { panic!("a primitive") };
    assert!(at.is_none(), "the place is the input's to work out, not written directly: {at:?}");
}

#[test]
fn a_coil_with_a_sketch_profile_has_no_marker() {
    let mut h = plate();
    h.step(&json!({"start": "solid.coil"})).unwrap();
    h.settle();
    assert!(h.at(&json!({"handle": "primitive:position"})).is_ok(), "a coil of its own section has the marker");
    {
        let d = h.app.dialog.as_mut().expect("a dialog");
        assert!(!d.inputs.is_empty(), "the Coil dialog has a Profile input");
        d.inputs[0].items.push(Sel::Face { body: "Body1".into(), index: 0, point: Vec3::new(1.0, 1.0, 10.0) });
    }
    h.frames(2);
    assert!(h.at(&json!({"handle": "primitive:position"})).is_err(), "with a profile picked, no marker");
}
