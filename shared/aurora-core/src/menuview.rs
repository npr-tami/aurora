//! The in-game menu's picture, drawn on the CPU. The DLL copies the result into the game's frame;
//! the mock example writes it to a PNG.
//!
//! Layout, as in Stand: a coloured bar with the path on top, the sections on the left, the open
//! list on the right, a box with a short help line under the sections. Every panel is opaque, so
//! the DLL can copy the panels straight into the frame.

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Does something when picked.
    Action,
    /// Opens a list (drawn with ">").
    Sub,
    /// A tick box.
    Toggle(bool),
    /// A switch a profile turned on that waits for the world: a ring of dots where the tick box
    /// goes, its bright dot one step on at each `step` (0..8).
    Waiting(u8),
    /// A value shown on the right.
    Value(String),
    /// A value Left and Right step through: "< value >" while picked (Stand's look).
    Choice(String),
    /// A title in the middle of the list, as Stand's "Hotkeys" over a hotkey list.
    Header,
    /// Just text.
    Info,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub text: String,
    pub kind: Kind,
    /// Greyed out (not usable right now).
    pub dim: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// "Aurora > Self > Fly"
    pub path: String,
    pub sections: Vec<String>,
    pub section: usize,
    pub rows: Vec<Row>,
    /// The highlighted row; None while the sections have the cursor.
    pub pick: Option<usize>,
    /// First row shown (the list scrolls).
    pub scroll: usize,
    /// The help boxes: a blank line (`NEW_BOX`) starts the next box, a plain line break is a new
    /// line in the same box.
    pub help: String,
    pub accent: [u8; 3],
    /// Tabs width, tabs height, address bar height and address bar width, in pixels at 1080p;
    /// `LAYOUT` is the usual. They grow and shrink with `list`.
    pub layout: [f32; 4],
    /// List Width and List Height (one row), as Stand's: `LIST` is how the menu looked at Menu Size
    /// 85%. Everything follows them: text, tabs, address bar, help boxes and where the mouse
    /// clicks.
    pub list: [f32; 2],
    /// Max Visible Commands: rows shown before the list scrolls.
    pub visible: usize,
    /// Spacer Size: the see-through gap between the menu's parts; `SPACER` is the usual.
    pub spacer: f32,
}

/// The usual tabs width, tabs height, address bar height and width. The bar's width is its own: the
/// usual is the tabs, the gap and the list together.
pub const LAYOUT: [f32; 4] = [118.0, 34.0, 26.0, 602.0];

/// List Width and List Height that look exactly as the menu did at Menu Size 85%. One List Width
/// unit is 480 x 0.85 / 450 pixels at 1080p (the list itself), one List Height unit is 34 x 0.85 /
/// 32 pixels at 1080p (one row); both grow with the screen.
pub const LIST: [f32; 2] = [450.0, 32.0];
/// The Menu Size `LIST` stands for.
pub const LIST_LOOK: f32 = 0.85;
/// Spacer Size 3 (Stand's usual) is the gap the menu always had: 4 pixels at 1080p and 100%.
pub const SPACER: f32 = 3.0;
/// A blank line in the help starts a new box.
pub const NEW_BOX: &str = "\n\n";

/// List Width and List Height that draw as the old Menu Size `pct` did (for old profiles).
pub fn list_at(pct: f32) -> [f32; 2] {
    [LIST[0] * pct / 100.0 / LIST_LOOK, LIST[1] * pct / 100.0 / LIST_LOOK]
}

/// The across and down scales for List Width / Height on a screen `scale` high (screen height /
/// 1080): 1.0 is the old Menu Size 100% at 1080p. Kept to 0.2 .. 4.
pub fn axes(scale: f32, list: [f32; 2]) -> (f32, f32) {
    let k = scale * LIST_LOOK;
    ((k * list[0] / LIST[0]).clamp(0.2, 4.0), (k * list[1] / LIST[1]).clamp(0.2, 4.0))
}

/// How many rows fit under the address bar in `height` pixels at this List Height and address bar
/// (at least 1), so Max Visible Commands never makes the menu too tall to draw.
pub fn rows_fit(layout: [f32; 4], list: [f32; 2], spacer: f32, scale: f32, height: f32) -> usize {
    let (sx, sy) = axes(scale, list);
    let m = Metrics::laid_out(sx, sy, layout, spacer);
    if m.row_h <= 0.0 {
        return 1;
    }
    ((height - m.bar_h - m.gap) / m.row_h).floor().max(1.0) as usize
}

pub struct Frame {
    pub w: u32,
    pub h: u32,
    /// RGBA, opaque wherever a panel is.
    pub rgba: Vec<u8>,
    /// The panels: [x0, y0, x1, y1]. Only these get copied into the game.
    pub panels: Vec<[u32; 4]>,
}

/// How a notification looks: its width in pixels at 1080p, the bar on its left and the colour
/// behind the text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteLook {
    pub width: f32,
    pub border: [u8; 3],
    pub back: [u8; 3],
    /// The text's size in pixels on screen.
    pub font: f32,
}

/// A notification's text size in pixels at 1080p and Text Size 100%: the size it always had.
pub const NOTE_FONT: f32 = 22.0 * 0.82;

/// One line of notification text: a little more than the text is tall, so lines never touch.
fn note_line_h(size: f32) -> f32 {
    (size * 1.3).round()
}

/// Pictures one under the other, `gap` pixels apart, as one picture: only their own panels are
/// copied into the game, so the gaps stay see-through. `right` lines them up on their right edge,
/// for the right corners.
pub fn stack(frames: &[Frame], gap: u32, right: bool) -> Frame {
    let w = frames.iter().map(|f| f.w).max().unwrap_or(0);
    let h = frames.iter().map(|f| f.h).sum::<u32>() + gap * frames.len().saturating_sub(1) as u32;
    let mut rgba = vec![0u8; (w * h) as usize * 4];
    let mut panels = Vec::new();
    let mut y0 = 0;
    for f in frames {
        let x0 = if right { w - f.w } else { 0 };
        for y in 0..f.h {
            let s = (y * f.w * 4) as usize;
            let d = (((y0 + y) * w + x0) * 4) as usize;
            rgba[d..d + (f.w * 4) as usize].copy_from_slice(&f.rgba[s..s + (f.w * 4) as usize]);
        }
        panels.extend(f.panels.iter().map(|p| [p[0] + x0, p[1] + y0, p[2] + x0, p[3] + y0]));
        y0 += f.h + gap;
    }
    Frame { w, h, rgba, panels }
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];
const GREY: [u8; 3] = [140, 140, 140];

/// Sizes at 1080p; the across scale multiplies widths, the down scale heights and the text.
struct Metrics {
    bar_h: f32,
    bar_w: f32,
    bar_font: f32,
    row_h: f32,
    /// A tab's height (the sections on the left).
    tab_h: f32,
    font: f32,
    left_w: f32,
    right_w: f32,
    gap: f32,
    /// The space before text, across; `vpad` the same space up and down.
    pad: f32,
    vpad: f32,
    help_w: f32,
    /// The across and down scales themselves, for lines and marks.
    sx: f32,
    sy: f32,
}

impl Metrics {
    fn at(s: f32) -> Self {
        Self::laid_out(s, s, LAYOUT, SPACER)
    }

