//! The offscreen recorder: the scenario harness's frames painted on an offscreen GPU device the
//! way the window paints them (egui's renderer, with the viewport drawn through its real paint
//! callbacks and the app's split scene target), read back and written as PNG frames. No window
//! server is involved, so it works with the screen locked, the display asleep and in CI.
//!
//! A recording script is a JSON object:
//! - `size`: `[w, h]` in points (default 1600 x 1000), `ppp`:
//!   physical pixels per point (default 2, a Retina screen)
//! - `pointer`: `"system"` (default: the driver draws a plain pointer, shaped by the cursor the
//!   app asks for each frame) or `"agent"` (the app's agent pointer with its tag)
//! - `keys`: how the driver shows keys: `"hud"` (default: one dark box in the picture), `"band"`
//!   (keycaps in the caption band) or `false`; `keys_at`: `"lower"` (default) or `"centre"`
//! - `steps`: scenario steps (see [`crate::scenario`]) and these:
//!   - `{"record": true | false}`: write every frame from here on (or stop)
//!   - `{"crop": [x0, y0, x1, y1]}`: points; frames written from here on are cut to it
//!   - `{"zoom": {"to": [x0, y0, x1, y1] | null, "ms": 600}}`: the clip moves, eased, onto that
//!     part of the picture (points; null: back to all of it); the driver crops and scales
//!   - `{"caption": "…"}`: the caption of the frames from here on (`null`: none); the driver
//!     draws it under the picture
//!   - `{"glide": AT, "frames"?: 30}`: move the pointer there, eased, one event per frame
//!   - `{"press": AT?}`, `{"drag_to": AT, "frames"?: 30}`, `{"release": true}`: an eased drag
//!   - `{"still": n}`: n frames with nothing happening; `{"pause": ms}`: the same in milliseconds
//!
//! With the agent pointer, while text is typed the drawn pointer steps aside (right of where it
//! points), so its tag leaves the value box readable; it comes back before the pointer acts again.
//! Before a harness step that acts somewhere (`click`, `move`, `hold`, `drag`, `widget`) the
//! pointer glides there from where it was, so nothing jumps; `queued` requests are acted out by
//! the agent pointer itself.
//!
//! The agent pointer is on: pointer input moves it, and queued requests are acted out by it. With
//! the system pointer it is not painted, and each frame logs where it is and the cursor icon.
//! The app starts as the macOS app does (title bar in the app bar), with the preferences in
//! `$SOLIDCRAFT_RECORD_PREFS` (the desktop app's saved JSON) when set.
//! Frames go to `out/frame-00000.png` …, with `out/frames.json` listing each frame's caption,
//! crop and input (keys, modifiers, clicks, pointer), from which the driver draws the keys.
//!
//! Run: `SOLIDCRAFT_RECORD=script.json SOLIDCRAFT_RECORD_OUT=dir cargo test -p solidcraft-ui-egui
//! --lib recorder::record -- --ignored --nocapture` (`plan/recorder/record.sh` wraps it).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use egui_wgpu::wgpu;
use serde_json::{Value, json};

use crate::scenario::Harness;

/// The window's surface format on macOS (egui-wgpu prefers a non-sRGB 8-bit format).
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Frames a glide takes by default (0.5 s).
const GLIDE: usize = 30;

/// What the frame hook shares with the script runner.
#[derive(Default)]
struct Tape {
    recording: bool,
    crop: Option<egui::Rect>,
    caption: Option<String>,
    /// The zoom: from where, to where (points; `None`: the whole picture), over which frames.
    zoom: Option<Zoom>,
    out: PathBuf,
    frames: Vec<Value>,
    error: Option<String>,
}

/// An eased move of the part of the picture the clip shows.
#[derive(Clone, Copy)]
struct Zoom {
    from: Option<egui::Rect>,
    to: Option<egui::Rect>,
    start: usize,
    frames: usize,
}

