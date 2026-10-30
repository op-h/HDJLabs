//! A compact IDE-style code editor: line-number gutter, syntax highlighting, Tab / Shift+Tab
//! indentation (also on multi-line selections), smart auto-indent on Enter, bracket-aware
//! dedent, Ctrl+/ line comments, and a cursor position readout for the status bar.

use crate::model::Lang;
use crate::ui::theme::{is_light, pal};
use egui::text::{CCursor, CCursorRange};
use egui::{Key, Modifiers};
use egui_extras::syntax_highlighting::{highlight, CodeTheme};

const INDENT: &str = "    ";

pub struct EditorOutput {
    pub changed: bool,
    pub line: usize,
    pub col: usize,
    pub focused: bool,
}

fn byte_of(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

fn line_start(s: &str, byte: usize) -> usize {
    s[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn comment_token(lang: Lang) -> &'static str {
    if lang == Lang::Python { "# " } else { "// " }
}

/// Byte ranges of each full line touched by [a, b).
fn touched_lines(s: &str, a: usize, b: usize) -> Vec<usize> {
    let mut starts = vec![line_start(s, a)];
    let end = if b > a && s[..b].ends_with('\n') { b - 1 } else { b };
    for (i, c) in s[a..end].char_indices() {
        if c == '\n' {
            starts.push(a + i + 1);
        }
    }
    starts
}

/// Applies keyboard edits that egui's TextEdit doesn't do itself. Returns the new cursor range
/// (char indices) when the text changed.
fn handle_keys(ui: &egui::Ui, code: &mut String, lang: Lang, sel: (usize, usize)) -> Option<(usize, usize)> {
    let (ca, cb) = (sel.0.min(sel.1), sel.0.max(sel.1));
    let (a, b) = (byte_of(code, ca), byte_of(code, cb));

    let shift_tab = ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab));
    let tab = !shift_tab && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let comment = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Slash));

    if tab && !code[a..b].contains('\n') {
        code.replace_range(a..b, INDENT);
        return Some((ca + 4, ca + 4));
    }
    if tab || shift_tab || comment {
        let starts = touched_lines(code, a, b);
        let mut delta_first = 0i64;
        let mut delta_total = 0i64;
        let token = comment_token(lang);
        let all_commented = starts.iter().all(|&ls| code[ls..].trim_start().starts_with(token.trim_end()) || code[ls..].starts_with('\n'));
        for (n, &ls) in starts.iter().enumerate().rev() {
            let d: i64 = if tab {
                code.insert_str(ls, INDENT);
                4
            } else if shift_tab {
                let spaces = code[ls..].chars().take(4).take_while(|c| *c == ' ').count();
                code.replace_range(ls..ls + spaces, "");
                -(spaces as i64)
            } else if all_commented {
                let indent = code[ls..].len() - code[ls..].trim_start_matches([' ', '\t']).len();
                let rest = &code[ls + indent..];
                let strip = if rest.starts_with(token) { token.len() } else if rest.starts_with(token.trim_end()) { token.trim_end().len() } else { 0 };
                code.replace_range(ls + indent..ls + indent + strip, "");
                -(strip as i64)
            } else {
                let indent = code[ls..].len() - code[ls..].trim_start_matches([' ', '\t']).len();
                code.insert_str(ls + indent, token);
                token.len() as i64
            };
            if n == 0 {
                delta_first = d;
            }
            delta_total += d;
        }
        let na = (ca as i64 + delta_first).max(0) as usize;
        let nb = (cb as i64 + delta_total).max(0) as usize;
        return Some((na, nb));
    }
    if enter {
        let ls = line_start(code, a);
        let line = &code[ls..a];
        let mut indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let trimmed = line.trim_end();
        let opens = trimmed.ends_with(':') && lang == Lang::Python || trimmed.ends_with('{') || trimmed.ends_with('(') || trimmed.ends_with('[');
        let closes_next = code[b..].starts_with('}') || code[b..].starts_with(')') || code[b..].starts_with(']');
        if opens {
            let inner = format!("{indent}{INDENT}");
            if closes_next {
                // `{|}` + Enter  ->  block with the closing brace on its own line.
                let ins = format!("\n{inner}\n{indent}");
                code.replace_range(a..b, &ins);
                let c = ca + 1 + inner.chars().count();
                return Some((c, c));
            }
            indent = inner;
        }
        let ins = format!("\n{indent}");
        code.replace_range(a..b, &ins);
        let c = ca + ins.chars().count();
        return Some((c, c));
    }
    None
}

