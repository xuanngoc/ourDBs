//! Look and feel: design tokens, fonts, both themes and the shared widgets.
//!
//! Every screen takes its colors from [`Palette`] and its controls from the
//! helpers here, so the app looks the same everywhere.

use std::{hash::Hash, sync::Arc};

use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Context, CornerRadius, FontData, FontDefinitions,
    FontFamily, FontId, Frame, Galley, Margin, Painter, Rect, Response, Sense, Shadow, Stroke,
    StrokeKind, TextEdit, TextStyle, TextureHandle, Theme, Ui, Vec2, Visuals, Widget,
    epaint::text::{FontTweak, VariationCoords},
    pos2,
    style::WidgetVisuals,
    text::{LayoutJob, TextFormat, TextWrapping},
    vec2,
};
use ourdb_core::{Env, Kind};

use crate::icons::{self, Icon};

/// Grid body row height in points.
pub const ROW_HEIGHT: f32 = 24.0;
/// Grid header height in points.
pub const HEADER_HEIGHT: f32 = 26.0;
/// The top bar (traffic lights over the sidebar, tab strip over the content).
pub const BAR_HEIGHT: f32 = 38.0;
/// Toolbars under the tab strip.
pub const TOOLBAR_HEIGHT: f32 = 40.0;
/// Status bars at the bottom.
pub const STATUS_HEIGHT: f32 = 24.0;
/// Height of buttons, text fields and combo boxes.
pub const CONTROL_HEIGHT: f32 = 24.0;
/// Corner radius of controls and selected rows.
pub const RADIUS: u8 = 6;

const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Design tokens for one theme.
pub struct Palette {
    pub dark: bool,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    /// Accent tint for selected rows.
    pub accent_soft: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub content: Color32,
    pub sidebar: Color32,
    /// Tab strip and status bars.
    pub chrome: Color32,
    pub dialog: Color32,
    pub separator: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_pressed: Color32,
    pub control_border: Color32,
    pub control_border_hover: Color32,
    /// Overlay for hovered borderless things.
    pub hover: Color32,
    pub pressed: Color32,
    pub header_bg: Color32,
    pub zebra: Color32,
    pub grid_line: Color32,
    pub selected_row: Color32,
    pub code_bg: Color32,
    pub segment_bg: Color32,
    pub text_selection: Color32,
    pub edited: Color32,
    pub new_row: Color32,
    pub deleted: Color32,
    /// Red text.
    pub error: Color32,
    /// Red fill for dangerous buttons.
    pub danger: Color32,
    pub danger_hover: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn rgba(hex: u32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, alpha)
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            accent: rgb(0x0A84FF),
            accent_hover: rgb(0x409CFF),
            accent_pressed: rgb(0x0070E0),
            accent_soft: rgba(0x0A84FF, 64),
            text: rgb(0xF5F5F7),
            text2: rgb(0x98989D),
            text3: rgb(0x636366),
            content: rgb(0x1E1E1E),
            sidebar: rgb(0x252527),
            chrome: rgb(0x252527),
            dialog: rgb(0x2A2A2C),
            separator: rgb(0x38383A),
            control: rgb(0x2C2C2E),
            control_hover: rgb(0x363638),
            control_pressed: rgb(0x424245),
            control_border: rgb(0x48484A),
            control_border_hover: rgb(0x5A5A5E),
            hover: Color32::from_white_alpha(15),
            pressed: Color32::from_white_alpha(30),
            header_bg: rgb(0x262628),
            zebra: rgb(0x232324),
            grid_line: rgb(0x2E2E30),
            selected_row: rgba(0x0A84FF, 46),
            code_bg: rgb(0x1E1E1E),
            segment_bg: rgb(0x1C1C1E),
            text_selection: rgb(0x3F638B),
            edited: rgb(0x433B1C),
            new_row: rgb(0x1E3B27),
            deleted: rgb(0x402525),
            error: rgb(0xFF6961),
            danger: rgb(0xFF453A),
            danger_hover: rgb(0xFF6259),
            ok: rgb(0x30D158),
            warn: rgb(0xFF9F0A),
            keyword: rgb(0xFF7AB2),
            string: rgb(0xFC6A5D),
            number: rgb(0xD0BF69),
            comment: rgb(0x7F8C98),
        }
    } else {
        Palette {
            dark,
            accent: rgb(0x007AFF),
            accent_hover: rgb(0x0068DB),
            accent_pressed: rgb(0x0058BA),
            accent_soft: rgba(0x007AFF, 38),
            text: rgb(0x1D1D1F),
            text2: rgb(0x6E6E73),
            text3: rgb(0xAEAEB2),
            content: Color32::WHITE,
            sidebar: rgb(0xF5F5F7),
            chrome: rgb(0xF5F5F7),
            dialog: Color32::WHITE,
            separator: rgb(0xE5E5EA),
            control: Color32::WHITE,
            control_hover: rgb(0xF5F5F7),
            control_pressed: rgb(0xE8E8ED),
            control_border: rgb(0xD1D1D6),
            control_border_hover: rgb(0xBDBDC3),
            hover: Color32::from_black_alpha(13),
            pressed: Color32::from_black_alpha(26),
            header_bg: rgb(0xF7F7F8),
            zebra: rgb(0xF6F6F8),
            grid_line: rgb(0xEDEDF0),
            selected_row: rgba(0x007AFF, 24),
            code_bg: rgb(0xF7F7F9),
            segment_bg: rgb(0xEBEBEF),
            text_selection: rgb(0xB3D7FF),
            edited: rgb(0xFFF5CC),
            new_row: rgb(0xE4F6E8),
            deleted: rgb(0xFDE6E5),
            error: rgb(0xD70015),
            danger: rgb(0xFF3B30),
            danger_hover: rgb(0xE6352B),
            ok: rgb(0x248A3D),
            warn: rgb(0xFF9F0A),
            keyword: rgb(0xAD3DA4),
            string: rgb(0xC41A16),
            number: rgb(0x1C00CF),
            comment: rgb(0x8A8A8E),
        }
    }
}

