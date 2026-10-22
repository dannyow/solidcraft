//! The hole matrix: every way of placing a hole (on a face, at sketch points; top, bottom and side
//! faces, faces pointing -Z, offset planes, the XY/XZ/YZ planes, curved faces, near edges, several
//! points, mirrored and patterned bodies, neighbouring bodies, thin plates), every hole type
//! (simple, drilled, counterbore, countersink, tapped, clearance) and extent (blind, through all,
//! to a face or point), editing a hole and undoing it, and designs saved by 0.9.0.
//!
//! Each case asserts what a machinist would check: the drill goes into the material (the axis
//! probe was solid before and is empty after), the bore sits on its axis (material just outside
//! its radius), a blind hole stops where it should, the removed volume against its analytic
//! value, the change in face count, and that every body is still one closed, valid solid.
//!
//! Ratchet: cases that fail today are listed in `tests/hole_matrix/known_failures.txt`. A listed
//! case that passes fails the test (take it off the list), and so does an unlisted one that fails.
//! `HOLE_MATRIX_BLESS=1 cargo test -p solidcraft-engine hole_matrix` rewrites the list.

use std::f64::consts::PI;

use serde_json::{Value, json};
use solidcraft_geom::Vec3;

use crate::Session;

const KNOWN_FAILURES: &str = include_str!("../../../tests/hole_matrix/known_failures.txt");

fn run(s: &mut Session, id: &str, p: Value) -> Result<Value, String> {
    s.execute(id, &p).map_err(|e| format!("{id} {p}: {e}"))
}

fn measure(s: &mut Session) -> Result<(f64, i64), String> {
    let m = run(s, "inspect.measure", json!({}))?;
    let v = m["total"]["volume_mm3"].as_f64().ok_or("no volume")?;
    let f = m["total"]["faces"].as_i64().ok_or("no face count")?;
    Ok((v, f))
}

fn solid_at(s: &Session, p: Vec3) -> bool {
    s.model.state().bodies.iter().any(|b| b.body.contains(p))
}

fn v3(a: [f64; 3]) -> Vec3 {
    Vec3::new(a[0], a[1], a[2])
}

/// One drilled spot: where the axis meets the surface, which way it drills (into the material),
/// how far a probe along the axis may go and still be inside the bore, and how deep a blind hole
/// ends (a probe just past it is solid again).
#[derive(Clone, Copy)]
struct Spot {
    entry: [f64; 3],
    dir: [f64; 3],
    probe: f64,
    bottom: Option<f64>,
}

fn spot(entry: [f64; 3], dir: [f64; 3], probe: f64, bottom: Option<f64>) -> Spot {
    Spot { entry, dir, probe, bottom }
}

struct Case {
    name: &'static str,
    /// The part before the hole, as commands.
    setup: Vec<(&'static str, Value)>,
    /// The hole command (or a design file to open instead: see `open`).
    hole: Value,
    /// A design saved by an earlier release to open instead of running `setup` and `hole`; the
    /// volume before the hole is then `start`.
    open: Option<&'static str>,
    start: Option<f64>,
    removed: f64,
    /// The change in the (merged) face count, where it is fixed.
    faces: Option<i64>,
    radius: f64,
    spots: Vec<Spot>,
    /// Commands after the hole (an edit, an undo), and the removed volume after them.
    after: Vec<(&'static str, Value)>,
    removed_after: Option<f64>,
}

impl Case {
    fn new(name: &'static str, setup: Vec<(&'static str, Value)>, hole: Value, removed: f64, radius: f64, spots: Vec<Spot>) -> Case {
        Case { name, setup, hole, open: None, start: None, removed, faces: None, radius, spots, after: Vec::new(), removed_after: None }
    }
    fn faces(mut self, n: i64) -> Case {
        self.faces = Some(n);
        self
    }
    fn then(mut self, cmds: Vec<(&'static str, Value)>, removed: f64) -> Case {
        self.after = cmds;
        self.removed_after = Some(removed);
        self
    }
}

// ---- parts ---------------------------------------------------------------------------------

fn rect_extrude(plane: Value, p0: [f64; 2], p1: [f64; 2], extent: Value, body: &str) -> Vec<(&'static str, Value)> {
    let mut e = json!({"sketch": format!("S{body}"), "body_name": body, "operation": "new"});
    if let (Some(m), Some(x)) = (e.as_object_mut(), extent.as_object()) {
        m.extend(x.clone());
    }
    vec![
        ("sketch.create", json!({"plane": plane, "name": format!("S{body}")})),
        ("sketch.rectangle.two_point", json!({"p0": p0, "p1": p1})),
        ("sketch.finish", json!({})),
        ("solid.extrude", e),
    ]
}

/// A 40 x 30 x 20 block sketched on XY and extruded up (z 0..20): 24000 mm³.
fn block() -> Vec<(&'static str, Value)> {
    rect_extrude(json!("XY"), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20}), "B")
}

fn lifted(z: f64) -> Value {
    json!({"origin": [0, 0, z], "x_dir": [1, 0, 0], "y_dir": [0, 1, 0]})
}

fn points(plane: Value, at: &[[f64; 2]]) -> Vec<(&'static str, Value)> {
    let mut v = vec![("sketch.create", json!({"plane": plane, "name": "P"}))];
    for (i, p) in at.iter().enumerate() {
        v.push(("sketch.point", json!({"point": p, "id": format!("q{i}")})));
    }
    v.push(("sketch.finish", json!({})));
    v
}

