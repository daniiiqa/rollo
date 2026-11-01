//! Los pixeles de rollo: dónde va cada miniatura, las fotos (la pantalla, el
//! wallpaper, las ventanas capturadas) pasadas a Pixmap, y los colores del
//! look en tiny-skia.

use ratatui::style::Color;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

/// Los workspaces que se ven: 1…10, en 5 columnas.
pub const WS: usize = 10;
pub const COLS: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    pub fn lerp(&self, b: &Rect, t: f32) -> Rect {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Rect { x: l(self.x, b.x), y: l(self.y, b.y), w: l(self.w, b.w), h: l(self.h, b.h) }
    }
}

/// Las 10 miniaturas centradas en `w`×`h`, con la forma del monitor.
pub fn layout(w: f32, h: f32, aspect: f32, s: f32) -> Vec<Rect> {
    let (side, top, gap) = (64.0 * s, 72.0 * s, 26.0 * s);
    let rows = WS.div_ceil(COLS) as f32;
    let cols = COLS as f32;
    let mut cw = (w - 2.0 * side - (cols - 1.0) * gap) / cols;
    let mut ch = cw / aspect;
    if rows * ch + (rows - 1.0) * gap > h - 2.0 * top {
        ch = (h - 2.0 * top - (rows - 1.0) * gap) / rows;
        cw = ch * aspect;
    }
    let (tw, th) = (cols * cw + (cols - 1.0) * gap, rows * ch + (rows - 1.0) * gap);
    let (x0, y0) = ((w - tw) / 2.0, (h - th) / 2.0);
    (0..WS)
        .map(|i| {
            let (c, r) = ((i % COLS) as f32, (i / COLS) as f32);
            Rect { x: (x0 + c * (cw + gap)).round(), y: (y0 + r * (ch + gap)).round(), w: cw.round(), h: ch.round() }
        })
        .collect()
}

pub fn sk(c: Color, a: f32) -> tiny_skia::Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    };
    tiny_skia::Color::from_rgba8(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Un PPM binario (P6, el de grim y magick) como Pixmap opaco.
pub fn ppm(data: &[u8]) -> Option<Pixmap> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if data.get(i) == Some(&b'#') {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?);
    }
    if fields[0] != "P6" || fields[3] != "255" {
        return None;
    }
    let (w, h): (u32, u32) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let px = data.get(i + 1..i + 1 + (w * h * 3) as usize)?;
    let mut pix = Pixmap::new(w, h)?;
    for (d, s) in pix.data_mut().chunks_exact_mut(4).zip(px.chunks_exact(3)) {
        d.copy_from_slice(&[s[0], s[1], s[2], 255]);
    }
    Some(pix)
}

/// Una captura (BGRA/BGRX de Wayland, con su `stride`) achicada a lo mucho a
/// `max_w` de ancho, promediando: una ventana de 1920 en una miniatura de
/// 300 se ve limpia, no picada.
pub fn shrink_bgra(data: &[u8], w: u32, h: u32, stride: u32, max_w: u32) -> Option<Pixmap> {
    let k = w.div_ceil(max_w.max(1)).max(1);
    let (tw, th) = ((w / k).max(1), (h / k).max(1));
    let mut pix = Pixmap::new(tw, th)?;
    let n = (k * k) as u32;
    for (ty, row) in pix.data_mut().chunks_exact_mut(tw as usize * 4).enumerate() {
        for (tx, d) in row.chunks_exact_mut(4).enumerate() {
            let mut acc = [0u32; 3];
            for dy in 0..k {
                let line = ((ty as u32 * k + dy) * stride) as usize;
                for dx in 0..k {
                    let p = line + ((tx as u32 * k + dx) * 4) as usize;
                    acc[0] += data[p + 2] as u32;
                    acc[1] += data[p + 1] as u32;
                    acc[2] += data[p] as u32;
                }
            }
            d.copy_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, 255]);
        }
    }
    Some(pix)
}

/// `src` estirada para llenar `r`.
pub fn blit(dst: &mut Pixmap, src: &Pixmap, r: &Rect, opacity: f32) {
    if r.w < 1.0 || r.h < 1.0 || opacity <= 0.0 {
        return;
    }
    let (sx, sy) = (r.w / src.width() as f32, r.h / src.height() as f32);
    let paint = PixmapPaint { opacity: opacity.min(1.0), quality: FilterQuality::Bilinear, ..Default::default() };
    dst.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_row(sx, 0.0, 0.0, sy, r.x, r.y), None);
}

/// El Pixmap (RGBA premultiplicado) en un buffer de Wayland (BGRA).
pub fn to_wayland(pix: &Pixmap, canvas: &mut [u8]) {
    for (dst, src) in canvas.chunks_exact_mut(4).zip(pix.data().chunks_exact(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
