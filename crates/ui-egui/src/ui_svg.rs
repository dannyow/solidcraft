//! The UI of one frame as an annotated SVG (`ui.export_svg`): every shape egui painted, as vector
//! geometry a design tool can edit, with the names a redesign needs to find its way back to the
//! code. Rects, paths, circles, lines, Béziers and text become their SVG elements; icons are the
//! masters inlined; the 3D viewport is a bitmap. On top of the geometry:
//!
//! - every egui layer is a `<g id="layer:…">` in paint order, every clip rect a `clip-path` group;
//! - a fill or stroke whose colour is a theme token carries `data-token="<name>"`;
//! - text carries `data-font` (the family in `fonts.rs`) and `data-i18n` (the catalog id of an
//!   English message), with `font-family`, `font-weight` and `font-size` a design tool understands;
//! - an icon carries `data-icon="<master>"` and the master's SVG, in the master's own colours;
//! - a trailing `<g id="names">` holds one invisible rect or point per control the frame published
//!   for the test harness (`toolbar:<command>`, `dialog:row:<label>`, `panel:<tab>:<panel>`…);
//! - the root carries the build, theme, language, scale, window and open dialog as `data-*`.
//!
//! Positions are absolute screen points: a design tool shows editable boxes and text, not a
//! component library. Fonts are referenced by name (IBM Plex Sans and Mono), not embedded.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use egui::epaint::{ClippedShape, Shape, TextureId};
use egui::{Color32, LayerId, Pos2, Rect};
use serde_json::{Value, json};

use crate::SolveApp;

/// Facts about the build and the session, on the root element.
pub struct Meta {
    pub version: String,
    pub commit: String,
    pub theme: String,
    pub dark: bool,
    pub language: String,
    pub ui_scale: f32,
    pub pixels_per_point: f32,
    pub window: egui::Vec2,
    pub dialog: Option<String>,
}

/// What a frame published for the harness: rects and handles by name (the latest of a name wins).
#[derive(Default)]
pub struct Names {
    pub rects: Vec<(String, Rect)>,
    pub handles: Vec<(String, Pos2)>,
}

/// An icon texture's master id and the master's SVG source.
pub type IconSources = HashMap<TextureId, (String, &'static str)>;

/// Counts of what the writer could not convert.
#[derive(Default, Debug)]
pub struct Skipped {
    pub meshes: usize,
    pub triangles: usize,
    pub callbacks: usize,
}

struct Writer<'a> {
    out: String,
    tokens: &'a BTreeMap<[u8; 4], String>,
    icons: &'a IconSources,
    viewport: Option<&'a (Rect, Vec<u8>)>,
    skipped: Skipped,
    clips: usize,
    shapes: usize,
    inlined: usize,
    /// Ids given out so far: a sanitised name that repeats gets a counter.
    used_ids: std::collections::HashSet<String>,
}

