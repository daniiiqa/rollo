//! La superficie de rollo y todo lo que pasa en ella. Al abrir: captura cada
//! ventana (por su dirección, con el protocolo de Hyprland: sale aunque esté
//! en otro workspace), toma una foto de la pantalla y arma las 10 miniaturas;
//! luego la foto se encoge hasta su miniatura (el zoom) y aparecen las demás.
//! Al elegir, la miniatura elegida crece hasta llenar la pantalla y ahí se
//! cambia de workspace, sin la animación de Hyprland (ya se hizo esta).

use crate::draw::{self, COLS, Rect, WS, blit, ease_out, sk};
use crate::hypr::{self, Mon, Win};
use crate::proto::hyprland_toplevel_export_frame_v1::{self as frame_v1, HyprlandToplevelExportFrameV1 as Frame};
use crate::proto::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1 as Manager;
use disco::icon::Icons;
use disco::look::Look;
use disco::paint::{Painter, rounded, stroke_rounded};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect as CellRect;
use ratatui::style::{Style, Stylize};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop::channel::{Event, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle, WEnum};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::{Buffer, SlotPool}},
};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;

const OPEN: f32 = 0.24;
const CLOSE: f32 = 0.2;
/// Lo que espera a las capturas antes de abrir sin las que falten.
const CAPTURE_WAIT: Duration = Duration::from_millis(400);
const BTN_LEFT: u32 = 0x110;

/// Una captura en camino.
struct Cap {
    frame: Frame,
    fmt: Option<(wl_shm::Format, u32, u32, u32)>,
    buf: Option<Buffer>,
    done: bool,
}

pub struct CapId(u64);
pub struct Mgr;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Opening(Instant),
    Open,
    /// hacia el workspace `ws` (índice), y la ventana a enfocar si la hay
    Closing(Instant, usize, Option<u64>),
}

/// Una ventana que se está arrastrando.
struct Drag {
    addr: u64,
    from: usize,
    /// dónde se agarró, relativo a su esquina, y su tamaño en la miniatura
    grab: (f32, f32),
    size: (f32, f32),
    start: (f32, f32),
    moving: bool,
}

struct App {
    registry: RegistryState,
    seat: SeatState,
    output: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<App>,
    manager: Option<Manager>,
    manager_v: u32,
    cap_pool: SlotPool,
    caps: HashMap<u64, Cap>,
    shapes: Option<CursorShapeManager>,
    pointer: Option<wl_pointer::WlPointer>,
    shape_dev: Option<WpCursorShapeDeviceV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    mon: Mon,
    mons: Vec<Mon>,
    wins: Vec<Win>,
    look: Look,
    painter: Painter,
    icons: Icons,
    /// la pantalla de ahora (pixeles físicos) y el wallpaper a tamaño de miniatura
    shot: Option<Pixmap>,
    wall: Option<Pixmap>,
    thumbs: HashMap<u64, Pixmap>,
    /// el contenido de cada miniatura (sin borde ni número)
    cells: Vec<Pixmap>,
    /// el activo ya cambió (se le movió una ventana): su foto ya no sirve
    active_dirty: bool,

    layer: Option<LayerSurface>,
    pool: Option<SlotPool>,
    size: (u32, u32),
    scale: i32,
    rects: Vec<Rect>,
    frame_pending: bool,
    /// algo cambió mientras se esperaba el cuadro: dibujar al llegar
    dirty: bool,
    /// la vista abierta ya dibujada (fondo, miniaturas, bordes, números):
    /// arrastrar solo pinta la ventana encima de esto. None: rehacerla
    scene: Option<Pixmap>,
    phase: Phase,
    sel: usize,
    hover: Option<usize>,
    mouse: (f32, f32),
    drag: Option<Drag>,
    exit: bool,
}

impl App {
    fn active(&self) -> usize {
        (self.mon.active_ws.clamp(1, WS as i32) - 1) as usize
    }