/// The palette of the theme `ui` shows.
pub fn pal(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// Dev green, Staging amber, Prod red.
pub fn env_color(env: Env) -> Option<Color32> {
    match env {
        Env::None => None,
        Env::Dev => Some(rgb(0x34C759)),
        Env::Staging => Some(rgb(0xFF9F0A)),
        Env::Prod => Some(rgb(0xFF3B30)),
    }
}

// -- setup --------------------------------------------------------------------

/// Set up fonts and both themes once at start.
pub fn setup(ctx: &Context) {
    fonts(ctx);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, regular(11.0)),
            (TextStyle::Body, regular(13.0)),
            (TextStyle::Button, regular(13.0)),
            (TextStyle::Heading, semibold(15.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        let sp = &mut s.spacing;
        sp.item_spacing = vec2(8.0, 6.0);
        sp.button_padding = vec2(10.0, 3.0);
        sp.interact_size = vec2(32.0, CONTROL_HEIGHT);
        sp.icon_width = 14.0;
        sp.icon_width_inner = 8.0;
        sp.icon_spacing = 6.0;
        sp.window_margin = Margin::same(20);
        sp.menu_margin = Margin::same(5);
        sp.indent = 14.0;
        sp.combo_height = 320.0;
        s.interaction.selectable_labels = false;
        s.interaction.tooltip_delay = 0.4;
    });
    ctx.set_visuals_of(Theme::Light, visuals(&palette(false)));
    ctx.set_visuals_of(Theme::Dark, visuals(&palette(true)));
}