impl Zoom {
    /// The rectangle shown at frame `n` (ease-in-out), `None` for the whole picture.
    fn at(&self, n: usize, whole: egui::Rect) -> Option<egui::Rect> {
        let t = ((n.saturating_sub(self.start)) as f32 / self.frames.max(1) as f32).clamp(0.0, 1.0);
        if t >= 1.0 && self.to.is_none() {
            return None;
        }
        let e = t * t * (3.0 - 2.0 * t);
        let (a, b) = (self.from.unwrap_or(whole), self.to.unwrap_or(whole));
        Some(egui::Rect::from_min_max(a.min + (b.min - a.min) * e, a.max + (b.max - a.max) * e))
    }
}

/// egui's renderer on an offscreen device, with the viewport's resources installed as the app
/// installs them.
pub(crate) struct Painter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    target: Option<(wgpu::Texture, [u32; 2])>,
}

impl Painter {
    /// An offscreen device, handed to the app as the window's render state (scene drawn
    /// offscreen with depth, stencil and MSAA, then blitted into egui's pass, as on the desktop).
    pub(crate) fn install(app: &mut crate::SolveApp) -> Result<Painter, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        desc.backends = crate::gpu::offscreen_backends();
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: solidcraft_gpu::depth_features(&adapter),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("no device: {e}"))?;
        let renderer = egui_wgpu::Renderer::new(&device, FORMAT, egui_wgpu::RendererOptions { msaa_samples: 1, ..Default::default() });
        let renderer = Arc::new(egui::mutex::RwLock::new(renderer));
        let rs = egui_wgpu::RenderState {
            adapter: adapter.clone(),
            available_adapters: vec![adapter],
            instance,
            device: device.clone(),
            queue: queue.clone(),
            target_format: FORMAT,
            msaa_samples: 1,
            msaa_supported: vec![1],
            renderer: renderer.clone(),
            surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
            device_lost: egui_wgpu::DeviceLost::default(),
        };
        // The desktop app's depth (24 bits; 32 where every adapter has float depth with
        // stencil), stencil and MSAA request (4x).
        let depth = if solidcraft_gpu::depth_features(&rs.adapter).contains(wgpu::Features::DEPTH32FLOAT_STENCIL8) { 32 } else { 24 };
        app.set_wgpu_split(&rs, depth, 8, 4);
        Ok(Painter { device, queue, renderer, target: None })
    }

    /// Apply the frame's texture changes and, with `read`, paint it and return its pixels
    /// (straight RGBA, `w` x `h` physical pixels).
    pub(crate) fn frame(&mut self, ctx: &egui::Context, out: &egui::FullOutput, read: bool) -> Option<image::RgbaImage> {
        let ppp = out.pixels_per_point;
        let mut r = self.renderer.write();
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                r.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let pixels = if read {
            let screen = ctx.content_rect();
            let size = [(screen.width() * ppp).round() as u32, (screen.height() * ppp).round() as u32];
            if self.target.as_ref().is_none_or(|(_, s)| *s != size) {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("recorder"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                self.target = Some((tex, size));
            }
            let (tex, size) = self.target.as_ref()?;
            let view = tex.create_view(&Default::default());
            let jobs = ctx.tessellate(out.shapes.clone(), ppp);
            let desc = egui_wgpu::ScreenDescriptor { size_in_pixels: *size, pixels_per_point: ppp };
            let mut encoder = self.device.create_command_encoder(&Default::default());
            let extra = r.update_buffers(&self.device, &self.queue, &mut encoder, &jobs, &desc);
            {
                let back = ctx.global_style().visuals.panel_fill.to_normalized_gamma_f32();
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("recorder"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: f64::from(back[0]),
                                    g: f64::from(back[1]),
                                    b: f64::from(back[2]),
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                r.render(&mut pass, &jobs, &desc);
            }
            let row = (size[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("recorder read"),
                size: u64::from(row * size[1]),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(size[1]) },
                },
                wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            );
            self.queue.submit(extra.into_iter().chain([encoder.finish()]));
            let slice = buf.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
            let mapped = slice.get_mapped_range().ok()?;
            let mut rgba = Vec::with_capacity((size[0] * size[1] * 4) as usize);
            for y in 0..size[1] as usize {
                let at = y * row as usize;
                rgba.extend_from_slice(&mapped[at..at + size[0] as usize * 4]);
            }
            image::RgbaImage::from_raw(size[0], size[1], rgba)
        } else {
            None
        };
        for id in &out.textures_delta.free {
            r.free_texture(id);
        }
        pixels
    }
}

