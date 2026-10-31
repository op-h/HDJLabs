//! "How to write a good question": a step-by-step guide drawn over the real question editor.
//! Each slide circles the field it talks about in red, scrolls it into view and explains it,
//! with the steps chosen for the question's type and how it's checked.

use crate::model::*;
use crate::ui::theme::{self, icon, pal};
use egui::{Align, Align2, Color32, Id, Layout, Margin, Rect, RichText, Stroke};

pub struct Step {
    /// Which marked field the slide circles.
    pub key: &'static str,
    pub title: String,
    pub body: String,
    pub example: Option<String>,
}

pub struct Tour {
    pub title: String,
    pub steps: Vec<Step>,
    pub step: usize,
}

const SCROLL_KEY: &str = "tour-scroll-to";

fn rect_id(key: &str) -> Id {
    Id::new(("tour-rect", key))
}

/// Records where a field is this frame (and scrolls to it when its slide just opened).
pub fn mark(ui: &egui::Ui, key: &'static str, rect: Rect) {
    ui.ctx().data_mut(|d| d.insert_temp(rect_id(key), rect));
    let pending = ui.ctx().data(|d| d.get_temp::<&'static str>(Id::new(SCROLL_KEY)));
    if pending == Some(key) {
        ui.scroll_to_rect(rect.expand(40.0), Some(Align::Center));
        ui.ctx().data_mut(|d| d.remove::<&'static str>(Id::new(SCROLL_KEY)));
    }
}

fn step(key: &'static str, title: &str, body: &str, example: Option<&str>) -> Step {
    Step { key, title: title.into(), body: body.into(), example: example.map(Into::into) }
}

/// The slides for this question, from its type and checking mode.
pub fn for_question(q: &Question, lang_tip: &str, example: (&str, &str, &str, &str)) -> Tour {
    let mut s = Vec::new();
    let files = step("files", "Add files if the question needs them", "Images, packet captures (.pcap), logs or code. Students open them inside the exam, even with lockdown on.", None);
    let points = step("points", "Set the points", "Give longer or harder tasks more points. The exam's total appears in the question list.", None);
    let title = match &q.kind {
        Kind::Code(spec) => {
            s.push(step("prompt", "Write the task", "Say what the program reads and exactly what it prints, with one small example. Students read it beside their editor.", Some(example.0)));
            s.push(step("language", "Choose the language", lang_tip, None));
            s.push(step("starter", "Give students a starting point", "The line that reads the input and a comment where their code goes. Never the answer itself.", Some(example.1)));
            if spec.lang == Lang::Sql {
                s.push(step("setup", "Create the data", "CREATE TABLE and INSERT the rows. It runs before every query, and students can read it beside the question.", None));
            }
            if spec.lang.is_preview_only() {
                s.push(step("mode", "You grade the page", "Students see their page live as they type. After they submit, open the Live classroom, click a name, look at the page and give Full marks, Half, Zero or exact points.", None));
            } else {
                s.push(step("mode", "Choose how it's checked", "Compare the printed output: one expected result.\nSeveral test cases: inputs with expected outputs, some hidden.\nI'll grade it myself: you read every answer and score it.", None));
                if spec.tests.is_empty() {
                    s.push(step("mode", "You grade it yourself", "No automatic score. When students submit, open the Live classroom, click a name, read and run the code, then Full marks / Half / Zero and \"Finish grading & send score\". Until then students see \"Waiting for your teacher\".", None));
                } else if spec.simple {
                    let ex = if example.3.is_empty() { None } else { Some(format!("Input: {}", example.3)) };
                    s.push(Step { key: "expected", title: "Type what a correct program prints".into(), body: "Exactly, line by line. Spaces at the end of lines are ignored. If the program reads input, put it in the small box under it.".into(), example: ex });
                    s.push(step("solution", "Let HDJLabs fill it in", "Paste your own solution here and press Run: the expected output fills itself in, so it is never mistyped. Students never see your solution.", Some(example.2).filter(|e| !e.is_empty())));
                } else {
                    s.push(step("tests", "Add 3 to 5 tests", "Normal cases and edge cases: 0, empty, negative, very large. The score is tests passed ÷ tests × points.", None));
                    s.push(step("hidden", "Hide some tests", "Hidden tests run only when the student submits, so nobody can hard-code the answers they can see.", None));
                    s.push(step("solution", "Fill every expected output at once", "Paste your own solution and press Run: HDJLabs runs it on every test and fills in what students must print.", Some(example.2).filter(|e| !e.is_empty())));
                }
            }
            s.push(files);
            s.push(points);
            format!("{} question", spec.lang.name())
        }
        Kind::Choice { .. } => {
            s.push(step("prompt", "Ask one clear question", "One idea per question. Avoid \"all of the above\" and double negatives.", None));
            s.push(step("options", "Write the options and tick the right one(s)", "Make the wrong options believable. Tick more than one and it becomes multiple answer, with partial credit: +1 per right pick, −1 per wrong pick.", None));
            s.push(files);
            s.push(points);
            "Multiple choice question".into()
        }
        Kind::TrueFalse { .. } => {
            s.push(step("prompt", "Write a statement", "Something clearly true or clearly false, e.g. In Python, len(\"hello\") returns 5.", None));
            s.push(step("answer", "Pick the correct answer", "True or False. Shuffling in Settings doesn't change this.", None));
            s.push(points);
            "True / False question".into()
        }
        Kind::Short { .. } => {
            s.push(step("prompt", "Ask for one short answer", "A word, number or flag. Say what form you expect, e.g. \"the flag, copied exactly\".", None));
            s.push(step("accepted", "List every accepted answer", "Add spellings students might use. Extra spaces are ignored; tick Case sensitive only for flags and codes.", None));
            s.push(step("accepted", "Show the answer's shape (optional)", "TryHackMe style: letters become *, symbols stay visible, e.g. HDJ{****_****}. Each typed character fills one star.", Some("HDJ{l0ck3d_d0wn!}  →  ***{******_****!}")));
            s.push(files);
            s.push(points);
            "Short answer question".into()
        }
        Kind::Upload { .. } => {
            s.push(step("prompt", "Describe the file you want", "Say which file type and what it must contain.", None));
            s.push(points);
            "File upload question".into()
        }
    };
    Tour { title, steps: s, step: 0 }
}

/// Starts the guide on its first slide.
pub fn start(ctx: &egui::Context, tour: &Tour) {
    if let Some(s) = tour.steps.first() {
        ctx.data_mut(|d| d.insert_temp(Id::new(SCROLL_KEY), s.key));
    }
}

/// Draws the red circle and the slide card. Returns false when the guide is closed.
pub fn show(ctx: &egui::Context, tour: &mut Tour) -> bool {
    let p = pal();
    let red = Color32::from_rgb(232, 64, 64);
    let Some(st) = tour.steps.get(tour.step) else { return false };
    let target = ctx.data(|d| d.get_temp::<Rect>(rect_id(st.key)));
    if let Some(r) = target {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("tour-ring")));
        // Hug the field: a flatter ellipse for wide fields, rounder for small ones.
        let radius = egui::vec2(r.width() * 0.5 * 1.08 + 14.0, r.height() * 0.5 * 1.25 + 12.0);
        painter.add(egui::Shape::ellipse_stroke(r.center(), radius, Stroke::new(3.0_f32, red)));
    }
    // Keep the card out of the way of the field it talks about.
    let screen = ctx.content_rect();
    let low = target.is_some_and(|r| r.center().y > screen.center().y);
    let (anchor, offset) = if low { (Align2::CENTER_TOP, egui::vec2(0.0, 70.0)) } else { (Align2::CENTER_BOTTOM, egui::vec2(0.0, -22.0)) };
    let mut keep = true;
    let (n, i) = (tour.steps.len(), tour.step);
    let mut go: Option<usize> = None;
    egui::Area::new(Id::new("tour-card")).order(egui::Order::Foreground).anchor(anchor, offset).show(ctx, |ui| {
        theme::card_frame().stroke(Stroke::new(1.5_f32, red.gamma_multiply(0.8))).inner_margin(Margin::same(20)).show(ui, |ui| {
            ui.set_width(600.0);
            ui.horizontal(|ui| {
                ui.label(theme::eyebrow(format!("How to write a good {} · step {} of {}", tour.title.to_lowercase(), i + 1, n)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::icon_button(ui, icon::CANCEL, "Close the guide").clicked() {
                        keep = false;
                    }
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_stroke(dot.center(), 6.0, Stroke::new(2.5_f32, red));
                ui.label(theme::heading(&st.title, 21.0));
            });
            ui.add(egui::Label::new(RichText::new(&st.body).size(15.5).color(p.text)).wrap());
            if let Some(ex) = &st.example {
                ui.add_space(4.0);
                egui::Frame::new().fill(p.code_bg).corner_radius(egui::CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::eyebrow("Example"));
                    ui.label(RichText::new(ex).font(theme::mono(13.5)).color(p.text));
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for k in 0..n {
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
                    ui.painter().circle_filled(r.center(), if k == i { 5.0 } else { 3.5 }, if k == i { p.accent } else { p.border_strong });
                    if resp.clicked() {
                        go = Some(k);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if i + 1 < n {
                        if theme::primary(ui, &format!("Next   {}", icon::FORWARD)).clicked() {
                            go = Some(i + 1);
                        }
                    } else if theme::primary(ui, &format!("{}  Done", icon::CHECK)).clicked() {
                        keep = false;
                    }
                    if i > 0 && theme::ghost(ui, &format!("{}  Back", icon::BACK)).clicked() {
                        go = Some(i - 1);
                    }
                });
            });
        });
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep = false;
    }
    if ctx.input(|inp| inp.key_pressed(egui::Key::ArrowRight)) && i + 1 < n && !ctx.egui_wants_keyboard_input() {
        go = Some(i + 1);
    }
    if ctx.input(|inp| inp.key_pressed(egui::Key::ArrowLeft)) && i > 0 && !ctx.egui_wants_keyboard_input() {
        go = Some(i - 1);
    }
    if let Some(k) = go {
        tour.step = k;
        ctx.data_mut(|d| d.insert_temp(Id::new(SCROLL_KEY), tour.steps[k].key));
    }
    keep
}