/// SF Pro (text optical size) as the body face, the same file at weight 590 as
/// the "semibold" family, and SF Mono. Falls back to the egui fonts.
fn fonts(ctx: &Context) {
    let mut defs = FontDefinitions::default();
    let fallback = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut semibold_family = fallback.clone();
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        let data = FontData::from_owned(bytes);
        let face = |weight: f32| FontTweak {
            coords: VariationCoords::new([(b"opsz", 17.0), (b"wght", weight)]),
            ..Default::default()
        };
        defs.font_data
            .insert("sf".into(), Arc::new(data.clone().tweak(face(400.0))));
        defs.font_data
            .insert("sf-semibold".into(), Arc::new(data.tweak(face(590.0))));
        if let Some(f) = defs.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "sf".into());
        }
        semibold_family.insert(0, "sf-semibold".into());
    }
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_family);
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        defs.font_data
            .insert("sf-mono".into(), Arc::new(FontData::from_owned(bytes)));
        if let Some(f) = defs.families.get_mut(&FontFamily::Monospace) {
            f.insert(0, "sf-mono".into());
        }
    }
    ctx.set_fonts(defs);
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(RADIUS);
    let widget = |fill: Color32, border: Color32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(1.0, border),
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, p.text),
        expansion: 0.0,
    };
    v.widgets.noninteractive = WidgetVisuals {
        bg_fill: p.content,
        weak_bg_fill: p.content,
        bg_stroke: Stroke::new(1.0, p.separator),
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, p.text),
        expansion: 0.0,
    };
    v.widgets.inactive = widget(p.control, p.control_border);
    v.widgets.hovered = widget(p.control_hover, p.control_border_hover);
    v.widgets.active = widget(p.control_pressed, p.control_border_hover);
    v.widgets.open = widget(p.control_hover, p.control_border_hover);
    v.selection.bg_fill = p.text_selection;
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);
    v.weak_text_color = Some(p.text3);
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.error;
    v.warn_fg_color = p.warn;
    v.panel_fill = p.content;
    v.window_fill = p.dialog;
    v.window_stroke = Stroke::new(
        1.0,
        if p.dark {
            Color32::from_white_alpha(22)
        } else {
            Color32::from_black_alpha(18)
        },
    );
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 14],
        blur: 44,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 120 } else { 46 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 100 } else { 34 }),
    };
    v.extreme_bg_color = p.content;
    v.text_edit_bg_color = Some(p.control);
    v.faint_bg_color = p.zebra;
    v.code_bg_color = p.code_bg;
    v.striped = false;
    v.interact_cursor = None;
    v
}

/// The app icon as a texture, for the empty state.
pub fn load_app_icon(ctx: &Context) -> Option<TextureHandle> {
    let bytes: &[u8] = include_bytes!("../../../assets/icon-1024.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let image = match info.color_type {
        png::ColorType::Rgba => egui::ColorImage::from_rgba_unmultiplied([w, h], data),
        png::ColorType::Rgb => egui::ColorImage::from_rgb([w, h], data),
        _ => return None,
    };
    let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
    Some(ctx.load_texture("app-icon", image, options))
}

// -- text ---------------------------------------------------------------------

/// One line of text, cut with "…" when it is wider than `width`.
pub fn one_line(text: &str, font: FontId, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    job.break_on_newline = false;
    job
}

pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, color)
}

/// Paint `galley` vertically centered in `rect`, from `x`.
pub fn paint_galley_v(painter: &Painter, rect: Rect, x: f32, galley: Arc<Galley>) {
    let y = rect.center().y - galley.size().y / 2.0;
    painter.galley(pos2(x, y), galley, Color32::PLACEHOLDER);
}

/// Text, clipped with "…", vertically centered in `rect`.
pub fn paint_clipped(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) -> f32 {
    let g = ui
        .painter()
        .layout_job(one_line(text, font, color, rect.width()));
    let w = g.size().x;
    paint_galley_v(ui.painter(), rect, rect.left(), g);
    w
}

// -- buttons ------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    /// Accent fill, white text: the main action.
    Primary,
    /// Control fill with a border.
    Secondary,
    /// Red fill: an action that deletes or changes data.
    Danger,
    /// Accent text, no frame until hovered.
    Plain,
}

/// A push button. Use with `ui.add` or `ui.add_enabled`.
pub struct Btn<'a> {
    kind: BtnKind,
    text: &'a str,
    icon: Option<Icon>,
    shortcut: Option<&'a str>,
    min_width: f32,
    height: f32,
}