/// Write the frame: `layers` in paint order (name, shapes), the theme `tokens` (name, colour),
/// the icon masters by texture, and the viewport bitmap (its rect and PNG) when one was drawn.
pub fn write(
    layers: &[(String, Vec<ClippedShape>)],
    names: &Names,
    meta: &Meta,
    tokens: &[(String, Color32)],
    icons: &IconSources,
    viewport: Option<&(Rect, Vec<u8>)>,
) -> (String, Skipped, usize) {
    let mut by_colour: BTreeMap<[u8; 4], String> = BTreeMap::new();
    for (name, c) in tokens {
        let e = by_colour.entry(c.to_srgba_unmultiplied()).or_default();
        if !e.is_empty() {
            e.push(' ');
        }
        e.push_str(name);
    }
    let mut w = Writer {
        out: String::new(),
        tokens: &by_colour,
        icons,
        viewport,
        skipped: Skipped::default(),
        clips: 0,
        shapes: 0,
        inlined: 0,
        used_ids: std::collections::HashSet::new(),
    };
    let (width, height) = (meta.window.x, meta.window.y);
    let _ = writeln!(
        w.out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" \
         data-app=\"SolidCraft\" data-version=\"{}\" data-commit=\"{}\" data-theme=\"{}\" data-dark=\"{}\" data-language=\"{}\" \
         data-ui-scale=\"{}\" data-pixels-per-point=\"{}\" data-dialog=\"{}\" data-exported=\"{}\">",
        fmt(width),
        fmt(height),
        fmt(width),
        fmt(height),
        esc(&meta.version),
        esc(&meta.commit),
        esc(&meta.theme),
        meta.dark,
        esc(&meta.language),
        fmt(meta.ui_scale),
        fmt(meta.pixels_per_point),
        esc(meta.dialog.as_deref().unwrap_or("")),
        esc(&now_utc()),
    );
    let _ = writeln!(
        w.out,
        "<title>SolidCraft UI {}{}</title>",
        esc(&meta.version),
        meta.dialog.as_deref().map(|d| format!(", {d}")).unwrap_or_default()
    );
    let _ = writeln!(
        w.out,
        "<desc>Exported by ui.export_svg. Attributes: data-token = theme token of a colour; data-font = font family in fonts.rs; data-i18n = catalog id of the text; data-icon = icon master in assets/icons/ui/design/svg; the names group lists the controls the frame published for the test harness.</desc>"
    );
    w.out.push_str("<style>text{white-space:pre}</style>\n");
    for (name, shapes) in layers {
        let id = w.unique_id(&format!("layer:{}", xml_id(name)));
        let _ = writeln!(w.out, "<g id=\"{id}\" data-name=\"{}\">", esc(name));
        let mut clip: Option<Rect> = None;
        for s in shapes {
            if clip != Some(s.clip_rect) {
                if clip.is_some() {
                    w.out.push_str("</g>\n");
                }
                w.open_clip(s.clip_rect);
                clip = Some(s.clip_rect);
            }
            w.shape(&s.shape);
        }
        if clip.is_some() {
            w.out.push_str("</g>\n");
        }
        w.out.push_str("</g>\n");
    }
    w.names(names);
    w.out.push_str("</svg>\n");
    let shapes = w.shapes;
    (w.out, w.skipped, shapes)
}

