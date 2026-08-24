//! dgpui — Dragon's own immediate-mode UI kit.
//! state -> paint list -> raster(tiny-skia) -> present(softbuffer)
//! Text shaped by cosmic-text, blitted glyph pixels straight into the pixmap.

pub const EMBER: [u8; 3] = [255, 99, 71];
pub const FLAME: [u8; 3] = [255, 152, 74];
pub const GOLD: [u8; 3] = [255, 205, 112];
pub const JADE: [u8; 3] = [105, 210, 150];
pub const SKY: [u8; 3] = [108, 170, 245];
pub const VIOLET: [u8; 3] = [172, 140, 250];
pub const BLOOD: [u8; 3] = [240, 84, 84];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: ThemeName,
    pub bg: [u8; 3],
    pub panel: [u8; 3],
    pub panel2: [u8; 3],
    pub line: [u8; 3],
    pub ash: [u8; 3],
    pub bone: [u8; 3],
}

impl Theme {
    pub fn new(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => Self {
                name,
                bg: [19, 17, 24],
                panel: [30, 28, 35],
                panel2: [38, 35, 46],
                line: [58, 53, 68],
                ash: [138, 135, 144],
                bone: [236, 234, 228],
            },
            ThemeName::Light => Self {
                name,
                bg: [244, 242, 246],
                panel: [255, 255, 255],
                panel2: [241, 239, 245],
                line: [201, 196, 212],
                ash: [109, 104, 117],
                bone: [36, 34, 43],
            },
        }
    }

    pub fn scale_hint(&self) -> [u8; 3] {
        [
            self.ash[0].saturating_sub(24),
            self.ash[1].saturating_sub(24),
            self.ash[2].saturating_sub(24),
        ]
    }
}

#[derive(Clone)]
pub struct Hit {
    pub rect: (i32, i32, u32, u32),
    pub action: String,
}

pub struct Frame<'a> {
    pub pix: &'a mut tiny_skia::Pixmap,
    pub font: &'a mut cosmic_text::FontSystem,
    pub swash: &'a mut cosmic_text::SwashCache,
    pub theme: Theme,
    pub hits: Vec<Hit>,
}

fn rgb(c: [u8; 3]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], 255)
}
fn rgba(c: [u8; 3], a: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], (a * 255.0) as u8)
}

/// Rounded-rect path (quadratic corners - portable).
fn rr_path(b: &mut tiny_skia::PathBuilder, x: f32, y: f32, w: f32, h: f32, r0: f32) {
    let r = r0.min(w / 2.0).min(h / 2.0);
    b.move_to(x + r, y);
    b.line_to(x + w - r, y);
    b.quad_to(x + w, y, x + w, y + r);
    b.line_to(x + w, y + h - r);
    b.quad_to(x + w, y + h, x + w - r, y + h);
    b.line_to(x + r, y + h);
    b.quad_to(x, y + h, x, y + h - r);
    b.line_to(x, y + r);
    b.quad_to(x, y, x + r, y);
    b.close();
}