impl<'a> Btn<'a> {
    fn new(kind: BtnKind, text: &'a str) -> Self {
        Self {
            kind,
            text,
            icon: None,
            shortcut: None,
            min_width: 0.0,
            height: CONTROL_HEIGHT,
        }
    }
    pub fn primary(text: &'a str) -> Self {
        Self::new(BtnKind::Primary, text)
    }
    pub fn secondary(text: &'a str) -> Self {
        Self::new(BtnKind::Secondary, text)
    }
    pub fn danger(text: &'a str) -> Self {
        Self::new(BtnKind::Danger, text)
    }
    pub fn plain(text: &'a str) -> Self {
        Self::new(BtnKind::Plain, text)
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn shortcut(mut self, text: &'a str) -> Self {
        self.shortcut = Some(text);
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
}

impl Widget for Btn<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = pal(ui);
        // A disabled main button looks like a plain secondary one, like on macOS.
        let kind = match self.kind {
            BtnKind::Primary | BtnKind::Danger if !ui.is_enabled() => BtnKind::Secondary,
            k => k,
        };
        let fg = match kind {
            BtnKind::Primary | BtnKind::Danger => Color32::WHITE,
            BtnKind::Secondary => p.text,
            BtnKind::Plain => p.accent,
        };
        let font = regular(13.0);
        let text = (!self.text.is_empty()).then(|| galley(ui, self.text, font, fg));
        let hint = self.shortcut.map(|s| {
            let c = if kind == BtnKind::Secondary {
                p.text2
            } else {
                fg.gamma_multiply(0.72)
            };
            galley(ui, s, regular(12.0), c)
        });
        let pad = if kind == BtnKind::Plain { 6.0 } else { 10.0 };
        let icon_w = if self.icon.is_some() { 14.0 } else { 0.0 };
        let mut w = pad * 2.0 + icon_w;
        if let Some(t) = &text {
            w += t.size().x + if self.icon.is_some() { 5.0 } else { 0.0 };
        }
        if let Some(h) = &hint {
            w += h.size().x + 8.0;
        }
        let (rect, resp) =
            ui.allocate_exact_size(vec2(w.max(self.min_width), self.height), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let down = resp.is_pointer_button_down_on();
        let hovered = resp.hovered();
        let painter = ui.painter();
        let r = CornerRadius::same(RADIUS);
        match kind {
            BtnKind::Primary | BtnKind::Danger => {
                let (base, hover, press) = if kind == BtnKind::Primary {
                    (p.accent, p.accent_hover, p.accent_pressed)
                } else {
                    (p.danger, p.danger_hover, p.danger_hover)
                };
                let fill = if down {
                    press
                } else if hovered {
                    hover
                } else {
                    base
                };
                painter.rect_filled(rect, r, fill);
            }
            BtnKind::Secondary => {
                if !p.dark {
                    painter.rect_filled(
                        rect.translate(vec2(0.0, 0.5)),
                        r,
                        Color32::from_black_alpha(10),
                    );
                }
                let fill = if down {
                    p.control_pressed
                } else if hovered {
                    p.control_hover
                } else {
                    p.control
                };
                painter.rect_filled(rect, r, fill);
                painter.rect_stroke(
                    rect,
                    r,
                    Stroke::new(1.0, p.control_border),
                    StrokeKind::Inside,
                );
            }
            BtnKind::Plain => {
                if down {
                    painter.rect_filled(rect, r, p.pressed);
                } else if hovered {
                    painter.rect_filled(rect, r, p.hover);
                }
            }
        }
        // Content, centered.
        let mut x = rect.center().x - w / 2.0 + pad;
        if let Some(icon) = self.icon {
            let ir = Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0));
            icons::paint(painter, ir, icon, fg);
            x += 14.0 + 5.0;
        }
        if let Some(t) = text {
            let tw = t.size().x;
            paint_galley_v(painter, rect, x, t);
            x += tw + 8.0;
        }
        if let Some(h) = hint {
            paint_galley_v(painter, rect, x, h);
        }
        resp
    }
}

/// A borderless square button with an icon and a tooltip.
pub struct IconBtn<'a> {
    icon: Icon,
    tip: &'a str,
    size: f32,
}