impl Writer<'_> {
    /// `base`, or `base-2`, `base-3`… when it was given out already (names that differ only in
    /// characters an id cannot hold, such as ▲ and ▼, sanitise to the same id).
    fn unique_id(&mut self, base: &str) -> String {
        let mut id = base.to_string();
        let mut n = 1;
        while !self.used_ids.insert(id.clone()) {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    fn open_clip(&mut self, r: Rect) {
        self.clips += 1;
        let id = format!("clip{}", self.clips);
        let _ = writeln!(
            self.out,
            "<clipPath id=\"{id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath><g clip-path=\"url(#{id})\" data-clip=\"{} {} {} {}\">",
            fmt(r.min.x),
            fmt(r.min.y),
            fmt(r.width()),
            fmt(r.height()),
            fmt(r.min.x),
            fmt(r.min.y),
            fmt(r.width()),
            fmt(r.height())
        );
    }

    /// `fill="…"` (with `data-token`) or `fill="none"`.
    fn fill(&self, c: Color32) -> String {
        if c.a() == 0 {
            return "fill=\"none\"".into();
        }
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        let mut s = format!("fill=\"#{r:02x}{g:02x}{b:02x}\"");
        if a < 255 {
            let _ = write!(s, " fill-opacity=\"{}\"", fmt(f32::from(a) / 255.0));
        }
        if let Some(t) = self.tokens.get(&[r, g, b, a]) {
            let _ = write!(s, " data-token=\"{}\"", esc(t));
        }
        s
    }

    fn stroke(&self, width: f32, c: Color32) -> String {
        if width <= 0.0 || c.a() == 0 {
            return String::new();
        }
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        let mut s = format!(" stroke=\"#{r:02x}{g:02x}{b:02x}\" stroke-width=\"{}\"", fmt(width));
        if a < 255 {
            let _ = write!(s, " stroke-opacity=\"{}\"", fmt(f32::from(a) / 255.0));
        }
        if let Some(t) = self.tokens.get(&[r, g, b, a]) {
            let _ = write!(s, " data-stroke-token=\"{}\"", esc(t));
        }
        s
    }

    fn path_stroke(&self, s: &egui::epaint::PathStroke) -> String {
        match s.color {
            egui::epaint::ColorMode::Solid(c) => self.stroke(s.width, c),
            egui::epaint::ColorMode::UV(_) => String::new(),
        }
    }

    fn shape(&mut self, shape: &Shape) {
        self.shapes += 1;
        match shape {
            Shape::Noop => {}
            Shape::Vec(v) => {
                for s in v {
                    self.shape(s);
                }
            }
            Shape::Rect(r) => {
                if r.fill.a() == 0 && (r.stroke.width <= 0.0 || r.stroke.color.a() == 0) {
                    return;
                }
                // SVG strokes are centred on the edge; egui's may sit inside or outside it.
                let rect = match r.stroke_kind {
                    egui::StrokeKind::Inside => r.rect.shrink(r.stroke.width / 2.0),
                    egui::StrokeKind::Middle => r.rect,
                    egui::StrokeKind::Outside => r.rect.expand(r.stroke.width / 2.0),
                };
                let cr = r.corner_radius;
                let rx = cr.nw.max(cr.ne).max(cr.sw).max(cr.se);
                let _ = write!(
                    self.out,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                    fmt(rect.min.x),
                    fmt(rect.min.y),
                    fmt(rect.width().max(0.0)),
                    fmt(rect.height().max(0.0))
                );
                if rx > 0 {
                    let _ = write!(self.out, " rx=\"{rx}\"");
                }
                let _ = writeln!(self.out, " {}{}/>", self.fill(r.fill), self.stroke(r.stroke.width, r.stroke.color));
            }
            Shape::Circle(c) => {
                let _ = writeln!(
                    self.out,
                    "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" {}{}/>",
                    fmt(c.center.x),
                    fmt(c.center.y),
                    fmt(c.radius),
                    self.fill(c.fill),
                    self.stroke(c.stroke.width, c.stroke.color)
                );
            }
            Shape::Ellipse(e) => {
                let _ = writeln!(
                    self.out,
                    "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" {}{}/>",
                    fmt(e.center.x),
                    fmt(e.center.y),
                    fmt(e.radius.x),
                    fmt(e.radius.y),
                    self.fill(e.fill),
                    self.stroke(e.stroke.width, e.stroke.color)
                );
            }
            Shape::LineSegment { points, stroke } => {
                let st = self.stroke(stroke.width, stroke.color);
                if st.is_empty() {
                    return;
                }
                let _ = writeln!(
                    self.out,
                    "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"{}/>",
                    fmt(points[0].x),
                    fmt(points[0].y),
                    fmt(points[1].x),
                    fmt(points[1].y),
                    st
                );
            }
            Shape::Path(p) => {
                let Some(first) = p.points.first() else { return };
                let mut d = format!("M{} {}", fmt(first.x), fmt(first.y));
                for q in p.points.iter().skip(1) {
                    let _ = write!(d, "L{} {}", fmt(q.x), fmt(q.y));
                }
                if p.closed {
                    d.push('Z');
                }
                let _ = writeln!(self.out, "<path d=\"{d}\" {}{}/>", self.fill(p.fill), self.path_stroke(&p.stroke));
            }
            Shape::QuadraticBezier(b) => {
                let [a, c, e] = b.points;
                let mut d = format!("M{} {}Q{} {} {} {}", fmt(a.x), fmt(a.y), fmt(c.x), fmt(c.y), fmt(e.x), fmt(e.y));
                if b.closed {
                    d.push('Z');
                }
                let _ = writeln!(self.out, "<path d=\"{d}\" {}{}/>", self.fill(b.fill), self.path_stroke(&b.stroke));
            }
            Shape::CubicBezier(b) => {
                let [a, c, e, f] = b.points;
                let mut d = format!("M{} {}C{} {} {} {} {} {}", fmt(a.x), fmt(a.y), fmt(c.x), fmt(c.y), fmt(e.x), fmt(e.y), fmt(f.x), fmt(f.y));
                if b.closed {
                    d.push('Z');
                }
                let _ = writeln!(self.out, "<path d=\"{d}\" {}{}/>", self.fill(b.fill), self.path_stroke(&b.stroke));
            }
            Shape::Text(t) => self.text(t),
            Shape::Mesh(m) => self.mesh(m),
            Shape::Callback(cb) => self.callback(cb.rect),
        }
    }

    fn text(&mut self, t: &egui::epaint::TextShape) {
        let galley = &t.galley;
        let format = galley.job.sections.first().map(|s| &s.format);
        let (family, weight, data_font) = match format.map(|f| &f.font_id.family) {
            Some(egui::FontFamily::Monospace) => ("IBM Plex Mono, monospace", 400, "plex-mono".to_string()),
            Some(egui::FontFamily::Name(n)) => match n.as_ref() {
                "ui-medium" => ("IBM Plex Sans, sans-serif", 500, n.to_string()),
                "ui-semibold" => ("IBM Plex Sans, sans-serif", 600, n.to_string()),
                "ui-bold" => ("IBM Plex Sans, sans-serif", 700, n.to_string()),
                other => (other, 400, other.to_string()),
            },
            _ => ("IBM Plex Sans, sans-serif", 400, "plex-sans".to_string()),
        };
        let size = format.map_or(12.0, |f| f.font_id.size);
        let mut colour = t.override_text_color.unwrap_or_else(|| match format {
            Some(f) if f.color != Color32::PLACEHOLDER => f.color,
            _ => t.fallback_color,
        });
        if t.opacity_factor < 1.0 {
            colour = colour.gamma_multiply(t.opacity_factor);
        }
        let whole: String = galley.job.text.clone();
        let i18n = crate::i18n::label_id(whole.trim()).map(|id| format!(" data-i18n=\"{}\"", esc(id))).unwrap_or_default();
        let rotate = if t.angle != 0.0 {
            format!(" transform=\"rotate({} {} {})\"", fmt(t.angle.to_degrees()), fmt(t.pos.x), fmt(t.pos.y))
        } else {
            String::new()
        };
        for row in &galley.rows {
            let Some(first) = row.glyphs.first() else { continue };
            let text: String = row.glyphs.iter().map(|g| g.chr).collect();
            if text.trim().is_empty() {
                continue;
            }
            let x = t.pos.x + row.pos.x + first.pos.x;
            // A glyph's position is its baseline, relative to the row.
            let y = t.pos.y + row.pos.y + first.pos.y;
            let _ = writeln!(
                self.out,
                "<text x=\"{}\" y=\"{}\" font-family=\"{}\" font-weight=\"{}\" font-size=\"{}\" {} data-font=\"{}\"{}{}>{}</text>",
                fmt(x),
                fmt(y),
                esc(family),
                weight,
                fmt(size),
                self.fill(colour),
                esc(&data_font),
                i18n,
                rotate,
                esc(&text)
            );
        }
    }

    fn mesh(&mut self, m: &egui::epaint::Mesh) {
        if m.vertices.is_empty() {
            return;
        }
        let bounds = Rect::from_points(&m.vertices.iter().map(|v| v.pos).collect::<Vec<_>>());
        if let Some((master, src)) = self.icons.get(&m.texture_id) {
            let tint = m.vertices.first().map_or(Color32::WHITE, |v| v.color);
            let opacity = f32::from(tint.a()) / 255.0;
            let body = src.trim_start_matches(|c: char| c.is_whitespace());
            let body = body.strip_prefix("<?xml").and_then(|s| s.split_once("?>")).map_or(body, |(_, rest)| rest.trim_start());
            let Some(end_of_root) = body.find('>') else { return };
            let _ = write!(
                self.out,
                "<svg x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" viewBox=\"0 0 64 64\" data-icon=\"{}\"",
                fmt(bounds.min.x),
                fmt(bounds.min.y),
                fmt(bounds.width()),
                fmt(bounds.height()),
                esc(master)
            );
            if opacity < 1.0 {
                let _ = write!(self.out, " opacity=\"{}\"", fmt(opacity));
            }
            self.out.push('>');
            self.inlined += 1;
            let inner = body.get(end_of_root + 1..).unwrap_or("");
            self.out.push_str(&prefix_ids(inner, &format!("i{}-", self.inlined)));
            self.out.push('\n');
            return;
        }
        if m.texture_id != TextureId::default() {
            // A texture the writer does not know (a thumbnail, a canvas image): its place.
            self.skipped.meshes += 1;
            let _ = writeln!(
                self.out,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"none\" stroke=\"#ff00ff\" stroke-dasharray=\"2 2\" data-texture=\"{:?}\"/>",
                fmt(bounds.min.x),
                fmt(bounds.min.y),
                fmt(bounds.width()),
                fmt(bounds.height()),
                m.texture_id
            );
            return;
        }
        // Vertex-coloured triangles (gradients, the navigation cube): one polygon each.
        const CAP: usize = 3000;
        let triangles = m.indices.len() / 3;
        if triangles > CAP {
            self.skipped.meshes += 1;
            self.skipped.triangles += triangles;
            return;
        }
        let _ = write!(self.out, "<g data-mesh=\"{triangles}\">");
        for tri in m.indices.chunks_exact(3) {
            let pts: Option<Vec<&egui::epaint::Vertex>> = tri.iter().map(|&i| m.vertices.get(i as usize)).collect();
            let Some(pts) = pts else { continue };
            let Some(c) = pts.first().map(|v| v.color) else { continue };
            if c.a() == 0 {
                continue;
            }
            let _ = write!(
                self.out,
                "<polygon points=\"{} {},{} {},{} {}\" {}/>",
                fmt(pts[0].pos.x),
                fmt(pts[0].pos.y),
                fmt(pts[1].pos.x),
                fmt(pts[1].pos.y),
                fmt(pts[2].pos.x),
                fmt(pts[2].pos.y),
                self.fill(c)
            );
        }
        self.out.push_str("</g>\n");
    }

    fn callback(&mut self, rect: Rect) {
        match self.viewport {
            Some((r, png)) if *r == rect => {
                let data = base64(png);
                let id = self.unique_id("viewport");
                let _ = writeln!(
                    self.out,
                    "<image id=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" data-source=\"ui.render\" xlink:href=\"data:image/png;base64,{data}\"/>",
                    id,
                    fmt(rect.min.x),
                    fmt(rect.min.y),
                    fmt(rect.width()),
                    fmt(rect.height())
                );
            }
            _ => {
                self.skipped.callbacks += 1;
                let id = self.unique_id("viewport");
                let _ = writeln!(
                    self.out,
                    "<rect id=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#d8dde3\" data-source=\"viewport (not rendered)\"/>",
                    id,
                    fmt(rect.min.x),
                    fmt(rect.min.y),
                    fmt(rect.width()),
                    fmt(rect.height())
                );
            }
        }
    }

    fn names(&mut self, names: &Names) {
        self.out.push_str("<g id=\"names\" fill=\"none\" stroke=\"none\">\n");
        // The latest published rect or handle of a name is the one the harness finds.
        let mut rects: BTreeMap<&str, Rect> = BTreeMap::new();
        for (n, r) in &names.rects {
            rects.insert(n, *r);
        }
        for (n, r) in rects {
            let id = self.unique_id(&xml_id(n));
            let _ = writeln!(
                self.out,
                "<rect id=\"{}\" data-name=\"{}\" data-kind=\"rect\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"><title>{}</title></rect>",
                id,
                esc(n),
                fmt(r.min.x),
                fmt(r.min.y),
                fmt(r.width()),
                fmt(r.height()),
                esc(n)
            );
        }
        let mut handles: BTreeMap<&str, Pos2> = BTreeMap::new();
        for (n, p) in &names.handles {
            handles.insert(n, *p);
        }
        for (n, p) in handles {
            let id = self.unique_id(&format!("handle:{}", xml_id(n)));
            let _ = writeln!(
                self.out,
                "<circle id=\"{}\" data-name=\"{}\" data-kind=\"handle\" cx=\"{}\" cy=\"{}\" r=\"3\"><title>{}</title></circle>",
                id,
                esc(n),
                fmt(p.x),
                fmt(p.y),
                esc(n)
            );
        }
        self.out.push_str("</g>\n");
    }
}

/// A legal XML id for a published name: letters, digits, `_`, `-`, `.` and `:` stay, anything
/// else (spaces in "dialog:row:Hole Type") becomes `_`. The exact name is in `data-name`.
fn xml_id(name: &str) -> String {
    let mut id: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':') { c } else { '_' }).collect();
    if !id.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        id.insert(0, '_');
    }
    id
}