    fn laid_out(sx: f32, sy: f32, [tab_w, tab_h, bar_h, bar_w]: [f32; 4], spacer: f32) -> Self {
        Metrics {
            bar_h: (bar_h * sy).round(),
            bar_w: (bar_w * sx).round(),
            // 17 at the usual 26.
            bar_font: (bar_h * 0.65).max(10.0) * sy,
            row_h: (34.0 * sy).round(),
            tab_h: (tab_h * sy).round(),
            font: 22.0 * sy,
            left_w: (tab_w * sx).round(),
            right_w: (480.0 * sx).round(),
            // Spacer Size 3 is 4 px at 1080p, as it always was; 0 is no gap at all. At least 1 px,
            // so Size 1 and 2 look different.
            gap: if spacer <= 0.0 { 0.0 } else { (spacer * 4.0 / 3.0 * sy).round().max(1.0) },
            pad: (7.0 * sx).round(),
            vpad: (7.0 * sy).round(),
            help_w: (330.0 * sx).round(),
            sx,
            sy,
        }
    }
}

/// Where the menu's parts sit in its picture, at a scale. Drawing and the mouse both use this, so a
/// click lands on what the picture shows.
///
/// The right wall: the picture's right edge is the menu's right edge, a fixed wall. The list and
/// the address bar end on it, the tabs sit left of the list, the help boxes under the tabs. A wider
/// list, tabs or bar grows to the left only; the wall never moves.
struct Geo {
    m: Metrics,
    /// The address bar's left edge (its right edge is the wall).
    bar_x: f32,
    /// The tabs' left and right edges, the list's left edge and the wall (the list's right).
    lx: f32,
    tabs_r: f32,
    rx: f32,
    list_r: f32,
    /// Where the tabs and the list start (under the bar).
    top: f32,
    left_h: f32,
    right_h: f32,
}

impl Geo {
    /// `room`: screen pixels left of the wall the picture may use. What would stick out further
    /// left is cut at the picture's left edge, so it stays on screen and the wall stays put.
    /// Infinity: all the room it wants.
    fn of(v: &View, scale: f32, room: f32) -> Geo {
        let (sx, sy) = axes(scale, v.list);
        let m = Metrics::laid_out(sx, sy, v.layout, v.spacer);
        let shown = v.rows.len().saturating_sub(v.scroll).min(v.visible.max(1));
        let left_h = v.sections.len() as f32 * m.tab_h;
        let right_h = shown.max(1) as f32 * m.row_h;
        let wall = full_reach(&m).min(room.max(0.0)).round();
        let rx = (wall - m.right_w).max(0.0);
        let tabs_r = (wall - m.right_w - m.gap).max(0.0);
        let lx = (tabs_r - m.left_w).max(0.0);
        let bar_x = (wall - m.bar_w).max(0.0);
        let top = m.bar_h + m.gap;
        Geo { m, bar_x, lx, tabs_r, rx, list_r: wall, top, left_h, right_h }
    }
}

/// How far the whole picture reaches left of the wall with all the room it wants: the bar, or the
/// list, the gap and the tabs (or the help boxes under them, when wider).
fn full_reach(m: &Metrics) -> f32 {
    m.bar_w.max(m.right_w + m.gap + m.left_w.max(m.help_w))
}

/// How wide the menu's picture is with all the room it wants.
pub fn menu_width(v: &View, scale: f32) -> f32 {
    Geo::of(v, scale, f32::INFINITY).list_r
}

/// The list's width on screen: the wall never comes closer to the screen's left edge than this, so
/// the list itself always shows.
pub fn list_width(v: &View, scale: f32) -> f32 {
    Geo::of(v, scale, f32::INFINITY).m.right_w
}

/// X was the address bar's left edge: the wall (the list's right edge) sat this far right of it, at
/// the old single `scale` (screen height / 1080 x Menu Size). Old profiles add it to their X, so
/// the menu stays where it was.
pub fn old_bar_to_wall(scale: f32) -> f32 {
    let m = Metrics::at(scale.clamp(0.5, 4.0));
    (LAYOUT[0] * m.sx).round() + m.gap + m.right_w
}

/// How far the picture reached left of the address bar before with all the room it wanted (tabs
/// wider than usual, or the help boxes), for tabs `tabs_w` wide (pixels at 1080p) at `scale`: where
/// the bar sat in the picture when X / Y were the picture's corner, so the player's oldest profiles
/// are moved by this.
pub fn left_reach(tabs_w: f32, scale: f32) -> f32 {
    let s = scale.clamp(0.5, 4.0);
    let mut layout = LAYOUT;
    layout[0] = tabs_w;
    let m = Metrics::laid_out(s, s, layout, SPACER);
    let usual = (LAYOUT[0] * s).round();
    (m.left_w - usual).max(0.0).max(m.help_w - usual)
}

/// What the mouse is on in the menu's picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// The address bar on top (Move With Mouse drags it).
    Bar,
    /// A tab, by its number.
    Tab(usize),
    /// A row of the list, by its number in the whole list (scrolling counted).
    Row(usize),
}

/// What is at (`x`, `y`), in pixels from the menu picture's top-left corner, when `v` is drawn at
/// `scale` (as `Painter::render`). None: outside the bar, the tabs and the list.
pub fn spot(v: &View, scale: f32, x: f32, y: f32) -> Option<Spot> {
    spot_in(v, scale, f32::INFINITY, x, y)
}

/// `spot` for a picture drawn with `room` pixels free left of its wall (`Painter::render_in`).
pub fn spot_in(v: &View, scale: f32, room: f32, x: f32, y: f32) -> Option<Spot> {
    let g = Geo::of(v, scale, room);
    let m = &g.m;
    if x >= g.bar_x && x < g.list_r && (0.0..m.bar_h).contains(&y) {
        return Some(Spot::Bar);
    }
    if x >= g.lx && x < g.tabs_r && y >= g.top && y < g.top + g.left_h && m.tab_h > 0.0 {
        let i = ((y - g.top) / m.tab_h) as usize;
        return (i < v.sections.len()).then_some(Spot::Tab(i));
    }
    if x >= g.rx && x < g.list_r && y >= g.top && y < g.top + g.right_h && m.row_h > 0.0 {
        let i = v.scroll + ((y - g.top) / m.row_h) as usize;
        return (i < v.rows.len()).then_some(Spot::Row(i));
    }
    None
}

/// The mouse pointer, as Windows' arrow: X is its black edge, a dot its white inside, a space
/// see-through. Its tip is the top-left pixel.
const POINTER: [&str; 19] = [
    "X",
    "XX",
    "X.X",
    "X..X",
    "X...X",
    "X....X",
    "X.....X",
    "X......X",
    "X.......X",
    "X........X",
    "X.........X",
    "X......XXXXX",
    "X...X..X",
    "X..XX..X",
    "X.X  X..X",
    "XX   X..X",
    "X     X..X",
    "      X..X",
    "       XX",
];

