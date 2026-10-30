//! A text field that edits Arabic (and any right-to-left or mixed text) correctly.
//!
//! egui's `TextEdit` lays text out strictly left to right, one glyph per character, without
//! letter joining, and its caret and hit-testing assume x grows with the character index. That
//! is fine for English and wrong for Arabic. `TextField` behaves exactly like `TextEdit` while
//! the text has no right-to-left characters, and switches — keeping focus and caret — to its own
//! editor as soon as it does:
//!
//! * letters take their joined forms (lam-alef becomes one ligature), brackets mirror;
//! * every line is reordered visually with the Unicode bidi algorithm, RTL paragraphs are
//!   right-aligned, and wrapping happens on words in logical order;
//! * the caret sits on the correct edge of each letter, clicks and drags select what's under
//!   the pointer, ←/→ move the way the text is displayed, ↑/↓ keep the column;
//! * typing, Backspace/Delete (Ctrl for words), Home/End, Ctrl+A, copy/cut/paste and
//!   undo/redo work as in any Windows text box.

use crate::ui::rtl::has_rtl;
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Color32, Event, EventFilter, FontId, Galley, Key, Margin, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2, Widget};
use std::sync::{Arc, OnceLock};
use unicode_bidi::BidiInfo;

pub struct TextField<'t> {
    text: &'t mut String,
    multiline: bool,
    hint: String,
    hint_font: Option<FontId>,
    hint_color: Option<Color32>,
    font: Option<FontId>,
    width: f32,
    rows: usize,
    margin: Margin,
    char_limit: usize,
}

impl<'t> TextField<'t> {
    pub fn singleline(text: &'t mut String) -> Self {
        TextField { text, multiline: false, hint: String::new(), hint_font: None, hint_color: None, font: None, width: f32::INFINITY, rows: 1, margin: Margin::symmetric(4, 2), char_limit: usize::MAX }
    }

    pub fn multiline(text: &'t mut String) -> Self {
        TextField { multiline: true, rows: 4, ..Self::singleline(text) }
    }

    pub fn hint_text(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Font and colour for the hint (defaults: the field's font, faint).
    pub fn hint_style(mut self, font: FontId, color: Color32) -> Self {
        self.hint_font = Some(font);
        self.hint_color = Some(color);
        self
    }

    pub fn font(mut self, font: FontId) -> Self {
        self.font = Some(font);
        self
    }

    /// Width of the text area; the margin is added outside it (as with `TextEdit`).
    pub fn desired_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn desired_rows(mut self, rows: usize) -> Self {
        self.rows = rows.max(1);
        self
    }

    pub fn margin(mut self, margin: Margin) -> Self {
        self.margin = margin;
        self
    }

    pub fn char_limit(mut self, limit: usize) -> Self {
        self.char_limit = limit;
        self
    }
}

#[derive(Clone, Default)]
struct EditState {
    /// The bidi editor (not `TextEdit`) owns this field right now.
    active: bool,
    caret: usize,
    anchor: usize,
    /// Horizontal scroll of a single-line field whose text is wider than the box.
    scroll: f32,
    /// Column kept while moving up and down.
    preferred_x: Option<f32>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    last_edit: f64,
    last_interaction: f64,
}

impl Widget for TextField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let id = ui.next_auto_id();
        let sid = id.with("bidi-edit");
        let mut st: EditState = ui.data_mut(|d| d.get_temp(sid)).unwrap_or_default();
        let font = self.font.clone().unwrap_or_else(|| TextStyle::Body.resolve(ui.style()));
        let rtl = has_rtl(self.text);

        if !rtl {
            if st.active {
                // Hand the caret back to egui's editor where ours left it.
                let mut te = TextEditState::load(ui.ctx(), id).unwrap_or_default();
                te.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(st.anchor), CCursor::new(st.caret))));
                te.store(ui.ctx(), id);
                st.active = false;
                ui.data_mut(|d| d.insert_temp(sid, st));
            }
            let hint = RichText::new(&self.hint).font(self.hint_font.clone().unwrap_or(font.clone())).color(self.hint_color.unwrap_or(ui.visuals().weak_text_color()));
            let te = if self.multiline { egui::TextEdit::multiline(self.text).desired_rows(self.rows) } else { egui::TextEdit::singleline(self.text) };
            return ui.add(te.id(id).font(font).hint_text(hint).desired_width(self.width).margin(self.margin).char_limit(self.char_limit));
        }

        if !st.active {
            // Take over from egui's editor at its caret (usually right after the letter just typed).
            let n = self.text.chars().count();
            let (a, c) = TextEditState::load(ui.ctx(), id).and_then(|s| s.cursor.char_range()).map(|r| (r.secondary.index, r.primary.index)).unwrap_or((n, n));
            st = EditState { active: true, caret: c.min(n), anchor: a.min(n), ..Default::default() };
        }
        bidi_edit(ui, id, sid, st, self, font)
    }
}

