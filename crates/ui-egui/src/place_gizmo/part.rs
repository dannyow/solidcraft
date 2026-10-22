//! Insert Part: without a hole picked, the part used to go to the origin. The marker now puts it
//! on the face under the pointer, standing on it with its shank into it and no joint; dropped on
//! the wall of a round hole, it takes that hole as the dialog's Hole pick, so OK seats it there
//! with its joint as a click on the hole does. A standard part is round about its axis: no knob.

use egui::Pos2;
use solidcraft_engine::Sel;
use solidcraft_engine::geom::Vec3;

use super::Spec;
use crate::SolveApp;
use crate::dialogs::{Dialog, Kind};
use crate::viewport::{Hit, Proj};

pub(super) static SPEC: Spec = Spec {
    name: "part",
    serves: |d| matches!(d.kind, Kind::Part(_)),
    snap_commands: &[],
    move_tip: "Drag to place the part on a face, or onto a hole's wall to seat it there.",
    frame,
    move_to,
    footprint: |_, _, _, _| Vec::new(),
    turn: None,
};

/// The picked hole's place, else the free place, else the origin.
fn frame(_: &SolveApp, d: &Dialog) -> Option<(Vec3, Vec3)> {
    let Kind::Part(k) = &d.kind else { return None };
    match d.inputs.first().and_then(|i| i.items.first()) {
        Some(Sel::Face { point, .. } | Sel::Edge { point, .. }) => Some((*point, Vec3::Z)),
        _ => Some(k.place.unwrap_or((Vec3::ZERO, Vec3::Z))),
    }
}

/// A hole's wall under the pointer is the Hole pick; any other face is a free place on it,
/// standing along its normal there.
fn move_to(app: &SolveApp, d: &mut Dialog, proj: &Proj, pos: Pos2) -> bool {
    let s = &app.session;
    let hits = crate::viewport::pick(app, proj, pos);
    let Some(h @ Hit::Face { body, index, point }) = hits.iter().find(|h| matches!(h, Hit::Face { .. })) else { return false };
    let normal = crate::dialogs::planar_face(s, body, *index).map(|(_, n)| n).or_else(|| normal_near(app, body, *index, *point));
    let hole = match normal {
        Some(n) if crate::dialogs::planar_face(s, body, *index).is_none() && hole_wall(app, body, *point, n) => d.candidate_on(s, h, None),
        Some(n) => {
            if let Kind::Part(k) = &mut d.kind {
                k.place = Some((*point, n));
            }
            None
        }
        None => return false,
    };
    if let Some(inp) = d.inputs.first_mut() {
        inp.items = hole.into_iter().collect();
    }
    d.error = None;
    true
}

/// The outward normal of the body's face `face` near `p`, from its display mesh.
fn normal_near(app: &SolveApp, body: &str, face: usize, p: Vec3) -> Option<Vec3> {
    let st = app.session.world_state();
    let m = st.body(body)?.mesh();
    m.triangles
        .iter()
        .zip(&m.tri_face)
        .filter(|(_, f)| usize::try_from(**f).is_ok_and(|f| f == face))
        .filter_map(|(t, _)| m.tri(t))
        .min_by(|a, b| ((a[0] + a[1] + a[2]) / 3.0).dist(p).total_cmp(&((b[0] + b[1] + b[2]) / 3.0).dist(p)))
        .and_then(|[a, b, c]| (b - a).cross(c - a).normalized())
}

/// Is `p` on a hole's wall? Out along the wall's normal, a hole's opposite wall faces back at it,
/// on the same body and curved too. A boss, a screw head or a rounded corner face open space or
/// another body instead.
fn hole_wall(app: &SolveApp, body: &str, p: Vec3, n: Vec3) -> bool {
    let st = app.session.world_state();
    let Some(b) = st.body(body) else { return false };
    let m = b.mesh();
    // Start a meshing tolerance off the wall: a faceted wall lies a little inside the true one.
    let off = (b.body.size() * 2e-3).max(1e-3);
    let Some((_, tri)) = m.raycast(p + n * off, n) else { return false };
    let Some(face) = m.tri_face.get(tri).and_then(|f| usize::try_from(*f).ok()) else { return false };
    let back = m.triangles.get(tri).and_then(|t| m.tri(t)).and_then(|[a, b, c]| (b - a).cross(c - a).normalized());
    back.is_some_and(|q| q.dot(n) < -0.5) && crate::dialogs::planar_face(&app.session, body, face).is_none()
}

#[cfg(test)]
#[path = "part_tests.rs"]
mod tests;
