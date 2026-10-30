//! Visual system — "ops console".
//!
//! Graphite base, hairline 1px borders, no gradients or glows. One signal-lime accent reserved
//! for action and the current state. Monospace (Cascadia Mono) for data, labels, timers and
//! numbers; Segoe UI for reading. Small, differentiated radii: tags 2, controls 4, panels 6,
//! dialogs 8. Fonts come from Windows itself, so nothing is bundled.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Stroke, TextStyle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static LIGHT: AtomicBool = AtomicBool::new(false);

pub const R_TAG: u8 = 2;
pub const R_CONTROL: u8 = 4;
pub const R_PANEL: u8 = 6;
pub const R_DIALOG: u8 = 8;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub accent_soft: Color32,
    /// Accent fill under the pointer.
    pub accent_hover: Color32,
    /// Background of a hovered control or row.
    pub hover: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub info: Color32,
    pub code_bg: Color32,
}

pub fn pal() -> Palette {
    if LIGHT.load(Ordering::Relaxed) {
        Palette {
            bg: Color32::from_rgb(243, 244, 240),
            surface: Color32::from_rgb(255, 255, 255),
            surface2: Color32::from_rgb(236, 238, 232),
            border: Color32::from_rgb(216, 220, 211),
            border_strong: Color32::from_rgb(186, 192, 182),
            text: Color32::from_rgb(20, 25, 23),
            muted: Color32::from_rgb(88, 99, 93),
            faint: Color32::from_rgb(150, 158, 152),
            accent: Color32::from_rgb(63, 111, 10),
            on_accent: Color32::from_rgb(255, 255, 255),
            accent_soft: Color32::from_rgb(230, 239, 216),
            accent_hover: Color32::from_rgb(82, 138, 22),
            hover: Color32::from_rgb(226, 232, 218),
            success: Color32::from_rgb(47, 122, 33),
            warning: Color32::from_rgb(154, 91, 0),
            danger: Color32::from_rgb(192, 50, 45),
            info: Color32::from_rgb(31, 111, 134),
            code_bg: Color32::from_rgb(250, 251, 248),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(11, 14, 13),
            surface: Color32::from_rgb(17, 22, 21),
            surface2: Color32::from_rgb(24, 30, 28),
            border: Color32::from_rgb(46, 56, 52),
            border_strong: Color32::from_rgb(68, 80, 75),
            text: Color32::from_rgb(220, 227, 223),
            muted: Color32::from_rgb(134, 147, 141),
            faint: Color32::from_rgb(84, 95, 90),
            accent: Color32::from_rgb(168, 227, 58),
            on_accent: Color32::from_rgb(11, 14, 13),
            accent_soft: Color32::from_rgb(30, 41, 22),
            accent_hover: Color32::from_rgb(194, 240, 108),
            hover: Color32::from_rgb(33, 42, 37),
            success: Color32::from_rgb(130, 214, 92),
            warning: Color32::from_rgb(229, 168, 75),
            danger: Color32::from_rgb(239, 91, 85),
            info: Color32::from_rgb(99, 197, 218),
            code_bg: Color32::from_rgb(13, 17, 16),
        }
    }
}

pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

pub fn set_light(ctx: &egui::Context, light: bool) {
    LIGHT.store(light, Ordering::Relaxed);
    apply_style(ctx);
}