fn with(mut a: Vec<(&'static str, Value)>, b: Vec<(&'static str, Value)>) -> Vec<(&'static str, Value)> {
    a.extend(b);
    a
}

fn hole(extra: Value) -> Value {
    let mut p = json!({"diameter": 6, "name": "H"});
    if let (Some(m), Some(x)) = (p.as_object_mut(), extra.as_object()) {
        m.extend(x.clone());
    }
    p
}

fn edit(set: Value) -> (&'static str, Value) {
    ("timeline.edit", json!({"feature": "H", "set": set}))
}

// ---- analytic volumes ----------------------------------------------------------------------

fn cyl(r: f64, h: f64) -> f64 {
    PI * r * r * h
}

/// The drill point of a 118° drill on a hole of radius r.
fn tip(r: f64) -> f64 {
    let h = r / (59.0f64.to_radians()).tan();
    PI * r * r * h / 3.0
}

/// A countersink of top radius `rc`, 90° included, over a hole of radius r: the cone frustum.
fn csink(rc: f64, r: f64) -> (f64, f64) {
    let h = rc - r;
    (PI * h / 3.0 * (rc * rc + rc * r + r * r), h)
}

/// The part of a disc of radius r whose centre is `a` inside a straight edge.
fn disc_inside(r: f64, a: f64) -> f64 {
    PI * r * r - (r * r * (a / r).acos() - a * (r * r - a * a).sqrt())
}

/// The cross-section of a teardrop hole of radius r: the round part plus the two tangent walls at
/// `ang` degrees from the apex direction, less the tip above a flat cap `cap` above the centre.
fn teardrop(r: f64, ang: f64, cap: Option<f64>) -> f64 {
    let a = ang.to_radians();
    let full = r * r * (PI / 2.0 + a + 1.0 / a.tan());
    match cap {
        Some(c) => full - (r / a.sin() - c).powi(2) * a.tan(),
        None => full,
    }
}

/// A radial hole of radius r, `depth` deep, drilled into a cylinder of radius `rr` from its
/// surface: the material between the surface and the bottom, over the bore's disc.
fn radial(rr: f64, r: f64, depth: f64) -> f64 {
    let n = 2000;
    let mut sum = 0.0;
    let h = 2.0 * r / n as f64;
    for i in 0..n {
        let y = -r + (i as f64 + 0.5) * h;
        let w = 2.0 * (r * r - y * y).max(0.0).sqrt();
        sum += w * ((rr * rr - y * y).sqrt() - (rr - depth)) * h;
    }
    sum
}

// ---- the cases -------------------------------------------------------------------------------

const DOWN: [f64; 3] = [0.0, 0.0, -1.0];
const UP: [f64; 3] = [0.0, 0.0, 1.0];