/// An inlined master's own ids (masks, clips, gradients) made unique per instance: `id="x"`,
/// `url(#x)` and `href="#x"` become `prefix` + x, so two copies of one icon do not share them.
fn prefix_ids(svg: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(svg.len() + 64);
    let mut rest = svg;
    while let Some(i) = rest.find(['i', 'u', 'h']) {
        let (before, at) = rest.split_at(i);
        out.push_str(before);
        let hit = [(" id=\"", " id=\"", ""), ("url(#", "url(#", ""), ("href=\"#", "href=\"#", "")].into_iter().find(|(pat, _, _)| {
            at.starts_with(pat.trim_start()) && (!pat.starts_with(' ') || before.ends_with(' ') || before.ends_with('\n') || before.ends_with('\t'))
        });
        match hit {
            Some((pat, lead, _)) => {
                let pat = pat.trim_start();
                out.push_str(lead.trim_start());
                out.push_str(prefix);
                rest = at.get(pat.len()..).unwrap_or("");
            }
            None => {
                out.push_str(at.get(..1).unwrap_or(""));
                rest = at.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

fn fmt(v: f32) -> String {
    if v.is_finite() {
        let s = format!("{v:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
    } else {
        "0".into()
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o
}

fn now_utc() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Civil date from days since the epoch (Howard Hinnant's algorithm), no dependency needed.
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// ---- the live frame ---------------------------------------------------------------------------

/// Export the frame that was just drawn (call it as the last step of the frame): the paint lists
/// are still full, egui drains them at the end of the pass. Writes `path` (or returns the SVG in
/// the reply when there is none) and answers like a control request.
pub(crate) fn export_live(app: &mut SolveApp, ctx: &egui::Context, path: Option<&str>) -> Value {
    let mut layer_ids: Vec<LayerId> = vec![LayerId::background()];
    layer_ids.extend(ctx.memory(|m| m.layer_ids().collect::<Vec<_>>()));
    layer_ids.dedup();
    let mut groups: Vec<(LayerId, Vec<ClippedShape>)> = Vec::new();
    for layer in layer_ids {
        let mut shapes: Vec<ClippedShape> = ctx.graphics(|g| g.get(layer).map(|l| l.all_entries().cloned().collect())).unwrap_or_default();
        if shapes.is_empty() {
            continue;
        }
        if let Some(t) = ctx.layer_transform_to_global(layer) {
            for s in &mut shapes {
                s.transform(t);
            }
        }
        groups.push((layer, shapes));
    }
    // Paint order: by egui's order (background, middle, foreground, tooltip, debug), then as listed.
    groups.sort_by_key(|(l, _)| l.order as usize);
    let layers: Vec<(String, Vec<ClippedShape>)> = groups.into_iter().map(|(l, s)| (l.short_debug_format(), s)).collect();

    // The viewport as the window shows it, at the window's pixel density.
    let ppp = ctx.pixels_per_point();
    let viewport_rect = layers.iter().flat_map(|(_, s)| s.iter()).find_map(|s| if let Shape::Callback(cb) = &s.shape { Some(cb.rect) } else { None });
    let viewport = viewport_rect.and_then(|r| {
        let (w, h) = (((r.width() * ppp).round() as u32).clamp(16, 8192), ((r.height() * ppp).round() as u32).clamp(16, 8192));
        let scene = crate::control::view_scene(app, crate::control::window_guides(app));
        solidcraft_engine::capture::png(&scene, &app.cam, w, h).ok().map(|png| (r, png.bytes))
    });

    // Icon textures by name (`icon:<master>:<px>`), with the master's SVG source.
    let mut icons: IconSources = HashMap::new();
    {
        let tex = ctx.tex_manager();
        let tex = tex.read();
        for (id, meta) in tex.allocated() {
            if let Some(rest) = meta.name.strip_prefix("icon:")
                && let Some((master, _px)) = rest.rsplit_once(':')
                && let Some(src) = crate::icon_svg::master_source(master)
            {
                icons.insert(*id, (master.to_string(), src));
            }
        }
    }

    // Theme tokens by name, from the theme file the preferences would export.
    let theme_file = crate::prefs::current_theme_file(app);
    let mut tokens: Vec<(String, Color32)> = Vec::new();
    if let Some(colors) = theme_file.get("colors").and_then(Value::as_object) {
        for (name, v) in colors {
            if let Some(c) = v.as_str().and_then(parse_hex) {
                tokens.push((name.clone(), c));
            }
        }
    }
    let names = Names { rects: crate::scenario::rects(""), handles: crate::scenario::handles("") };
    let meta = Meta {
        version: crate::update::current_version().to_string(),
        commit: crate::help::COMMIT.to_string(),
        theme: theme_file.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        dark: app.ui.dark,
        language: crate::i18n::language().to_string(),
        ui_scale: app.preferences.ui_scale,
        pixels_per_point: ppp,
        window: ctx.content_rect().size(),
        dialog: app.dialog.as_ref().and_then(|d| crate::dialogs::command_of(&d.kind)).map(str::to_string),
    };
    let (svg, skipped, shapes) = write(&layers, &names, &meta, &tokens, &icons, viewport.as_ref());
    let mut result = json!({
        "bytes": svg.len(),
        "layers": layers.len(),
        "shapes": shapes,
        "names": names.rects.len() + names.handles.len(),
        "icons": icons.len(),
        "viewport": viewport.is_some(),
        "skipped": {"meshes": skipped.meshes, "triangles": skipped.triangles, "callbacks": skipped.callbacks},
    });
    match path {
        Some(p) => match std::fs::write(p, svg.as_bytes()) {
            Ok(()) => {
                result["path"] = json!(p);
            }
            Err(e) => return json!({"ok": false, "error": format!("cannot write {p}: {e}")}),
        },
        None => {
            result["svg"] = json!(svg);
        }
    }
    json!({"ok": true, "result": result})
}

/// Standard base64 with padding (the PNG goes into a data URI; no dependency needed).
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let sextet = |i: u32| char::from(T[((v >> (18 - 6 * i)) & 63) as usize]);
        out.push(sextet(0));
        out.push(sextet(1));
        out.push(if n > 1 { sextet(2) } else { '=' });
        out.push(if n > 2 { sextet(3) } else { '=' });
    }
    out
}

fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Harness;

    /// The Parameters dialog over the sample: the SVG carries the dialog's rows by name, text with
    /// catalog ids and fonts, a theme token on a fill, an inlined icon, and the build on the root.
    #[test]
    fn the_parameters_dialog_exports_as_annotated_svg() {
        let mut h = Harness::new();
        h.app.run("parameters.add", json!({"name": "width", "expression": "30 mm"})).unwrap();
        h.step(&json!({"start": "parameters.change"})).unwrap();
        // A feature dialog as well: the Parameters window is not a command dialog.
        h.step(&json!({"start": "solid.extrude"})).unwrap();
        h.frames(2);
        let path = h.dir.join("params.svg");
        let out = h.call("ui.export_svg", json!({"path": path.to_string_lossy()}));
        assert_eq!(out["ok"], true, "{out}");
        let svg = std::fs::read_to_string(&path).unwrap();
        assert!(svg.starts_with("<svg "), "{}", &svg[..80]);
        assert!(svg.contains(&format!("data-version=\"{}\"", crate::update::current_version())));
        assert!(svg.contains("data-dialog=\"solid.extrude\""), "the open dialog is named on the root");
        assert!(svg.contains("<g id=\"layer:"), "layers are groups");
        assert!(svg.contains("data-token=\""), "a fill is traced to a theme token");
        assert!(svg.contains("data-font=\"plex-sans\"") || svg.contains("data-font=\"ui-medium\""), "text names its font family");
        assert!(svg.contains("data-i18n=\""), "a catalog message is traced to its id");
        assert!(svg.contains("data-icon=\""), "an icon is inlined with its master id");
        assert!(svg.contains("<g id=\"names\""), "the names group exists");
        assert!(svg.contains("id=\"handle:toolbar:solid.extrude\""), "toolbar buttons are named by command id");
        assert!(svg.contains("data-name=\"dialog:row:Hole Type\"") || svg.contains("data-name=\"dialog:row:Distance\""), "exact names survive");
        // Every id is a legal XML name and unique.
        let mut ids: Vec<&str> = svg.match_indices("id=\"").map(|(i, _)| svg[i + 4..].split('"').next().unwrap_or("")).collect();
        assert!(ids.iter().all(|id| !id.contains(' ')), "an id with a space: {:?}", ids.iter().find(|id| id.contains(' ')));
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate ids");
        assert!(svg.contains(">width<"), "the parameter's name is text: {}", svg.len());
        assert!(!svg.contains("<text x=\"NaN\""));
        // Without a path the SVG comes back inline.
        let inline = h.call("ui.export_svg", json!({}));
        assert!(inline["result"]["svg"].as_str().is_some_and(|s| s.starts_with("<svg ")));
    }

    #[test]
    fn a_path_outside_the_allowed_folders_is_refused() {
        let mut h = Harness::new();
        let out = h.call("ui.export_svg", json!({"path": "/nonexistent-root/x.svg"}));
        assert_eq!(out["ok"], false, "{out}");
    }

    #[test]
    fn numbers_and_text_are_written_safely() {
        assert_eq!(fmt(12.0), "12");
        assert_eq!(fmt(12.5), "12.5");
        assert_eq!(fmt(0.004), "0");
        assert_eq!(fmt(f32::NAN), "0");
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert_eq!(parse_hex("#1d2021"), Some(Color32::from_rgb(0x1d, 0x20, 0x21)));
        assert_eq!(parse_hex("1d2021"), None);
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(xml_id("dialog:row:Hole Type"), "dialog:row:Hole_Type");
        assert_eq!(xml_id("1st"), "_1st");
        assert_eq!(
            prefix_ids(r##"<mask id="m"><rect/></mask><g mask="url(#m)" fill="#fff"><use href="#m"/><path id="p" d="M1 1"/></g>"##, "i7-"),
            r##"<mask id="i7-m"><rect/></mask><g mask="url(#i7-m)" fill="#fff"><use href="#i7-m"/><path id="i7-p" d="M1 1"/></g>"##
        );
    }
}