// ───────────── layout ─────────────

const LAM: char = '\u{0644}';

/// Lam + this alef → (isolated, final) ligature.
fn lam_alef(alef: char) -> Option<(char, char)> {
    match alef {
        '\u{0622}' => Some(('\u{FEF5}', '\u{FEF6}')),
        '\u{0623}' => Some(('\u{FEF7}', '\u{FEF8}')),
        '\u{0625}' => Some(('\u{FEF9}', '\u{FEFA}')),
        '\u{0627}' => Some(('\u{FEFB}', '\u{FEFC}')),
        _ => None,
    }
}

/// Contextual letter forms, one output character per input character (ligatures are made
/// separately so the caret can still stop between lam and alef).
fn shape(chars: &[char]) -> Vec<char> {
    static RESHAPER: OnceLock<ar_reshaper::ArabicReshaper> = OnceLock::new();
    let r = RESHAPER.get_or_init(|| {
        let mut c = ar_reshaper::ReshaperConfig::new(ar_reshaper::config::Language::Arabic, ar_reshaper::config::LigaturesFlags::none());
        c.delete_harakat = false;
        c.delete_tatweel = false;
        c.support_zwj = false;
        ar_reshaper::ArabicReshaper::new(c)
    });
    let s: String = chars.iter().collect();
    let out: Vec<char> = r.reshape(&s).chars().collect();
    if out.len() == chars.len() { out } else { chars.to_vec() }
}

fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        c => c,
    }
}

struct Line {
    /// Index (in chars of the whole text) of the first character.
    start: usize,
    len: usize,
    /// Last line of its paragraph (the caret may sit after its last character).
    last: bool,
    /// Paragraph direction.
    rtl: bool,
    galley: Arc<Galley>,
    /// Left edge of the galley inside the text area.
    x: f32,
    /// Per logical character: left and right edge relative to the galley, and whether it runs RTL.
    spans: Vec<(f32, f32, bool)>,
}

impl Line {
    /// Caret position before logical column `col` (0..=len), relative to the text area.
    fn caret_x(&self, col: usize) -> f32 {
        if self.len == 0 {
            return self.x;
        }
        let x = if col < self.len {
            let (a, b, r) = self.spans[col];
            if r { b } else { a }
        } else {
            let (a, b, r) = self.spans[self.len - 1];
            if r { a } else { b }
        };
        self.x + x
    }

    fn end(&self) -> usize {
        self.start + self.len
    }

    /// Columns the caret can take on this line.
    fn columns(&self) -> std::ops::RangeInclusive<usize> {
        let last = if self.last || self.len == 0 { self.len } else { self.len - 1 };
        0..=last
    }
}