fn cases() -> Vec<Case> {
    let r = 3.0;
    let blind = cyl(r, 10.0);
    let thru = cyl(r, 20.0);
    let mut c = vec![
        // Placement on the faces of a block on XY.
        Case::new(
            "face/top/simple/blind",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/top/simple/through",
            block(),
            hole(json!({"position": [20, 15, 20]})),
            thru,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 20.0, None)],
        )
        .faces(1),
        Case::new(
            "face/bottom/simple/blind",
            block(),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/side+x/simple/blind",
            block(),
            hole(json!({"position": [40, 15, 10], "depth": 10})),
            blind,
            r,
            vec![spot([40.0, 15.0, 10.0], [-1.0, 0.0, 0.0], 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/side-x/simple/blind",
            block(),
            hole(json!({"position": [0, 15, 10], "depth": 10})),
            blind,
            r,
            vec![spot([0.0, 15.0, 10.0], [1.0, 0.0, 0.0], 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/side+y/simple/blind",
            block(),
            hole(json!({"position": [20, 30, 10], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 30.0, 10.0], [0.0, -1.0, 0.0], 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/side-y/simple/through",
            block(),
            hole(json!({"position": [20, 0, 10]})),
            cyl(r, 30.0),
            r,
            vec![spot([20.0, 0.0, 10.0], [0.0, 1.0, 0.0], 30.0, None)],
        )
        .faces(1),
        // A block extruded down from XY (z -20..0): its top lies on XY, its bottom faces -Z.
        Case::new(
            "face/below-xy/top/blind",
            rect_extrude(json!("XY"), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20, "direction": "negative"}), "B"),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/below-xy/bottom-z/blind",
            rect_extrude(json!("XY"), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20, "direction": "negative"}), "B"),
            hole(json!({"position": [20, 15, -20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, -20.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        // A thin plate on XY (150 x 100 x 1.5): both faces lie within the old plane-matching band.
        Case::new(
            "face/thin-plate/bottom/blind",
            rect_extrude(json!("XY"), [0.0, 0.0], [150.0, 100.0], json!({"distance": 1.5}), "B"),
            hole(json!({"position": [50, 50, 0], "depth": 1})),
            cyl(r, 1.0),
            r,
            vec![spot([50.0, 50.0, 0.0], UP, 1.0, Some(1.0))],
        )
        .faces(2),
        Case::new(
            "face/thin-plate/top/blind",
            rect_extrude(json!("XY"), [0.0, 0.0], [150.0, 100.0], json!({"distance": 1.5}), "B"),
            hole(json!({"position": [50, 50, 1.5], "depth": 1})),
            cyl(r, 1.0),
            r,
            vec![spot([50.0, 50.0, 1.5], DOWN, 1.0, Some(1.0))],
        )
        .faces(2),
        // Another body elsewhere with a face in the same plane, facing the other way.
        Case::new(
            "face/neighbour-coplanar/top/blind",
            with(block(), rect_extrude(lifted(20.0), [60.0, 0.0], [80.0, 30.0], json!({"distance": 20}), "N")),
            hole(json!({"position": [20, 15, 20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/neighbour-coplanar/bottom/blind",
            with(block(), rect_extrude(lifted(-20.0), [60.0, 0.0], [80.0, 30.0], json!({"distance": 20}), "N")),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        // On an offset plane (z 10..30).
        Case::new(
            "face/offset-plane/bottom/blind",
            rect_extrude(lifted(10.0), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20}), "B"),
            hole(json!({"position": [20, 15, 10], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 10.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        // Bodies made by mirror and pattern.
        Case::new(
            "face/mirrored-body/bottom-z/blind",
            with(block(), vec![("solid.mirror", json!({"bodies": ["B"], "plane": "XY"}))]),
            hole(json!({"position": [20, 15, -20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, -20.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/mirrored-body/side/blind",
            with(block(), vec![("solid.mirror", json!({"bodies": ["B"], "plane": "XY"}))]),
            hole(json!({"position": [0, 15, -10], "depth": 10})),
            blind,
            r,
            vec![spot([0.0, 15.0, -10.0], [1.0, 0.0, 0.0], 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/patterned-body/bottom/blind",
            with(block(), vec![("solid.pattern.rectangular", json!({"bodies": ["B"], "dir1": [0, 0, 1], "count1": 2, "spacing1": 50}))]),
            hole(json!({"position": [20, 15, 50], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 50.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "face/patterned-body/top/blind",
            with(block(), vec![("solid.pattern.rectangular", json!({"bodies": ["B"], "dir1": [0, 0, 1], "count1": 2, "spacing1": 50}))]),
            hole(json!({"position": [20, 15, 70], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 70.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        // Near an edge: the bore breaks out of the side (a 10 mm hole 3 mm in from x = 0).
        Case::new(
            "face/near-edge/through",
            block(),
            json!({"position": [3, 15, 20], "diameter": 10, "name": "H"}),
            disc_inside(5.0, 3.0) * 20.0,
            5.0,
            vec![spot([3.0, 15.0, 20.0], DOWN, 20.0, None)],
        ),
        // On a curved face: radial, into the material (a 20 mm round bar, 40 long, on XY).
        Case::new(
            "face/curved/radial/blind",
            vec![("solid.cylinder", json!({"radius": 10, "height": 40, "name": "Bar"}))],
            json!({"position": [10, 0, 20], "diameter": 4, "depth": 5, "name": "H"}),
            radial(10.0, 2.0, 5.0),
            2.0,
            vec![spot([10.0, 0.0, 20.0], [-1.0, 0.0, 0.0], 5.0, Some(5.0))],
        ),
        Case::new(
            "face/curved/radial-direction-given/blind",
            vec![("solid.cylinder", json!({"radius": 10, "height": 40, "name": "Bar"}))],
            json!({"position": [10, 0, 20], "direction": [-1, 0, 0], "diameter": 4, "depth": 5, "name": "H"}),
            radial(10.0, 2.0, 5.0),
            2.0,
            vec![spot([10.0, 0.0, 20.0], [-1.0, 0.0, 0.0], 5.0, Some(5.0))],
        ),
        // Hole types.
        Case::new(
            "type/drilled/blind",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10, "type": "drilled"})),
            blind + tip(r),
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0 + r / 59f64.to_radians().tan()))],
        )
        .faces(2),
        Case::new(
            "type/counterbore/blind",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10, "type": "counterbore", "cb_diameter": 12, "cb_depth": 3})),
            cyl(6.0, 3.0) + cyl(r, 7.0),
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(4),
        Case::new(
            "type/counterbore/through",
            block(),
            hole(json!({"position": [20, 15, 20], "type": "counterbore", "cb_diameter": 12, "cb_depth": 3})),
            cyl(6.0, 3.0) + cyl(r, 17.0),
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 20.0, None)],
        )
        .faces(3),
        Case::new(
            "type/countersink/blind",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10, "type": "countersink", "cs_diameter": 12})),
            csink(6.0, r).0 + cyl(r, 10.0 - csink(6.0, r).1),
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(3),
        Case::new(
            "type/countersink/through-bottom",
            block(),
            hole(json!({"position": [20, 15, 0], "type": "countersink", "cs_diameter": 12})),
            csink(6.0, r).0 + cyl(r, 20.0 - csink(6.0, r).1),
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 20.0, None)],
        )
        .faces(2),
        // A teardrop: a hole across the build direction (apex +Z) in the +x side face, pointed
        // and cut flat at the radius (never shorter than nominal).
        Case::new(
            "type/teardrop/through",
            block(),
            hole(json!({"position": [40, 15, 10], "type": "teardrop", "angle": "45 deg"})),
            teardrop(r, 45.0, None) * 40.0,
            r,
            vec![spot([40.0, 15.0, 10.0], [-1.0, 0.0, 0.0], 40.0, None)],
        )
        .faces(3),
        Case::new(
            "type/teardrop-capped/blind",
            block(),
            hole(json!({"position": [40, 15, 10], "type": "teardrop", "depth": 10, "cap": 3, "apex": [0, 0, 1]})),
            teardrop(r, 45.0, Some(3.0)) * 10.0,
            r,
            vec![spot([40.0, 15.0, 10.0], [-1.0, 0.0, 0.0], 10.0, Some(10.0))],
        )
        .faces(5),
        Case::new(
            "type/tapped-m6/blind",
            block(),
            json!({"position": [20, 15, 20], "thread": "M6", "depth": 10, "name": "H"}),
            cyl(2.5, 10.0),
            2.5,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "type/clearance-m6/through",
            block(),
            json!({"position": [20, 15, 20], "clearance": "M6", "name": "H"}),
            cyl(3.3, 20.0),
            3.3,
            vec![spot([20.0, 15.0, 20.0], DOWN, 20.0, None)],
        )
        .faces(1),
        // Extents: to an object.
        Case::new(
            "extent/to-face/bottom",
            block(),
            hole(json!({"position": [20, 15, 20], "to": [35, 25, 0]})),
            thru,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 20.0, None)],
        )
        .faces(1),
        Case::new(
            "extent/to-point",
            block(),
            hole(json!({"position": [20, 15, 20], "to": [5, 5, 8]})),
            cyl(r, 12.0),
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 12.0, Some(12.0))],
        )
        .faces(2),
        // At sketch points.
        Case::new(
            "points/top-face/blind/three",
            with(block(), points(lifted(20.0), &[[8.0, 8.0], [20.0, 15.0], [32.0, 22.0]])),
            hole(json!({"sketch": "P", "points": ["q0", "q1", "q2"], "depth": 10})),
            3.0 * blind,
            r,
            vec![
                spot([8.0, 8.0, 20.0], DOWN, 10.0, Some(10.0)),
                spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0)),
                spot([32.0, 22.0, 20.0], DOWN, 10.0, Some(10.0)),
            ],
        )
        .faces(6),
        Case::new(
            "points/xy-under-part/blind",
            with(block(), points(json!("XY"), &[[10.0, 10.0], [30.0, 20.0]])),
            hole(json!({"sketch": "P", "points": ["q0", "q1"], "depth": 10})),
            2.0 * blind,
            r,
            vec![spot([10.0, 10.0, 0.0], UP, 10.0, Some(10.0)), spot([30.0, 20.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .faces(4),
        Case::new(
            "points/xy-under-part/through",
            with(block(), points(json!("XY"), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"]})),
            thru,
            r,
            vec![spot([10.0, 10.0, 0.0], UP, 20.0, None)],
        )
        .faces(1),
        Case::new(
            "points/below-part-air-gap/blind",
            with(block(), points(lifted(-5.0), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 15})),
            cyl(r, 15.0),
            r,
            vec![spot([10.0, 10.0, 0.0], UP, 15.0, Some(15.0))],
        )
        .faces(2),
        // Inside the part the holes go down from the top; Flip drills up from the bottom.
        Case::new(
            "points/mid-plane/blind",
            with(block(), points(lifted(10.0), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 5})),
            cyl(r, 5.0),
            r,
            vec![spot([10.0, 10.0, 20.0], DOWN, 5.0, Some(5.0))],
        )
        .faces(2),
        Case::new(
            "points/mid-plane/flip",
            with(block(), points(lifted(10.0), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 5, "flip": true})),
            cyl(r, 5.0),
            r,
            vec![spot([10.0, 10.0, 0.0], UP, 5.0, Some(5.0))],
        )
        .faces(2)
        // Saved and opened again, it still drills up.
        .then(Vec::new(), cyl(r, 5.0)),
        Case::new(
            "points/xy-over-part-below-xy/blind",
            with(
                rect_extrude(json!("XY"), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20, "direction": "negative"}), "B"),
                points(json!("XY"), &[[10.0, 10.0]]),
            ),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 10})),
            blind,
            r,
            vec![spot([10.0, 10.0, 0.0], DOWN, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "points/offset-plane/blind",
            with(rect_extrude(lifted(10.0), [0.0, 0.0], [40.0, 30.0], json!({"distance": 20}), "B"), points(lifted(10.0), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 10})),
            blind,
            r,
            vec![spot([10.0, 10.0, 10.0], UP, 10.0, Some(10.0))],
        )
        .faces(2),
        Case::new(
            "points/xz-plane/blind",
            with(rect_extrude(json!("XZ"), [0.0, 0.0], [40.0, 20.0], json!({"distance": 30}), "B"), points(json!("XZ"), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 10})),
            blind,
            r,
            Vec::new(),
        )
        .faces(2),
        Case::new(
            "points/yz-plane/blind",
            with(rect_extrude(json!("YZ"), [0.0, 0.0], [30.0, 20.0], json!({"distance": 40}), "B"), points(json!("YZ"), &[[10.0, 10.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 10})),
            blind,
            r,
            Vec::new(),
        )
        .faces(2),
        Case::new(
            "points/counterbore-from-bottom",
            with(block(), points(json!("XY"), &[[20.0, 15.0]])),
            hole(json!({"sketch": "P", "points": ["q0"], "depth": 10, "type": "counterbore", "cb_diameter": 12, "cb_depth": 3})),
            cyl(6.0, 3.0) + cyl(r, 7.0),
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .faces(4),
        // Editing a hole, then rebuilding; undoing it.
        Case::new(
            "edit/diameter",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .then(vec![edit(json!({"diameter": "8"}))], cyl(4.0, 10.0)),
        Case::new(
            "edit/depth",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .then(vec![edit(json!({"depth": "15"}))], cyl(r, 15.0)),
        Case::new(
            "edit/to-through-all",
            block(),
            hole(json!({"position": [20, 15, 20], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))],
        )
        .then(vec![edit(json!({"depth": null}))], thru),
        Case::new(
            "edit/type-counterbore",
            block(),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .then(vec![edit(json!({"hole": {"type": "counterbore", "cb_diameter": "12", "cb_depth": "3"}}))], cyl(6.0, 3.0) + cyl(r, 7.0)),
        Case::new(
            "edit/part-grows",
            block(),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .then(vec![("timeline.edit", json!({"feature": "Extrude1", "set": {"extent": {"distance": "30"}}}))], blind),
        Case::new(
            "undo",
            block(),
            hole(json!({"position": [20, 15, 0], "depth": 10})),
            blind,
            r,
            vec![spot([20.0, 15.0, 0.0], UP, 10.0, Some(10.0))],
        )
        .then(vec![("edit.undo", json!({}))], 0.0),
    ];
    // Designs saved by 0.9.0 (format 17), opened as they are.
    let old = |name, file, start, removed, sp: Spot| Case {
        name,
        setup: Vec::new(),
        hole: Value::Null,
        open: Some(file),
        start: Some(start),
        removed,
        faces: None,
        radius: r,
        spots: vec![sp],
        after: Vec::new(),
        removed_after: None,
    };
    c.push(old("old-0.9/top/blind", "top_blind", 24_000.0, blind, spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))));
    c.push(old("old-0.9/points-top/blind", "points_top_blind", 24_000.0, 2.0 * blind, spot([10.0, 10.0, 20.0], DOWN, 10.0, Some(10.0))));
    c.push(old("old-0.9/mirrored/bottom", "mirror_bottom_blind", 48_000.0, blind, spot([20.0, 15.0, -20.0], UP, 10.0, Some(10.0))));
    c.push(old("old-0.9/neighbour-coplanar/top", "neighbour_top_blind", 36_000.0, blind, spot([20.0, 15.0, 20.0], DOWN, 10.0, Some(10.0))));
    c.push(old("old-0.9/thin-plate/bottom", "thin_plate_bottom_blind", 22_500.0, cyl(r, 1.0), spot([50.0, 50.0, 0.0], UP, 1.0, Some(1.0))));
    c
}

fn old_design(name: &str) -> Option<&'static str> {
    Some(match name {
        "top_blind" => include_str!("../../../tests/hole_matrix/old-0.9/top_blind.json"),
        "points_top_blind" => include_str!("../../../tests/hole_matrix/old-0.9/points_top_blind.json"),
        "mirror_bottom_blind" => include_str!("../../../tests/hole_matrix/old-0.9/mirror_bottom_blind.json"),
        "neighbour_top_blind" => include_str!("../../../tests/hole_matrix/old-0.9/neighbour_top_blind.json"),
        "thin_plate_bottom_blind" => include_str!("../../../tests/hole_matrix/old-0.9/thin_plate_bottom_blind.json"),
        _ => return None,
    })
}

/// The volume tolerance: half a percent of the removed volume, at least 0.3 mm³, and at least
/// 1e-4 of the part (curved faces are measured on their mesh, before and after).
fn close(a: f64, b: f64, part: f64) -> bool {
    (a - b).abs() <= (b.abs() * 0.005).max(0.3).max(part.abs() * 1e-4)
}

fn check(case: &Case) -> Result<(), String> {
    let (mut s, start, faces0) = match case.open {
        Some(file) => {
            let text = old_design(file).ok_or("no such design")?;
            let doc = solidcraft_doc::Document::from_json(text).map_err(|e| format!("open: {e}"))?;
            let s = Session::new(doc);
            (s, case.start.unwrap_or_default(), None)
        }
        None => {
            let mut s = Session::default();
            for (id, p) in &case.setup {
                run(&mut s, id, p.clone())?;
            }
            let (v, f) = measure(&mut s)?;
            (s, v, Some(f))
        }
    };
    // Before the hole: every probe along an axis is in the material (the drill goes into it).
    let probes: Vec<(Vec3, Vec3, Spot)> = case
        .spots
        .iter()
        .map(|sp| {
            let (e, d) = (v3(sp.entry), v3(sp.dir));
            (e, d, *sp)
        })
        .collect();
    if case.open.is_none() {
        for (e, d, sp) in &probes {
            let mid = *e + *d * (sp.probe * 0.5);
            if !solid_at(&s, mid) {
                return Err(format!("case setup: no material at {mid:?} before the hole"));
            }
        }
        run(&mut s, "solid.hole", case.hole.clone())?;
    }
    if let Some(id) = s
        .doc
        .find_feature("H")
        .map(|f| f.id)
        .or_else(|| s.doc.features.iter().rev().find(|f| matches!(f.kind, solidcraft_doc::FeatureKind::Hole { .. })).map(|f| f.id))
        && let Some(r) = s.model.result(id)
        && let Some(e) = &r.error
    {
        return Err(format!("the hole failed: {e}"));
    }
    let (v, f) = measure(&mut s)?;
    let removed = start - v;
    if !close(removed, case.removed, start) {
        return Err(format!("removed {removed:.2} mm³, expected {:.2}", case.removed));
    }
    if let (Some(n), Some(f0)) = (case.faces, faces0)
        && f - f0 != n
    {
        return Err(format!("faces {f0} -> {f} (+{}), expected +{n}", f - f0));
    }
    for b in &s.model.state().bodies {
        let bad = b.body.validity();
        if !bad.is_empty() || b.body.shell_count() != 1 {
            return Err(format!("body {} is not one closed valid solid: {bad:?}, {} shells", b.name, b.body.shell_count()));
        }
    }
    for (e, d, sp) in &probes {
        let mid = *e + *d * (sp.probe * 0.5);
        if solid_at(&s, mid) {
            return Err(format!("the bore is not at {mid:?} (drilled the wrong way or off its axis)"));
        }
        let side = d.any_perp() * (case.radius + 0.4);
        if !solid_at(&s, mid + side) && !solid_at(&s, mid - side) {
            return Err(format!("no material just outside the bore's radius at {mid:?}: the hole is too wide or off its axis"));
        }
        if let Some(b) = sp.bottom {
            let past = *e + *d * (b + 0.4);
            if !solid_at(&s, past) {
                return Err(format!("the blind hole goes past {b} mm (no material at {past:?})"));
            }
        }
    }
    for (id, p) in &case.after {
        run(&mut s, id, p.clone())?;
    }
    if let Some(after) = case.removed_after {
        let (v, _) = measure(&mut s)?;
        let start = if case.name == "edit/part-grows" { 36_000.0 } else { start };
        if !close(start - v, after, start) {
            return Err(format!("after {:?}: removed {:.2} mm³, expected {after:.2}", case.after.iter().map(|x| x.0).collect::<Vec<_>>(), start - v));
        }
        // Saved and opened again, it rebuilds the same.
        let doc = solidcraft_doc::Document::from_json(&s.doc.to_json()).map_err(|e| format!("reopen: {e}"))?;
        let mut again = Session::new(doc);
        let (v2, _) = measure(&mut again)?;
        if !close(start - v2, after, start) {
            return Err(format!("reopened: removed {:.2} mm³, expected {after:.2}", start - v2));
        }
    }
    Ok(())
}

#[test]
fn hole_matrix() {
    let known: Vec<&str> = KNOWN_FAILURES.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let mut failing = Vec::new();
    let mut report = Vec::new();
    for case in cases() {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&case))).unwrap_or_else(|_| Err("panicked".into()));
        if let Err(e) = &r {
            failing.push(case.name);
            report.push(format!("{}: {e}", case.name));
        }
    }
    if std::env::var_os("HOLE_MATRIX_BLESS").is_some() {
        let mut text = String::from("# Hole matrix cases that fail today (crates/engine/src/hole_matrix_tests.rs). A fix takes its case off.\n");
        for f in &failing {
            text.push_str(f);
            text.push('\n');
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/hole_matrix/known_failures.txt");
        std::fs::write(path, text).ok();
        eprintln!("{}", report.join("\n"));
        return;
    }
    let names: Vec<&str> = cases().iter().map(|c| c.name).collect();
    let stale: Vec<&&str> = known.iter().filter(|k| !names.contains(k)).collect();
    let fixed: Vec<&&str> = known.iter().filter(|k| names.contains(k) && !failing.contains(k)).collect();
    let broken: Vec<String> = report.iter().filter(|r| !known.iter().any(|k| r.starts_with(&format!("{k}: ")))).cloned().collect();
    assert!(
        broken.is_empty() && fixed.is_empty() && stale.is_empty(),
        "hole matrix:\nnewly failing:\n{}\npassing now (take them off tests/hole_matrix/known_failures.txt): {fixed:?}\nlisted but unknown: {stale:?}\n(all failures: {})",
        broken.join("\n"),
        report.join(" | ")
    );
}

/// A teardrop prints only across its apex: along it (within 10°) the hole stays round, with a
/// warning; refused are an overhang the printer cannot do and a cap that would shorten the hole
/// or miss the peak. Edited back to a plain hole, the teardrop is gone; saved and reopened, it is
/// still a teardrop.
#[test]
fn a_teardrop_hole_is_refused_where_it_cannot_print() {
    let mut s = Session::default();
    for (id, p) in block() {
        run(&mut s, id, p).unwrap();
    }
    let td = |extra: Value| {
        let mut p = json!({"position": [40, 15, 10], "type": "teardrop", "diameter": 6, "name": "H"});
        if let (Some(m), Some(x)) = (p.as_object_mut(), extra.as_object()) {
            m.extend(x.clone());
        }
        p
    };
    // Into the top face the hole runs along Z, the apex: round, with a warning. Tilted 5° it
    // still counts as along; tilted 20° it is a teardrop.
    let (v0, _) = measure(&mut s).unwrap();
    for (what, dir, round) in [
        ("straight down", [0.0f64, 0.0, -1.0], true),
        ("tilted 5°", [0.0871557, 0.0, -0.9961947], true),
        ("tilted 20°", [0.3420201, 0.0, -0.9396926], false),
    ] {
        let v = run(&mut s, "solid.hole", td(json!({"position": [20, 15, 20], "direction": dir, "name": "V"}))).unwrap();
        let said = v["warning"].as_str().unwrap_or_default().contains("stays round");
        assert_eq!(said, round, "{what}: warning {:?}", v["warning"]);
        let got = v0 - measure(&mut s).unwrap().0;
        let bore = cyl(3.0, 20.0 / dir[2].abs());
        assert_eq!(got < bore * 1.03, round, "{what}: removed {got} against a plain bore of {bore}");
        run(&mut s, "timeline.delete", json!({"feature": "V"})).unwrap();
    }
    for (what, extra, says) in [
        ("a 0° overhang", json!({"angle": "0 deg"}), "between 0° and 90°"),
        ("a 90° overhang", json!({"angle": "90 deg"}), "between 0° and 90°"),
        ("a cap below the radius", json!({"cap": 2}), "at least the hole's radius"),
        ("a cap above the peak", json!({"cap": 5}), "below the peak"),
        ("a zero apex", json!({"apex": [0, 0, 0]}), "non-zero"),
    ] {
        let e = s.execute("solid.hole", &td(extra)).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains(says), "{what}: {e:?} should say {says:?}");
        assert!(s.doc.features.iter().all(|f| f.name != "H"), "{what}: the refused hole is not kept");
    }
    // A capped teardrop, then edited to a plain hole and back, and reopened from its JSON.
    let (v0, _) = measure(&mut s).unwrap();
    run(&mut s, "solid.hole", td(json!({"cap": 3, "apex": [0, 0, 1]}))).unwrap();
    let (v1, _) = measure(&mut s).unwrap();
    assert!(close(v0 - v1, teardrop(3.0, 45.0, Some(3.0)) * 40.0, v0), "capped teardrop through: {}", v0 - v1);
    let (id, p) = edit(json!({"hole": {"type": "simple"}}));
    run(&mut s, id, p).unwrap();
    let (v2, _) = measure(&mut s).unwrap();
    assert!(close(v0 - v2, cyl(3.0, 40.0), v0), "edited to a plain hole: {}", v0 - v2);
    let (id, p) = edit(json!({"hole": {"type": "teardrop", "apex": [0, 0, 1], "angle": "45 deg"}}));
    run(&mut s, id, p).unwrap();
    let (v3, _) = measure(&mut s).unwrap();
    assert!(close(v0 - v3, teardrop(3.0, 45.0, None) * 40.0, v0), "edited back to a pointed teardrop: {}", v0 - v3);
    let doc = solidcraft_doc::Document::from_json(&s.doc.to_json()).unwrap();
    let kind = doc.features.iter().find(|f| f.name == "H").map(|f| &f.kind).unwrap();
    assert!(matches!(kind, solidcraft_doc::FeatureKind::Hole { hole: solidcraft_doc::HoleKind::Teardrop { cap: None, .. }, .. }), "{kind:?}");
}

/// A Teardrop over a round hole made any other way: the bore keeps its diameter and depth and
/// gets the peak. Through a plate it clears both rims; over a blind hole it stops at the floor,
/// over a drilled one at the drill point. It refuses a shaft, a hole along the apex and a point
/// off every round hole. Edited to a flat peak it is capped; reopened from JSON it is still a
/// teardrop. Over a circle cut whose diameter is a parameter, it follows the parameter.
#[test]
fn a_teardrop_over_an_existing_hole_keeps_its_bore_and_follows_it() {
    let fresh = || {
        let mut s = Session::default();
        for (id, p) in block() {
            run(&mut s, id, p).unwrap();
        }
        let (v0, _) = measure(&mut s).unwrap();
        (s, v0)
    };
    let slivers = |r: f64, cap: Option<f64>| teardrop(r, 45.0, cap) - PI * r * r;
    let removed = |s: &mut Session, before: f64| before - measure(s).unwrap().0;

    // Through the +x face (axis x, apex +Z): only the peak's slivers go, the whole length.
    let (mut s, v0) = fresh();
    run(&mut s, "solid.hole", hole(json!({"position": [40, 15, 10]}))).unwrap();
    let (v1, _) = measure(&mut s).unwrap();
    run(&mut s, "solid.teardrop", json!({"face": [30, 12, 10], "name": "T"})).unwrap();
    assert!(close(removed(&mut s, v1), slivers(3.0, None) * 40.0, v0), "through: {}", removed(&mut s, v1));
    assert!(!solid_at(&s, v3([20.0, 15.0, 13.8])) && solid_at(&s, v3([20.0, 15.0, 14.5])), "the peak reaches r / sin 45° = 4.24 above the centre");
    assert!(!solid_at(&s, v3([20.0, 15.0, 10.0])) && solid_at(&s, v3([20.0, 15.0, 6.5])), "the bore is still open and the material below it stays");
    // Edited to a flat peak 3 mm above the centre; saved and reopened, still a teardrop.
    run(&mut s, "timeline.edit", json!({"feature": "T", "set": {"cap": "3 mm"}})).unwrap();
    assert!(close(removed(&mut s, v1), slivers(3.0, Some(3.0)) * 40.0, v0), "capped: {}", removed(&mut s, v1));
    let doc = solidcraft_doc::Document::from_json(&s.doc.to_json()).unwrap();
    let kind = doc.features.iter().find(|f| f.name == "T").map(|f| &f.kind).unwrap();
    assert!(matches!(kind, solidcraft_doc::FeatureKind::Teardrop { cap: Some(_), .. }), "{kind:?}");

    // Blind with a flat bottom, 10 deep: the slivers stop short of the floor, which stays.
    // Drilled (a 118° point): they run the cylindrical part only and the point stays.
    for (what, kind, past) in [("blind", json!({}), [29.0, 15.0, 10.0]), ("drilled", json!({"type": "drilled"}), [28.5, 15.0, 10.0])] {
        let (mut s, _) = fresh();
        let mut h = hole(json!({"position": [40, 15, 10], "depth": 10}));
        if let (Some(m), Some(x)) = (h.as_object_mut(), kind.as_object()) {
            m.extend(x.clone());
        }
        run(&mut s, "solid.hole", h).unwrap();
        let (v1, _) = measure(&mut s).unwrap();
        run(&mut s, "solid.teardrop", json!({"face": [35, 12, 10], "name": "T"})).unwrap();
        let got = removed(&mut s, v1);
        let (lo, hi) = (slivers(3.0, None) * 9.5, slivers(3.0, None) * 10.0 + 0.1);
        assert!(got > lo && got < hi, "{what}: {got} is not in {lo}..{hi}");
        assert!(solid_at(&s, v3([29.8, 15.0, 13.5])) && !solid_at(&s, v3([30.5, 15.0, 13.5])), "{what}: the slivers stop just short of the bottom");
        assert!(!solid_at(&s, v3([30.2, 15.0, 10.0])), "{what}: the bore is still open");
        assert_eq!(solid_at(&s, v3(past)), what == "blind", "{what}: the floor stays, the drill point stays");
    }

    // Refused: a shaft and a point off every round hole; neither is kept. A hole along the apex
    // is left as it is, with a warning; with the apex across it, it gets its teardrop.
    let (mut s, _) = fresh();
    run(&mut s, "solid.cylinder", json!({"radius": 5, "height": 20, "base": [60, 15, 0], "name": "Post"})).unwrap();
    run(&mut s, "solid.hole", hole(json!({"position": [20, 15, 20]}))).unwrap();
    for (what, p, says) in
        [("a shaft", json!({"face": [65, 15, 10]}), "shaft"), ("a point in the air", json!({"face": [100, 100, 100]}), "no cylindrical face")]
    {
        let mut p = p;
        p["name"] = json!("T");
        let e = s.execute("solid.teardrop", &p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains(says), "{what}: {e:?} should say {says:?}");
        assert!(s.doc.features.iter().all(|f| f.name != "T"), "{what}: the refused teardrop is not kept");
    }
    let (v1, faces1) = measure(&mut s).unwrap();
    for (what, apex) in [("the default apex", json!(null)), ("a given apex", json!([0, 0, -1]))] {
        let mut p = json!({"face": [23, 15, 10], "name": "N"});
        if !apex.is_null() {
            p["apex"] = apex;
        }
        let v = run(&mut s, "solid.teardrop", p).unwrap();
        assert!(v["warning"].as_str().unwrap_or_default().contains("nothing is cut"), "{what}: {:?}", v["warning"]);
        assert_eq!(measure(&mut s).unwrap(), (v1, faces1), "{what}: the hole is as it was");
        run(&mut s, "timeline.delete", json!({"feature": "N"})).unwrap();
    }
    let v = run(&mut s, "solid.teardrop", json!({"face": [23, 15, 10], "apex": [0, 1, 0], "name": "T"})).unwrap();
    assert!(v["warning"].is_null(), "across the hole: {:?}", v["warning"]);
    assert!(close(v1 - measure(&mut s).unwrap().0, slivers(3.0, None) * 20.0, v1), "across the hole: {}", v1 - measure(&mut s).unwrap().0);

    // A circle cut from the YZ plane, its diameter the parameter `d` and its centre `y`: the
    // teardrop over it follows both, with no complaint from the feature. The move (7 mm, past
    // the nearest-face fallback's reach of 5 % of the body) is found by the face's history name,
    // which the pick recorded.
    let (mut s, v0) = fresh();
    run(&mut s, "parameters.add", json!({"name": "d", "expression": "6 mm"})).unwrap();
    run(&mut s, "parameters.add", json!({"name": "y", "expression": "15 mm"})).unwrap();
    run(&mut s, "sketch.create", json!({"plane": "YZ", "name": "C"})).unwrap();
    run(&mut s, "sketch.circle.center", json!({"center": [15, 10], "radius": 3, "id": "c"})).unwrap();
    run(&mut s, "sketch.dimension", json!({"entities": ["c"], "type": "diameter", "value": "d"})).unwrap();
    run(&mut s, "sketch.dimension", json!({"entities": ["origin", "c.center"], "type": "horizontal", "value": "y"})).unwrap();
    run(&mut s, "sketch.dimension", json!({"entities": ["origin", "c.center"], "type": "vertical", "value": "10 mm"})).unwrap();
    run(&mut s, "sketch.finish", json!({})).unwrap();
    run(&mut s, "solid.extrude", json!({"sketch": "C", "distance": 40, "operation": "cut", "name": "Bore"})).unwrap();
    let (v1, _) = measure(&mut s).unwrap();
    assert!(close(v0 - v1, cyl(3.0, 40.0), v0), "the circle cut: {}", v0 - v1);
    run(&mut s, "solid.teardrop", json!({"face": [20, 12, 10], "name": "T"})).unwrap();
    assert!(close(removed(&mut s, v1), slivers(3.0, None) * 40.0, v0), "over the cut: {}", removed(&mut s, v1));
    let t = s.doc.features.iter().find(|f| f.name == "T").cloned().unwrap();
    assert!(t.face_names.iter().any(|n| !n.is_empty()), "the pick recorded the bore's history name: {:?}", t.face_names);
    let fine = |s: &Session, what: &str| {
        let r = s.model.result(t.id).cloned().unwrap();
        assert!(r.error.is_none() && r.warning.is_none(), "{what}: {:?} {:?}", r.error, r.warning);
    };
    run(&mut s, "parameters.change", json!({"name": "d", "expression": "8 mm"})).unwrap();
    fine(&s, "after the resize");
    assert!(close(v0 - measure(&mut s).unwrap().0, teardrop(4.0, 45.0, None) * 40.0, v0), "a Ø8 teardrop: {}", v0 - measure(&mut s).unwrap().0);
    run(&mut s, "parameters.change", json!({"name": "y", "expression": "22 mm"})).unwrap();
    fine(&s, "after the move");
    assert!(close(v0 - measure(&mut s).unwrap().0, teardrop(4.0, 45.0, None) * 40.0, v0), "moved to y = 22: {}", v0 - measure(&mut s).unwrap().0);
    assert!(!solid_at(&s, v3([20.0, 22.0, 15.0])) && solid_at(&s, v3([20.0, 15.0, 10.0])), "the teardrop is at the new place, the old one is gone");
}
