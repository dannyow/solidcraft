//! Spike: handles that place and turn a Rest on its face (Preferences › Design › Rest handles,
//! off by default, `rest_gizmo` through `ui.prefs`).
//!
//! A ball at the pad's centre drags it across the model, near-white so the blue preview it sits on
//! does not hide it, cased in a darker edge so it also carries on a light body. Under the pointer
//! it brightens, shows the move cursor and a tooltip. While it drags, the pad's footprint is
//! outlined where the ball is: the solid preview is recomputed off the window thread and trails
//! the pointer by its boolean, the outline does not. Each frame the pointer is picked as a click into the
//! Position input would be, sketch points and all, so the pad lands where a click would put it;
//! the dialog's Position input follows, and so does the stored growth direction when the pad
//! crosses onto another face. A rectangular pad also gets a knob past the end of
//! its length: dragging it turns the pad about the face normal in 5° steps (Alt or Ctrl held:
//! free), kept as the command's `along`. Nothing runs until OK: the handles edit the open
//! dialog, which the live preview follows, so a new Rest and an Edit Feature behave alike.
//!
//! The handles are published as `rest:position` and `rest:rotate` for `ui.handles`.

use egui::{Color32, Pos2, Stroke, vec2};
use serde_json::{Value, json};
use solidcraft_engine::Sel;
use solidcraft_engine::doc::expr::Kind as ValueKind;
use solidcraft_engine::geom::Vec3;

use crate::SolveApp;
use crate::dialogs::{Dialog, Kind};
use crate::dialogs_plastic::Pl;
use crate::theme::Tokens;
use crate::viewport::Proj;

/// Reach of a handle around its centre (pixels): wide, so a finger finds it.
const REACH: f32 = 18.0;
/// Radius of a drawn ball handle (pixels).
const DOT_PX: f32 = 6.0;
/// The knob sits this far past the pad's end (pixels).
const KNOB_GAP_PX: f64 = 28.0;
/// Rotation step without a modifier (degrees).
const STEP_DEG: f64 = 5.0;
/// Points on the round footprint's outline.
const FOOTPRINT_SEGS: usize = 48;
/// The handles are near-white, so the blue preview they sit on does not hide them; `EDGE` is the
/// darker casing drawn under every line and around every ball, so they also carry on a light body.
const HANDLE: Color32 = Color32::from_rgb(237, 240, 245);
const EDGE: Color32 = Color32::from_rgb(60, 68, 84);

/// A line in `core`, cased in `EDGE` under it so it reads on the preview and on the body alike.
fn cased_line(painter: &egui::Painter, a: Pos2, b: Pos2, w: f32, core: Color32) {
    painter.line_segment([a, b], Stroke::new(w + 2.5, EDGE));
    painter.line_segment([a, b], Stroke::new(w, core));
}

/// A closed outline in `core`, cased in `EDGE` under it.
fn cased_closed(painter: &egui::Painter, pts: &[Pos2], w: f32, core: Color32) {
    for s in [Stroke::new(w + 2.0, EDGE), Stroke::new(w, core)] {
        painter.add(egui::Shape::closed_line(pts.to_vec(), s));
    }
}

/// A ball handle: near-white with the contrast edge. Hot (under the pointer or dragged) it
/// brightens to pure white, grows a little, gets a soft halo and an accent edge to say it is live.
fn ball(painter: &egui::Painter, c: Pos2, hot: bool, t: &Tokens) {
    if hot {
        painter.circle_filled(c, REACH, t.accent.gamma_multiply(0.14));
    }
    let r = if hot { DOT_PX + 1.5 } else { DOT_PX };
    let fill = if hot { Color32::WHITE } else { HANDLE };
    let edge = if hot { t.accent } else { EDGE };
    painter.circle(c, r, fill, Stroke::new(1.5, edge));
}

/// Is this dialog a Rest with the handles switched on?
pub(crate) fn enabled(app: &SolveApp, d: &Dialog) -> bool {
    app.preferences.rest_gizmo && matches!(d.kind, Kind::Plastic(Pl::Rest { .. }))
}

fn vec3_of(v: &Value) -> Option<Vec3> {
    let a = v.as_array()?;
    let p = Vec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?);
    p.is_finite().then_some(p)
}

