//! Box, Cylinder, Sphere, Torus and Coil: the marker places the primitive on the face under the
//! pointer, standing on it as a pre-selected face places it. These dialogs have no input for the
//! place (Coil's Profile input picks its section, not where it goes), so today the place is only
//! what was selected before the dialog opened; with the handles on, the marker re-places it while
//! the dialog is open, from Edit Feature too. The primitives do not turn in their face: no knob.
//!
//! The marker sits at the dialog's picked place when there is one, else at the stored position
//! (Edit Feature: the centre, the base, or the middle of the box's bottom face), else where the
//! command puts a primitive with none (the origin).
//!
//! A Coil with a sketch profile has no marker: its place is the profile's, and the base and axis
//! the dialog would send from a picked place move the axis but not the section.

use serde_json::Value;
use solidcraft_engine::doc::expr::Kind as ValueKind;
use solidcraft_engine::geom::Vec3;

use super::{PlaceSource, Spec, circle, face_pick, len, rect, vec3_of};
use crate::SolveApp;
use crate::dialogs::{Dialog, Kind};
use crate::selection::PLANAR_FACES;
use crate::viewport::Hit;

pub(super) static SPEC: Spec = Spec {
    name: "primitive",
    serves: |app, d| super::on(app) && matches!(d.kind, Kind::Primitive { .. }) && !coil_with_profile(d),
    snap_commands: &[],
    move_tip: "Drag to place it on another face.",
    many: false,
    place: PlaceSource::Own { places: |app, d| super::one(frame(app, d)), move_to },
    click: None,
    footprint,
    freedom: super::anywhere,
    turn: None,
};

/// Is this a Coil whose section comes from a sketch (its Profile input holds something)?
fn coil_with_profile(d: &Dialog) -> bool {
    matches!(&d.kind, Kind::Primitive { cmd: "solid.coil", .. }) && d.inputs.iter().any(|i| !i.items.is_empty())
}

/// A size field's value (mm), zero when it is missing or does not evaluate.
fn size(app: &SolveApp, d: &Dialog, key: &str) -> f64 {
    let Kind::Primitive { fields, .. } = &d.kind else { return 0.0 };
    fields.iter().find(|f| f.0 == key).map(|f| len(app, &f.1)).unwrap_or(0.0)
}

/// The radius, from whichever of `radius` (Edit Feature) or `diameter` (a new one) the dialog holds.
fn radius(app: &SolveApp, d: &Dialog) -> f64 {
    let Kind::Primitive { fields, .. } = &d.kind else { return 0.0 };
    if fields.iter().any(|f| f.0 == "radius") { size(app, d, "radius") } else { size(app, d, "diameter") * 0.5 }
}

/// A stored point of expressions (numbers, or strings in the design's terms).
fn expr_point(app: &SolveApp, v: &Value) -> Option<Vec3> {
    let c = |i: usize| -> Option<f64> {
        let x = v.as_array()?.get(i)?;
        x.as_f64().or_else(|| app.session.doc.eval(x.as_str()?, ValueKind::Length).ok()).filter(|v| v.is_finite())
    };
    Some(Vec3::new(c(0)?, c(1)?, c(2)?))
}

fn frame(app: &SolveApp, d: &Dialog) -> Option<(Vec3, Vec3)> {
    let Kind::Primitive { cmd, at, .. } = &d.kind else { return None };
    if let Some((p, n)) = at {
        return Some((*p, n.unwrap_or(Vec3::Z)));
    }
    match *cmd {
        "solid.box" => {
            // The middle of the bottom face: from the corner, or from the box's centre.
            if let Some(centre) = d.extra.get("center").and_then(|v| expr_point(app, v)) {
                return Some((centre - Vec3::Z * (size(app, d, "height") * 0.5), Vec3::Z));
            }
            let corner = d.extra.get("corner").and_then(|v| expr_point(app, v)).unwrap_or(Vec3::ZERO);
            Some((corner + Vec3::new(size(app, d, "length") * 0.5, size(app, d, "width") * 0.5, 0.0), Vec3::Z))
        }
        "solid.cylinder" | "solid.coil" => {
            let base = d.extra.get("base").and_then(|v| expr_point(app, v)).unwrap_or(Vec3::ZERO);
            let axis = d.extra.get("axis").and_then(vec3_of).and_then(|a| a.normalized()).unwrap_or(Vec3::Z);
            Some((base, axis))
        }
        _ => Some((d.extra.get("center").and_then(|v| expr_point(app, v)).unwrap_or(Vec3::ZERO), Vec3::Z)),
    }
}

/// Stand the primitive on the face under the pointer, as a pre-selected face places it. A dialog
/// whose first input takes a face as the place (a Cylinder's, once it stands by its reference
/// edges) is moved through that input, as a Rest is, and works its place out from it itself.
fn move_to(app: &SolveApp, d: &mut Dialog, hits: &[Hit], _: usize) -> bool {
    let s = &app.session;
    if d.inputs.first().is_some_and(|i| i.accept & PLANAR_FACES != 0) {
        let Some(to) = face_pick(d, s, hits) else { return false };
        if let Some(inp) = d.inputs.first_mut() {
            inp.items = vec![to];
        }
        d.error = None;
        return true;
    }
    let Some((p, n)) = hits.iter().find_map(|h| match h {
        Hit::Face { body, index, point } => Some((*point, crate::dialogs::planar_face(s, body, *index).map(|(_, n)| n))),
        _ => None,
    }) else {
        return false;
    };
    let Kind::Primitive { at, .. } = &mut d.kind else { return false };
    *at = Some((p, n));
    d.error = None;
    true
}

/// The primitive's outline on the face: the box's side that stands on it (a face square to an
/// axis), the cylinder's or coil's base circle, the sphere's or torus's widest circle.
fn footprint(app: &SolveApp, d: &Dialog, base: Vec3, n: Vec3) -> Vec<Vec3> {
    let Kind::Primitive { cmd, .. } = &d.kind else { return Vec::new() };
    match *cmd {
        "solid.box" => {
            let (l, w, h) = (size(app, d, "length") * 0.5, size(app, d, "width") * 0.5, size(app, d, "height") * 0.5);
            let square = |axis: Vec3| n.dot(axis).abs() > 1.0 - 1e-9;
            if square(Vec3::Z) {
                rect(base, n, Vec3::X, l, w)
            } else if square(Vec3::X) {
                rect(base, n, Vec3::Y, w, h)
            } else if square(Vec3::Y) {
                rect(base, n, Vec3::X, l, h)
            } else {
                Vec::new()
            }
        }
        "solid.torus" => circle(base, n, size(app, d, "major") + size(app, d, "minor")),
        _ => circle(base, n, radius(app, d)),
    }
}

#[cfg(test)]
#[path = "primitive_tests.rs"]
mod tests;