    fn s(&self) -> f32 {
        self.scale.max(1) as f32
    }

    // ─── capturas ───────────────────────────────────────────────────────────

    fn capture_all(&mut self) {
        let Some(mgr) = &self.manager else { return };
        for w in &self.wins {
            let frame = mgr.capture_toplevel(0, w.addr as u32, &self.qh, CapId(w.addr));
            self.caps.insert(w.addr, Cap { frame, fmt: None, buf: None, done: false });
        }
    }

    fn capturing(&self) -> bool {
        self.caps.values().any(|c| !c.done)
    }

    fn copy(&mut self, addr: u64) {
        let Some(cap) = self.caps.get_mut(&addr) else { return };
        let Some((fmt, w, h, stride)) = cap.fmt else {
            cap.done = true;
            return;
        };
        match self.cap_pool.create_buffer(w as i32, h as i32, stride as i32, fmt) {
            Ok((buf, _)) => {
                cap.frame.copy(buf.wl_buffer(), 1);
                cap.buf = Some(buf);
            }
            Err(_) => cap.done = true,
        }
    }

    fn on_capture(&mut self, addr: u64, ev: frame_v1::Event) {
        match ev {
            frame_v1::Event::Buffer { format: WEnum::Value(f), width, height, stride } => {
                if let Some(cap) = self.caps.get_mut(&addr)
                    && cap.fmt.is_none()
                    && matches!(f, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)
                {
                    cap.fmt = Some((f, width, height, stride));
                    if self.manager_v < 2 {
                        self.copy(addr);
                    }
                }
            }
            frame_v1::Event::BufferDone => self.copy(addr),
            frame_v1::Event::Ready { .. } => {
                let max_w = (self.rects.first().map_or(400.0, |r| r.w) as u32).max(64);
                if let Some(cap) = self.caps.get_mut(&addr) {
                    if let (Some(buf), Some((_, w, h, stride))) = (&cap.buf, cap.fmt)
                        && let Some(data) = buf.canvas(&mut self.cap_pool)
                        && let Some(pix) = draw::shrink_bgra(data, w, h, stride, max_w)
                    {
                        self.thumbs.insert(addr, pix);
                    }
                    cap.done = true;
                    cap.frame.destroy();
                }
            }
            frame_v1::Event::Failed => {
                if let Some(cap) = self.caps.get_mut(&addr) {
                    cap.done = true;
                    cap.frame.destroy();
                }
            }
            _ => {}
        }
    }

    // ─── miniaturas ─────────────────────────────────────────────────────────

    /// Dónde cae una ventana dentro de la miniatura `r` (su monitor a escala).
    fn win_rect(&self, w: &Win, r: &Rect) -> Rect {
        let m = self.mons.iter().find(|m| m.id == w.mon).unwrap_or(&self.mon);
        let (kx, ky) = (r.w / m.w, r.h / m.h);
        Rect { x: r.x + (w.x - m.x) * kx, y: r.y + (w.y - m.y) * ky, w: w.w * kx, h: w.h * ky }
    }

    /// Las ventanas del workspace `i` en el orden en que se pintan (la
    /// enfocada y las flotantes encima).
    fn ws_wins(&self, i: usize) -> Vec<&Win> {
        let mut v: Vec<&Win> = self.wins.iter().filter(|w| w.ws == i as i32 + 1).collect();
        v.sort_by_key(|w| (w.floating, -w.focus));
        v
    }