/// Phosphor icons (MIT), embedded so they look the same on Windows 7 through 11.
pub mod icon {
    pub const PLAY: &str = "\u{E3D0}";
    pub const CHECK: &str = "\u{E182}";
    pub const FLAG: &str = "\u{E244}";
    pub const CLOCK: &str = "\u{E492}";
    pub const SAVE: &str = "\u{E248}";
    pub const ADD: &str = "\u{E3D4}";
    pub const DELETE: &str = "\u{E4A6}";
    pub const EDIT: &str = "\u{E3B4}";
    pub const NETWORK: &str = "\u{EDDE}";
    pub const LOCK: &str = "\u{E308}";
    pub const SEND: &str = "\u{E398}";
    pub const OPEN: &str = "\u{E256}";
    pub const SETTINGS: &str = "\u{E272}";
    pub const UP: &str = "\u{E13C}";
    pub const DOWN: &str = "\u{E136}";
    pub const BACK: &str = "\u{E058}";
    pub const FORWARD: &str = "\u{E06C}";
    pub const DOWNLOAD: &str = "\u{E20C}";
    pub const CANCEL: &str = "\u{E4F6}";
    pub const WARNING: &str = "\u{E4E0}";
    pub const INFO: &str = "\u{E2CE}";
    pub const COPY: &str = "\u{E1CA}";
    pub const STOP: &str = "\u{E46C}";
    pub const CHART: &str = "\u{E150}";
    pub const DOC: &str = "\u{E23A}";
    pub const TESTS: &str = "\u{EADC}";
    pub const SUN: &str = "\u{E472}";
    pub const MOON: &str = "\u{E330}";
    pub const EXPORT: &str = "\u{EAF0}";
    pub const MESSAGE: &str = "\u{E17A}";
    pub const ZOOM_IN: &str = "\u{E310}";
    pub const ZOOM_OUT: &str = "\u{E30E}";
    pub const UNDO: &str = "\u{E038}";
    pub const LIBRARY: &str = "\u{E758}";
}

fn load_font(names: &[&str]) -> Option<Vec<u8>> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    names.iter().find_map(|n| std::fs::read(std::path::Path::new(&windir).join("Fonts").join(n)).ok())
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |key: &str, files: &[&str]| -> bool {
        match load_font(files) {
            Some(bytes) => {
                fonts.font_data.insert(key.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            None => false,
        }
    };
    let ui = add("ui", &["segoeui.ttf"]);
    let semi = add("ui-semibold", &["seguisb.ttf", "segoeuib.ttf"]);
    let mono = add("mono", &["CascadiaMono.ttf", "CascadiaCode.ttf", "consola.ttf"]);
    let mono_bold = add("mono-bold", &["CascadiaMono.ttf", "consolab.ttf"]);
    let symbols = add("symbols", &["seguisym.ttf"]);
    // Arabic presentation forms (joined letters) for text shaped by `rtl`.
    let arabic = add("arabic", &["tahoma.ttf", "segoeui.ttf"]);
    fonts.font_data.insert("icons".into(), Arc::new(FontData::from_static(include_bytes!("../../assets/Phosphor.ttf"))));
    let icons = true;

    let mut tail = Vec::new();
    if icons {
        tail.push("icons".to_string());
    }
    if symbols {
        tail.push("symbols".to_string());
    }
    if arabic {
        tail.push("arabic".to_string());
    }
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    if ui {
        prop.insert(0, "ui".into());
    }
    prop.extend(tail.clone());
    let monos = fonts.families.entry(FontFamily::Monospace).or_default();
    if mono {
        monos.insert(0, "mono".into());
    }
    monos.extend(tail.clone());

    let mut semibold = vec![if semi { "ui-semibold".to_string() } else if ui { "ui".into() } else { "Ubuntu-Light".into() }];
    semibold.extend(tail.clone());
    semibold.push("Ubuntu-Light".into());
    fonts.families.insert(FontFamily::Name("semibold".into()), semibold);

    let mut mono_strong = vec![if mono_bold { "mono-bold".to_string() } else { "Hack".into() }];
    mono_strong.extend(tail);
    mono_strong.push("Hack".into());
    fonts.families.insert(FontFamily::Name("mono-strong".into()), mono_strong);
    ctx.set_fonts(fonts);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn heading(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(semibold(size)).color(pal().text)
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(pal().muted)
}

/// Small monospace label above a section: "QUESTIONS", "01 / BUILD".
pub fn eyebrow(text: impl Into<String>) -> RichText {
    RichText::new(text.into().to_uppercase()).font(mono(11.0)).color(pal().muted).extra_letter_spacing(1.2)
}

/// Monospace figure for counts, scores, times.
pub fn figure(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text).font(mono(size)).color(color)
}

pub fn apply_style(ctx: &egui::Context) {
    let p = pal();
    let mut style = (*ctx.global_style()).clone();
    style.visuals = if is_light() { egui::Visuals::light() } else { egui::Visuals::dark() };
    let v = &mut style.visuals;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0_f32, p.border_strong);
    v.window_corner_radius = CornerRadius::same(R_DIALOG);
    v.menu_corner_radius = CornerRadius::same(R_CONTROL);
    v.extreme_bg_color = p.code_bg;
    v.faint_bg_color = p.surface2;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.28);
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0_f32, p.accent);
    v.striped = true;
    let r = CornerRadius::same(R_CONTROL);
    for (w, fill) in [
        (&mut v.widgets.noninteractive, p.surface),
        (&mut v.widgets.inactive, p.surface2),
        (&mut v.widgets.hovered, p.hover),
        (&mut v.widgets.active, p.accent_soft),
        (&mut v.widgets.open, p.surface2),
    ] {
        w.corner_radius = r;
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent.gamma_multiply(0.7));
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(70) };
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(90) };

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 32.0;
    style.spacing.window_margin = Margin::same(20);
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = true;
    style.text_styles = [
        (TextStyle::Heading, semibold(22.0)),
        (TextStyle::Body, FontId::proportional(14.5)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Monospace, FontId::monospace(13.5)),
    ]
    .into();
    ctx.set_global_style(style);
}