impl<'a> IconBtn<'a> {
    pub fn new(icon: Icon, tip: &'a str) -> Self {
        Self {
            icon,
            tip,
            size: CONTROL_HEIGHT,
        }
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl Widget for IconBtn<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = pal(ui);
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::click());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if resp.is_pointer_button_down_on() {
                painter.rect_filled(rect, RADIUS, p.pressed);
            } else if resp.hovered() {
                painter.rect_filled(rect, RADIUS, p.hover);
            }
            let color = if resp.hovered() { p.text } else { p.text2 };
            let s = (self.size * 0.62).round().min(16.0);
            icons::paint(
                painter,
                Rect::from_center_size(rect.center(), Vec2::splat(s)),
                self.icon,
                color,
            );
        }
        if self.tip.is_empty() {
            resp
        } else {
            resp.on_hover_text(self.tip)
        }
    }
}

/// A thin vertical line between groups of toolbar items.
pub fn toolbar_divider(ui: &mut Ui) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 16.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, p.separator),
    );
}

// -- text fields ----------------------------------------------------------------

/// A bordered text field with an optional leading icon and an accent focus ring.
pub fn field(ui: &mut Ui, edit: TextEdit<'_>, icon: Option<Icon>, width: f32) -> Response {
    field_sized(ui, edit, icon, width, CONTROL_HEIGHT)
}

pub fn field_sized(
    ui: &mut Ui,
    edit: TextEdit<'_>,
    icon: Option<Icon>,
    width: f32,
    height: f32,
) -> Response {
    let p = pal(ui);
    let left = if icon.is_some() { 26 } else { 8 };
    let frame = Frame::NONE
        .fill(p.control)
        .corner_radius(RADIUS)
        .inner_margin(Margin {
            left,
            right: 8,
            top: 2,
            bottom: 2,
        });
    let width = width.max(40.0);
    let resp = ui.add(
        edit.frame(frame)
            .desired_width(width)
            .min_size(vec2(width, height))
            .align(Align2::LEFT_CENTER),
    );
    let rect = resp.rect;
    let painter = ui.painter();
    let focus = resp.has_focus();
    let border = if focus {
        p.accent
    } else if resp.hovered() {
        p.control_border_hover
    } else {
        p.control_border
    };
    painter.rect_stroke(rect, RADIUS, Stroke::new(1.0, border), StrokeKind::Inside);
    if focus {
        focus_ring(painter, rect, &p);
    }
    if let Some(icon) = icon {
        let ir =
            Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(13.0, 13.0));
        icons::paint(painter, ir, icon, p.text3);
    }
    resp
}

/// The soft accent ring around a focused control.
pub fn focus_ring(painter: &Painter, rect: Rect, p: &Palette) {
    painter.rect_stroke(
        rect,
        RADIUS as f32 + 1.0,
        Stroke::new(2.5, p.accent.gamma_multiply(0.45)),
        StrokeKind::Outside,
    );
}

/// A bordered box around a multiline editor. Paint the ring with [`focus_ring`].
pub fn editor_frame(p: &Palette, fill: Color32) -> Frame {
    Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, p.control_border))
        .corner_radius(RADIUS)
}

// -- combo box ------------------------------------------------------------------

/// A combo box with a small painted chevron.
pub fn combo<'a>(
    id_salt: impl Hash + std::fmt::Debug,
    selected: impl Into<String>,
    width: f32,
) -> ComboBox<'a> {
    ComboBox::from_id_salt(id_salt)
        .width(width)
        .selected_text(selected.into())
        .icon(|ui, rect, _visuals, _open| {
            let p = pal(ui);
            let r = Rect::from_center_size(rect.center() + vec2(1.0, 0.0), vec2(11.0, 11.0));
            let painter = ui.painter();
            icons::paint(
                painter,
                r.translate(vec2(0.0, -2.4)),
                icons::Icon::ChevronUp,
                p.text2,
            );
            icons::paint(
                painter,
                r.translate(vec2(0.0, 2.4)),
                icons::Icon::ChevronDown,
                p.text2,
            );
        })
}

// -- segmented control ----------------------------------------------------------

pub struct Segment<'a> {
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub dot: Option<Color32>,
}