/// A harness that paints every frame offscreen and writes the recorded ones.
struct Recorder {
    h: Harness,
    tape: Rc<RefCell<Tape>>,
    /// Where the pointer was last sent.
    pointer: egui::Pos2,
    /// The drawn agent pointer stepped aside while text is typed (so its tag leaves the value
    /// box readable); the input pointer stays where it is.
    aside: bool,
    /// The driver draws the pointer (the agent pointer is not painted, so nothing steps aside).
    system: bool,
}

impl Recorder {
    fn new(size: egui::Vec2, ppp: f32, system: bool, prefs: Option<&str>, out: &Path) -> Result<Recorder, String> {
        let tape = Rc::new(RefCell::new(Tape { out: out.to_path_buf(), ..Default::default() }));
        let mut failed = None;
        let h = Harness::new_with(|h| {
            h.ppp = Some(ppp);
            h.size = size;
            // As the macOS app starts: the title bar in the app bar, Daniel's saved preferences.
            h.app.integrated_titlebar = true;
            if let Some(prefs) = prefs {
                h.app.load_prefs(prefs);
            }
            h.app.ui.agent_cursor.show = true;
            h.app.agent.hidden = system;
            let mut painter = match Painter::install(&mut h.app) {
                Ok(p) => p,
                Err(e) => {
                    failed = Some(e);
                    return;
                }
            };
            let t = tape.clone();
            h.on_frame = Some(Box::new(move |ctx, full| {
                let mut tape = t.borrow_mut();
                let Some(img) = painter.frame(ctx, full, tape.recording) else { return };
                let img = match tape.crop {
                    Some(c) => {
                        let ppp = full.pixels_per_point;
                        let (x0, y0) = ((c.min.x * ppp).round().max(0.0) as u32, (c.min.y * ppp).round().max(0.0) as u32);
                        let x1 = ((c.max.x * ppp).round() as u32).min(img.width()).max(x0 + 2);
                        let y1 = ((c.max.y * ppp).round() as u32).min(img.height()).max(y0 + 2);
                        image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
                    }
                    None => img,
                };
                let n = tape.frames.len();
                let path = tape.out.join(format!("frame-{n:05}.png"));
                if let Err(e) = write_png(&img, &path) {
                    tape.error.get_or_insert(format!("{}: {e}", path.display()));
                }
                let caption = tape.caption.clone();
                let crop = tape.crop.map(|c| [c.min.x, c.min.y, c.max.x, c.max.y]);
                let whole = tape.crop.unwrap_or(ctx.content_rect());
                let view = tape.zoom.and_then(|z| z.at(n, whole)).map(|v| [v.min.x, v.min.y, v.max.x, v.max.y]);
                tape.frames.push(json!({
                    "file": path.file_name().map(|f| f.to_string_lossy().into_owned()),
                    "caption": caption,
                    "crop": crop,
                    "view": view,
                    "input": input(ctx),
                    "cursor": {
                        "at": ctx.data(|d| d.get_temp::<Option<egui::Pos2>>(egui::Id::new("sc_agent_pos"))).flatten().map(|p| [p.x, p.y]),
                        "icon": format!("{:?}", full.platform_output.cursor_icon),
                    },
                }));
            }));
        });
        if let Some(e) = failed {
            return Err(e);
        }
        let mut h = h;
        // A frame so the renderer has the fonts before anything is recorded.
        h.frames(2);
        Ok(Recorder { h, tape, pointer: egui::pos2(size.x / 2.0, size.y / 2.0), aside: false, system })
    }