/// The mouse pointer at `scale` (whole pixels: 1x at 1080p, 2x from 1.5). Each run of drawn pixels
/// in a line is its own panel, so only the arrow is copied into the game.
pub fn render_pointer(scale: f32) -> Frame {
    let k = (scale.clamp(0.5, 4.0).round() as u32).max(1);
    let w = POINTER.iter().map(|l| l.len()).max().unwrap_or(0) as u32 * k;
    let h = POINTER.len() as u32 * k;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut panels = Vec::new();
    for (row, line) in POINTER.iter().enumerate() {
        let mut run: Option<u32> = None;
        for (col, ch) in line.chars().chain(std::iter::once(' ')).enumerate() {
            let col = col as u32;
            if ch == ' ' {
                if let Some(start) = run.take() {
                    for dy in 0..k {
                        let y = row as u32 * k + dy;
                        panels.push([start * k, y, col * k, y + 1]);
                    }
                }
                continue;
            }
            run.get_or_insert(col);
            let c = if ch == 'X' { BLACK } else { WHITE };
            for dy in 0..k {
                for dx in 0..k {
                    let i = (((row as u32 * k + dy) * w + col * k + dx) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
        }
    }
    Frame { w, h, rgba, panels }
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn fill(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3]) {
        let (x0, y0) = (x0.max(0.0) as u32, y0.max(0.0) as u32);
        let (x1, y1) = ((x1.max(0.0) as u32).min(self.w), (y1.max(0.0) as u32).min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * self.w + x) * 4) as usize;
                self.px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }

    /// Blend colour `c` over a pixel with coverage `a` (0..1).
    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        for (px, &ck) in self.px[i..i + 3].iter_mut().zip(&c) {
            let old = f32::from(*px);
            *px = (old + (f32::from(ck) - old) * a).round() as u8;
        }
        self.px[i + 3] = 255;
    }

    /// An anti-aliased thick line.
    fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, c: [u8; 3]) {
        let (minx, maxx) = (a.0.min(b.0) - width, a.0.max(b.0) + width);
        let (miny, maxy) = (a.1.min(b.1) - width, a.1.max(b.1) + width);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in miny.floor() as i32..=maxy.ceil() as i32 {
            for x in minx.floor() as i32..=maxx.ceil() as i32 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (a.0 + t * dx - px, a.1 + t * dy - py);
                let d = (qx * qx + qy * qy).sqrt();
                self.blend(x, y, c, width / 2.0 + 0.5 - d);
            }
        }
    }

    /// A rounded square: filled, or just its outline.
    fn rounded(&mut self, x0: f32, y0: f32, size: f32, radius: f32, stroke: Option<f32>, c: [u8; 3]) {
        let (cx, cy, half) = (x0 + size / 2.0, y0 + size / 2.0, size / 2.0);
        for y in y0.floor() as i32..=(y0 + size).ceil() as i32 {
            for x in x0.floor() as i32..=(x0 + size).ceil() as i32 {
                let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let (qx, qy) = (px.abs() - (half - radius), py.abs() - (half - radius));
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
                let a = match stroke {
                    None => 0.5 - outside,
                    Some(w) => (0.5 - outside).min(0.5 + outside + w),
                };
                self.blend(x, y, c, a.clamp(0.0, 1.0));
            }
        }
    }
}

pub struct Painter {
    font: FontVec,
}