impl<'a> Segment<'a> {
    pub fn text(label: &'a str) -> Self {
        Self {
            label,
            icon: None,
            dot: None,
        }
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn dot(mut self, color: Option<Color32>) -> Self {
        self.dot = color;
        self
    }
}

/// A row of mutually exclusive segments. Returns true when `value` changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    items: &[(T, Segment)],
    min_seg: f32,
) -> bool {
    let p = pal(ui);
    let h = CONTROL_HEIGHT;
    let font = regular(12.5);
    let bold = semibold(12.5);
    let widths: Vec<f32> = items
        .iter()
        .map(|(_, s)| {
            let mut w = galley(ui, s.label, bold.clone(), p.text).size().x + 22.0;
            if s.icon.is_some() {
                w += 18.0;
            }
            if s.dot.is_some() {
                w += 12.0;
            }
            w.max(min_seg)
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, RADIUS + 1, p.segment_bg);
    let mut changed = false;
    let mut x = rect.left() + 2.0;
    let selected = items.iter().position(|(v, _)| v == value);
    for (i, ((v, seg), w)) in items.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(*w, h - 4.0));
        let id = ui
            .scope_id()
            .with(("segment", rect.min.x as i32, rect.min.y as i32, i));
        let resp = ui.interact(r, id, Sense::click());
        let is_sel = selected == Some(i);
        if is_sel {
            if !p.dark {
                painter.rect_filled(
                    r.translate(vec2(0.0, 0.5)),
                    RADIUS - 1,
                    Color32::from_black_alpha(22),
                );
            }
            painter.rect_filled(
                r,
                RADIUS - 1,
                if p.dark {
                    rgb(0x5A5A5E)
                } else {
                    Color32::WHITE
                },
            );
        } else if resp.hovered() {
            painter.rect_filled(r, RADIUS - 1, p.hover);
        }
        // Divider between two segments that are not selected.
        if i > 0 && selected != Some(i) && selected != Some(i - 1) {
            painter.vline(
                r.left(),
                (r.top() + 4.0)..=(r.bottom() - 4.0),
                Stroke::new(1.0, p.control_border),
            );
        }
        let color = if is_sel { p.text } else { p.text2 };
        let label = galley(
            ui,
            seg.label,
            if is_sel { bold.clone() } else { font.clone() },
            color,
        );
        let mut content = label.size().x;
        if seg.icon.is_some() {
            content += 18.0;
        }
        if seg.dot.is_some() {
            content += 12.0;
        }
        let mut cx = r.center().x - content / 2.0;
        if let Some(icon) = seg.icon {
            icons::paint(
                &painter,
                Rect::from_center_size(pos2(cx + 7.0, r.center().y), vec2(13.0, 13.0)),
                icon,
                color,
            );
            cx += 18.0;
        }
        if let Some(c) = seg.dot {
            painter.circle_filled(pos2(cx + 3.5, r.center().y), 3.5, c);
            cx += 12.0;
        }
        paint_galley_v(&painter, r, cx, label);
        if resp.clicked() && !is_sel {
            *value = *v;
            changed = true;
        }
        x += w;
    }
    changed
}

// -- toggle switch --------------------------------------------------------------

/// A Mac-style switch for a boolean.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = pal(ui);
    let size = vec2(32.0, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let painter = ui.painter();
    let off = if p.dark { rgb(0x39393D) } else { rgb(0xE3E3E8) };
    let track = lerp_color(off, p.accent, t);
    painter.rect_filled(rect, 9.0, track);
    if !p.dark {
        painter.rect_stroke(
            rect,
            9.0,
            Stroke::new(1.0, Color32::from_black_alpha((14.0 * (1.0 - t)) as u8)),
            StrokeKind::Inside,
        );
    }
    let x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), t);
    let c = pos2(x, rect.center().y);
    painter.circle_filled(c + vec2(0.0, 0.6), 7.6, Color32::from_black_alpha(40));
    painter.circle_filled(c, 7.4, Color32::WHITE);
    resp
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

// -- badges ---------------------------------------------------------------------

/// Size of a pill for `text`.
pub fn pill_size(ui: &Ui, text: &str, font: &FontId) -> Vec2 {
    let g = galley(ui, text, font.clone(), Color32::WHITE);
    vec2(g.size().x + 10.0, (font.size + 5.0).round())
}