pub fn card_frame() -> egui::Frame {
    let p = pal();
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(R_PANEL)).inner_margin(Margin::same(18))
}

pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame().show(ui, add).inner
}

/// Whether the widget about to be added is under the pointer. egui keeps last frame's response,
/// and buttons take `next_auto_id`, so this lets explicitly coloured controls still react to hover.
pub fn will_hover(ui: &egui::Ui) -> bool {
    ui.ctx().read_response(ui.next_auto_id()).is_some_and(|r| r.hovered() && r.enabled())
}

/// Accent fill that brightens under the pointer.
pub fn accent_fill(ui: &egui::Ui) -> Color32 {
    let p = pal();
    if will_hover(ui) { p.accent_hover } else { p.accent }
}

/// Transparent fill that gets a soft background under the pointer.
pub fn clear_fill(ui: &egui::Ui) -> Color32 {
    if will_hover(ui) { pal().hover } else { Color32::TRANSPARENT }
}

/// Outline that turns accent under the pointer.
pub fn outline(ui: &egui::Ui, base: Color32) -> Stroke {
    Stroke::new(1.0_f32, if will_hover(ui) { pal().accent.gamma_multiply(0.8) } else { base })
}

fn button(text: RichText, fill: Color32, stroke: Color32) -> egui::Button<'static> {
    egui::Button::new(text).fill(fill).stroke(Stroke::new(1.0_f32, stroke)).corner_radius(CornerRadius::same(R_CONTROL)).min_size(egui::vec2(0.0, 34.0))
}

/// The one dominant action on a screen.
pub fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = pal();
    let fill = accent_fill(ui);
    ui.add(button(RichText::new(text).font(semibold(14.0)).color(p.on_accent), fill, fill))
}

/// Destructive action: outlined in red, tinted under the pointer.
pub fn danger(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = pal();
    let h = will_hover(ui);
    ui.add(button(RichText::new(text).font(semibold(14.0)).color(p.danger), if h { p.danger.gamma_multiply(0.14) } else { Color32::TRANSPARENT }, p.danger.gamma_multiply(if h { 1.0 } else { 0.7 })))
}

/// Secondary action: outline only.
pub fn ghost(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = pal();
    let h = will_hover(ui);
    ui.add(button(RichText::new(text).color(p.text), if h { p.hover } else { Color32::TRANSPARENT }, if h { p.accent.gamma_multiply(0.8) } else { p.border_strong }))
}