fn pt(v: Vec3) -> Value {
    json!([v.x, v.y, v.z])
}

/// The pad's centre and the direction it grows: the picked face's normal, else the stored one.
fn frame(app: &SolveApp, d: &Dialog) -> Option<(Vec3, Vec3)> {
    let Sel::Face { body, index, point } = d.inputs.first()?.items.first()? else { return None };
    let n = crate::dialogs::planar_face(&app.session, body, *index)
        .map(|(_, n)| n)
        .or_else(|| d.extra.get("direction").and_then(vec3_of))
        .and_then(|n| n.normalized())?;
    Some((*point, n))
}

/// In-plane axes about `n`: x is the engine's default `along` (its `any_perp`), y completes
/// them, so angle 0 is an untouched pad.
fn axes(n: Vec3) -> (Vec3, Vec3) {
    let x = n.any_perp();
    (x, n.cross(x))
}

/// The stored `along` as an angle about `n` from the default direction.
fn angle_of(d: &Dialog, n: Vec3) -> f64 {
    let (x, y) = axes(n);
    d.extra.get("along").and_then(vec3_of).map(|a| a.dot(y).atan2(a.dot(x))).unwrap_or(0.0)
}

fn along_at(n: Vec3, angle: f64) -> Vec3 {
    let (x, y) = axes(n);
    x * angle.cos() + y * angle.sin()
}

/// Put the pad where a click at `pos` would: the picked face place (snapped to a sketch point
/// as a click is), with the growth direction and `along` carried onto the new face.
fn move_to(app: &SolveApp, d: &mut Dialog, proj: &Proj, pos: Pos2, n: Vec3) -> bool {
    let s = &app.session;
    let hits = crate::viewport::pick(app, proj, pos);
    let snap = d.face_snap(s, &hits);
    let Some(to) = hits.iter().find_map(|h| d.candidate_on(s, h, snap.as_ref()).filter(|c| matches!(c, Sel::Face { .. }))) else {
        return false;
    };
    let angle = angle_of(d, n);
    if let Sel::Face { body, index, .. } = &to
        && let Some((_, nn)) = crate::dialogs::planar_face(s, body, *index)
    {
        if d.extra.contains_key("direction") {
            d.extra.insert("direction".into(), pt(nn));
        }
        if d.extra.contains_key("along") {
            d.extra.insert("along".into(), pt(along_at(nn, angle)));
        }
    }
    if let Some(inp) = d.inputs.first_mut() {
        inp.items = vec![to];
    }
    d.error = None;
    true
}

/// Where the pointer's ray meets the plane through `base` across `n`.
fn on_plane(proj: &Proj, pos: Pos2, base: Vec3, n: Vec3) -> Option<Vec3> {
    let (o, dir) = proj.ray(pos);
    let den = dir.dot(n);
    // Near edge-on, a plane intersection magnifies subpixel pointer error.
    if den.abs() < 0.05 {
        return None;
    }
    let p = o + dir * ((base - o).dot(n) / den);
    p.is_finite().then_some(p)
}

/// The pad's footprint on its face: a circle of the diameter, or the rectangle of the length
/// along `along` and the width across it.
fn footprint(app: &SolveApp, painter: &egui::Painter, proj: &Proj, d: &Dialog, base: Vec3, n: Vec3) {
    let Kind::Plastic(Pl::Rest { round, width, length, .. }) = &d.kind else { return };
    let len = |e: &String| app.session.doc.eval(e, ValueKind::Length).ok().filter(|v| v.is_finite()).unwrap_or(0.0).abs();
    let (x, y) = axes(n);
    let pts: Option<Vec<Pos2>> = if *round {
        let r = len(width) * 0.5;
        (0..FOOTPRINT_SEGS)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / FOOTPRINT_SEGS as f64;
                proj.to_screen(base + (x * a.cos() + y * a.sin()) * r)
            })
            .collect()
    } else {
        let a = along_at(n, angle_of(d, n));
        let b = n.cross(a);
        let (hl, hw) = (len(length) * 0.5, len(width) * 0.5);
        [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)]
            .into_iter()
            .map(|(sa, sb)| proj.to_screen(base + a * (sa * hl) + b * (sb * hw)))
            .collect()
    };
    if let Some(pts) = pts.filter(|p| p.len() >= 3) {
        cased_closed(painter, &pts, 1.5, HANDLE);
    }
}