    fn render_cell(&mut self, i: usize) -> Pixmap {
        let r0 = self.rects[i];
        let local = Rect { x: 0.0, y: 0.0, ..r0 };
        let mut pix = Pixmap::new(r0.w as u32, r0.h as u32).expect("miniatura");
        let t = self.look.theme;
        let use_shot = i == self.active() && !self.active_dirty;
        match (&self.shot, &self.wall) {
            (Some(shot), _) if use_shot => blit(&mut pix, shot, &local, 1.0),
            (_, Some(wall)) => blit(&mut pix, wall, &local, 1.0),
            _ => pix.fill(sk(t.base, 1.0)),
        }
        if use_shot {
            return pix;
        }
        let wins: Vec<(Rect, u64, String)> = self.ws_wins(i).iter().map(|w| (self.win_rect(w, &local), w.addr, w.class.clone())).collect();
        if wins.is_empty() {
            // vacío: el wallpaper apagado
            pix.fill_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, r0.w, r0.h).unwrap(), &paint(sk(t.base, 0.45)), tiny_skia::Transform::identity(), None);
        }
        let s = self.s();
        for (wr, addr, class) in wins {
            match self.thumbs.get(&addr) {
                Some(th) => blit(&mut pix, th, &wr, 1.0),
                None => {
                    // sin captura: su ícono sobre el fondo del look
                    rounded(&mut pix, wr.x, wr.y, wr.w, wr.h, 0.0, t.base, 0.92);
                    if let Some(icon) = self.icons.get(&class.to_lowercase()) {
                        let side = (wr.w.min(wr.h) * 0.4).min(64.0 * s);
                        blit(&mut pix, &icon.pix, &Rect { x: wr.x + (wr.w - side) / 2.0, y: wr.y + (wr.h - side) / 2.0, w: side, h: side }, 1.0);
                    }
                }
            }
            stroke_rounded(&mut pix, wr.x, wr.y, wr.w, wr.h, 0.0, s.max(1.0), t.border, 0.9, None);
        }
        pix
    }

    fn render_cells(&mut self) {
        self.cells = (0..WS).map(|i| self.render_cell(i)).collect();
    }

    // ─── superficie ─────────────────────────────────────────────────────────

    fn open_layer(&mut self) {
        let output = self.output.outputs().find(|o| self.output.info(o).and_then(|i| i.name).as_deref() == Some(self.mon.name.as_str()));
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("rollo"), output.as_ref());
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        layer.commit();
        self.layer = Some(layer);
    }

    fn close(&mut self, ws: usize, window: Option<u64>) {
        if matches!(self.phase, Phase::Closing(..)) {
            return;
        }
        self.drag = None;
        self.phase = Phase::Closing(Instant::now(), ws, window);
        self.redraw();
    }

    /// Dibujar ya, o al siguiente cuadro si el compositor no ha pedido otro:
    /// el mouse manda cientos de eventos por segundo y cada uno no puede ser
    /// un cuadro (se encolaban y el arrastre iba a 2 fps).
    fn redraw(&mut self) {
        if self.frame_pending {
            self.dirty = true;
        } else {
            self.draw();
        }
    }

    /// Cambió lo que se ve quieto (hover, selección, una ventana movida).
    fn invalidate(&mut self) {
        self.scene = None;
    }

    fn draw(&mut self) {
        let Some(layer) = self.layer.clone() else { return };
        let s = self.scale.max(1);
        let (w, h) = (self.size.0 as i32 * s, self.size.1 as i32 * s);
        if w <= 0 || h <= 0 || self.cells.len() != WS {
            return;
        }
        let Some(mut pix) = Pixmap::new(w as u32, h as u32) else { return };
        let full = Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
        let t = self.look.theme;
        let active = self.active();
        let mut animating = false;
        let mut finished = None;
        match self.phase {
            Phase::Opening(t0) => {
                let e = ease_out(t0.elapsed().as_secs_f32() / OPEN);
                animating = e < 1.0;
                if !animating {
                    self.phase = Phase::Open;
                }
                pix.fill(sk(t.base, 0.78 * e));
                for i in (0..WS).filter(|&i| i != active) {
                    blit(&mut pix, &self.cells[i], &self.rects[i], e);
                }
                // la pantalla de ahora se encoge hasta su miniatura
                let r = full.lerp(&self.rects[active], e);
                match &self.shot {
                    Some(shot) => blit(&mut pix, shot, &r, 1.0),
                    None => blit(&mut pix, &self.cells[active], &r, 1.0),
                }
                if !animating {
                    self.chrome(&mut pix);
                }
            }
            Phase::Open => {
                if self.scene.as_ref().is_none_or(|sc| sc.width() != pix.width() || sc.height() != pix.height()) {
                    let mut sc = pix.clone();
                    sc.fill(sk(t.base, 0.78));
                    for i in 0..WS {
                        blit(&mut sc, &self.cells[i], &self.rects[i], 1.0);
                    }
                    self.chrome(&mut sc);
                    self.scene = Some(sc);
                }
                pix.data_mut().copy_from_slice(self.scene.as_ref().expect("escena").data());
                if let Some(d) = &self.drag
                    && d.moving
                    && let Some(th) = self.thumbs.get(&d.addr)
                {
                    let r = Rect { x: self.mouse.0 * s as f32 - d.grab.0, y: self.mouse.1 * s as f32 - d.grab.1, w: d.size.0, h: d.size.1 };
                    blit(&mut pix, th, &r, 0.9);
                    stroke_rounded(&mut pix, r.x, r.y, r.w, r.h, 0.0, 2.0 * s as f32, t.primary, 1.0, None);
                }
            }
            Phase::Closing(t0, ws, window) => {
                let e = ease_out(t0.elapsed().as_secs_f32() / CLOSE);
                animating = e < 1.0;
                pix.fill(sk(t.base, 0.78 * (1.0 - e)));
                for i in (0..WS).filter(|&i| i != ws) {
                    blit(&mut pix, &self.cells[i], &self.rects[i], 1.0 - e);
                }
                // la elegida crece hasta llenar la pantalla
                let r = self.rects[ws].lerp(&full, e);
                match &self.shot {
                    Some(shot) if ws == active && !self.active_dirty => blit(&mut pix, shot, &r, 1.0),
                    _ => blit(&mut pix, &self.cells[ws], &r, 1.0),
                }
                if !animating {
                    finished = Some((ws, window));
                }
            }
        }

        let pool = match &mut self.pool {
            Some(p) => p,
            None => self.pool.insert(SlotPool::new((w * h * 4) as usize, &self.shm).expect("memoria compartida")),
        };
        let Ok((buffer, canvas)) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        draw::to_wayland(&pix, canvas);
        let surface = layer.wl_surface();
        surface.set_buffer_scale(s);
        surface.damage_buffer(0, 0, w, h);
        // siempre un aviso de cuadro: así se sabe cuándo se puede dibujar el
        // siguiente (animando, o si mientras tanto se movió el mouse)
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
        self.dirty = animating;
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
        if let Some((ws, window)) = finished {
            // ya tapa la pantalla con la miniatura: el cambio de verdad,
            // debajo y sin animar, y fuera
            if ws != active || window.is_some() {
                hypr::jump(ws as i32 + 1, window);
            }
            self.exit = true;
        }
    }

    /// Bordes y números de las miniaturas.
    fn chrome(&mut self, pix: &mut Pixmap) {
        let t = self.look.theme;
        let s = self.s();
        let active = self.active();
        let radius = if is_rounded(&self.look) { 10.0 * s } else { 0.0 };
        for (i, r) in self.rects.clone().iter().enumerate() {
            let (c, width) = if i == self.sel {
                (t.primary, 3.0 * s)
            } else if Some(i) == self.hover {
                (t.accent, 2.0 * s)
            } else if i == active {
                (t.primary, 1.5 * s)
            } else {
                (t.border, 1.5 * s)
            };
            stroke_rounded(pix, r.x - width, r.y - width, r.w + 2.0 * width, r.h + 2.0 * width, radius, width, c, 1.0, None);
            // el número, en un recuadrito como los de la barra
            let label = format!(" {} ", (i + 1) % 10);
            let mut cells = Cells::empty(CellRect::new(0, 0, label.chars().count() as u16, 1));
            let style = if i == active { Style::new().bg(t.primary).fg(t.base).bold() } else { Style::new().bg(t.base).fg(t.fg).bold() };
            cells.set_string(0, 0, &label, style);
            self.painter.cells(pix, &cells, &self.look, r.x + 6.0 * s, r.y + 6.0 * s, s);
        }
    }

    // ─── entrada ────────────────────────────────────────────────────────────

    fn cell_at(&self, x: f32, y: f32) -> Option<usize> {
        let s = self.s();
        self.rects.iter().position(|r| r.contains(x * s, y * s))
    }

    fn window_at(&self, i: usize, x: f32, y: f32) -> Option<(u64, Rect)> {
        let s = self.s();
        let r = self.rects[i];
        self.ws_wins(i).iter().rev().map(|w| (w.addr, self.win_rect(w, &r))).find(|(_, wr)| wr.contains(x * s, y * s))
    }

    fn press(&mut self, x: f32, y: f32) {
        let s = self.s();
        let Some(i) = self.cell_at(x, y) else {
            let a = self.active();
            self.close(a, None);
            return;
        };
        let win = self.window_at(i, x, y);
        self.drag = Some(match win {
            Some((addr, wr)) => Drag { addr, from: i, grab: (x * s - wr.x, y * s - wr.y), size: (wr.w, wr.h), start: (x, y), moving: false },
            None => Drag { addr: 0, from: i, grab: (0.0, 0.0), size: (0.0, 0.0), start: (x, y), moving: false },
        });
    }

    fn release(&mut self, x: f32, y: f32) {
        let Some(d) = self.drag.take() else { return };
        if !d.moving {
            // un clic: ir al workspace, o a la ventana
            let window = (d.addr != 0).then_some(d.addr);
            self.close(d.from, window);
            return;
        }
        match self.cell_at(x, y) {
            Some(to) if to != d.from => {
                hypr::move_to(d.addr, to as i32 + 1);
                // Hyprland vuelve a acomodar los dos workspaces: releerlos
                std::thread::sleep(Duration::from_millis(80));
                self.wins = hypr::windows();
                if d.from == self.active() || to == self.active() {
                    self.active_dirty = true;
                }
                self.cells[d.from] = self.render_cell(d.from);
                self.cells[to] = self.render_cell(to);
                if self.active_dirty {
                    let a = self.active();
                    self.cells[a] = self.render_cell(a);
                }
                self.sel = to;
                self.invalidate();
            }
            _ => {}
        }
        self.redraw();
    }

    fn motion(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        let hover = self.cell_at(x, y);
        let mut dirty = hover != self.hover;
        if dirty {
            self.invalidate();
        }
        self.hover = hover;
        if let Some(d) = &mut self.drag
            && d.addr != 0
        {
            if !d.moving && (x - d.start.0).hypot(y - d.start.1) > 6.0 {
                d.moving = true;
            }
            dirty |= d.moving;
        }
        if dirty && self.phase == Phase::Open {
            self.redraw();
        }
    }

    fn key(&mut self, k: Keysym) {
        let (c, r) = (self.sel % COLS, self.sel / COLS);
        let rows = WS / COLS;
        let sel = match k {
            Keysym::Escape => {
                let a = self.active();
                return self.close(a, None);
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => return self.close(self.sel, None),
            Keysym::Left | Keysym::h => r * COLS + (c + COLS - 1) % COLS,
            Keysym::Right | Keysym::l | Keysym::Tab => r * COLS + (c + 1) % COLS,
            Keysym::Up | Keysym::k => ((r + rows - 1) % rows) * COLS + c,
            Keysym::Down | Keysym::j => ((r + 1) % rows) * COLS + c,
            _ => {
                let raw = k.raw();
                if (Keysym::_0.raw()..=Keysym::_9.raw()).contains(&raw) {
                    let n = (raw - Keysym::_0.raw()) as usize;
                    return self.close(if n == 0 { 9 } else { n - 1 }, None);
                }
                return;
            }
        };
        self.sel = sel;
        self.invalidate();
        if self.phase == Phase::Open {
            self.redraw();
        }
    }
}

