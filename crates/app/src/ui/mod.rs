//! dgpui — Dragon's own immediate-mode UI kit.
//!
//! GPUI-inspired architecture with its own identity:
//!   state -> build paint list each frame -> raster (tiny-skia) -> present (softbuffer)
//!   widgets register hit-rects while painting, so input dispatch is trivial.
//! Text is shaped by cosmic-text and blitted glyph-by-glyph into the pixmap.

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
}

/// One registered interactive rectangle for this frame.
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

impl<'a> Frame<'a> {
    pub fn rgb(c: [u8; 3]) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(c[0], c[1], c[2], 255)
    }
    pub fn rgba(c: [u8; 3], a: f32) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(c[0], c[1], c[2], (a * 255.0) as u8)
    }

    // ------------------------------------------------------------ primitives

    pub fn fill_all(&mut self, c: [u8; 3]) {
        self.pix.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], 255));
    }

    pub fn rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: tiny_skia::Color) {
        let Some(r) = tiny_skia::Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        let path = tiny_skia::PathBuilder::from_rect(r);
        self.pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }

    /// Rounded-rect path via quadratic corners (portable across tiny-skia versions).
    fn rr(b: &mut tiny_skia::PathBuilder, x: f32, y: f32, w: f32, h: f32, r0: f32) {
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

    pub fn rounded(&mut self, x: i32, y: i32, w: u32, h: u32, radius: f32, color: tiny_skia::Color) {
        let mut pb = tiny_skia::PathBuilder::new();
        Self::rr(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        self.pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }

    pub fn gradient_rounded(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        radius: f32,
        from: [u8; 3],
        to: [u8; 3],
    ) {
        let mut pb = tiny_skia::PathBuilder::new();
        Self::rr(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let shader = tiny_skia::LinearGradient::new(
            tiny_skia::Point { x: x as f32, y: y as f32 },
            tiny_skia::Point { x: (x + w as i32) as f32, y: (y + h as i32) as f32 },
            vec![
                tiny_skia::GradientStop::new(0.0, Self::rgb(from)),
                tiny_skia::GradientStop::new(1.0, Self::rgb(to)),
            ],
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        );
        let mut paint = tiny_skia::Paint::default();
        paint.shader = shader.unwrap_or_else(|| tiny_skia::Shader::SolidColor(Self::rgb(from)));
        self.pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }

    pub fn outline(&mut self, x: i32, y: i32, w: u32, h: u32, radius: f32, width: f32, color: tiny_skia::Color) {
        let mut pb = tiny_skia::PathBuilder::new();
        Self::rr(&mut pb, x as f32, y as f32, w as f32, h as f32, radius);
        let Some(path) = pb.finish() else { return };
        let mut paint = tiny_skia::Paint::default();
        paint.shader = tiny_skia::Shader::SolidColor(color);
        let stroke = tiny_skia::Stroke { width, ..tiny_skia::Stroke::default() };
        self.pix.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
    }

    /// Soft vertical shadow rising from `y` upward by `h` px.
    pub fn bottom_shadow(&mut self, x: i32, w: u32, y_bottom: i32, h: i32, dark: bool) {
        for i in 0..h.max(1) {
            let t = 1.0 - (i as f32 / h.max(1) as f32);
            let a = (t * t * 0.55 * 255.0) as u8;
            let c = if dark {
                tiny_skia::Color::from_rgba8(0, 0, 0, a)
            } else {
                tiny_skia::Color::from_rgba8(120, 110, 140, (a as f32 * 0.5) as u8)
            };
            self.rect(x, y_bottom - i - 1, w, 1, c);
        }
    }

    // ---------------------------------------------------------------- text

    /// Draw text wrapped to max_w; returns total height used.
    pub fn text(
        &mut self,
        x: i32,
        y: i32,
        max_w: u32,
        size: f32,
        color: [u8; 3],
        s: &str,
        bold: bool,
    ) -> i32 {
        if s.is_empty() || max_w == 0 {
            return 0;
        }
        let metrics = cosmic_text::Metrics::new(size, size * 1.42);
        let mut buffer = cosmic_text::Buffer::new(self.font, metrics);
        buffer.set_size(Some(max_w as f32), None);
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(if bold { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL });
        buffer.set_text(self.font, s, attrs);
        buffer.shape_until_scroll(self.font, false);

        let fg = tiny_skia::ColorU8::from_rgba(color[0], color[1], color[2], 255).premultiply();
        let _ = fg;
        // Split borrows so the draw closure can mutate pixels.
        let Self { pix, font, swash, .. } = self;
        buffer.draw(font, swash, cosmic_text::Color::rgba(color[0], color[1], color[2], 255), |px, py, pw, ph, col| {
            let alpha = col.a();
            if alpha == 0 {
                return;
            }
            for dy in 0..ph {
                for dx in 0..pw {
                    let gx = px + dx;
                    let gy = py + dy;
                    if gx < 0 || gy < 0 {
                        continue;
                    }
                    let idx = (gy as usize * pix.width() as usize + gx as usize) * 4;
                    if idx + 3 >= pix.data().len() {
                        continue;
                    }
                    unsafe {
                        let data = pix.data_mut();
                        let a = ((alpha as u32) * 255 / 255) as u32;
                        let inv = 255 - a;
                        data[idx] = ((color[0] as u32 * a + data[idx] as u32 * inv) / 255) as u8;
                        data[idx + 1] = ((color[1] as u32 * a + data[idx + 1] as u32 * inv) / 255) as u8;
                        data[idx + 2] = ((color[2] as u32 * a + data[idx + 2] as u32 * inv) / 255) as u8;
                        data[idx + 3] = data[idx + 3].max(a.min(255) as u8);
                    }
                }
            }
        });
        let height: i32 = buffer
            .layout_runs()
            .map(|r| r.line_height as i32)
            .sum::<i32>()
            .max(size as i32);
        height
    }

    /// Measure wrapped height without drawing.
    pub fn measure(&mut self, max_w: u32, size: f32, s: &str, bold: bool) -> i32 {
        if s.is_empty() || max_w == 0 {
            return 0;
        }
        let metrics = cosmic_text::Metrics::new(size, size * 1.42);
        let mut buffer = cosmic_text::Buffer::new(self.font, metrics);
        buffer.set_size(Some(max_w as f32), None);
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(if bold { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL });
        buffer.set_text(self.font, s, attrs);
        buffer.shape_until_scroll(self.font, false);
        buffer.layout_runs().map(|r| r.line_height as i32).sum::<i32>().max(size as i32)
    }

    // -------------------------------------------------------------- widgets

    pub fn button(&mut self, x: i32, y: i32, label: &str, action: &str, primary: bool) -> i32 {
        let w = (label.len() as u32 * 9 + 34).max(74);
        let h = 38;
        if primary {
            self.gradient_rounded(x, y, w, h, 11.0, EMBER, FLAME);
            self.text(x + 17, y + 10, w - 20, 14.5, [20, 18, 22], label, true);
        } else {
            self.rounded(x, y, w, h, 11.0, Self::rgba(self.theme.panel2, 0.95));
            self.outline(x, y, w, h, 11.0, 1.4, Self::rgb(self.theme.line));
            self.text(x + 17, y + 10, w - 20, 14.0, self.theme.ash, label, false);
        }
        self.hits.push(Hit { rect: (x, y, w, h), action: action.to_string() });
        h
    }

    pub fn chip(&mut self, x: i32, y: i32, label: &str, action: &str, active: bool) -> i32 {
        let w = (label.chars().count() as u32 * 9 + 30).max(60);
        let h = 30;
        if active {
            self.gradient_rounded(x, y, w, h, 999.0, EMBER, FLAME);
            self.text(x + 15, y + 7, w, 13.0, [20, 18, 22], label, true);
        } else {
            self.rounded(x, y, w, h, 999.0, Self::rgba(self.theme.panel2, 0.9));
            self.outline(x, y, w, h, 999.0, 1.2, Self::rgb(self.theme.line));
            self.text(x + 15, y + 7, w, 13.0, self.theme.ash, label, false);
        }
        self.hits.push(Hit { rect: (x, y, w, h), action: action.to_string() });
        w as i32
    }

    /// Single-line editor. Returns height. Registers hit for focus.
    pub fn field(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        focused: bool,
        value: &str,
        hint: &str,
        action_focus: &str,
        caret: usize,
    ) -> i32 {
        let h = 40;
        self.rounded(x, y, w, h, 11.0, Self::rgba(self.theme.panel2, 0.95));
        self.outline(
            x, y, w, h, 11.0,
            if focused { 1.8 } else { 1.2 },
            if focused { Self::rgb(EMBER) } else { Self::rgb(self.theme.line) },
        );
        let shown = if value.is_empty() && !focused { hint.to_string() } else { String::new() };
        if !shown.is_empty() {
            self.text(x + 13, y + 12, w - 20, 13.5, self.scale_hint(), &shown, false);
        } else {
            self.text(x + 13, y + 12, w - 20, 13.5, self.theme.bone, value, false);
            if focused {
                let before: String = value.chars().take(caret).collect();
                let cw = 7 + before.chars().count() as i32 * 8;
                self.rect(x + 11 + cw.min(w as i32 - 22), y + 11, 2, 18, Self::rgb(GOLD));
            }
        }
        self.hits.push(Hit { rect: (x, y, w, h), action: action_focus.to_string() });
        h
    }

    pub fn scale_hint(&self) -> [u8; 3] {
        [self.theme.ash[0] - 20, self.theme.ash[1] - 20, self.theme.ash[2] - 20]
    }

    pub fn checkbox(&mut self, x: i32, y: i32, checked: bool, label: &str, action: &str) -> i32 {
        self.rounded(x, y + 2, 18, 18, 5.0, if checked { Self::rgb(EMBER) } else { Self::rgb(self.theme.panel2) });
        self.outline(x, y + 2, 18, 18, 5.0, 1.3, Self::rgb(self.theme.line));
        if checked {
            self.text(x + 4, y + 3, 14, 12.5, [255, 255, 255], "✓", true);
        }
        let tw = self.measure(u32::MAX, 13.0, label, false);
        self.text(x + 26, y + 5, u32::MAX, 13.0, self.theme.bone, label, false);
        self.hits.push(Hit { rect: (x, y, 26 + tw as u32, 24), action: action.to_string() });
        h_row()
    }
}

pub fn h_row() -> i32 {
    26
}