/// Draw the handles and take their drags (after the dialog's own arrow, so the ball wins
/// the pointer near the base).
pub(crate) fn show(app: &SolveApp, ui: &mut egui::Ui, painter: &egui::Painter, proj: &Proj, d: &mut Dialog) {
    if !enabled(app, d) {
        return;
    }
    let Some((base, n)) = frame(app, d) else { return };
    let Some(bs) = proj.to_screen(base) else { return };
    let t = Tokens::get();
    let area = |c: Pos2| egui::Rect::from_center_size(c, vec2(2.0 * REACH, 2.0 * REACH));

    // The ball that moves the pad.
    let r = ui
        .interact(area(bs), egui::Id::new("sc_rest_position"), egui::Sense::drag())
        .on_hover_text(crate::i18n::tr_label("Drag to move the rest across the model. It snaps to sketch points on the face."));
    if r.dragged() {
        crate::tool_modifiers::set_manipulator_dragging(true);
        if let Some(p) = r.interact_pointer_pos() {
            move_to(app, d, proj, p, n);
        }
    }
    // Read the frame again: a drag moved it this frame.
    let Some((base, n)) = frame(app, d) else { return };
    let Some(bs) = proj.to_screen(base) else { return };
    crate::scenario::publish_handle("rest:position", bs);
    let hot = r.hovered() || r.dragged();
    if r.dragged() {
        footprint(app, painter, proj, d, base, n);
    }
    ball(painter, bs, hot, &t);
    if hot {
        ui.ctx().set_cursor_icon(if r.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Move });
    }

    // The rotation knob: a rectangular pad only.
    let Kind::Plastic(Pl::Rest { round: false, length, .. }) = &d.kind else { return };
    let half = app.session.doc.eval(length, ValueKind::Length).ok().filter(|v| v.is_finite()).unwrap_or(0.0).abs() * 0.5;
    // World length of one pixel at this zoom (orthographic views; near enough in perspective).
    let per_px = 2.0 * app.cam.half_height() / f64::from(proj.rect.height().max(1.0));
    let radius = half + KNOB_GAP_PX * per_px;
    let mut angle = angle_of(d, n);
    let (x, y) = axes(n);
    let knob_at = |a: f64| proj.to_screen(base + (x * a.cos() + y * a.sin()) * radius);
    let Some(mut ks) = knob_at(angle) else { return };
    let r = ui
        .interact(area(ks), egui::Id::new("sc_rest_rotate"), egui::Sense::drag())
        .on_hover_text(crate::i18n::tr_label("Drag to turn the rest in 5° steps. Hold Alt or Ctrl to turn it freely."));
    if r.dragged()
        && let Some(p) = r.interact_pointer_pos()
    {
        crate::tool_modifiers::set_manipulator_dragging(true);
        let raw = match on_plane(proj, p, base, n) {
            Some(q) => Some((q - base).dot(y).atan2((q - base).dot(x))),
            None => crate::canvas::projected_rotation_angle(proj, p, base, x, y, radius, angle, (-std::f64::consts::PI, std::f64::consts::PI)),
        };
        if let Some(raw) = raw.filter(|a| a.is_finite()) {
            let mut deg = raw.to_degrees();
            if !crate::tool_modifiers::manipulator_free() {
                deg = (deg / STEP_DEG).round() * STEP_DEG;
            }
            angle = deg.to_radians();
            d.extra.insert("along".into(), pt(along_at(n, angle)));
            d.error = None;
            if let Some(k) = knob_at(angle) {
                ks = k;
            }
        }
    }
    crate::scenario::publish_handle("rest:rotate", ks);
    let hot = r.hovered() || r.dragged();
    cased_line(painter, bs, ks, 1.5, if hot { Color32::WHITE } else { HANDLE });
    ball(painter, ks, hot, &t);
    if hot {
        ui.ctx().set_cursor_icon(if r.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
        let text = format!("{:.0}°", angle.to_degrees());
        painter.text(ks + vec2(12.0, -12.0), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), t.text);
    }
}