fn paint(c: tiny_skia::Color) -> tiny_skia::Paint<'static> {
    let mut p = tiny_skia::Paint::default();
    p.set_color(c);
    p
}

/// Si el look va redondeado: lo dice el borde de su recuadro (╭).
fn is_rounded(look: &Look) -> bool {
    let mut b = Cells::empty(CellRect::new(0, 0, 3, 3));
    ratatui::widgets::Widget::render(look.block(false), CellRect::new(0, 0, 3, 3), &mut b);
    b[(0, 0)].symbol() == "╭"
}

pub fn run(sigs: libc::sigset_t) -> Result<(), String> {
    let mons = hypr::monitors();
    let mon_i = mons.iter().position(|m| m.focused).ok_or("no hay monitor enfocado")?;
    let wins = hypr::windows();
    let m = &mons[mon_i];
    let s_int = m.scale.round().max(1.0) as i32;
    let s = s_int as f32;
    let (bw, bh) = (m.w * s, m.h * s);
    let rects = draw::layout(bw, bh, m.w / m.h, s);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, std::path::PathBuf::from);

    // la foto de la pantalla (para el zoom y la miniatura del activo) y el
    // wallpaper a tamaño de miniatura, en paralelo
    let shot_path = runtime.join("rollo-shot.ppm");
    let grim = Command::new("grim").args(["-t", "ppm", "-o", &m.name]).arg(&shot_path).spawn();
    let wall = hypr::wallpaper(&m.name).and_then(|p| wall_thumb(&p, rects[0].w as u32, rects[0].h as u32));
    let shot = grim.ok().and_then(|mut c| c.wait().ok()).and_then(|_| std::fs::read(&shot_path).ok()).and_then(|d| draw::ppm(&d));
    let _ = std::fs::remove_file(&shot_path);

    let conn = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
    let (globals, queue) = registry_queue_init(&conn).map_err(|e| format!("Wayland: {e}"))?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).map_err(|e| e.to_string())?;

    // `rollo` otra vez: cerrar
    let (tx, rx) = channel::<()>();
    std::thread::spawn(move || {
        loop {
            let mut sig = 0;
            if unsafe { libc::sigwait(&sigs, &mut sig) } == 0 && tx.send(()).is_err() {
                return;
            }
        }
    });
    handle
        .insert_source(rx, |ev, _, app: &mut App| {
            if let Event::Msg(()) = ev {
                let a = app.active();
                app.close(a, None);
            }
        })
        .map_err(|e| e.to_string())?;

    let manager: Option<Manager> = globals.bind(&qh, 1..=2, Mgr).ok();
    let manager_v = manager.as_ref().map_or(0, |m| smithay_client_toolkit::reexports::client::Proxy::version(m));
    let shm = Shm::bind(&globals, &qh).map_err(|_| "el compositor no tiene wl_shm")?;
    let cap_pool = SlotPool::new(4 << 20, &shm).map_err(|e| e.to_string())?;
    let shapes = CursorShapeManager::bind(&globals, &qh).ok();
    let mut app = App {
        registry: RegistryState::new(&globals),
        seat: SeatState::new(&globals, &qh),
        output: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh).map_err(|_| "el compositor no tiene wl_compositor")?,
        layer_shell: LayerShell::bind(&globals, &qh).map_err(|_| "el compositor no tiene layer-shell")?,
        shm,
        qh,
        manager,
        manager_v,
        cap_pool,
        caps: HashMap::new(),
        shapes,
        pointer: None,
        shape_dev: None,
        keyboard: None,
        mon: mons.into_iter().nth(mon_i).expect("monitor"),
        mons: hypr::monitors(),
        wins,
        look: Look::load("rollo"),
        painter: Painter::new(),
        icons: Icons::new(),
        shot,
        wall,
        thumbs: HashMap::new(),
        cells: Vec::new(),
        active_dirty: false,
        layer: None,
        pool: None,
        size: (0, 0),
        scale: s_int,
        rects,
        frame_pending: false,
        dirty: false,
        scene: None,
        phase: Phase::Open,
        sel: 0,
        hover: None,
        mouse: (0.0, 0.0),
        drag: None,
        exit: false,
    };
    app.sel = app.active();

    // las capturas, todas a la vez; lo que no llegue a tiempo sale con su ícono
    app.capture_all();
    let t0 = Instant::now();
    while app.capturing() && t0.elapsed() < CAPTURE_WAIT {
        event_loop.dispatch(Duration::from_millis(10), &mut app).map_err(|e| e.to_string())?;
    }
    app.render_cells();
    app.open_layer();
    app.phase = Phase::Opening(Instant::now());
    while !app.exit {
        event_loop.dispatch(Duration::from_millis(100), &mut app).map_err(|e| e.to_string())?;
    }
    let _ = conn.flush();
    Ok(())
}