fn layout(ui: &Ui, text: &str, font: &FontId, color: Color32, wrap: Option<f32>, area_w: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut global = 0;
    let mut prev_rtl = has_rtl(text);
    for para in text.split('\n') {
        let chars: Vec<char> = para.chars().collect();
        let n = chars.len();
        let info = BidiInfo::new(para, None);
        let pinfo = info.paragraphs.first();
        let base_rtl = pinfo.map(|p| p.level.is_rtl()).unwrap_or(prev_rtl);
        prev_rtl = base_rtl;
        let byte_of: Vec<usize> = para.char_indices().map(|(b, _)| b).chain(std::iter::once(para.len())).collect();
        let shaped = shape(&chars);
        let widths: Vec<f32> = ui.fonts_mut(|f| shaped.iter().map(|c| f.glyph_width(font, *c)).collect());

        // Wrap on spaces, in logical order.
        let mut ranges = Vec::new();
        let mut start = 0;
        if let Some(max) = wrap {
            let mut w = 0.0;
            let mut brk: Option<usize> = None;
            let mut i = 0;
            while i < n {
                w += widths[i];
                if chars[i] == ' ' {
                    brk = Some(i + 1);
                }
                if w > max && i > start {
                    let cut = brk.filter(|b| *b > start && *b <= i).unwrap_or(i);
                    ranges.push((start, cut));
                    start = cut;
                    w = widths[start..=i].iter().sum();
                    brk = (start..=i).filter(|k| chars[*k] == ' ').map(|k| k + 1).last();
                }
                i += 1;
            }
        }
        ranges.push((start, n));

        let count = ranges.len();
        for (li, (a, b)) in ranges.into_iter().enumerate() {
            let len = b - a;
            // Real embedding levels (English inside Arabic is level 2, not 0) decide the order.
            let lv: Vec<unicode_bidi::Level> = match pinfo {
                Some(p) if len > 0 => info.reordered_levels_per_char(p, byte_of[a]..byte_of[b])[a..b].to_vec(),
                _ => vec![if base_rtl { unicode_bidi::Level::rtl() } else { unicode_bidi::Level::ltr() }; len],
            };
            let levels: Vec<bool> = lv.iter().map(|l| l.is_rtl()).collect();
            let order: Vec<usize> = if len > 0 { BidiInfo::reorder_visual(&lv) } else { vec![] };
            // Lam followed by alef shows as one ligature glyph covering both characters.
            let mut partner = vec![None; len];
            for k in 0..len.saturating_sub(1) {
                if chars[a + k] == LAM && levels[k] && levels[k + 1] && lam_alef(chars[a + k + 1]).is_some() && partner[k].is_none() {
                    partner[k] = Some(k + 1);
                    partner[k + 1] = Some(k);
                }
            }
            let mut vis = String::new();
            let mut vis_of = vec![usize::MAX; len];
            let mut count_vis = 0;
            for &k in &order {
                if vis_of[k] != usize::MAX {
                    continue;
                }
                let ch = match partner[k] {
                    Some(o) => {
                        let lam = k.min(o);
                        let (iso, fin) = lam_alef(chars[a + lam + 1]).unwrap();
                        // A lam that joins the letter before it takes the final ligature form.
                        let joined = matches!(shaped[a + lam], '\u{FEE0}' | '\u{FEDE}');
                        vis_of[o] = count_vis;
                        if joined { fin } else { iso }
                    }
                    None if levels[k] => mirror(shaped[a + k]),
                    None => shaped[a + k],
                };
                vis_of[k] = count_vis;
                vis.push(ch);
                count_vis += 1;
            }
            let galley = ui.fonts_mut(|f| f.layout_no_wrap(vis, font.clone(), color));
            let glyphs: Vec<(f32, f32)> = galley.rows.first().map(|r| r.glyphs.iter().map(|g| (g.pos.x, g.advance_width)).collect()).unwrap_or_default();
            let spans: Vec<(f32, f32, bool)> = (0..len)
                .map(|k| {
                    let (gx, gw) = glyphs.get(vis_of[k]).copied().unwrap_or((0.0, 0.0));
                    match partner[k] {
                        // Right half for the lam (read first), left half for the alef.
                        Some(o) if o > k => (gx + gw / 2.0, gx + gw, true),
                        Some(_) => (gx, gx + gw / 2.0, true),
                        None => (gx, gx + gw, levels[k]),
                    }
                })
                .collect();
            let width = galley.size().x;
            let x = if base_rtl { area_w - width } else { 0.0 };
            lines.push(Line { start: global + a, len, last: li + 1 == count, rtl: base_rtl, galley, x, spans });
        }
        global += n + 1;
    }
    lines
}