    /// Eased pointer events from where the pointer is to `to`, one per frame (button held or not).
    fn glide(&mut self, to: egui::Pos2, frames: usize) {
        let from = self.pointer;
        for k in 1..=frames.max(1) {
            let t = k as f32 / frames.max(1) as f32;
            let e = t * t * (3.0 - 2.0 * t);
            self.h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * e));
        }
        self.pointer = to;
        self.h.settle();
    }

    /// Glide only the drawn agent pointer to `to` (no input events), eased.
    fn glide_drawn(&mut self, to: egui::Pos2, frames: usize) {
        let from = self.h.app.agent.pos.unwrap_or(self.pointer);
        for k in 1..=frames.max(1) {
            let t = k as f32 / frames.max(1) as f32;
            self.h.app.agent.pos = Some(from + (to - from) * (t * t * (3.0 - 2.0 * t)));
            self.h.frame();
        }
    }

    /// Typing: the drawn pointer steps aside, right of the value it points at.
    fn step_aside(&mut self) {
        if !self.aside && !self.system {
            self.aside = true;
            self.glide_drawn(self.pointer + egui::vec2(84.0, -26.0), 12);
        }
    }

    /// Before the pointer acts again, the drawn pointer comes back to it.
    fn step_back(&mut self) {
        if self.aside {
            self.aside = false;
            self.glide_drawn(self.pointer, 12);
        }
    }

    fn point(&mut self, v: &Value) -> Result<egui::Pos2, String> {
        let [x, y] = self.h.at(v)?;
        Ok(egui::pos2(x as f32, y as f32))
    }

    fn button(&mut self, pressed: bool) {
        let pos = self.pointer;
        self.h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        self.h.settle();
    }

    fn step(&mut self, s: &Value) -> Result<(), String> {
        let frames = |d: usize| s.get("frames").and_then(Value::as_u64).map_or(d, |n| n.clamp(1, 600) as usize);
        if let Some(on) = s.get("record").and_then(Value::as_bool) {
            self.tape.borrow_mut().recording = on;
        } else if let Some(c) = s.get("crop") {
            let n = |i: usize| c.get(i).and_then(Value::as_f64).map(|v| v as f32);
            self.tape.borrow_mut().crop = match (n(0), n(1), n(2), n(3)) {
                (Some(x0), Some(y0), Some(x1), Some(y1)) => Some(egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))),
                _ => None,
            };
        } else if let Some(z) = s.get("zoom") {
            // {"to": [x0, y0, x1, y1] | null, "ms": 600}: from what the clip shows now.
            let r = z.get("to").and_then(Value::as_array).and_then(|a| {
                let n: Vec<f32> = a.iter().filter_map(Value::as_f64).map(|v| v as f32).collect();
                (n.len() == 4).then(|| egui::Rect::from_min_max(egui::pos2(n[0], n[1]), egui::pos2(n[2], n[3])))
            });
            let ms = z.get("ms").and_then(Value::as_f64).unwrap_or(600.0).clamp(0.0, 10_000.0);
            let mut tape = self.tape.borrow_mut();
            let n = tape.frames.len();
            let whole = tape.crop.unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, self.h.size));
            let from = tape.zoom.and_then(|z| z.at(n, whole));
            tape.zoom = Some(Zoom { from, to: r, start: n, frames: (ms * 60.0 / 1000.0).round() as usize });
        } else if let Some(c) = s.get("caption") {
            self.tape.borrow_mut().caption = c.as_str().map(str::to_string);
        } else if let Some(at) = s.get("glide") {
            self.step_back();
            let to = self.point(at)?;
            self.glide(to, frames(GLIDE));
        } else if let Some(at) = s.get("press") {
            self.step_back();
            if !at.is_boolean() {
                let to = self.point(at)?;
                self.glide(to, frames(GLIDE));
            }
            self.button(true);
        } else if let Some(at) = s.get("drag_to") {
            let to = self.point(at)?;
            self.glide(to, frames(GLIDE));
        } else if s.get("release").is_some() {
            self.button(false);
        } else if let Some(n) = s.get("still").and_then(Value::as_u64) {
            self.h.frames(n.min(600) as usize);
        } else if let Some(ms) = s.get("pause").and_then(Value::as_f64) {
            self.h.frames((ms.clamp(0.0, 10_000.0) * 60.0 / 1000.0).round() as usize);
        } else {
            // A pointer step of the harness: the pointer glides to where it acts first.
            let target = if let Some(at) = ["click", "move", "hold"].into_iter().find_map(|k| s.get(k)) {
                Some(self.point(at)?)
            } else if let Some(d) = s.get("drag") {
                Some(self.point(d.get(0).unwrap_or(&Value::Null))?)
            } else if let Some(text) = s.get("widget").and_then(Value::as_str) {
                let found = match s.get("near").and_then(Value::as_str) {
                    Some(near) => self.h.widget_near(text, near),
                    None => self.h.widget(text),
                };
                found.map(|[x, y]| egui::pos2(x as f32, y as f32))
            } else {
                None
            };
            if target.is_some() {
                self.step_back();
            }
            if s.get("text").is_some() {
                self.step_aside();
            }
            if let Some(to) = target
                && (to - self.pointer).length() > 0.5
            {
                self.glide(to, GLIDE);
            }
            self.h.step(s)?;
            if let Some(d) = s.get("drag")
                && let Ok(end) = self.point(d.get(1).unwrap_or(&Value::Null))
            {
                self.pointer = end;
            }
        }
        match self.tape.borrow_mut().error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// What the frame's input was, for the keys and clicks the driver draws over the picture:
/// modifiers held, keys pressed (with their modifiers), text typed, button presses, buttons
/// held and the pointer, in points.
fn input(ctx: &egui::Context) -> Value {
    fn mods(m: egui::Modifiers) -> Value {
        // The harness's primary modifier is Command on macOS.
        json!({"cmd": m.command || m.mac_cmd, "ctrl": m.ctrl && !m.command && !m.mac_cmd, "alt": m.alt, "shift": m.shift})
    }
    ctx.input(|i| {
        let (mut keys, mut text, mut presses) = (Vec::new(), String::new(), Vec::new());
        for e in &i.events {
            match e {
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                    keys.push(json!({"key": key.name(), "mods": mods(*modifiers)}))
                }
                egui::Event::Text(t) => text.push_str(t),
                egui::Event::PointerButton { pos, button, pressed: true, .. } => {
                    presses.push(json!({"at": [pos.x, pos.y], "button": format!("{button:?}").to_lowercase()}))
                }
                _ => {}
            }
        }
        let mut held: Vec<&str> = i.keys_down.iter().map(|k| k.name()).collect();
        held.sort_unstable();
        json!({
            "mods": mods(i.modifiers),
            "keys": keys,
            "text": text,
            "held": held,
            "presses": presses,
            "down": {
                "primary": i.pointer.button_down(egui::PointerButton::Primary),
                "secondary": i.pointer.button_down(egui::PointerButton::Secondary),
            },
            "pointer": i.pointer.latest_pos().map(|p| [p.x, p.y]),
        })
    })
}

fn write_png(img: &image::RgbaImage, path: &Path) -> Result<(), String> {
    use image::ImageEncoder;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let enc = image::codecs::png::PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    enc.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())
}