pub fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str) -> egui::Response {
    let p = pal();
    let col = if will_hover(ui) { p.text } else { p.muted };
    ui.add(egui::Button::new(RichText::new(icon).size(14.0).color(col)).frame_when_inactive(false).min_size(egui::vec2(28.0, 28.0))).on_hover_text(tip)
}

/// Square status tag with a monospace label — used for state, never for decoration.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::same(R_TAG))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| ui.label(RichText::new(text).font(mono(11.5)).color(color)))
        .response
}

pub fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(eyebrow(text));
}

/// Width for a text field that should fill the row. A finite width already includes the field's
/// inner margin, whereas `f32::INFINITY` adds the margin on top and overflows the column.
pub fn fill(ui: &egui::Ui, _margin_x: i8) -> f32 {
    ui.available_width().max(40.0)
}

pub fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> egui::Response {
    let width = if width.is_finite() { width.min(fill(ui, 10)) } else { fill(ui, 10) };
    ui.add(crate::ui::text_field::TextField::singleline(value).hint_text(hint).desired_width(width).margin(Margin::symmetric(10, 8)))
}

pub fn progress_bar(ui: &mut egui::Ui, frac: f32, height: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let p = pal();
    ui.painter().rect_filled(rect, CornerRadius::same(1), p.surface2);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    if fill.width() > 0.5 {
        ui.painter().rect_filled(fill, CornerRadius::same(1), color);
    }
}

/// Status line: a small square marker plus a monospace label.
pub fn status_dot(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    // Painted as one unit so the marker stays on the left in any layout direction.
    let galley = ui.painter().layout_no_wrap(text.to_string(), mono(12.0), color);
    let size = egui::vec2(13.0 + galley.size().x, galley.size().y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mark = egui::Rect::from_center_size(egui::pos2(rect.left() + 3.5, rect.center().y), egui::vec2(7.0, 7.0));
    ui.painter().rect_filled(mark, CornerRadius::same(1), color);
    ui.painter().galley(egui::pos2(rect.left() + 13.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// The HDJLabs mark: a framed square with a monospace "H" and a cursor bar.
pub fn brand(ui: &mut egui::Ui, subtitle: &str) {
    let p = pal();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect(rect, CornerRadius::same(R_CONTROL), p.surface2, Stroke::new(1.0_f32, p.border_strong), egui::StrokeKind::Inside);
        painter.text(rect.center() - egui::vec2(0.0, 2.0), egui::Align2::CENTER_CENTER, "H", FontId::new(16.0, FontFamily::Name("mono-strong".into())), p.accent);
        let bar = egui::Rect::from_min_size(egui::pos2(rect.center().x - 5.0, rect.bottom() - 7.0), egui::vec2(10.0, 2.0));
        painter.rect_filled(bar, CornerRadius::ZERO, p.accent);
        ui.add_space(2.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new("HDJLabs").font(semibold(15.0)).color(p.text));
            ui.add(egui::Label::new(RichText::new(subtitle.to_uppercase()).font(mono(10.0)).color(p.muted).extra_letter_spacing(0.8)).truncate());
        });
    });
}

/// Faint 1px measurement grid — used only behind the exam's entry and exit screens.
pub fn paint_grid(ui: &egui::Ui, rect: egui::Rect) {
    let p = pal();
    let c = if is_light() { p.border.gamma_multiply(0.55) } else { p.border.gamma_multiply(0.45) };
    let step = 32.0;
    let painter = ui.painter_at(rect);
    let mut x = rect.left() - rect.left() % step;
    while x < rect.right() {
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(1.0_f32, c));
        x += step;
    }
    let mut y = rect.top() - rect.top() % step;
    while y < rect.bottom() {
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], Stroke::new(1.0_f32, c));
        y += step;
    }
}

pub fn fmt_secs(s: i64) -> String {
    let s = s.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

pub fn fmt_points(p: f32) -> String {
    if (p - p.round()).abs() < 0.005 { format!("{}", p.round() as i64) } else { format!("{p:.2}") }
}
