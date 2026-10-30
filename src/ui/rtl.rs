//! Arabic and other right-to-left text. egui draws glyphs one at a time, left to right, with no
//! shaping, so Arabic comes out as separate letters in reverse order. For text that contains RTL
//! script we wrap it ourselves in logical order, give every letter its joined form, reorder each
//! line visually (Unicode bidi) and right-align RTL paragraphs. Text without RTL is untouched.

use egui::{Color32, FontId, RichText};
use unicode_bidi::BidiInfo;

pub fn has_rtl(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

/// One line in logical order -> the string to draw left to right, and whether it reads RTL.
fn visual(line: &str) -> (String, bool) {
    let shaped = ar_reshaper::reshape_line(line);
    let info = BidiInfo::new(&shaped, None);
    let Some(para) = info.paragraphs.first() else { return (shaped, false) };
    let levels = info.reordered_levels_per_char(para, para.range.clone());
    let chars: Vec<char> = shaped.chars().collect();
    // Visual order, with brackets mirrored inside right-to-left runs: "(Stack)" stays "(Stack)".
    let out = BidiInfo::reorder_visual(&levels).into_iter().map(|i| if levels[i].is_rtl() { mirror(chars[i]) } else { chars[i] }).collect();
    (out, para.level.is_rtl())
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

/// Single-line text (names, titles, truncated prompts) ready for an egui label.
pub fn line(s: &str) -> String {
    if has_rtl(s) { s.lines().map(|l| visual(l).0).collect::<Vec<_>>().join("\n") } else { s.to_string() }
}

/// A wrapped label that handles RTL paragraphs; identical to a normal wrapped label otherwise.
pub fn label(ui: &mut egui::Ui, text: &str, font: FontId, color: Color32) -> egui::Response {
    if !has_rtl(text) {
        return ui.add(egui::Label::new(RichText::new(text).font(font).color(color)).wrap());
    }
    let width = ui.available_width().max(40.0);
    let measure = |ui: &egui::Ui, s: &str| ui.painter().layout_no_wrap(visual(s).0, font.clone(), color).size().x;
    let mut rows: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if !cur.is_empty() && measure(ui, &cand) > width {
                rows.push(std::mem::replace(&mut cur, word.to_string()));
            } else {
                cur = cand;
            }
        }
        rows.push(cur);
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for row in rows {
            let (vis, rtl) = visual(&row);
            let galley = ui.painter().layout_no_wrap(vis, font.clone(), color);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, galley.size().y), egui::Sense::hover());
            let x = if rtl { rect.right() - galley.size().x } else { rect.left() };
            ui.painter().galley(egui::pos2(x, rect.top()), galley, color);
        }
    })
    .response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_is_joined_and_reordered() {
        assert!(has_rtl("ما هو الناتج؟"));
        assert!(!has_rtl("print(1)"));
        let (v, rtl) = visual("سلام");
        assert!(rtl);
        // Joined presentation forms, drawn right-to-left: the first letter (seen) ends up last.
        assert!(v.chars().all(|c| (0xFE70..=0xFEFF).contains(&(c as u32))), "{v:?}");
        assert_eq!(v.chars().count(), 3, "lam-alef becomes one ligature: {v:?}");
        // Mixed text keeps the Latin run readable left to right.
        let (m, _) = visual("اطبع print(1)");
        assert!(m.contains("print(1)"), "{m:?}");
        let (b, _) = visual("مكدس (Stack)");
        assert!(b.starts_with("(Stack)"), "brackets stay readable: {b:?}");
        assert_eq!(line("Hello"), "Hello");
    }
}