fn line_of(lines: &[Line], caret: usize) -> usize {
    lines.iter().position(|l| caret >= l.start && (caret < l.end() || (caret == l.end() && (l.last || l.len == 0)))).unwrap_or(lines.len().saturating_sub(1))
}

fn nearest_col(line: &Line, x: f32) -> usize {
    line.columns().min_by(|a, b| (line.caret_x(*a) - x).abs().total_cmp(&(line.caret_x(*b) - x).abs())).unwrap_or(0)
}

// ───────────── editing ─────────────

fn byte_at(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_left(chars: &[char], mut i: usize) -> usize {
    while i > 0 && !is_word(chars[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word(chars[i - 1]) {
        i -= 1;
    }
    i
}

fn word_right(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && !is_word(chars[i]) {
        i += 1;
    }
    while i < chars.len() && is_word(chars[i]) {
        i += 1;
    }
    i
}

struct Editor<'a> {
    text: &'a mut String,
    st: EditState,
    now: f64,
    changed: bool,
    limit: usize,
}

impl Editor<'_> {
    fn selection(&self) -> (usize, usize) {
        (self.st.anchor.min(self.st.caret), self.st.anchor.max(self.st.caret))
    }

    fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.text.chars().skip(a).take(b - a).collect()
    }

    fn snapshot(&mut self) {
        if self.now - self.st.last_edit > 1.0 || self.st.undo.is_empty() {
            self.st.undo.push((self.text.clone(), self.st.caret));
            if self.st.undo.len() > 100 {
                self.st.undo.remove(0);
            }
        }
        self.st.redo.clear();
        self.st.last_edit = self.now;
    }

    fn delete_range(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        self.snapshot();
        let (ba, bb) = (byte_at(self.text, a), byte_at(self.text, b));
        self.text.replace_range(ba..bb, "");
        self.st.caret = a;
        self.st.anchor = a;
        self.changed = true;
    }

    fn insert(&mut self, s: &str) {
        let (a, b) = self.selection();
        self.delete_range(a, b);
        let room = self.limit.saturating_sub(self.text.chars().count());
        let s: String = s.chars().take(room).collect();
        if s.is_empty() {
            return;
        }
        self.snapshot();
        let at = byte_at(self.text, self.st.caret);
        self.text.insert_str(at, &s);
        self.st.caret += s.chars().count();
        self.st.anchor = self.st.caret;
        self.changed = true;
    }

    fn move_to(&mut self, i: usize, select: bool) {
        self.st.caret = i;
        if !select {
            self.st.anchor = i;
        }
    }

    fn undo(&mut self, redo: bool) {
        let (from, to) = if redo { (&mut self.st.redo, &mut self.st.undo) } else { (&mut self.st.undo, &mut self.st.redo) };
        if let Some((t, c)) = from.pop() {
            to.push((self.text.clone(), self.st.caret));
            *self.text = t;
            self.st.caret = c.min(self.text.chars().count());
            self.st.anchor = self.st.caret;
            self.changed = true;
        }
    }
}