/// A small rounded label, e.g. a type name or "PROD".
pub fn paint_pill(ui: &Ui, rect: Rect, text: &str, font: FontId, fg: Color32, bg: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, bg);
    let g = galley(ui, text, font, fg);
    painter.galley(rect.center() - g.size() / 2.0, g, Color32::PLACEHOLDER);
}

/// Allocate and paint a pill.
pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let font = semibold(10.0);
    let size = pill_size(ui, text, &font);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_pill(ui, rect, text, font, fg, bg);
    resp
}

pub fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Postgres => rgb(0x336791),
        Kind::MySql => rgb(0x00758F),
        Kind::Sqlite => rgb(0x0F80CC),
        Kind::MongoDb => rgb(0x13AA52),
        Kind::DocumentDb => rgb(0x2E73B8),
    }
}

pub fn kind_monogram(kind: Kind) -> &'static str {
    match kind {
        Kind::Postgres => "Pg",
        Kind::MySql => "My",
        Kind::Sqlite => "Sq",
        Kind::MongoDb => "Mg",
        Kind::DocumentDb => "Dd",
    }
}

/// The database type badge: a rounded square in a brand color with a monogram.
pub fn paint_kind_badge(ui: &Ui, rect: Rect, kind: Kind) {
    let painter = ui.painter();
    let side = rect.width().min(rect.height());
    painter.rect_filled(rect, (side * 0.26).round(), kind_color(kind));
    let g = galley(
        ui,
        kind_monogram(kind),
        semibold((side * 0.5).round()),
        Color32::WHITE,
    );
    painter.galley(
        rect.center() - g.size() / 2.0 + vec2(0.0, 0.2),
        g,
        Color32::PLACEHOLDER,
    );
}

// -- dialogs --------------------------------------------------------------------

/// The frame of every modal dialog.
pub fn modal_frame(ctx: &Context) -> Frame {
    let style = ctx.global_style();
    let v = &style.visuals;
    Frame::NONE
        .fill(v.window_fill)
        .stroke(v.window_stroke)
        .shadow(v.window_shadow)
        .corner_radius(12)
        .inner_margin(20)
}

/// A modal dialog with the app's frame and a light backdrop.
pub fn modal(id: egui::Id, ctx: &Context) -> egui::Modal {
    let dark = ctx.global_style().visuals.dark_mode;
    egui::Modal::new(id)
        .frame(modal_frame(ctx))
        .backdrop_color(Color32::from_black_alpha(if dark { 110 } else { 64 }))
}

pub fn dialog_title(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(egui::RichText::new(text).font(semibold(15.0)).color(p.text));
}

/// Secondary text that wraps.
pub fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
}

/// A spinner in the secondary color.
pub fn spinner(ui: &mut Ui, size: f32) -> Response {
    let p = pal(ui);
    ui.add(egui::Spinner::new().size(size).color(p.text2))
}

/// Paint a spinner into `rect`.
pub fn paint_spinner(ui: &Ui, rect: Rect) {
    let p = pal(ui);
    egui::Spinner::new()
        .size(rect.height())
        .color(p.text2)
        .paint_at(ui, rect);
}

/// Right-aligned label column of a form, then the field.
pub fn form_row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(label_w, CONTROL_HEIGHT), Sense::hover());
        let g = galley(ui, label, regular(13.0), p.text2);
        let x = rect.right() - g.size().x;
        paint_galley_v(ui.painter(), rect, x, g);
        add(ui)
    })
    .inner
}

/// A horizontal hairline across `ui`.
pub fn hairline(ui: &mut Ui) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.separator),
    );
}

/// Center `text` in the free space of `ui`.
pub fn center_text(ui: &mut Ui, text: &str, color: Color32) {
    ui.centered_and_justified(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
    });
}

/// The top of an alert: an icon and a title.
pub fn alert_header(ui: &mut Ui, icon: Icon, color: Color32, title: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        icons::paint(ui.painter(), rect, icon, color);
        ui.add_space(2.0);
        dialog_title(ui, title);
    });
}

/// A row of buttons, laid out from the right edge. Add the main button first.
pub fn footer<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), CONTROL_HEIGHT),
        egui::Layout::right_to_left(Align::Center),
        add,
    )
    .inner
}