/// El wallpaper a tamaño de miniatura (recortado como awww), guardado por
/// ruta, fecha y tamaño en ~/.cache/rollo.
fn wall_thumb(path: &str, w: u32, h: u32) -> Option<Pixmap> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::PathBuf::from(home).join(".cache/rollo");
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut key = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(path, mtime, w, h), &mut key);
    let out = dir.join(format!("{:016x}.ppm", std::hash::Hasher::finish(&key)));
    if !out.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        let geo = format!("{w}x{h}");
        Command::new("magick")
            .arg(format!("{path}[0]"))
            .args(["-resize", &format!("{geo}^"), "-gravity", "center", "-extent", &geo])
            .arg(format!("ppm:{}", out.display()))
            .status()
            .ok()?;
    }
    draw::ppm(&std::fs::read(out).ok()?)
}

// ─── Wayland ────────────────────────────────────────────────────────────────

impl Dispatch2<Manager, App> for Mgr {
    fn event(&self, _: &mut App, _: &Manager, _: <Manager as smithay_client_toolkit::reexports::client::Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<Frame, App> for CapId {
    fn event(&self, app: &mut App, _: &Frame, ev: frame_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        app.on_capture(self.0, ev);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.draw();
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        if cfg.new_size.0 > 0 && cfg.new_size.1 > 0 && cfg.new_size != self.size {
            self.size = cfg.new_size;
            self.pool = None;
        }
        if let Phase::Opening(_) = self.phase {
            // el reloj del zoom arranca con el primer cuadro
            self.phase = Phase::Opening(Instant::now());
        }
        self.redraw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat.get_pointer(qh, &seat).ok();
            if let (Some(p), Some(shapes)) = (&self.pointer, &self.shapes) {
                self.shape_dev = Some(shapes.get_shape_device(p, qh));
            }
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat.get_keyboard(qh, &seat, None).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            let (x, y) = (e.position.0 as f32, e.position.1 as f32);
            match e.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(dev) = &self.shape_dev {
                        dev.set_shape(serial, Shape::Default);
                    }
                    self.motion(x, y);
                }
                PointerEventKind::Motion { .. } => self.motion(x, y),
                PointerEventKind::Press { button: BTN_LEFT, .. } if self.phase == Phase::Open => self.press(x, y),
                PointerEventKind::Release { button: BTN_LEFT, .. } if self.phase == Phase::Open => self.release(x, y),
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        if !matches!(self.phase, Phase::Closing(..)) {
            self.key(event.keysym);
        }
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);