impl<'a> Frame<'a> {
    pub fn fill_all(&mut self, c: [u8; 3]) {
        self.pix.fill(rgb(c));
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: tiny_skia::Color) {
        if w <= 0 || h <= 0 { return; }
        let Some(r) = tiny_skia::Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        if let Some(p) = tiny_skia::PathBuilder::from_rect(r) {
            self.pix.fill_path(&p, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
        }
    }

    pub fn rounded(&mut self, x: i32, y: i32, w: i32, h: i32, radius: f32, color: tiny_skia::Color) {
        if w <= 0 || h <= 0 { return; }
        let mut pb = tiny_skia::PathBuilder::new();
        rr_path(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        self.pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }

    pub fn gradient_rounded(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        radius: f32,
        from: [u8; 3],
        to: [u8; 3],
    ) {
        if w <= 0 || h <= 0 { return; }
        let mut pb = tiny_skia::PathBuilder::new();
        rr_path(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let shader = tiny_skia::LinearGradient::new(
            tiny_skia::Point { x: x as f32, y: y as f32 },
            tiny_skia::Point { x: (x + w) as f32, y: (y + h) as f32 },
            vec![
                tiny_skia::GradientStop::new(0.0, rgb(from)),
                tiny_skia::GradientStop::new(1.0, rgb(to)),
            ],
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        );
        let mut paint = tiny_skia::Paint::default();
        if let Some(sh) = shader {
            paint.shader = sh;
        } else {
            paint.shader = tiny_skia::Shader::SolidColor(rgb(from));
        }
        self.pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }

    pub fn outline(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        radius: f32,
        width: f32,
        color: tiny_skia::Color,
    ) {
        if w <= 0 || h <= 0 { return; }
        let mut pb = tiny_skia::PathBuilder::new();
        rr_path(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        let stroke = tiny_skia::Stroke { width, ..tiny_skia::Stroke::default() };
        self.pix.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
    }

    /// Soft shadow rising from `y_bottom` upward, fading out.
    pub fn bottom_shadow(&mut self, x: i32, w: i32, y_bottom: i32, h: i32, dark: bool) {
        for i in 0..h.max(1) {
            let t = 1.0 - (i as f32 / h.max(1) as f32);
            let a = (t * t * 140.0) as u8;
            let c = if dark {
                tiny_skia::Color::from_rgba8(0, 0, 0, a)
            } else {
                tiny_skia::Color::from_rgba8(120, 110, 140, (a / 2) as u8)
            };
            self.rect(x, y_bottom - i - 1, w, 1, c);
        }
    }

    /// Shape + draw text wrapped to max_w; returns height used.
    pub fn text(
        &mut self,
        x: i32,
        y: i32,
        max_w: i32,
        size: f32,
        color: [u8; 3],
        s: &str,
        bold: bool,
    ) -> i32 {
        if s.is_empty() || max_w <= 0 {
            return 0;
        }
        let metrics = cosmic_text::Metrics::new(size, size * 1.42);
        let mut buffer = cosmic_text::Buffer::new(self.font, metrics);
        buffer.set_size(self.font, Some(max_w as f32), None);
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(if bold { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL });
        buffer.set_text(self.font, s, attrs, cosmic_text::Shaping::Advanced);
        buffer.shape_until_scroll(self.font, false);

        let height: i32 = buffer
            .layout_runs()
            .map(|r| r.line_height as i32)
            .sum::<i32>()
            .max(size as i32);

        let default_col =
            cosmic_text::Color::rgba(color[0], color[1], color[2], 255);
        let Self { pix, font, swash, .. } = self;
        buffer.draw(font, swash, default_col, |px, py, pw, ph, col| {
            let alpha = col.a();
            if alpha == 0 {
                return;
            }
            let gx0 = px + x;
            let gy0 = py + y;
            for dy in 0..ph {
                for dx in 0..pw {
                    let gx = gx0 + dx;
                    let gy = gy0 + dy;
                    if gx < 0 || gy < 0 {
                        continue;
                    }
                    let idx = (gy as usize * pix.width() as usize + gx as usize) * 4;
                    if idx + 3 >= pix.data().len() {
                        continue;
                    }
                    unsafe {
                        let data = pix.data_mut();
                        let a = alpha as u32;
                        let inv = 255 - a;
                        data[idx] = ((color[0] as u32 * a + data[idx] as u32 * inv) / 255) as u8;
                        data[idx + 1] =
                            ((color[1] as u32 * a + data[idx + 1] as u32 * inv) / 255) as u8;
                        data[idx + 2] =
                            ((color[2] as u32 * a + data[idx + 2] as u32 * inv) / 255) as u8;
                        data[idx + 3] = data[idx + 3].max(a.min(255) as u8);
                    }
                }
            }
        });
        height
    }

    pub fn measure(&mut self, max_w: i32, size: f32, s: &str, bold: bool) -> i32 {
        if s.is_empty() || max_w <= 0 {
            return 0;
        }
        let metrics = cosmic_text::Metrics::new(size, size * 1.42);
        let mut buffer = cosmic_text::Buffer::new(self.font, metrics);
        buffer.set_size(self.font, Some(max_w as f32), None);
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(if bold { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL });
        buffer.set_text(self.font, s, attrs, cosmic_text::Shaping::Advanced);
        buffer.shape_until_scroll(self.font, false);
        buffer.layout_runs().map(|r| r.line_height as i32).sum::<i32>().max(size as i32)
    }

    // -------------------------------------------------------------- widgets

    pub fn button(&mut self, x: i32, y: i32, label: &str, action: &str, primary: bool) -> i32 {
        let w = (label.chars().count() as i32 * 9 + 34).max(74);
        let h = 38;
        if primary {
            self.gradient_rounded(x, y, w, h, 11.0, EMBER, FLAME);
            self.text(x + 17, y + 10, w - 20, 14.5, [20, 18, 22], label, true);
        } else {
            self.rounded(x, y, w, h, 11.0, rgba(self.theme.panel2, 0.95));
            self.outline(x, y, w, h, 11.0, 1.4, rgb(self.theme.line));
            self.text(x + 17, y + 10, w - 20, 14.0, self.theme.ash, label, false);
        }
        self.hits.push(Hit { rect: (x, y, w as u32, h as u32), action: action.to_string() });
        h
    }

    pub fn chip(&mut self, x: i32, y: i32, label: &str, action: &str, active: bool) -> i32 {
        let w = (label.chars().count() as i32 * 9 + 30).max(60);
        let h = 30;
        if active {
            self.gradient_rounded(x, y, w, h, 999.0, EMBER, FLAME);
            self.text(x + 15, y + 7, w, 13.0, [20, 18, 22], label, true);
        } else {
            self.rounded(x, y, w, h, 999.0, rgba(self.theme.panel2, 0.9));
            self.outline(x, y, w, h, 999.0, 1.2, rgb(self.theme.line));
            self.text(x + 15, y + 7, w, 13.0, self.theme.ash, label, false);
        }
        self.hits.push(Hit { rect: (x, y, w as u32, h as u32), action: action.to_string() });
        w
    }

    pub fn field(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        focused: bool,
        value: &str,
        hint: &str,
        action_focus: &str,
        caret: usize,
    ) -> i32 {
        let h = 40;
        self.rounded(x, y, w, h, 11.0, rgba(self.theme.panel2, 0.95));
        self.outline(
            x,
            y,
            w,
            h,
            11.0,
            if focused { 1.8 } else { 1.2 },
            rgb(if focused { EMBER } else { self.theme.line }),
        );
        if value.is_empty() && !focused {
            self.text(x + 13, y + 12, w - 20, 13.5, self.theme.scale_hint(), hint, false);
        } else {
            self.text(x + 13, y + 12, w - 20, 13.5, self.theme.bone, value, false);
            if focused {
                let before: String = value.chars().take(caret).collect();
                let cw = 7 + before.chars().count() as i32 * 8;
                self.rect(x + 11 + cw.min(w - 22), y + 11, 2, 18, rgb(GOLD));
            }
        }
        self.hits.push(Hit { rect: (x, y, w as u32, h as u32), action: action_focus.to_string() });
        h
    }
}