/// Record the script `$SOLIDCRAFT_RECORD` into `$SOLIDCRAFT_RECORD_OUT`.
pub(crate) fn record(script: &Path, prefs: Option<&str>, out: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", script.display()))?;
    let n = |k: &str, i: usize, d: f64| v.get(k).and_then(|a| a.get(i)).and_then(Value::as_f64).unwrap_or(d) as f32;
    let size = egui::vec2(n("size", 0, 1600.0), n("size", 1, 1000.0));
    let ppp = v.get("ppp").and_then(Value::as_f64).unwrap_or(2.0) as f32;
    // Keys drawn over the clip: "hud" (also `true`), "band" or none (`false`); where the box sits.
    let keys = match v.get("keys") {
        Some(Value::Bool(false)) => "off",
        Some(Value::String(k)) if k == "band" || k == "off" => k.as_str(),
        _ => "hud",
    };
    let keys_at = if v.get("keys_at").and_then(Value::as_str) == Some("centre") { "centre" } else { "lower" };
    let system = v.get("pointer").and_then(Value::as_str) != Some("agent");
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for e in std::fs::read_dir(out).map_err(|e| e.to_string())?.flatten() {
        if e.file_name().to_string_lossy().starts_with("frame-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let mut rec = Recorder::new(size, ppp, system, prefs, out)?;
    for (i, s) in v.get("steps").and_then(Value::as_array).ok_or("a recording has `steps`")?.iter().enumerate() {
        rec.step(s).map_err(|e| format!("step {i}: {e}"))?;
    }
    let frames = std::mem::take(&mut rec.tape.borrow_mut().frames);
    let count = frames.len();
    std::fs::write(
        out.join("frames.json"),
        serde_json::to_string(&json!({
            "ppp": ppp,
            "keys": keys,
            "keys_at": keys_at,
            "pointer": if system { "system" } else { "agent" },
            "frames": frames,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(count)
}

/// Capture patch (private): every Preferences page whole, in a tall virtual window, as a PNG cut
/// to the window, an SVG of the same frame and the rects and handles the frame published.
pub(crate) fn prefs_pages(out: &Path, height: f32) -> Result<Vec<Value>, String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut pages = Vec::new();
    for (index, name) in crate::prefs::PAGES.iter().enumerate() {
        let slug = name.to_lowercase().replace(' ', "-");
        let mut rec = Recorder::new(egui::vec2(1600.0, height), 1.0, true, None, out)?;
        rec.h.app.ui.agent_cursor.show = false;
        crate::prefs::open_at(&mut rec.h.app, index);
        // The body takes the height the page drew a frame before, so let it settle.
        rec.h.frames(6);
        let window = crate::scenario::rect_of("dialog:window").ok_or("no Preferences window")?;
        // A page taller than the screen goes in tiles: the window moves up one tile at a time.
        let tile = height - 100.0;
        let tiles = (window.height() / tile).ceil().max(1.0) as usize;
        let mut images = Vec::new();
        for k in 0..tiles {
            let top = -(k as f32) * tile;
            rec.h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("sc_tall_window_top"), top));
            rec.h.frames(3);
            let w = crate::scenario::rect_of("dialog:window").ok_or("no Preferences window")?;
            let crop = egui::Rect::from_min_max(egui::pos2(w.min.x, w.min.y.max(0.0)), egui::pos2(w.max.x, w.max.y.min(tile)));
            {
                let mut tape = rec.tape.borrow_mut();
                tape.crop = Some(crop);
                tape.recording = true;
            }
            rec.h.frames(1);
            rec.tape.borrow_mut().recording = false;
            let frame = rec.tape.borrow().frames.last().and_then(|f| f["file"].as_str().map(str::to_string)).ok_or("no frame")?;
            let img = image::open(out.join(&frame)).map_err(|e| e.to_string())?.to_rgba8();
            let _ = std::fs::remove_file(out.join(&frame));
            images.push(img);
            if k + 1 < tiles {
                let svg = out.join(format!("{slug}.tile{k}.svg"));
                rec.h.call("ui.export_svg", json!({"path": svg.to_string_lossy()}));
            }
        }
        let width = images.iter().map(|i| i.width()).max().unwrap_or(0);
        let total: u32 = images.iter().map(|i| i.height()).sum();
        let mut whole = image::RgbaImage::new(width, total);
        let mut y = 0;
        for img in &images {
            image::imageops::replace(&mut whole, img, 0, i64::from(y));
            y += img.height();
        }
        let window = egui::Rect::from_min_size(egui::pos2(window.min.x, 0.0), window.size());
        let rects: Vec<Value> = crate::scenario::rects("")
            .into_iter()
            .map(|(n, r)| json!({"name": n, "rect": [r.min.x, r.min.y, r.max.x, r.max.y]}))
            .collect();
        let handles: Vec<Value> = crate::scenario::handles("").into_iter().map(|(n, p)| json!({"name": n, "at": [p.x, p.y]})).collect();
        write_png(&whole, &out.join(format!("{slug}.png")))?;
        let svg = out.join(format!("{slug}{}.svg", if tiles > 1 { format!(".tile{}", tiles - 1) } else { String::new() }));
        let exported = rec.h.call("ui.export_svg", json!({"path": svg.to_string_lossy()}));
        if exported["ok"] != true {
            return Err(format!("{name}: {exported}"));
        }
        let page = json!({
            "page": name,
            "slug": slug,
            "window": [window.min.x, window.min.y, window.max.x, window.max.y],
            "tiles": tiles,
            "tile": tile,
            "rects": rects,
            "handles": handles,
        });
        std::fs::write(out.join(format!("{slug}.json")), serde_json::to_string_pretty(&page).unwrap_or_default()).map_err(|e| e.to_string())?;
        pages.push(page);
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    /// Capture patch: `SOLIDCRAFT_TALL_PAGES=1 SOLIDCRAFT_PREFS_OUT=dir`, every Preferences page whole.
    #[test]
    #[ignore = "a tool: run with SOLIDCRAFT_TALL_PAGES and SOLIDCRAFT_PREFS_OUT set"]
    fn prefs_pages() {
        let Some(out) = std::env::var_os("SOLIDCRAFT_PREFS_OUT") else { panic!("set SOLIDCRAFT_PREFS_OUT") };
        let height = std::env::var("SOLIDCRAFT_PREFS_HEIGHT").ok().and_then(|h| h.parse().ok()).unwrap_or(12000.0);
        match super::prefs_pages(out.as_ref(), height) {
            Ok(pages) => eprintln!("captured {} pages", pages.len()),
            Err(e) => panic!("{e}"),
        }
    }

    /// Records `$SOLIDCRAFT_RECORD` (a recording script) into `$SOLIDCRAFT_RECORD_OUT`.
    #[test]
    #[ignore = "a tool: run with SOLIDCRAFT_RECORD set"]
    fn record() {
        let (Some(script), Some(out)) = (std::env::var_os("SOLIDCRAFT_RECORD"), std::env::var_os("SOLIDCRAFT_RECORD_OUT")) else {
            panic!("set SOLIDCRAFT_RECORD (script) and SOLIDCRAFT_RECORD_OUT (folder)");
        };
        // The desktop app's saved preferences (JSON), as `plan/recorder/record.sh` reads them.
        let prefs = std::env::var_os("SOLIDCRAFT_RECORD_PREFS").and_then(|p| std::fs::read_to_string(p).ok());
        let started = std::time::Instant::now();
        match super::record(script.as_ref(), prefs.as_deref(), out.as_ref()) {
            Ok(n) => eprintln!("recorded {n} frames in {:.1} s", started.elapsed().as_secs_f64()),
            Err(e) => panic!("{e}"),
        }
    }
}