fn bidi_edit(ui: &mut Ui, id: egui::Id, sid: egui::Id, st: EditState, f: TextField<'_>, font: FontId) -> Response {
    let row_h = ui.fonts_mut(|fo| fo.row_height(&font));
    let text_color = ui.visuals().text_color();
    // Same sizing as `TextEdit`: the text area never exceeds the space left after the margins.
    let available = (ui.available_width() - f.margin.sum().x).max(24.0);
    let inner_w = if f.width.is_finite() { f.width.min(available) } else { available };
    let wrap = f.multiline.then_some(inner_w);

    let lines = layout(ui, f.text, &font, text_color, wrap, inner_w);
    let rows = if f.multiline { lines.len().max(f.rows) } else { 1 };
    let outer = Vec2::new(inner_w, rows as f32 * row_h) + f.margin.sum();
    let (rect, mut response) = ui.allocate_exact_size(outer, Sense::click_and_drag());
    debug_assert_eq!(response.id, id);
    let inner = Rect::from_min_size(rect.min + f.margin.left_top(), Vec2::new(inner_w, rows as f32 * row_h));
    let now = ui.input(|i| i.time);
    let mut ed = Editor { text: f.text, st, now, changed: false, limit: f.char_limit };
    let shift = ui.input(|i| i.modifiers.shift);

    // Where a pointer position lands in the text.
    let hit = |lines: &[Line], scroll: f32, pos: Pos2| -> usize {
        let li = (((pos.y - inner.top()) / row_h).floor().max(0.0) as usize).min(lines.len().saturating_sub(1));
        let l = &lines[li];
        l.start + nearest_col(l, pos.x - inner.left() + scroll)
    };

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    if let Some(pos) = response.interact_pointer_pos() {
        if response.double_clicked() {
            let chars: Vec<char> = ed.text.chars().collect();
            let i = hit(&lines, ed.st.scroll, pos);
            ed.st.anchor = word_left(&chars, i.min(chars.len()));
            ed.st.caret = word_right(&chars, ed.st.anchor);
        } else if response.drag_started() || response.clicked() || ui.input(|i| i.pointer.primary_pressed()) {
            let i = hit(&lines, ed.st.scroll, pos);
            ed.move_to(i, shift);
        } else if response.dragged() {
            ed.st.caret = hit(&lines, ed.st.scroll, pos);
        }
        ed.st.preferred_x = None;
        ed.st.last_interaction = now;
        ui.memory_mut(|m| m.request_focus(id));
    }

    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        let filter = EventFilter { horizontal_arrows: true, vertical_arrows: f.multiline, tab: false, ..Default::default() };
        ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
        let events = ui.input(|i| i.filtered_events(&filter));
        for ev in events {
            let lines_now = layout(ui, ed.text, &font, text_color, wrap, inner_w);
            let chars: Vec<char> = ed.text.chars().collect();
            let n = chars.len();
            let cur = line_of(&lines_now, ed.st.caret);
            let mut keep_x = false;
            match ev {
                Event::Text(t) => {
                    let t: String = if f.multiline { t } else { t.replace(['\n', '\r'], "") };
                    ed.insert(&t);
                }
                Event::Ime(egui::ImeEvent::Commit(t)) => ed.insert(&t),
                Event::Paste(t) => {
                    let t = t.replace("\r\n", "\n");
                    let t = if f.multiline { t } else { t.replace('\n', " ") };
                    ed.insert(&t);
                }
                Event::Copy => {
                    let s = ed.selected_text();
                    if !s.is_empty() {
                        ui.ctx().copy_text(s);
                    }
                }
                Event::Cut => {
                    let s = ed.selected_text();
                    if !s.is_empty() {
                        ui.ctx().copy_text(s);
                        let (a, b) = ed.selection();
                        ed.delete_range(a, b);
                    }
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    let word = modifiers.command || modifiers.ctrl;
                    let sel = modifiers.shift;
                    match key {
                        Key::Backspace => {
                            let (a, b) = ed.selection();
                            if a != b {
                                ed.delete_range(a, b);
                            } else if word {
                                ed.delete_range(word_left(&chars, a), a);
                            } else if a > 0 {
                                ed.delete_range(a - 1, a);
                            }
                        }
                        Key::Delete => {
                            let (a, b) = ed.selection();
                            if a != b {
                                ed.delete_range(a, b);
                            } else if word {
                                ed.delete_range(a, word_right(&chars, a));
                            } else if a < n {
                                ed.delete_range(a, a + 1);
                            }
                        }
                        Key::Enter if f.multiline => ed.insert("\n"),
                        Key::Enter | Key::Escape => ui.memory_mut(|m| m.surrender_focus(id)),
                        Key::A if word => {
                            ed.st.anchor = 0;
                            ed.st.caret = n;
                        }
                        Key::Z if word && modifiers.shift => ed.undo(true),
                        Key::Z if word => ed.undo(false),
                        Key::Y if word => ed.undo(true),
                        Key::ArrowLeft | Key::ArrowRight => {
                            let right = key == Key::ArrowRight;
                            let (a, b) = ed.selection();
                            let l = &lines_now[cur];
                            let target = if a != b && !sel {
                                // Collapse the selection to the side the arrow points to.
                                let (xa, xb) = (caret_px(&lines_now, a), caret_px(&lines_now, b));
                                if (xa < xb) == right { b } else { a }
                            } else if word {
                                // Words follow reading order: → in RTL text goes to the previous word.
                                if right != l.rtl { word_right(&chars, ed.st.caret) } else { word_left(&chars, ed.st.caret) }
                            } else {
                                let x = l.caret_x(ed.st.caret - l.start);
                                let next = l
                                    .columns()
                                    .filter(|c| if right { l.caret_x(*c) > x + 0.5 } else { l.caret_x(*c) < x - 0.5 })
                                    .min_by(|p, q| (l.caret_x(*p) - x).abs().total_cmp(&(l.caret_x(*q) - x).abs()));
                                match next {
                                    Some(c) => l.start + c,
                                    // At the visual edge: continue on the neighbouring line.
                                    None if right != l.rtl => (ed.st.caret + 1).min(n),
                                    None => ed.st.caret.saturating_sub(1),
                                }
                            };
                            ed.move_to(target, sel);
                        }
                        Key::ArrowUp | Key::ArrowDown if f.multiline => {
                            let x = ed.st.preferred_x.unwrap_or_else(|| lines_now[cur].caret_x(ed.st.caret - lines_now[cur].start));
                            let to = if key == Key::ArrowUp { cur.checked_sub(1) } else { (cur + 1 < lines_now.len()).then_some(cur + 1) };
                            let target = match to {
                                Some(t) => lines_now[t].start + nearest_col(&lines_now[t], x),
                                None if key == Key::ArrowUp => 0,
                                None => n,
                            };
                            ed.move_to(target, sel);
                            ed.st.preferred_x = Some(x);
                            keep_x = true;
                        }
                        Key::Home => {
                            let t = if word { 0 } else { lines_now[cur].start };
                            ed.move_to(t, sel);
                        }
                        Key::End => {
                            let l = &lines_now[cur];
                            let t = if word { n } else { l.start + *l.columns().end() };
                            ed.move_to(t, sel);
                        }
                        _ => {}
                    }
                }
                _ => continue,
            }
            if !keep_x {
                ed.st.preferred_x = None;
            }
            ed.st.last_interaction = now;
        }
        let r = Rect::from_min_size(inner.min, Vec2::new(1.0, row_h));
        ui.output_mut(|o| o.ime = Some(egui::output::IMEOutput { rect: inner, cursor_rect: r }));
    }

    // ── paint ──
    let lines = if ed.changed { layout(ui, ed.text, &font, text_color, wrap, inner_w) } else { lines };
    let n = ed.text.chars().count();
    ed.st.caret = ed.st.caret.min(n);
    ed.st.anchor = ed.st.anchor.min(n);
    if !f.multiline {
        // Keep the caret inside a single-line box.
        let l = &lines[0];
        let cx = l.caret_x(ed.st.caret);
        let (lo, hi) = (l.x.min(0.0), (l.x + l.galley.size().x - inner_w).max(0.0));
        if cx - ed.st.scroll < 0.0 {
            ed.st.scroll = cx;
        } else if cx - ed.st.scroll > inner_w - 1.0 {
            ed.st.scroll = cx - inner_w + 1.0;
        }
        ed.st.scroll = ed.st.scroll.clamp(lo, hi);
    } else {
        ed.st.scroll = 0.0;
    }

    let visuals = ui.style().interact(&response).clone();
    let stroke = if focused { ui.visuals().selection.stroke } else { visuals.bg_stroke };
    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect(rect, visuals.corner_radius, ui.visuals().text_edit_bg_color(), stroke, StrokeKind::Inside);
    let clip = ui.painter_at(inner.expand2(Vec2::new(2.0, 0.0)));
    let origin = |li: usize| Pos2::new(inner.left() - ed.st.scroll, inner.top() + li as f32 * row_h);

    if ed.text.is_empty() && !f.hint.is_empty() {
        let hf = f.hint_font.clone().unwrap_or(font.clone());
        let hc = f.hint_color.unwrap_or(ui.visuals().weak_text_color());
        clip.text(Pos2::new(inner.left(), inner.top() + row_h / 2.0), egui::Align2::LEFT_CENTER, &f.hint, hf, hc);
    }
    let (sa, sb) = ed.selection();
    for (li, l) in lines.iter().enumerate() {
        let o = origin(li);
        if sa != sb && focused {
            for k in 0..l.len {
                let g = l.start + k;
                if g >= sa && g < sb {
                    let (a, b, _) = l.spans[k];
                    clip.rect_filled(Rect::from_min_max(Pos2::new(o.x + l.x + a, o.y), Pos2::new(o.x + l.x + b, o.y + row_h)), 0.0, ui.visuals().selection.bg_fill);
                }
            }
        }
        let gy = o.y + (row_h - l.galley.size().y) / 2.0;
        clip.galley(Pos2::new(o.x + l.x, gy), l.galley.clone(), text_color);
    }
    if focused {
        let li = line_of(&lines, ed.st.caret);
        let l = &lines[li];
        let x = origin(li).x + l.caret_x(ed.st.caret - l.start);
        let cr = Rect::from_min_size(Pos2::new(x, origin(li).y), Vec2::new(0.0, row_h));
        egui::text_selection::visuals::paint_text_cursor(ui, &clip, cr, now - ed.st.last_interaction);
        ui.output_mut(|o| {
            if let Some(ime) = &mut o.ime {
                ime.cursor_rect = cr;
            }
        });
    }
    let _ = Stroke::NONE;

    if ed.changed {
        response.mark_changed();
    }
    ui.data_mut(|d| d.insert_temp(sid, ed.st));
    response
}

fn caret_px(lines: &[Line], i: usize) -> f32 {
    let li = line_of(lines, i);
    let l = &lines[li];
    li as f32 * 10_000.0 + l.caret_x(i - l.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaping_keeps_one_form_per_letter() {
        let word: Vec<char> = "السلام".chars().collect();
        let s = shape(&word);
        assert_eq!(s.len(), word.len());
        assert!(s.iter().all(|c| (0xFE70..=0xFEFF).contains(&(*c as u32))), "{s:?}");
        assert_eq!(lam_alef('\u{0627}'), Some(('\u{FEFB}', '\u{FEFC}')));
    }

    #[test]
    fn words_and_bytes() {
        let c: Vec<char> = "مرحبا بالعالم".chars().collect();
        assert_eq!(word_left(&c, c.len()), 6);
        assert_eq!(word_right(&c, 0), 5);
        assert_eq!(byte_at("aب", 1), 1);
        assert_eq!(byte_at("aب", 2), 3);
    }
}