impl Painter {
    pub fn new(font_bytes: Vec<u8>) -> Option<Self> {
        FontVec::try_from_vec(font_bytes).ok().map(|font| Painter { font })
    }

    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        let f = self.font.as_scaled(PxScale::from(size));
        let mut last: Option<GlyphId> = None;
        let mut w = 0.0;
        for ch in text.chars() {
            let id = f.glyph_id(ch);
            if let Some(l) = last {
                w += f.kern(l, id);
            }
            w += f.h_advance(id);
            last = Some(id);
        }
        w
    }

    /// Where `text`'s character at byte `at` is drawn from (the kern before it included), as `text`
    /// places it; the whole width at or past the end. The command box's caret goes there.
    fn pen_at(&self, text: &str, at: usize, size: f32) -> f32 {
        let f = self.font.as_scaled(PxScale::from(size));
        let mut last: Option<GlyphId> = None;
        let mut pen = 0.0;
        for (i, ch) in text.char_indices() {
            let id = f.glyph_id(ch);
            if let Some(l) = last {
                pen += f.kern(l, id);
            }
            if i >= at {
                return pen;
            }
            pen += f.h_advance(id);
            last = Some(id);
        }
        pen
    }

    /// Text with its left end at `x`, centred on the line `mid_y`.
    fn text(&self, cv: &mut Canvas, text: &str, x: f32, mid_y: f32, size: f32, c: [u8; 3]) {
        let f = self.font.as_scaled(PxScale::from(size));
        // Centre on the height of capitals, as Stand does.
        let cap = f.ascent() * 0.72;
        let base = (mid_y + cap / 2.0).round();
        let mut pen = x;
        let mut last: Option<GlyphId> = None;
        for ch in text.chars() {
            let id = f.glyph_id(ch);
            if let Some(l) = last {
                pen += f.kern(l, id);
            }
            let g = id.with_scale_and_position(PxScale::from(size), point(pen, base));
            pen += f.h_advance(id);
            last = Some(id);
            if let Some(o) = self.font.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, a| cv.blend(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, c, a));
            }
        }
    }

    /// Draw the menu at `scale` (screen height / 1080).
    pub fn render(&self, v: &View, scale: f32) -> Frame {
        self.render_in(v, scale, f32::INFINITY)
    }

    /// Draw the menu with only `room` pixels free left of its wall on screen: the picture's right
    /// edge is the wall; what would stick out further left is cut at the picture's left edge (the
    /// help boxes wrap narrower), and the wall never moves for it.
    pub fn render_in(&self, v: &View, scale: f32, room: f32) -> Frame {
        // Where everything sits: shared with `spot`, so the mouse clicks what is drawn.
        let Geo { m, bar_x, lx, tabs_r, rx, list_r, top, left_h, right_h } = Geo::of(v, scale, room);
        // The help boxes, stacked, as Stand's. A blank line starts the next box; a plain line break
        // stays in the same box. As wide as the room from the picture's left edge to the tabs'
        // right edge allows. No room for a line of help left of the list (the wall near the
        // screen's left edge): no help boxes, rather than help drawn over the list.
        let help_w = m.help_w.min(tabs_r);
        let help_boxes: Vec<Vec<String>> = if help_w < 2.0 * m.pad + 4.0 * m.font {
            Vec::new()
        } else {
            v.help.split(NEW_BOX).map(|p| self.wrap(p, m.font * 0.82, help_w - 2.0 * m.pad)).filter(|l| !l.is_empty()).collect()
        };
        let help_line_h = (m.row_h * 0.78).round();
        let help_h = help_boxes.iter().map(|b| b.len() as f32 * help_line_h + m.vpad).sum::<f32>()
            + m.gap * help_boxes.len().saturating_sub(1) as f32;
        // The wall is the picture's right edge.
        let total_w = list_r;
        let total_h = top + left_h.max(right_h).max(left_h + if help_h > 0.0 { m.gap + help_h } else { 0.0 });
        let mut cv = Canvas { w: total_w.ceil() as u32, h: total_h.ceil() as u32, px: vec![0; (total_w.ceil() * total_h.ceil()) as usize * 4] };
        let mut panels = Vec::new();
        let mut panel = |cv: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3]| {
            cv.fill(x0, y0, x1, y1, c);
            panels.push([x0 as u32, y0 as u32, x1 as u32, y1 as u32]);
        };
        // The bar with the path.
        panel(&mut cv, bar_x, 0.0, list_r, m.bar_h, v.accent);
        self.text(&mut cv, &v.path, bar_x + m.pad * 0.6, m.bar_h / 2.0, m.bar_font, WHITE);
        // Sections. Cut on the left: the names stay where they sit in the whole tabs, so their
        // start is what goes off the picture, never the list's side.
        panel(&mut cv, lx, top, tabs_r, top + left_h, BLACK);
        for (i, name) in v.sections.iter().enumerate() {
            let y = top + i as f32 * m.tab_h;
            if i == v.section {
                cv.fill(lx, y, tabs_r, y + m.tab_h, v.accent);
            }
            if tabs_r > lx {
                self.text(&mut cv, name, tabs_r - m.left_w + m.pad, y + m.tab_h / 2.0, m.font, WHITE);
            }
        }
        // The list.
        panel(&mut cv, rx, top, list_r, top + right_h, BLACK);
        let visible = v.visible.max(1);
        for (k, row) in v.rows.iter().skip(v.scroll).take(visible).enumerate() {
            let i = v.scroll + k;
            let y = top + k as f32 * m.row_h;
            let on = v.pick == Some(i);
            let bg = if on { v.accent } else { BLACK };
            if on {
                cv.fill(rx, y, list_r, y + m.row_h, bg);
            }
            let ink = if row.dim { GREY } else { WHITE };
            let mid = y + m.row_h / 2.0;
            if row.kind == Kind::Header {
                let w = self.text_width(&row.text, m.font);
                self.text(&mut cv, &row.text, rx + (list_r - rx - w) / 2.0, mid, m.font, ink);
                continue;
            }
            self.text(&mut cv, &row.text, rx + m.pad, mid, m.font, ink);
            let right = list_r - m.pad - if v.rows.len() > visible { 5.0 * m.sx } else { 0.0 };
            match &row.kind {
                Kind::Sub => {
                    let s = m.row_h * 0.17;
                    let cx = right - s * 0.9;
                    cv.line((cx - s, mid - s * 1.6), (cx + s * 0.6, mid), 2.6 * m.sy, ink);
                    cv.line((cx + s * 0.6, mid), (cx - s, mid + s * 1.6), 2.6 * m.sy, ink);
                }
                Kind::Toggle(t) => {
                    let size = (m.row_h * 0.56).round();
                    let (bx, by) = (right - size, mid - size / 2.0);
                    let r = size * 0.18;
                    if *t {
                        cv.rounded(bx, by, size, r, None, ink);
                        let tick = |p: (f32, f32)| (bx + p.0 * size, by + p.1 * size);
                        cv.line(tick((0.22, 0.52)), tick((0.42, 0.72)), 2.2 * m.sy, bg);
                        cv.line(tick((0.42, 0.72)), tick((0.78, 0.30)), 2.2 * m.sy, bg);
                    } else {
                        cv.rounded(bx, by, size, r, Some(1.8 * m.sy), ink);
                    }
                }
                Kind::Waiting(step) => {
                    let size = (m.row_h * 0.56).round();
                    let (cx, cy) = (right - size / 2.0, mid);
                    let (ring, dot) = (size * 0.40, size * 0.11);
                    for d in 0..8u8 {
                        let a = f32::from(d) * std::f32::consts::FRAC_PI_4 - std::f32::consts::FRAC_PI_2;
                        let (x, y) = (cx + ring * a.cos(), cy + ring * a.sin());
                        // The bright dot, then the three before it fading out behind.
                        let age = (step.wrapping_sub(d)) % 8;
                        let k = [1.0, 0.7, 0.45, 0.3][usize::from(age.min(3))];
                        let c: [u8; 3] = std::array::from_fn(|j| (f32::from(ink[j]) * k + f32::from(bg[j]) * (1.0 - k)).round() as u8);
                        cv.line((x, y), (x, y), dot * 2.0, c);
                    }
                }
                Kind::Value(val) => {
                    let w = self.text_width(val, m.font);
                    self.text(&mut cv, val, right - w, mid, m.font, ink);
                }
                Kind::Choice(val) => {
                    let val = if on { format!("<  {val}  >") } else { val.clone() };
                    let w = self.text_width(&val, m.font);
                    self.text(&mut cv, &val, right - w, mid, m.font, ink);
                }
                Kind::Action | Kind::Info | Kind::Header => {}
            }
        }
        // Where in a long list we are.
        if v.rows.len() > visible {
            let track = right_h;
            let len = (track * visible as f32 / v.rows.len() as f32).max(m.row_h * 0.6);
            let y0 = top + (track - len) * v.scroll as f32 / (v.rows.len() - visible) as f32;
            cv.fill(list_r - 4.0 * m.sx, y0, list_r, y0 + len, v.accent);
        }
        // The help box.
        if help_h > 0.0 {
            let mut y0 = top + left_h + m.gap;
            let widest = help_boxes.iter().flatten().map(|l| self.text_width(l, m.font * 0.82)).fold(0.0f32, f32::max);
            let x0 = (tabs_r - widest - 2.0 * m.pad).min(lx).max(0.0);
            for lines in &help_boxes {
                let h = lines.len() as f32 * help_line_h + m.vpad;
                panel(&mut cv, x0, y0, tabs_r, y0 + h, BLACK);
                for (n, l) in lines.iter().enumerate() {
                    let mid = y0 + m.vpad / 2.0 + (n as f32 + 0.5) * help_line_h;
                    self.text(&mut cv, l, x0 + m.pad, mid, m.font * 0.82, WHITE);
                }
                y0 += h + m.gap;
            }
        }
        Frame { w: cv.w, h: cv.h, rgba: cv.px, panels }
    }

    /// The command box, as Stand's: a bar with `title`, the text with a caret, and under it one
    /// line per matching command. It follows List Width and List Height as the menu does. The caret
    /// stands before the character at byte `caret` (at the end when past it): Left / Right move it,
    /// and it is drawn over the gap, so the text never shifts when it moves.
    #[allow(clippy::too_many_arguments)]
    pub fn render_input(&self, title: &str, text: &str, caret: usize, lines: &[String], accent: [u8; 3], scale: f32, list: [f32; 2]) -> Frame {
        let (sx, sy) = axes(scale, list);
        let m = Metrics::laid_out(sx, sy, LAYOUT, SPACER);
        let small = m.font * 0.82;
        let line_h = (m.row_h * 0.86).round();
        let gap = (m.row_h * 0.05).max(1.0).round();
        let w = (m.left_w + m.gap + m.right_w).ceil();
        let top = m.bar_h + m.row_h;
        let h = (top + (line_h + gap) * lines.len() as f32).ceil();
        let mut cv = Canvas { w: w as u32, h: h as u32, px: vec![0; (w * h) as usize * 4] };
        cv.fill(0.0, 0.0, w, m.bar_h, accent);
        self.text(&mut cv, title, m.pad * 0.6, m.bar_h / 2.0, m.bar_font, WHITE);
        cv.fill(0.0, m.bar_h, w, top, BLACK);
        let mid = m.bar_h + m.row_h / 2.0;
        self.text(&mut cv, text, m.pad, mid, m.font, WHITE);
        // The caret: a "|" centred on the gap before the caret's character.
        let bar = self.text_width("|", m.font);
        self.text(&mut cv, "|", m.pad + self.pen_at(text, caret, m.font) - bar / 2.0, mid, m.font, WHITE);
        let mut panels = vec![[0, 0, w as u32, top as u32]];
        for (n, l) in lines.iter().enumerate() {
            let y0 = (top + gap + n as f32 * (line_h + gap)).round();
            cv.fill(0.0, y0, w, y0 + line_h, BLACK);
            self.text(&mut cv, l, m.pad, y0 + line_h / 2.0, small, WHITE);
            panels.push([0, y0 as u32, w as u32, (y0 + line_h) as u32]);
        }
        Frame { w: cv.w, h: cv.h, rgba: cv.px, panels }
    }

    /// A short message box with a coloured bar on its left, as Stand's toasts.
    pub fn render_toast(&self, text: &str, accent: [u8; 3], scale: f32) -> Frame {
        let m = Metrics::at(scale.clamp(0.5, 4.0));
        let size = m.font * 0.82;
        let lines = self.wrap(text, size, m.help_w * 1.4);
        let line_h = (m.row_h * 0.78).round();
        let bar = (3.0 * scale).round().max(2.0);
        let widest = lines.iter().map(|l| self.text_width(l, size)).fold(0.0f32, f32::max);
        let (w, h) = ((bar + 2.0 * m.pad + widest).ceil(), (lines.len().max(1) as f32 * line_h + m.pad).ceil());
        let mut cv = Canvas { w: w as u32, h: h as u32, px: vec![0; (w * h) as usize * 4] };
        cv.fill(0.0, 0.0, w, h, BLACK);
        cv.fill(0.0, 0.0, bar, h, accent);
        for (n, l) in lines.iter().enumerate() {
            self.text(&mut cv, l, bar + m.pad, m.pad / 2.0 + (n as f32 + 0.5) * line_h, size, WHITE);
        }
        Frame { w: cv.w, h: cv.h, rgba: cv.px, panels: vec![[0, 0, w as u32, h as u32]] }
    }

    /// A notification: the box fits its text snugly, a small padding all round and the height from
    /// the text's lines. `look.width` (pixels at 1080p, times `scale`) is only the most it may grow
    /// before the text wraps; `look.font` is the text's own size in pixels (Text Size), `scale` the
    /// box's (Size): its padding, its bar and its widest.
    pub fn render_note(&self, text: &str, look: &NoteLook, scale: f32) -> Frame {
        let s = scale.clamp(0.25, 8.0);
        let size = look.font.clamp(6.0, 200.0);
        let (pad, vpad) = ((7.0 * s).round().max(2.0), (3.0 * s).round().max(1.0));
        let bar = (3.0 * s).round().max(2.0);
        let most = (look.width * s).round().max(bar + 2.0 * pad + 20.0);
        let lines = self.wrap(text, size, most - bar - 2.0 * pad);
        let widest = lines.iter().map(|l| self.text_width(l, size)).fold(0.0f32, f32::max);
        let w = (bar + 2.0 * pad + widest).ceil().min(most).max(bar + 2.0 * pad + 1.0);
        let line_h = note_line_h(size);
        let h = (lines.len().max(1) as f32 * line_h + 2.0 * vpad).ceil();
        let mut cv = Canvas { w: w as u32, h: h as u32, px: vec![0; (w * h) as usize * 4] };
        cv.fill(0.0, 0.0, w, h, look.back);
        cv.fill(0.0, 0.0, bar, h, look.border);
        for (n, l) in lines.iter().enumerate() {
            self.text(&mut cv, l, bar + pad, vpad + (n as f32 + 0.5) * line_h, size, WHITE);
        }
        Frame { w: cv.w, h: cv.h, rgba: cv.px, panels: vec![[0, 0, w as u32, h as u32]] }
    }

    fn wrap(&self, text: &str, size: f32, width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.lines() {
            let mut line = String::new();

            for word in pieces(para, |p| self.text_width(p, size) <= width) {
                let next = if line.is_empty() { word.clone() } else { format!("{line} {word}") };
                if !line.is_empty() && self.text_width(&next, size) > width {
                    lines.push(std::mem::take(&mut line));
                    line = word;
                } else {
                    line = next;
                }
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }
}

/// The words of `para`, but a `[ ... ]` group (a command's range, like "[0.00 to
/// 1000.00 (0 is off)]") is one piece when `fits` says it fits on a line, so a help box never
/// breaks inside it (found in the 5 Nov night test: "(0 is" / "off)]"). Wider than a whole line,
/// its words stay apart and wrap one by one, as before.
fn pieces(para: &str, fits: impl Fn(&str) -> bool) -> Vec<String> {
    let words: Vec<&str> = para.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = if words[i].starts_with('[') && !words[i].ends_with(']') { words[i..].iter().position(|w| w.ends_with(']')).map(|n| i + n + 1) } else { None };
        match end.map(|e| (e, words[i..e].join(" "))).filter(|(_, g)| fits(g)) {
            Some((e, group)) => {
                out.push(group);
                i = e;
            }
            None => {
                out.push(words[i].to_string());
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu with `rows` rows and four tabs, its list scrolled to `scroll`.
    fn long_view(rows: usize, scroll: usize) -> View {
        View {
            path: "Aurora 0.1.0 > Menu".into(),
            sections: ["Self", "World", "Game", "Menu"].map(String::from).to_vec(),
            section: 3,
            rows: (0..rows).map(|i| Row { text: format!("Row {i}"), kind: Kind::Action, dim: false }).collect(),
            pick: Some(scroll),
            scroll,
            help: String::new(),
            accent: [140, 60, 255],
            layout: LAYOUT,
            // The old Menu Size 100% and 13 rows, which these tests' numbers are from.
            list: list_at(100.0),
            visible: 13,
            spacer: SPACER,
        }
    }

    /// The mouse finds rows, tabs and the bar where they are drawn. At 1080p and the usual layout:
    /// tabs 212..330, list 334..814, bar 212..814 x 0..26, tabs and rows 34 high from y 30.
    #[test]
    fn mouse_hits_rows_tabs_and_bar() {
        let v = long_view(20, 0);
        assert_eq!(spot(&v, 1.0, 300.0, 10.0), Some(Spot::Bar));
        assert_eq!(spot(&v, 1.0, 250.0, 30.0 + 34.0 * 1.5), Some(Spot::Tab(1)));
        assert_eq!(spot(&v, 1.0, 250.0, 30.0 + 34.0 * 3.9), Some(Spot::Tab(3)));
        // Under the last tab: no tab (the help box is there).
        assert_eq!(spot(&v, 1.0, 250.0, 30.0 + 34.0 * 4.1), None);
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 0.5), Some(Spot::Row(0)));
        assert_eq!(spot(&v, 1.0, 813.0, 30.0 + 34.0 * 2.99), Some(Spot::Row(2)));
        // The gap between tabs and list, past the list's right edge, under its 13th row.
        assert_eq!(spot(&v, 1.0, 332.0, 50.0), None);
        assert_eq!(spot(&v, 1.0, 815.0, 50.0), None);
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 13.0 + 1.0), None);
        // A short list: nothing where a 4th row would be.
        let short = long_view(3, 0);
        assert_eq!(spot(&short, 1.0, 400.0, 30.0 + 34.0 * 2.5), Some(Spot::Row(2)));
        assert_eq!(spot(&short, 1.0, 400.0, 30.0 + 34.0 * 3.5), None);
    }

    /// A scrolled list: the first row shown is the scroll's row, and the last shown rows count from
    /// there.
    #[test]
    fn mouse_hits_scrolled_rows() {
        let v = long_view(20, 5);
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 0.5), Some(Spot::Row(5)));
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 12.5), Some(Spot::Row(17)));
        // Scrolled to the end: 20 - 15 = 5 rows shown, nothing under them.
        let end = long_view(20, 15);
        assert_eq!(spot(&end, 1.0, 400.0, 30.0 + 34.0 * 4.5), Some(Spot::Row(19)));
        assert_eq!(spot(&end, 1.0, 400.0, 30.0 + 34.0 * 5.5), None);
    }

    /// At twice the size every spot is twice as far (1440p with a bigger menu, say), and the menu's
    /// own size limits hold as in drawing.
    #[test]
    fn mouse_hits_follow_the_scale() {
        let v = long_view(20, 0);
        assert_eq!(spot(&v, 2.0, 600.0, 20.0), Some(Spot::Bar));
        assert_eq!(spot(&v, 2.0, 500.0, 60.0 + 68.0 * 2.5), Some(Spot::Tab(2)));
        assert_eq!(spot(&v, 2.0, 800.0, 60.0 + 68.0 * 2.5), Some(Spot::Row(2)));
        // Row 2's spot at 1x is left of the tabs at 2x: nothing.
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 2.5), Some(Spot::Row(2)));
        assert_eq!(spot(&v, 2.0, 400.0, 30.0 + 34.0 * 2.5), None);
        // Drawing clamps the scale to 0.5..4; so does the mouse.
        assert_eq!(spot(&v, 9.0, 0.0, 0.0), spot(&v, 4.0, 0.0, 0.0));
        assert_eq!(spot(&v, 9.0, 4.0 * 800.0, 4.0 * (30.0 + 34.0 * 0.5)), Some(Spot::Row(0)));
    }

    /// The same numbers as the picture: the middle of each drawn panel is what the mouse finds
    /// there, at several scales and layouts.
    #[test]
    fn mouse_hits_match_the_picture() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        for scale in [0.75, 1.0, 1.33, 2.0] {
            for (layout, list) in [
                (LAYOUT, list_at(100.0)),
                ([60.0, 40.0, 30.0, 700.0], list_at(100.0)),
                ([300.0, 28.0, 22.0, 400.0], list_at(100.0)),
                // List Width and List Height on their own.
                (LAYOUT, LIST),
                (LAYOUT, [900.0, 32.0]),
                (LAYOUT, [450.0, 64.0]),
                ([60.0, 40.0, 30.0, 700.0], [300.0, 20.0]),
            ] {
                let mut v = long_view(20, 4);
                v.layout = layout;
                v.list = list;
                let f = p.render(&v, scale);
                let mid = |q: [u32; 4]| ((q[0] + q[2]) as f32 / 2.0, (q[1] + q[3]) as f32 / 2.0);
                let (bar, tabs, list) = (mid(f.panels[0]), f.panels[1], f.panels[2]);
                assert_eq!(spot(&v, scale, bar.0, bar.1), Some(Spot::Bar), "{scale} {layout:?}");
                // Top and bottom of the tabs and the list, just inside.
                assert_eq!(spot(&v, scale, mid(tabs).0, tabs[1] as f32 + 1.0), Some(Spot::Tab(0)), "{scale} {layout:?}");
                assert_eq!(spot(&v, scale, mid(tabs).0, tabs[3] as f32 - 1.0), Some(Spot::Tab(3)), "{scale} {layout:?}");
                assert_eq!(spot(&v, scale, mid(list).0, list[1] as f32 + 1.0), Some(Spot::Row(4)), "{scale} {layout:?}");
                assert_eq!(spot(&v, scale, mid(list).0, list[3] as f32 - 1.0), Some(Spot::Row(16)), "{scale} {layout:?}");
                assert_eq!(spot(&v, scale, mid(list).0, list[3] as f32 + 1.0), None, "{scale} {layout:?}");
            }
        }
    }

    /// The pointer: an arrow with its tip at the top-left, only its own pixels copied, twice as big
    /// at 2x.
    #[test]
    fn pointer_picture() {
        let one = render_pointer(1.0);
        assert_eq!((one.w, one.h), (12, 19));
        assert_eq!(&one.rgba[0..4], &[0, 0, 0, 255]);
        // Every copied pixel is drawn; nothing outside the panels is.
        let mut covered = vec![false; (one.w * one.h) as usize];
        for p in &one.panels {
            for y in p[1]..p[3] {
                for x in p[0]..p[2] {
                    covered[(y * one.w + x) as usize] = true;
                    assert_eq!(one.rgba[((y * one.w + x) * 4 + 3) as usize], 255);
                }
            }
        }
        for (i, c) in covered.iter().enumerate() {
            assert_eq!(*c, one.rgba[i * 4 + 3] == 255, "pixel {i}");
        }
        let two = render_pointer(2.0);
        assert_eq!((two.w, two.h), (24, 38));
        assert_eq!(two.panels.len(), one.panels.len() * 2);
        // Never bigger than vkdraw's slot for it (128 x 128).
        let big = render_pointer(10.0);
        assert!(big.w <= 128 && big.h <= 128);
    }

    fn view(layout: [f32; 4]) -> View {
        View {
            path: "Aurora 0.1.0 > Self".into(),
            sections: ["Self", "Weapon"].map(String::from).to_vec(),
            section: 0,
            rows: vec![Row { text: "God Mode".into(), kind: Kind::Toggle(true), dim: false }],
            pick: Some(0),
            scroll: 0,
            help: String::new(),
            accent: [140, 60, 255],
            layout,
            list: list_at(100.0),
            visible: 13,
            spacer: SPACER,
        }
    }

    /// The right wall: at every List Width, Tabs Width and Address Bar Width the list and the bar
    /// end on the picture's right edge, the tabs end a gap left of the list, and the wall's place
    /// on screen (X) never moves. The mouse finds each part where it is drawn.
    #[test]
    fn widths_grow_to_the_left_of_the_wall() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let wall_x = 3000i32;
        for scale in [1.0, 1440.0 / 1080.0] {
            for list_w in [200.0, 450.0, 900.0] {
                for tabs_w in [60.0, 118.0, 300.0] {
                    for bar_w in [200.0, 602.0, 1200.0] {
                        let mut v = view([tabs_w, 34.0, 26.0, bar_w]);
                        v.list = [list_w, 32.0];
                        let f = p.render_in(&v, scale, wall_x as f32);
                        let (bar, tabs, list) = (f.panels[0], f.panels[1], f.panels[2]);
                        let what = format!("{scale} list {list_w} tabs {tabs_w} bar {bar_w}");
                        // The list and the bar end on the wall: the picture's right edge.
                        assert_eq!(list[2], f.w, "{what}");
                        assert_eq!(bar[2], f.w, "{what}");
                        // On screen the picture goes at X minus its width: the wall is at X.
                        let x = wall_x - f.w as i32;
                        assert_eq!(x + list[2] as i32, wall_x, "{what}");
                        // The tabs end a gap left of the list.
                        let m = Geo::of(&v, scale, wall_x as f32).m;
                        assert_eq!(tabs[2] as f32, list[0] as f32 - m.gap, "{what}");
                        assert_eq!(list[2] - list[0], m.right_w as u32, "{what}");
                        // Each part is as wide as set (all fit here), grown to the left.
                        assert_eq!(bar[2] - bar[0], m.bar_w as u32, "{what}");
                        assert_eq!(tabs[2] - tabs[0], m.left_w as u32, "{what}");
                        let y = |q: [u32; 4]| (q[1] + q[3]) as f32 / 2.0;
                        assert_eq!(spot_in(&v, scale, wall_x as f32, f.w as f32 - 1.0, y(list)), Some(Spot::Row(0)), "{what}");
                        assert_eq!(spot_in(&v, scale, wall_x as f32, f.w as f32 - 1.0, y(bar)), Some(Spot::Bar), "{what}");
                        assert_eq!(spot_in(&v, scale, wall_x as f32, tabs[2] as f32 - 1.0, tabs[1] as f32 + 1.0), Some(Spot::Tab(0)), "{what}");
                        assert_eq!(spot_in(&v, scale, wall_x as f32, f.w as f32 + 1.0, y(list)), None, "{what}");
                    }
                }
            }
        }
    }

    /// X is the wall: with less room left of it than the menu wants, only what sticks out on the
    /// left is cut at the picture's (the screen's) left edge; the wall, the list and the bar's
    /// right end stay put, and the help boxes wrap narrower under the tabs.
    #[test]
    fn left_overflow_is_cut_not_the_wall() {
        let v = view(LAYOUT);
        // 1080p, the old 100%: the menu wants 814 px left of the wall (help 330 + 4 + 480).
        assert_eq!(menu_width(&v, 1.0), 814.0);
        assert_eq!(list_width(&v, 1.0), 480.0);
        for room in [814.0, 700.0, 600.0, 490.0] {
            let g = Geo::of(&v, 1.0, room);
            assert_eq!(g.list_r, room.min(814.0), "{room}");
            assert_eq!(g.rx, g.list_r - 480.0, "{room}");
            assert_eq!(g.tabs_r, g.rx - 4.0, "{room}");
            // Cut at the left edge, never past it.
            assert_eq!(g.lx, (g.tabs_r - 118.0).max(0.0), "{room}");
            assert_eq!(g.bar_x, (g.list_r - 602.0).max(0.0), "{room}");
        }
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let mut v = view(LAYOUT);
        v.help = ["Command: flyspeed [0.01 to 10000.00]", "How fast you go.", "Click to input a value."].join(NEW_BOX);
        for room in [520.0, 700.0, f32::INFINITY] {
            let f = p.render_in(&v, 1.0, room);
            let wall = f.w;
            assert_eq!(f.panels[2][2], wall, "{room}");
            // The help boxes (after bar, tabs, list) end at the tabs' right edge and start inside
            // the picture.
            let tabs_r = f.panels[1][2];
            for q in &f.panels[3..] {
                assert_eq!(q[2], tabs_r, "{room}");
                assert!(q[0] < q[2], "{room}");
            }
        }
    }

    /// The old geometry, for the player's old profiles: from the address bar's left edge to the
    /// list's right edge was the usual tabs, the gap and the list.
    #[test]
    fn old_bar_to_wall_is_tabs_gap_and_list() {
        assert_eq!(old_bar_to_wall(1.0), 118.0 + 4.0 + 480.0);
        // The player 1440p screen at Menu Size 85%: 134 + 5 + 544.
        assert_eq!(old_bar_to_wall(1440.0 / 1080.0 * 0.85), 683.0);
        assert_eq!(left_reach(118.0, 1.0), 212.0);
        assert_eq!(left_reach(300.0, 1.0), 212.0);
        assert_eq!(left_reach(500.0, 1.0), 382.0);
    }

    /// List Width 450 and List Height 32 are exactly the menu at Menu Size 85%: every size the
    /// drawing and the mouse use is the old one at 85%, on the player 1440p screen and at 1080p.
    #[test]
    fn list_450_by_32_is_the_old_85_percent() {
        for sh in [1080.0f32, 1440.0, 2160.0] {
            let screen = sh / 1080.0;
            // As the old worker worked it out: screen height / 1080 x Menu Size / 100.
            let old = Metrics::at((sh / 1080.0 * 85.0 / 100.0).clamp(0.5, 4.0));
            let mut v = view(LAYOUT);
            v.list = LIST;
            let new = Geo::of(&v, screen, f32::INFINITY).m;
            for (a, b, what) in [
                (old.bar_h, new.bar_h, "bar height"),
                (old.bar_w, new.bar_w, "bar width"),
                (old.row_h, new.row_h, "row"),
                (old.tab_h, new.tab_h, "tab"),
                (old.left_w, new.left_w, "tabs width"),
                (old.right_w, new.right_w, "list width"),
                (old.gap, new.gap, "gap"),
                (old.pad, new.pad, "pad"),
                (old.pad, new.vpad, "pad down"),
                (old.help_w, new.help_w, "help width"),
            ] {
                assert_eq!(a, b, "{what} at {sh}");
            }
            assert!((old.font - new.font).abs() < 1e-3 && (old.bar_font - new.bar_font).abs() < 1e-3, "{sh}");
        }
        // On the player 1440p screen: rows 39 px, the list 544 px wide.
        let mut v = view(LAYOUT);
        v.list = LIST;
        let m = Geo::of(&v, 1440.0 / 1080.0, f32::INFINITY).m;
        assert_eq!((m.row_h, m.right_w), (39.0, 544.0));
        // The old Menu Size, for old profiles: 85% is 450 / 32.
        assert_eq!(list_at(85.0).map(f32::round), LIST);
    }

    /// List Width changes only what goes across, List Height only what goes down and the text.
    #[test]
    fn width_and_height_each_scale_their_own_way() {
        let base = |list: [f32; 2]| {
            let mut v = view(LAYOUT);
            v.list = list;
            Geo::of(&v, 1.0, f32::INFINITY).m
        };
        let (one, wide, tall) = (base(LIST), base([900.0, 32.0]), base([450.0, 64.0]));
        // Twice as wide, give or take the rounding to whole pixels.
        let twice = |a: f32, b: f32| (a - 2.0 * b).abs() <= 1.0;
        assert!(twice(wide.right_w, one.right_w) && twice(wide.left_w, one.left_w) && twice(wide.bar_w, one.bar_w));
        assert_eq!((wide.row_h, wide.tab_h, wide.bar_h, wide.font), (one.row_h, one.tab_h, one.bar_h, one.font));
        assert!((tall.row_h - 2.0 * one.row_h).abs() <= 1.0 && (tall.font - 2.0 * one.font).abs() < 1e-3);
        assert_eq!((tall.right_w, tall.left_w, tall.bar_w), (one.right_w, one.left_w, one.bar_w));
    }

    /// One help box can hold several lines: a plain line break stays in the box, a blank line
    /// starts the next one.
    #[test]
    fn one_help_box_holds_several_lines() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let mut v = view(LAYOUT);
        v.help = format!("Command: fly{NEW_BOX}W A S D move\nL Shift up\nL Ctrl down\nSpace sprint");
        let f = p.render(&v, 1.0);
        // Bar, tabs, list, then two help boxes: the command and the four lines.
        assert_eq!(f.panels.len(), 5);
        let m = Geo::of(&v, 1.0, f32::INFINITY).m;
        let line_h = (m.row_h * 0.78).round();
        let (one, four) = (f.panels[3], f.panels[4]);
        assert_eq!(one[3] - one[1], (line_h + m.vpad) as u32);
        assert_eq!(four[3] - four[1], (4.0 * line_h + m.vpad) as u32);
        // Every line its own box before: now three plain lines are one box.
        v.help = "a\nb\nc".into();
        assert_eq!(p.render(&v, 1.0).panels.len(), 4);
    }

    /// A command's [range] never breaks inside when it fits on a line (the 5 Nov
    /// night test showed "... (0 is" / "off)]").
    #[test]
    fn wrap_keeps_a_range_whole() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let size = 22.0 * 0.85 * 0.82;
        let line = "Command: damagemultiplier [0.00 to 1000.00 (0 is off)]";
        // Room for all but the last word: the old wrap broke inside the brackets.
        let width = p.text_width("Command: damagemultiplier [0.00 to 1000.00 (0 is", size) + 1.0;
        assert_eq!(p.wrap(line, size, width), ["Command: damagemultiplier", "[0.00 to 1000.00 (0 is off)]"]);
        // A line that fits stays one line.
        assert_eq!(p.wrap(line, size, p.text_width(line, size) + 1.0), [line]);
        // A group wider than a whole line wraps word by word, as before.
        let narrow = p.text_width("[0.00 to 1000.00", size) + 1.0;
        let lines = p.wrap(line, size, narrow);
        assert!(lines.len() > 2, "{lines:?}");
        assert!(lines.iter().all(|l| p.text_width(l, size) <= narrow || !l.contains(' ')), "{lines:?}");
        // No brackets: as before.
        assert_eq!(p.wrap("one two three", size, p.text_width("one two", size) + 1.0), ["one two", "three"]);
        assert_eq!(pieces("a [b c] d [e", |_| true), ["a", "[b c]", "d", "[e"]);
        assert_eq!(pieces("[b c]", |_| false), ["[b", "c]"]);
    }

    /// Max Visible Commands: the list shows that many rows, scrolls past them, and the mouse finds
    /// nothing under the last one shown.
    #[test]
    fn max_visible_commands() {
        let mut v = long_view(20, 0);
        v.visible = 3;
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 2.5), Some(Spot::Row(2)));
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 3.5), None);
        v.scroll = 17;
        assert_eq!(spot(&v, 1.0, 400.0, 30.0 + 34.0 * 2.5), Some(Spot::Row(19)));
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let f = p.render(&v, 1.0);
        assert_eq!(f.panels[2][3] - f.panels[2][1], 3 * 34);
        // Rows that fit: 1080 px at the usual rows is 30 under the bar; huge rows still 1.
        assert_eq!(rows_fit(LAYOUT, list_at(100.0), SPACER, 1.0, 1080.0), (1080 - 26 - 4) / 34);
        assert_eq!(rows_fit(LAYOUT, [450.0, 32000.0], SPACER, 1.0, 100.0), 1);
    }

    /// The command box follows List Width and List Height as the menu does.
    #[test]
    fn command_box_follows_the_list() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let one = p.render_input("Box", "fly", 3, &["Fly".into()], [255, 0, 255], 1.0, LIST);
        let wide = p.render_input("Box", "fly", 3, &["Fly".into()], [255, 0, 255], 1.0, [900.0, 32.0]);
        let tall = p.render_input("Box", "fly", 3, &["Fly".into()], [255, 0, 255], 1.0, [450.0, 64.0]);
        // The gap between the parts follows the height, so the width moves by it only.
        assert!(wide.w > one.w * 19 / 10 && wide.h == one.h);
        assert!(tall.h > one.h * 19 / 10 && tall.w.abs_diff(one.w) <= 4);
    }

    /// Left / Right move the command box's caret. It is drawn over the text, so between two caret
    /// spots only the pixels around those two spots change: the text itself stays put. Past the end
    /// is the end.
    #[test]
    fn command_box_caret_moves_over_still_text() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let text = "credits 2147483647";
        let draw = |caret: usize| p.render_input("Box", text, caret, &[], [255, 0, 255], 1.0, LIST);
        let (sx, sy) = axes(1.0, LIST);
        let (m, at_end) = (Metrics::laid_out(sx, sy, LAYOUT, SPACER), draw(text.len()));
        assert_eq!(draw(99).rgba, at_end.rgba, "past the end is the end");
        let (a, b) = (draw(1), draw(10));
        assert_eq!((a.w, a.h), (at_end.w, at_end.h), "the box keeps its size");
        let (xa, xb) = (m.pad + p.pen_at(text, 1, m.font), m.pad + p.pen_at(text, 10, m.font));
        let bar = p.text_width("|", m.font);
        let (mut near_a, mut near_b) = (false, false);
        for y in 0..a.h {
            for x in 0..a.w {
                let i = ((y * a.w + x) * 4) as usize;
                if a.rgba[i..i + 4] == b.rgba[i..i + 4] {
                    continue;
                }
                let x = x as f32 + 0.5;
                near_a |= (x - xa).abs() <= bar;
                near_b |= (x - xb).abs() <= bar;
                assert!((x - xa).abs() <= bar || (x - xb).abs() <= bar, "pixel at x {x} moved: the text shifted");
            }
        }
        assert!(near_a && near_b, "the caret left one spot and showed at the other");
        // The caret walks right with its spot, and the start is at the text's left edge.
        assert_eq!(p.pen_at(text, 0, m.font), 0.0);
        assert!(p.pen_at(text, 1, m.font) < p.pen_at(text, 2, m.font));
        assert_eq!(p.pen_at(text, text.len(), m.font), p.text_width(text, m.font));
    }

    /// The notification fits its text: a short one is narrow, its height follows the text's size,
    /// and Width is only how wide it may grow before wrapping.
    #[test]
    fn notes_fit_their_text() {
        let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let p = Painter::new(font).unwrap();
        let look = |font: f32| NoteLook { width: 400.0, border: [255, 0, 255], back: [0, 0, 0], font };
        let small = p.render_note("Fly on", &look(NOTE_FONT), 1.0);
        let big = p.render_note("Fly on", &look(NOTE_FONT * 2.0), 1.0);
        assert!(small.w < 120, "snug: {}", small.w);
        assert!(small.h <= 30, "snug: {}", small.h);
        // Twice the text, about twice the height and wider.
        assert!(big.h > small.h * 3 / 2 && big.h <= small.h * 2 + 2, "{} {}", small.h, big.h);
        assert!(big.w > small.w);
        // The box's own Size scales the padding, not the text.
        let padded = p.render_note("Fly on", &look(NOTE_FONT), 2.0);
        assert!(padded.h > small.h && padded.h < big.h + 6, "{} {}", padded.h, big.h);
        // A long one wraps at Width and grows down, never wider.
        let long = p.render_note(&"word ".repeat(60), &look(NOTE_FONT), 1.0);
        assert!(long.w <= 400 && long.h > small.h * 3);
    }
}