/// Typing a closing brace on a whitespace-only line pulls it back one indent level.
fn auto_dedent(code: &mut String, cursor: usize) -> Option<usize> {
    let b = byte_of(code, cursor);
    if b == 0 {
        return None;
    }
    let last = code[..b].chars().last()?;
    if !matches!(last, '}' | ')' | ']') {
        return None;
    }
    let ls = line_start(code, b);
    let before = &code[ls..b - last.len_utf8()];
    if before.len() >= 4 && before.chars().all(|c| c == ' ') {
        code.replace_range(ls..ls + 4, "");
        return Some(cursor - 4);
    }
    None
}

pub fn code_editor(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, code: &mut String, lang: Lang, font_size: f32, read_only: bool) -> EditorOutput {
    let p = pal();
    let id = ui.make_persistent_id(id_salt);
    let lines = code.lines().count().max(1) + usize::from(code.ends_with('\n'));
    let theme = if is_light() { CodeTheme::light(font_size) } else { CodeTheme::dark(font_size) };
    let ext = lang.ext().to_string();

    let mut out = EditorOutput { changed: false, line: 1, col: 1, focused: false };

    if !read_only && ui.memory(|m| m.has_focus(id)) {
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            if let Some(range) = state.cursor.char_range() {
                let sel = (range.primary.index, range.secondary.index);
                if let Some((na, nb)) = handle_keys(ui, code, lang, sel) {
                    let r = if na == nb { CCursorRange::one(CCursor::new(na)) } else { CCursorRange::two(CCursor::new(na.min(nb)), CCursor::new(na.max(nb))) };
                    state.cursor.set_char_range(Some(r));
                    state.store(ui.ctx(), id);
                    out.changed = true;
                }
            }
        }
    }

    egui::Frame::new().fill(p.code_bg).corner_radius(egui::CornerRadius::same(10)).show(ui, |ui| {
        egui::ScrollArea::both().id_salt(("code-scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let width = format!("{lines}").len().max(2);
                let gutter: String = (1..=lines).map(|n| format!("{n:>width$}")).collect::<Vec<_>>().join("\n");
                let gutter_font = egui::FontId::monospace(font_size);
                egui::Frame::new().inner_margin(egui::Margin { left: 10, right: 12, top: 4, bottom: 4 }).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(gutter).font(gutter_font).color(p.muted.gamma_multiply(0.7))).selectable(false));
                });
                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                    let mut job = highlight(ui.ctx(), ui.style(), &theme, buf.as_str(), &ext);
                    job.wrap.max_width = wrap;
                    ui.fonts(|f| f.layout_job(job))
                };
                let edit = if read_only {
                    let mut view: &str = code.as_str();
                    ui.add(egui::TextEdit::multiline(&mut view).id(id).code_editor().frame(false).desired_width(f32::INFINITY).desired_rows(lines).layouter(&mut layouter).margin(egui::Margin::symmetric(4, 4)))
                } else {
                    ui.add(egui::TextEdit::multiline(code).id(id).code_editor().frame(false).lock_focus(true).desired_width(f32::INFINITY).desired_rows(lines.max(8)).layouter(&mut layouter).margin(egui::Margin::symmetric(4, 4)))
                };
                if edit.changed() {
                    out.changed = true;
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                        if let Some(r) = state.cursor.char_range() {
                            if r.is_empty() {
                                if let Some(nc) = auto_dedent(code, r.primary.index) {
                                    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(nc))));
                                    state.store(ui.ctx(), id);
                                }
                            }
                        }
                    }
                }
                out.focused = edit.has_focus();
            });
        });
    });

    if let Some(state) = egui::TextEdit::load_state(ui.ctx(), id) {
        if let Some(r) = state.cursor.char_range() {
            let b = byte_of(code, r.primary.index);
            out.line = code[..b].matches('\n').count() + 1;
            out.col = code[line_start(code, b)..b].chars().count() + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedent_closing_brace() {
        let mut s = String::from("if (x) {\n    }");
        let c = s.chars().count();
        assert_eq!(auto_dedent(&mut s, c), Some(c - 4));
        assert_eq!(s, "if (x) {\n}");
    }

    #[test]
    fn touched_lines_spans() {
        let s = "a\nbb\nccc\n";
        assert_eq!(touched_lines(s, 0, 4), vec![0, 2]);
        assert_eq!(touched_lines(s, 3, 3), vec![2]);
    }
}
