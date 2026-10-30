//! HDJLabs Studio — the teacher app: build an exam, export it as a locked-down .exe, run a live
//! classroom session over the LAN, and grade results.

use crate::grading::{self, TestOutcome};
use crate::model::*;
use crate::net::{self, LiveServer, Phase, S2C};
use crate::payload;
use crate::runner;
use crate::storage::{self, Project};
use crate::ui::editor;
use crate::ui::player::{run_output, test_report};
use crate::ui::theme::{self, icon, pal};
use egui::{Align, Color32, CornerRadius, Layout, Margin, RichText, Stroke};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Home,
    Build,
    Settings,
    Live,
    Results,
}

struct Toast {
    text: String,
    at: Instant,
    error: bool,
}

pub struct Studio {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    page: Page,
    selected: Option<usize>,
    live: Option<LiveServer>,
    live_selected: Option<String>,
    broadcast: String,
    direct_msg: String,
    result_selected: Option<usize>,
    toasts: Vec<Toast>,
    recent: Vec<PathBuf>,
    ref_job: Option<(u32, Receiver<Vec<runner::RunOutput>>)>,
    review_jobs: HashMap<(String, u32), Receiver<Vec<TestOutcome>>>,
    review_results: HashMap<(String, u32), Vec<TestOutcome>>,
    last_autosave: Instant,
    font: f32,
}

impl Studio {
    fn new() -> Self {
        Self {
            project: Project::new(),
            path: None,
            dirty: false,
            page: Page::Home,
            selected: None,
            live: None,
            live_selected: None,
            broadcast: String::new(),
            direct_msg: String::new(),
            result_selected: None,
            toasts: vec![],
            recent: storage::recent_load(),
            ref_job: None,
            review_jobs: HashMap::new(),
            review_results: HashMap::new(),
            last_autosave: Instant::now(),
            font: 14.0,
        }
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), at: Instant::now(), error });
    }

    fn exam(&mut self) -> &mut Exam {
        self.dirty = true;
        &mut self.project.exam
    }

    // ───────────── file operations ─────────────

    fn new_project(&mut self) {
        self.live = None;
        self.project = Project::new();
        self.path = None;
        self.dirty = false;
        self.selected = None;
        self.result_selected = None;
        self.page = Page::Build;
    }

    fn open_path(&mut self, path: PathBuf) {
        match Project::load(&path) {
            Ok(p) => {
                self.live = None;
                self.project = p;
                storage::recent_push(&path);
                self.recent = storage::recent_load();
                self.path = Some(path);
                self.dirty = false;
                self.selected = if self.project.exam.questions.is_empty() { None } else { Some(0) };
                self.result_selected = None;
                self.page = Page::Build;
            }
            Err(e) => self.toast(e, true),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("HDJLabs exam project", &["hdjx"]).set_directory(storage::projects_dir()).pick_file() {
            self.open_path(p);
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let path = match (&self.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = format!("{}.hdjx", net::sanitize_file_name(&self.project.exam.title));
                match rfd::FileDialog::new().add_filter("HDJLabs exam project", &["hdjx"]).set_directory(storage::projects_dir()).set_file_name(name).save_file() {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        match self.project.save(&path) {
            Ok(()) => {
                storage::recent_push(&path);
                self.recent = storage::recent_load();
                self.path = Some(path);
                self.dirty = false;
                true
            }
            Err(e) => {
                self.toast(format!("Save failed: {e}"), true);
                false
            }
        }
    }

    fn export(&mut self) {
        let problems = self.project.exam.validate();
        if !problems.is_empty() {
            self.page = Page::Settings;
            self.toast(format!("Fix {} issue(s) before exporting.", problems.len()), true);
            return;
        }
        let name = format!("{}.exe", net::sanitize_file_name(&self.project.exam.title));
        let dir = self.project.last_export.as_ref().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(storage::projects_dir);
        let Some(out) = rfd::FileDialog::new().add_filter("Windows app", &["exe"]).set_directory(dir).set_file_name(name).save_file() else { return };
        let hints = net::local_ipv4().iter().map(|(ip, _)| ip.to_string()).collect();
        match payload::export(&self.project.exam, &self.project.teacher_public, false, hints, &out) {
            Ok(()) => {
                self.project.last_export = Some(out.clone());
                self.dirty = true;
                if self.path.is_some() {
                    self.save(false);
                }
                self.toast(format!("Exported {}", out.display()), false);
            }
            Err(e) => self.toast(format!("Export failed: {e}"), true),
        }
    }

    fn preview(&mut self) {
        let dir = std::env::temp_dir().join("HDJLabs").join("preview");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("{} (Preview).exe", net::sanitize_file_name(&self.project.exam.title)));
        match payload::export(&self.project.exam, &self.project.teacher_public, true, vec!["127.0.0.1".into()], &out) {
            Ok(()) => {
                if let Err(e) = std::process::Command::new(&out).spawn() {
                    self.toast(format!("Could not start preview: {e}"), true);
                }
            }
            Err(e) => self.toast(format!("Preview failed: {e}"), true),
        }
    }

    // ───────────── background work ─────────────

    fn tick(&mut self, ctx: &egui::Context) {
        if let Some(live) = &self.live {
            let subs: Vec<Submission> = {
                let mut st = live.state.lock().unwrap();
                if !st.dirty {
                    vec![]
                } else {
                    st.dirty = false;
                    st.students.iter().filter_map(|s| s.submission.clone()).collect()
                }
            };
            let mut added = 0;
            for s in subs {
                if self.project.upsert_result(s) {
                    added += 1;
                }
            }
            if added > 0 {
                self.dirty = true;
            }
        }
        if let Some((qid, rx)) = &self.ref_job {
            if let Ok(outs) = rx.try_recv() {
                let qid = *qid;
                self.ref_job = None;
                let mut filled = 0;
                let mut failed = 0;
                if let Some(Kind::Code(spec)) = self.project.exam.questions.iter_mut().find(|q| q.id == qid).map(|q| &mut q.kind) {
                    for (t, o) in spec.tests.iter_mut().zip(outs) {
                        if o.ok() {
                            t.expected = grading::normalize_output(&o.stdout);
                            filled += 1;
                        } else {
                            failed += 1;
                        }
                    }
                }
                self.dirty = true;
                if failed > 0 {
                    self.toast(format!("Reference solution failed on {failed} test(s) — check it runs."), true);
                } else {
                    self.toast(format!("Filled {filled} expected output(s) from your solution."), false);
                }
            }
        }
        let done: Vec<(String, u32)> = self.review_jobs.keys().cloned().collect();
        for k in done {
            if let Some(rx) = self.review_jobs.get(&k) {
                if let Ok(r) = rx.try_recv() {
                    self.review_jobs.remove(&k);
                    self.review_results.insert(k, r);
                }
            }
        }
        if self.dirty && self.path.is_some() && self.last_autosave.elapsed() > Duration::from_secs(4) {
            self.last_autosave = Instant::now();
            self.save(false);
        }
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_secs(if t.error { 7 } else { 4 }));
        ctx.request_repaint_after(Duration::from_millis(if self.live.is_some() { 500 } else { 1000 }));
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(debug_assertions)]
        if crate::ui::shot::enabled() {
            self.shot_script(ctx);
            crate::ui::shot::tick(ctx);
        }
        self.tick(ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save(false);
        }
        let p = pal();
        egui::SidePanel::left("nav").exact_width(232.0).resizable(false).frame(egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).inner_margin(Margin::symmetric(16, 18))).show(ctx, |ui| self.nav(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(28, 22))).show(ctx, |ui| match self.page {
            Page::Home => self.home(ui),
            Page::Build => self.build(ui),
            Page::Settings => self.settings(ui),
            Page::Live => self.live_page(ui),
            Page::Results => self.results(ui),
        });
        self.draw_toasts(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty && self.path.is_some() {
            let _ = self.project.save(self.path.as_ref().unwrap());
        }
    }
}

fn nav_item(ui: &mut egui::Ui, icon_: &str, label: &str, active: bool, badge: Option<String>) -> bool {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, CornerRadius::same(theme::R_CONTROL), p.surface2);
        painter.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 9.0), egui::vec2(2.0, 18.0)), CornerRadius::ZERO, p.accent);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(theme::R_CONTROL), p.surface2);
    }
    let col = if active { p.text } else { p.muted };
    painter.text(rect.left_center() + egui::vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, icon_, egui::FontId::proportional(14.0), if active { p.accent } else { col });
    painter.text(rect.left_center() + egui::vec2(40.0, 0.0), egui::Align2::LEFT_CENTER, label, if active { theme::semibold(14.0) } else { egui::FontId::proportional(14.0) }, col);
    if let Some(b) = badge {
        painter.text(rect.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, b, theme::mono(11.5), p.muted);
    }
    resp.clicked()
}

impl Studio {
    fn nav(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        theme::brand(ui, "Exam studio");
        ui.add_space(26.0);
        ui.label(theme::eyebrow("Workspace"));
        ui.add_space(2.0);
        let qn = self.project.exam.questions.len();
        let live_n = self.live.as_ref().map(|l| l.state.lock().unwrap().students.iter().filter(|s| s.connected).count());
        let rn = self.project.results.len();
        for (page, ic, label, badge) in [
            (Page::Home, icon::LIBRARY, "Home", None),
            (Page::Build, icon::EDIT, "Questions", Some(qn.to_string())),
            (Page::Settings, icon::SETTINGS, "Settings & export", None),
            (Page::Live, icon::NETWORK, "Live classroom", live_n.map(|n| format!("{n} on"))),
            (Page::Results, icon::CHART, "Results", if rn > 0 { Some(rn.to_string()) } else { None }),
        ] {
            if nav_item(ui, ic, label, self.page == page, badge) {
                self.page = page;
            }
            ui.add_space(2.0);
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(theme::figure(format!("v{} · OPH", env!("CARGO_PKG_VERSION")), 11.0, p.faint));
            let light = theme::is_light();
            if ui.add(egui::Button::new(theme::muted(format!("{}  {} theme", if light { icon::MOON } else { icon::SUN }, if light { "Dark" } else { "Light" }))).frame(false)).clicked() {
                theme::set_light(ui.ctx(), !light);
            }
            ui.add_space(8.0);
            ui.separator();
            let state = if self.path.is_none() { "Not saved yet" } else if self.dirty { "Unsaved changes" } else { "All changes saved" };
            theme::status_dot(ui, if self.dirty || self.path.is_none() { p.warning } else { p.success }, state);
            ui.add(egui::Label::new(RichText::new(&self.project.exam.title).font(theme::semibold(14.0))).truncate());
            ui.label(theme::eyebrow("Current exam"));
        });
    }

    fn page_header(&mut self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(theme::heading(title, 24.0));
                ui.label(theme::muted(subtitle));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary(ui, &format!("{}  Export .exe", icon::EXPORT)).clicked() {
                    self.export();
                }
                if theme::ghost(ui, &format!("{}  Preview", icon::PLAY)).on_hover_text("Open the exam in a normal window, without lockdown").clicked() {
                    self.preview();
                }
                if ui.add(egui::Button::new(theme::muted(format!("{}  Save", icon::SAVE))).frame(false)).on_hover_text("Ctrl+S").clicked() {
                    self.save(false);
                }
            });
        });
        ui.add_space(16.0);
    }

    // ───────────── Home ─────────────

    fn home(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let w = ui.available_width();
            let left_w = (w * 0.58).min(640.0);
            ui.add_space(24.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(left_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.label(theme::eyebrow("Exam studio"));
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(theme::heading("Build an exam. Ship it as one locked-down .exe.", 34.0)).wrap());
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(RichText::new("Students run the file, enter the password you announce, and the PC stays inside the exam until they submit. Results come back over the classroom network — no internet, no accounts.").size(15.0).color(p.muted)).wrap());
                    ui.add_space(22.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new(format!("{}  New exam", icon::ADD)).font(theme::semibold(14.5)).color(p.on_accent)).fill(p.accent).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(0.0, 38.0))).clicked() {
                            self.new_project();
                        }
                        if ui.add(egui::Button::new(RichText::new(format!("{}  Open project", icon::OPEN)).font(theme::semibold(14.5))).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0_f32, p.border_strong)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(0.0, 38.0))).clicked() {
                            self.open_dialog();
                        }
                        if ui.add(egui::Button::new(RichText::new("Load the sample exam").color(p.accent)).frame(false)).on_hover_text("Six questions covering every type, including two code questions").clicked() {
                            self.new_project();
                            self.project.exam = sample_exam();
                            self.selected = Some(0);
                        }
                    });
                    ui.add_space(34.0);
                    ui.label(theme::eyebrow("Recent projects"));
                    ui.add_space(4.0);
                    if self.recent.is_empty() {
                        ui.label(theme::muted("Projects you save appear here. They live in Documents\\HDJLabs\\Exams."));
                    }
                    let recent = self.recent.clone();
                    for path in recent {
                        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
                        let painter = ui.painter();
                        if resp.hovered() {
                            painter.rect_filled(rect, CornerRadius::same(theme::R_CONTROL), p.surface2);
                        }
                        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, p.border));
                        painter.text(rect.left_center() + egui::vec2(10.0, -8.0), egui::Align2::LEFT_CENTER, &name, theme::semibold(14.0), p.text);
                        painter.text(rect.left_center() + egui::vec2(10.0, 10.0), egui::Align2::LEFT_CENTER, &dir, theme::mono(11.0), p.faint);
                        if resp.clicked() {
                            self.open_path(path);
                        }
                    }
                });
                ui.add_space(40.0);
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width().min(420.0), 0.0), Layout::top_down(Align::Min), |ui| {
                    egui::Frame::new().stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(20)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::eyebrow("How a session runs"));
                        ui.add_space(6.0);
                        for (i, (title, body)) in [
                            ("Write the questions", "Multiple choice, true/false, short answer, and code with stdin/stdout tests."),
                            ("Export the .exe", "Set the exam password and your proctor exit password. Questions stay encrypted until you announce it."),
                            ("Open the live classroom", "Students download the exam from your PC's address in any browser on the same network."),
                            ("Grade as they submit", "Scores arrive instantly. Read every line of code, run it, adjust points, export CSV."),
                        ]
                        .iter()
                        .enumerate()
                        {
                            if i > 0 {
                                ui.separator();
                            }
                            ui.horizontal_top(|ui| {
                                ui.label(theme::figure(format!("{:02}", i + 1), 13.0, p.accent));
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.label(RichText::new(*title).font(theme::semibold(14.5)));
                                    ui.add(egui::Label::new(theme::muted(*body).size(13.0)).wrap());
                                });
                            });
                        }
                    });
                });
            });
        });
    }

    // ───────────── Build ─────────────

    fn build(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let n = self.project.exam.questions.len();
        self.page_header(ui, "Questions", &format!("{n} question{} · {} points total", if n == 1 { "" } else { "s" }, theme::fmt_points(self.project.exam.total_points())));

        let list_w = 336.0;
        ui.horizontal_top(|ui| {
            // Question list.
            ui.allocate_ui_with_layout(egui::vec2(list_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    ui.spacing_mut().button_padding = egui::vec2(9.0, 5.0);
                    let id = self.project.exam.next_question_id();
                    let mut add = None;
                    if ui.button(format!("{} Choice", icon::ADD)).clicked() {
                        add = Some(Kind::Choice { options: vec![String::new(), String::new(), String::new(), String::new()], correct: vec![0], multiple: false });
                    }
                    if ui.button(format!("{} True/False", icon::ADD)).clicked() {
                        add = Some(Kind::TrueFalse { answer: true });
                    }
                    if ui.button(format!("{} Short", icon::ADD)).clicked() {
                        add = Some(Kind::Short { accepted: vec![String::new()], case_sensitive: false });
                    }
                    if ui.button(format!("{} Code", icon::ADD)).clicked() {
                        add = Some(Kind::Code(CodeSpec { lang: Lang::Python, starter: Lang::Python.template().into(), tests: vec![TestCase { input: "World".into(), expected: "Hello, World".into(), hidden: false }], reference: String::new() }));
                    }
                    if let Some(k) = add {
                        let mut q = Question::new(id, k);
                        if matches!(q.kind, Kind::Code(_)) {
                            q.points = 5.0;
                        }
                        self.exam().questions.push(q);
                        self.selected = Some(n);
                    }
                });
                ui.add_space(6.0);
                let mut action: Option<(usize, i32)> = None; // (index, -1 up / 1 down / 0 delete / 2 duplicate)
                egui::ScrollArea::vertical().id_salt("qlist").auto_shrink([false, false]).show(ui, |ui| {
                    if n == 0 {
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(icon::DOC).size(30.0).color(p.muted));
                            ui.label(theme::muted("No questions yet — add one above."));
                        });
                    }
                    for (i, q) in self.project.exam.questions.iter().enumerate() {
                        let sel = self.selected == Some(i);
                        let fr = egui::Frame::new()
                            .fill(if sel { p.accent_soft } else { p.surface })
                            .stroke(Stroke::new(1.0_f32, if sel { p.accent } else { p.border }))
                            .corner_radius(CornerRadius::same(theme::R_PANEL))
                            .inner_margin(Margin::symmetric(12, 10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{}", i + 1)).font(theme::semibold(15.0)).color(p.accent));
                                    theme::pill(ui, q.kind.label(), kind_color(&q.kind));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(theme::muted(format!("{} pt", theme::fmt_points(q.points))).size(12.5));
                                    });
                                });
                                let first = q.prompt.lines().next().unwrap_or("");
                                let mut snippet: String = first.chars().take(72).collect();
                                if first.chars().count() > 72 {
                                    snippet = format!("{}…", snippet.trim_end());
                                }
                                ui.label(if snippet.is_empty() { theme::muted("(no prompt yet)").italics() } else { RichText::new(snippet).color(p.text) });
                                if sel {
                                    ui.horizontal(|ui| {
                                        if theme::icon_button(ui, icon::UP, "Move up").clicked() {
                                            action = Some((i, -1));
                                        }
                                        if theme::icon_button(ui, icon::DOWN, "Move down").clicked() {
                                            action = Some((i, 1));
                                        }
                                        if theme::icon_button(ui, icon::COPY, "Duplicate").clicked() {
                                            action = Some((i, 2));
                                        }
                                        if theme::icon_button(ui, icon::DELETE, "Delete").clicked() {
                                            action = Some((i, 0));
                                        }
                                    });
                                }
                            });
                        if fr.response.interact(egui::Sense::click()).clicked() {
                            self.selected = Some(i);
                        }
                        ui.add_space(4.0);
                    }
                });
                if let Some((i, a)) = action {
                    let qs = &mut self.exam().questions;
                    match a {
                        -1 if i > 0 => {
                            qs.swap(i, i - 1);
                            self.selected = Some(i - 1);
                        }
                        1 if i + 1 < qs.len() => {
                            qs.swap(i, i + 1);
                            self.selected = Some(i + 1);
                        }
                        2 => {
                            let mut c = qs[i].clone();
                            c.id = qs.iter().map(|q| q.id).max().unwrap_or(0) + 1;
                            qs.insert(i + 1, c);
                            self.selected = Some(i + 1);
                        }
                        0 => {
                            qs.remove(i);
                            self.selected = if qs.is_empty() { None } else { Some(i.min(qs.len() - 1)) };
                        }
                        _ => {}
                    }
                }
            });
            ui.add_space(8.0);
            // Editor.
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ui.available_height()), Layout::top_down(Align::Min), |ui| {
                egui::ScrollArea::vertical().id_salt("qedit").auto_shrink([false, false]).show(ui, |ui| match self.selected.filter(|i| *i < n) {
                    None => {
                        theme::card(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(theme::heading("Select or add a question", 18.0));
                            ui.label(theme::muted("Questions appear on the left. Students get them in this order unless you enable shuffling in Settings."));
                        });
                    }
                    Some(i) => self.question_editor(ui, i),
                });
            });
        });
    }

    fn question_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        let p = pal();
        let mut q = self.project.exam.questions[i].clone();
        let before = q.clone();
        let mut ref_run = false;
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::heading(format!("Question {}", i + 1), 20.0));
                theme::pill(ui, q.kind.label(), kind_color(&q.kind));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut q.points).range(0.0..=1000.0).speed(0.25).suffix(" pts"));
                    ui.label(theme::muted("Points"));
                });
            });
            ui.add_space(4.0);
            theme::field_label(ui, "Prompt");
            ui.add(egui::TextEdit::multiline(&mut q.prompt).hint_text("Write the question…").desired_rows(4).desired_width(f32::INFINITY).margin(Margin::same(12)));
            ui.add_space(8.0);
            match &mut q.kind {
                Kind::Choice { options, correct, multiple } => {
                    ui.horizontal(|ui| {
                        theme::field_label(ui, "Options — tick the correct answer(s)");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.checkbox(multiple, "Allow several correct answers").changed() && !*multiple && correct.len() > 1 {
                                correct.truncate(1);
                            }
                        });
                    });
                    let mut remove = None;
                    let field_w = ui.available_width() - 96.0;
                    for (oi, opt) in options.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let mut on = correct.contains(&oi);
                            let letter = (b'A' + oi as u8) as char;
                            if ui.checkbox(&mut on, RichText::new(letter.to_string()).font(theme::semibold(14.0))).changed() {
                                if *multiple {
                                    if on { correct.push(oi) } else { correct.retain(|c| *c != oi) }
                                } else if on {
                                    *correct = vec![oi];
                                }
                            }
                            ui.add(egui::TextEdit::singleline(opt).hint_text(format!("Option {letter}")).desired_width(field_w).margin(Margin::symmetric(10, 7)));
                            if theme::icon_button(ui, icon::DELETE, "Remove option").clicked() {
                                remove = Some(oi);
                            }
                        });
                    }
                    if let Some(r) = remove {
                        options.remove(r);
                        correct.retain(|c| *c != r);
                        for c in correct.iter_mut() {
                            if *c > r {
                                *c -= 1;
                            }
                        }
                    }
                    if options.len() < 10 && ui.button(format!("{}  Add option", icon::ADD)).clicked() {
                        options.push(String::new());
                    }
                }
                Kind::TrueFalse { answer } => {
                    theme::field_label(ui, "Correct answer");
                    ui.horizontal(|ui| {
                        ui.radio_value(answer, true, "True");
                        ui.radio_value(answer, false, "False");
                    });
                }
                Kind::Short { accepted, case_sensitive } => {
                    theme::field_label(ui, "Accepted answers (extra spaces are ignored)");
                    let mut remove = None;
                    let field_w = ui.available_width() - 48.0;
                    for (ai, a) in accepted.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(a).hint_text("Accepted answer").desired_width(field_w).margin(Margin::symmetric(10, 7)));
                            if theme::icon_button(ui, icon::DELETE, "Remove").clicked() {
                                remove = Some(ai);
                            }
                        });
                    }
                    if let Some(r) = remove {
                        accepted.remove(r);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(format!("{}  Add alternative", icon::ADD)).clicked() {
                            accepted.push(String::new());
                        }
                        ui.checkbox(case_sensitive, "Case sensitive");
                    });
                }
                Kind::Code(spec) => {
                    ui.horizontal(|ui| {
                        theme::field_label(ui, "Language");
                        let old = spec.lang;
                        egui::ComboBox::from_id_salt(("lang", q.id)).selected_text(spec.lang.name()).show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                let avail = runner::tool(l).map(|t| t.label).unwrap_or_else(|| "not on this PC".into());
                                ui.selectable_value(&mut spec.lang, l, format!("{}  —  {avail}", l.name()));
                            }
                        });
                        if spec.lang != old && (spec.starter.trim().is_empty() || spec.starter == old.template()) {
                            spec.starter = spec.lang.template().into();
                        }
                        match runner::tool(spec.lang) {
                            Some(t) => theme::pill(ui, &format!("{}  {}", icon::CHECK, t.label), p.success),
                            None => theme::pill(ui, &format!("{}  Students' PCs need this compiler installed", icon::WARNING), p.warning),
                        };
                    });
                    if matches!(spec.lang, Lang::Python | Lang::JavaScript) {
                        ui.label(theme::muted("Runs everywhere — a built-in interpreter ships inside the exam .exe.").size(12.5));
                    }
                    theme::field_label(ui, "Starter code (what students see first)");
                    ui.allocate_ui(egui::vec2(ui.available_width(), 220.0), |ui| {
                        ui.set_min_height(200.0);
                        editor::code_editor(ui, ("starter", q.id), &mut spec.starter, spec.lang, self.font, false);
                    });
                    ui.add_space(6.0);
                    theme::field_label(ui, "Reference solution (private — never shipped; used to fill expected outputs)");
                    ui.allocate_ui(egui::vec2(ui.available_width(), 200.0), |ui| {
                        ui.set_min_height(180.0);
                        editor::code_editor(ui, ("ref", q.id), &mut spec.reference, spec.lang, self.font, false);
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(theme::eyebrow("Test cases"));
                        ui.label(theme::muted("stdin → expected stdout · score = passed / total × points").size(12.5));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let busy = self.ref_job.is_some();
                            if ui.add_enabled(!busy && !spec.reference.trim().is_empty() && !spec.tests.is_empty(), egui::Button::new(format!("{}  Fill outputs from solution", icon::PLAY))).clicked() {
                                ref_run = true;
                            }
                            if busy {
                                ui.add(egui::Spinner::new());
                            }
                        });
                    });
                    let mut remove = None;
                    for (ti, t) in spec.tests.iter_mut().enumerate() {
                        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(10)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("Test {}", ti + 1)).strong());
                                ui.checkbox(&mut t.hidden, "Hidden").on_hover_text("Hidden tests only run when the student submits");
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::icon_button(ui, icon::DELETE, "Remove test").clicked() {
                                        remove = Some(ti);
                                    }
                                });
                            });
                            ui.columns(2, |cols| {
                                cols[0].label(theme::muted("Input").size(12.0));
                                cols[0].add(egui::TextEdit::multiline(&mut t.input).font(egui::FontId::monospace(13.0)).desired_rows(2).desired_width(f32::INFINITY));
                                cols[1].label(theme::muted("Expected output").size(12.0));
                                cols[1].add(egui::TextEdit::multiline(&mut t.expected).font(egui::FontId::monospace(13.0)).desired_rows(2).desired_width(f32::INFINITY));
                            });
                        });
                    }
                    if let Some(r) = remove {
                        spec.tests.remove(r);
                    }
                    if ui.button(format!("{}  Add test", icon::ADD)).clicked() {
                        spec.tests.push(TestCase { input: String::new(), expected: String::new(), hidden: false });
                    }
                    if spec.tests.is_empty() {
                        ui.label(theme::muted("No tests: this question will be graded by you in Results.").size(12.5));
                    }
                }
            }
        });
        if ref_run {
            if let Kind::Code(spec) = &q.kind {
                let (tx, rx) = mpsc::channel();
                let spec = spec.clone();
                std::thread::spawn(move || {
                    let outs = spec.tests.iter().map(|t| runner::run(spec.lang, &spec.reference, &t.input, runner::RUN_TIMEOUT)).collect();
                    let _ = tx.send(outs);
                });
                self.ref_job = Some((q.id, rx));
            }
        }
        if q != before {
            self.exam().questions[i] = q;
        }
    }

    // ───────────── Settings & export ─────────────

    fn settings(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        self.page_header(ui, "Settings & export", "Exam details, access, delivery and security");
        let mut e = self.project.exam.clone();
        let before = e.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let problems = e.validate();
            theme::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::heading("Export", 18.0));
                        ui.label(theme::muted("Produces one Windows .exe containing the encrypted exam and the secure runtime."));
                        if let Some(lp) = &self.project.last_export {
                            ui.label(theme::muted(format!("Last export: {}", lp.display())).size(12.5));
                        }
                    });
                });
                if problems.is_empty() {
                    ui.label(RichText::new(format!("{}  Ready to export", icon::CHECK)).color(p.success));
                } else {
                    for pr in &problems {
                        ui.label(RichText::new(format!("{}  {pr}", icon::WARNING)).color(p.warning));
                    }
                }
            });
            ui.add_space(12.0);
            ui.columns(2, |cols| {
                theme::card(&mut cols[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::heading("Details", 18.0));
                    theme::field_label(ui, "Title");
                    theme::text_field(ui, &mut e.title, "Exam title", f32::INFINITY);
                    theme::field_label(ui, "Teacher / course");
                    theme::text_field(ui, &mut e.author, "e.g. Mr. Hussein — CS101", f32::INFINITY);
                    theme::field_label(ui, "Instructions shown before the exam");
                    ui.add(egui::TextEdit::multiline(&mut e.instructions).desired_rows(3).desired_width(f32::INFINITY).margin(Margin::same(10)));
                    theme::field_label(ui, "Duration");
                    ui.add(egui::DragValue::new(&mut e.duration_min).range(1..=600).suffix(" minutes"));
                });
                cols[0].add_space(12.0);
                theme::card(&mut cols[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::heading("Delivery", 18.0));
                    ui.checkbox(&mut e.settings.shuffle_questions, "Shuffle question order for each student");
                    ui.checkbox(&mut e.settings.shuffle_options, "Shuffle multiple-choice options");
                    ui.horizontal(|ui| {
                        ui.label("Questions per student");
                        let n = e.questions.len() as u32;
                        ui.add(egui::DragValue::new(&mut e.settings.pick).range(0..=n.max(1)).custom_formatter(|v, _| if v < 0.5 { "All".into() } else { format!("{}", v as u32) }));
                        ui.label(theme::muted("random subset").size(12.5));
                    });
                    ui.checkbox(&mut e.settings.show_score, "Show the score when the student submits");
                    ui.checkbox(&mut e.settings.show_review, "Show per-question results after submitting");
                });
                theme::card(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::heading("Access", 18.0));
                    theme::field_label(ui, "Exam password — tell students when the exam starts");
                    ui.add(egui::TextEdit::singleline(&mut e.settings.exam_password).hint_text("Leave empty for no password").desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
                    ui.label(theme::muted("The questions are encrypted with this password, so nobody can read them before you announce it.").size(12.5));
                    theme::field_label(ui, "Proctor exit password — only for you");
                    ui.add(egui::TextEdit::singleline(&mut e.settings.exit_password).hint_text("Unlocks a student's PC without submitting").desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
                    ui.label(theme::muted("On the student PC: Ctrl+Shift+F12 or the lock icon opens the proctor dialog.").size(12.5));
                });
                cols[1].add_space(12.0);
                theme::card(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::heading("Security", 18.0));
                    ui.checkbox(&mut e.settings.lockdown, "Lockdown mode (recommended)");
                    ui.label(theme::muted("Private exam desktop · blocks Alt+Tab, Win key, Task Manager, Ctrl+Alt+Del menu, touchpad & edge gestures, screenshots and screen sharing until submission.").size(12.5));
                    ui.add_enabled(e.settings.lockdown, egui::Checkbox::new(&mut e.settings.block_remote_tools, "Refuse to start while remote-control / screen-sharing apps run"));
                    ui.add_enabled(e.settings.lockdown, egui::Checkbox::new(&mut e.settings.block_vm, "Refuse to run inside a virtual machine"));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("LAN port");
                        ui.add(egui::DragValue::new(&mut e.settings.lan_port).range(1024..=65000));
                    });
                });
            });
        });
        if e != before {
            *self.exam() = e;
        }
    }

    // ───────────── Live classroom ─────────────

    fn live_page(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::heading("Live classroom", 26.0));
                ui.label(theme::muted("Students on the same Wi-Fi / switch connect automatically — no internet needed."));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.live.is_some() {
                    if theme::danger(ui, &format!("{}  Stop session", icon::STOP)).clicked() {
                        self.live = None;
                    }
                } else if theme::primary(ui, &format!("{}  Start session", icon::PLAY)).clicked() {
                    let share = self.project.last_export.clone().filter(|p| p.exists()).map(|p| (p, self.project.exam.title.clone()));
                    let ctx = ui.ctx().clone();
                    let server = LiveServer::start(self.project.exam.id.clone(), self.project.teacher_secret.clone(), self.project.exam.author.clone(), self.project.exam.settings.lan_port, share, move || ctx.request_repaint());
                    for e in &server.errors {
                        self.toasts.push(Toast { text: e.clone(), at: Instant::now(), error: true });
                    }
                    self.live = Some(server);
                }
                if theme::ghost(ui, &format!("{}  Allow through firewall", icon::SHIELD)).on_hover_text("Adds a Windows Firewall rule for HDJLabs (asks for administrator permission)").clicked() {
                    crate::ui::allow_firewall();
                }
            });
        });
        ui.add_space(12.0);

        let Some(live) = &self.live else {
            theme::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::heading("Before you start", 18.0));
                for (n, t) in [
                    ("1", "Connect this PC and the students' PCs to the same router, switch or hotspot. Internet is not required."),
                    ("2", "Export the exam (Settings & export). The live session shares that exact file with students."),
                    ("3", "Click \"Allow through firewall\" once, then Start session."),
                    ("4", "Students open the download address in any browser, run the exam, and appear here."),
                ] {
                    ui.horizontal(|ui| {
                        theme::pill(ui, n, p.accent);
                        ui.label(t);
                    });
                }
                if self.project.last_export.as_ref().is_none_or(|p| !p.exists()) {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("{}  This exam hasn't been exported yet — students won't be able to download it.", icon::WARNING)).color(p.warning));
                }
            });
            return;
        };

        // Connection card.
        let ips = net::local_ipv4();
        let http = live.http_url.clone();
        let port = live.tcp_port;
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{}  Session running", icon::NETWORK)).font(theme::semibold(15.0)).color(p.success));
                ui.separator();
                match &http {
                    Some(u) => {
                        ui.label(theme::muted("Students download from"));
                        ui.label(RichText::new(u).font(theme::semibold(16.0)).color(p.accent));
                        if theme::icon_button(ui, icon::COPY, "Copy link").clicked() {
                            ui.ctx().copy_text(u.clone());
                        }
                    }
                    None => {
                        ui.label(RichText::new("Download page off — export the exam, then restart the session.").color(p.warning));
                    }
                }
                ui.separator();
                ui.label(theme::muted(format!("Teacher address: {}  (port {port})", ips.iter().map(|(i, _)| i.to_string()).collect::<Vec<_>>().join(", "))));
            });
        });
        ui.add_space(10.0);

        let (students, log) = {
            let st = live.state.lock().unwrap();
            (st.students.clone(), st.log.iter().rev().take(5).cloned().collect::<Vec<_>>())
        };
        let connected = students.iter().filter(|s| s.connected).count();
        let working = students.iter().filter(|s| s.status.as_ref().is_some_and(|x| x.phase == Phase::Working) && s.submission.is_none()).count();
        let submitted = students.iter().filter(|s| s.submission.is_some()).count();
        let mut send: Vec<(Option<String>, S2C)> = vec![];

        ui.horizontal(|ui| {
            stat(ui, "Connected", &connected.to_string(), p.accent);
            stat(ui, "Working", &working.to_string(), p.warning);
            stat(ui, "Submitted", &submitted.to_string(), p.success);
            stat(ui, "Total seen", &students.len().to_string(), p.muted);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::ghost(ui, &format!("{}  +5 min all", icon::CLOCK)).clicked() {
                    send.push((None, S2C::AddTime { secs: 300 }));
                }
                if ui.add_enabled(!self.broadcast.trim().is_empty(), egui::Button::new(format!("{}  Send to all", icon::SEND))).clicked() {
                    send.push((None, S2C::Message { text: self.broadcast.trim().to_string() }));
                    self.broadcast.clear();
                }
                theme::text_field(ui, &mut self.broadcast, "Message to all students…", 260.0);
            });
        });
        ui.add_space(10.0);

        let total_q = self.project.exam.delivered_count();
        ui.horizontal_top(|ui| {
            let table_w = if self.live_selected.is_some() { ui.available_width() * 0.52 } else { ui.available_width() };
            ui.allocate_ui_with_layout(egui::vec2(table_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                theme::card_frame().inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(ui.available_height() - 30.0);
                    if students.is_empty() {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                            ui.label(theme::muted("Waiting for students to open the exam…"));
                        });
                    }
                    egui::ScrollArea::vertical().id_salt("roster").auto_shrink([false, false]).show(ui, |ui| {
                        egui::Grid::new("roster-grid").num_columns(6).striped(true).spacing([16.0, 10.0]).min_col_width(40.0).show(ui, |ui| {
                            for h in ["Student", "Status", "Progress", "Time left", "Alerts", "Score"] {
                                ui.label(theme::eyebrow(h));
                            }
                            ui.end_row();
                            for s in &students {
                                let sel = self.live_selected.as_deref() == Some(s.attempt_id.as_str());
                                let name = ui.add(egui::Button::new(RichText::new(&s.name).font(theme::semibold(14.0)).color(if sel { p.accent } else { p.text })).frame(false)).on_hover_text(format!("ID {} · {} · {}", s.student_id, s.machine, s.addr));
                                if name.clicked() {
                                    self.live_selected = if sel { None } else { Some(s.attempt_id.clone()) };
                                }
                                let (label, col) = if s.submission.is_some() {
                                    ("Submitted", p.success)
                                } else if !s.connected {
                                    ("Offline", p.muted)
                                } else {
                                    match s.status.as_ref().map(|x| x.phase) {
                                        Some(Phase::Working) => ("Working", p.warning),
                                        _ => ("Signing in", p.muted),
                                    }
                                };
                                theme::pill(ui, label, col);
                                let (ans, tot) = s.status.as_ref().map(|x| (x.answered, x.total)).unwrap_or((0, total_q));
                                ui.horizontal(|ui| {
                                    ui.set_width(120.0);
                                    ui.allocate_ui(egui::vec2(70.0, 8.0), |ui| theme::progress_bar(ui, ans as f32 / tot.max(1) as f32, 6.0, p.accent));
                                    ui.label(theme::muted(format!("{ans}/{tot}")).size(12.5));
                                });
                                ui.label(RichText::new(s.status.as_ref().map(|x| theme::fmt_secs(x.remaining_secs)).unwrap_or_else(|| "—".into())).monospace());
                                let v = s.status.as_ref().map(|x| x.violations).unwrap_or(0).max(s.events.len() as u32);
                                if v > 0 {
                                    theme::pill(ui, &format!("{}  {v}", icon::WARNING), p.danger);
                                } else {
                                    ui.label(theme::muted("—"));
                                }
                                match &s.submission {
                                    Some(sub) => ui.label(theme::figure(format!("{} / {}", theme::fmt_points(sub.score), theme::fmt_points(sub.max_score)), 13.0, p.text)),
                                    None => ui.label(theme::muted("—")),
                                };
                                ui.end_row();
                            }
                        });
                        if !log.is_empty() {
                            ui.add_space(10.0);
                            for l in &log {
                                ui.label(theme::muted(l).size(11.5));
                            }
                        }
                    });
                });
            });
            if let Some(sel) = self.live_selected.clone() {
                if let Some(s) = students.iter().find(|s| s.attempt_id == sel) {
                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ui.available_height()), Layout::top_down(Align::Min), |ui| {
                        theme::card_frame().inner_margin(Margin::same(14)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.set_min_height(ui.available_height() - 30.0);
                            ui.horizontal(|ui| {
                                ui.label(theme::heading(&s.name, 20.0));
                                ui.label(theme::muted(format!("{} · {}", s.student_id, s.machine)));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::icon_button(ui, icon::CANCEL, "Close").clicked() {
                                        self.live_selected = None;
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                theme::text_field(ui, &mut self.direct_msg, "Message to this student…", 220.0);
                                if ui.add_enabled(!self.direct_msg.trim().is_empty(), egui::Button::new(icon::SEND)).clicked() {
                                    send.push((Some(sel.clone()), S2C::Message { text: self.direct_msg.trim().to_string() }));
                                    self.direct_msg.clear();
                                }
                                if ui.button("+5 min").clicked() {
                                    send.push((Some(sel.clone()), S2C::AddTime { secs: 300 }));
                                }
                                if s.submission.is_none() && ui.add(egui::Button::new(RichText::new("End exam").color(p.danger))).on_hover_text("Submit this student's exam now").clicked() {
                                    send.push((Some(sel.clone()), S2C::ForceSubmit));
                                }
                            });
                            if !s.events.is_empty() {
                                egui::CollapsingHeader::new(RichText::new(format!("{}  {} alert(s)", icon::WARNING, s.events.len())).color(p.danger)).id_salt(("ev", &sel)).show(ui, |ui| {
                                    for (t, k) in s.events.iter().rev().take(30) {
                                        ui.label(theme::muted(format!("{}  {k}", storage::chrono_like(*t))).size(12.0));
                                    }
                                });
                            }
                            ui.separator();
                            egui::ScrollArea::vertical().id_salt(("live-answers", &sel)).auto_shrink([false, false]).show(ui, |ui| {
                                let order = if s.order.is_empty() { self.project.exam.questions.iter().map(|q| q.id).collect() } else { s.order.clone() };
                                for (i, qid) in order.iter().enumerate() {
                                    let Some(q) = self.project.exam.questions.iter().find(|q| q.id == *qid).cloned() else { continue };
                                    let a = s.answers.get(qid).cloned().unwrap_or_default();
                                    self.answer_view(ui, &sel, i, &q, &a, None);
                                }
                            });
                        });
                    });
                }
            }
        });

        if let Some(live) = &self.live {
            let mut st = live.state.lock().unwrap();
            for (to, msg) in send {
                match to {
                    Some(id) => st.send(&id, msg),
                    None => st.broadcast(msg),
                }
            }
        }
    }

    /// Shows one answer (with live grading for static types and a test runner for code).
    fn answer_view(&mut self, ui: &mut egui::Ui, attempt: &str, idx: usize, q: &Question, a: &Answer, result_idx: Option<usize>) {
        let p = pal();
        let auto = if matches!(q.kind, Kind::Code(_)) { None } else { Some(grading::grade_static(q, a)) };
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Q{}", idx + 1)).font(theme::semibold(14.0)).color(p.accent));
                let prompt_w = (ui.available_width() - 150.0).max(60.0);
                ui.allocate_ui_with_layout(egui::vec2(prompt_w, 22.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(q.prompt.lines().next().unwrap_or("")).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(ri) = result_idx {
                        let r = &mut self.project.results[ri];
                        let auto_pts = r.submission.results.iter().find(|x| x.qid == q.id).map(|x| x.earned).unwrap_or(0.0);
                        let mut v = r.earned(q.id);
                        let changed = ui.add(egui::DragValue::new(&mut v).range(0.0..=q.points).speed(0.25).suffix(format!(" / {}", theme::fmt_points(q.points)))).on_hover_text("Override the points for this answer").changed();
                        if changed {
                            if (v - auto_pts).abs() < 0.001 {
                                r.overrides.remove(&q.id);
                            } else {
                                r.overrides.insert(q.id, v);
                            }
                            self.dirty = true;
                        }
                        if r.overrides.contains_key(&q.id) {
                            theme::pill(ui, "manual", p.accent);
                        }
                    } else if let Some(g) = &auto {
                        let col = match g.status {
                            Status::Correct => p.success,
                            Status::Partial => p.warning,
                            Status::Unanswered => p.muted,
                            _ => p.danger,
                        };
                        theme::pill(ui, g.status.label(), col);
                    }
                });
            });
            match (&q.kind, a) {
                (_, Answer::Empty) => {
                    ui.label(theme::muted(if result_idx.is_some() { "No answer" } else { "No answer yet" }));
                }
                (Kind::Choice { options, .. }, Answer::Choice(v)) => {
                    let picked: Vec<String> = v.iter().filter_map(|i| options.get(*i).map(|o| format!("{}. {o}", (b'A' + *i as u8) as char))).collect();
                    ui.label(picked.join("   "));
                }
                (_, Answer::Bool(b)) => {
                    ui.label(if *b { "True" } else { "False" });
                }
                (_, Answer::Text(t)) => {
                    ui.label(RichText::new(t).monospace());
                }
                (Kind::Code(spec), Answer::Code { lang, source }) => {
                    let lines = source.lines().count().clamp(4, 22) as f32;
                    ui.allocate_ui(egui::vec2(ui.available_width(), lines * 19.0 + 16.0), |ui| {
                        let mut s = source.clone();
                        editor::code_editor(ui, ("review", attempt, q.id), &mut s, *lang, 13.0, true);
                    });
                    let key = (attempt.to_string(), q.id);
                    ui.horizontal(|ui| {
                        let busy = self.review_jobs.contains_key(&key);
                        if ui.add_enabled(!busy && !spec.tests.is_empty(), egui::Button::new(format!("{}  Run all tests here", icon::TESTS))).clicked() {
                            let (tx, rx) = mpsc::channel();
                            let (spec, lang, src) = (spec.clone(), *lang, source.clone());
                            std::thread::spawn(move || {
                                let _ = tx.send(grading::run_tests(&spec, lang, &src, true));
                            });
                            self.review_jobs.insert(key.clone(), rx);
                        }
                        if busy {
                            ui.add(egui::Spinner::new());
                        }
                        if ui.add_enabled(!busy, egui::Button::new(format!("{}  Run", icon::PLAY))).on_hover_text("Run with the first test's input").clicked() {
                            let (tx, rx) = mpsc::channel();
                            let (lang, src) = (*lang, source.clone());
                            let input = spec.tests.first().map(|t| t.input.clone()).unwrap_or_default();
                            std::thread::spawn(move || {
                                let o = runner::run(lang, &src, &input, runner::RUN_TIMEOUT);
                                let _ = tx.send(vec![TestOutcome { passed: o.ok(), hidden: false, input, expected: String::new(), output: o }]);
                            });
                            self.review_jobs.insert(key.clone(), rx);
                        }
                    });
                    if let Some(r) = self.review_results.get(&key) {
                        if r.len() == 1 && r[0].expected.is_empty() {
                            run_output(ui, &r[0].output);
                        } else {
                            test_report(ui, r);
                        }
                    }
                }
                _ => {
                    ui.label(theme::muted("(answer type does not match the question)"));
                }
            }
        });
        ui.add_space(6.0);
    }

    // ───────────── Results ─────────────

    fn results(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::heading("Results", 26.0));
                ui.label(theme::muted("Live submissions land here automatically. Import .hdjr files collected by USB."));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!self.project.results.is_empty(), egui::Button::new(format!("{}  Export CSV", icon::DOWNLOAD)).min_size(egui::vec2(0.0, 36.0))).clicked() {
                    let name = format!("{} results.csv", net::sanitize_file_name(&self.project.exam.title));
                    if let Some(out) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name(name).save_file() {
                        match std::fs::write(&out, storage::results_csv(&self.project)) {
                            Ok(()) => self.toast(format!("Saved {}", out.display()), false),
                            Err(e) => self.toast(e.to_string(), true),
                        }
                    }
                }
                if theme::ghost(ui, &format!("{}  Import result files", icon::OPEN)).clicked() {
                    if let Some(files) = rfd::FileDialog::new().add_filter("HDJLabs result", &["hdjr"]).set_directory(storage::documents().join("HDJLabs Results")).pick_files() {
                        let (mut ok, mut errs) = (0, vec![]);
                        for f in files {
                            match self.project.import_result_file(&f) {
                                Ok(true) => ok += 1,
                                Ok(false) => {}
                                Err(e) => errs.push(format!("{}: {e}", f.file_name().unwrap_or_default().to_string_lossy())),
                            }
                        }
                        if ok > 0 {
                            self.dirty = true;
                            self.toast(format!("Imported {ok} result(s)"), false);
                        }
                        for e in errs {
                            self.toast(e, true);
                        }
                    }
                }
            });
        });
        ui.add_space(12.0);
        if self.project.results.is_empty() {
            theme::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::heading("No results yet", 18.0));
                ui.label(theme::muted("Run a Live classroom session, or import the .hdjr files students saved (Documents\\HDJLabs Results)."));
            });
            return;
        }
        let pcts: Vec<f32> = self.project.results.iter().map(|r| r.percent()).collect();
        let avg = pcts.iter().sum::<f32>() / pcts.len() as f32;
        let hi = pcts.iter().cloned().fold(0.0, f32::max);
        let lo = pcts.iter().cloned().fold(100.0, f32::min);
        ui.horizontal(|ui| {
            stat(ui, "Submissions", &self.project.results.len().to_string(), p.accent);
            stat(ui, "Average", &format!("{avg:.0}%"), p.success);
            stat(ui, "Highest", &format!("{hi:.0}%"), p.success);
            stat(ui, "Lowest", &format!("{lo:.0}%"), p.warning);
        });
        ui.add_space(10.0);

        ui.horizontal_top(|ui| {
            let left_w = if self.result_selected.is_some() { ui.available_width() * 0.45 } else { ui.available_width() };
            ui.allocate_ui_with_layout(egui::vec2(left_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                egui::ScrollArea::vertical().id_salt("results-list").auto_shrink([false, false]).show(ui, |ui| {
                    theme::card_frame().inner_margin(Margin::same(12)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::Grid::new("results-grid").num_columns(5).striped(true).spacing([16.0, 10.0]).show(ui, |ui| {
                            for h in ["Student", "Score", "%", "Alerts", "Submitted"] {
                                ui.label(theme::eyebrow(h));
                            }
                            ui.end_row();
                            for (i, r) in self.project.results.iter().enumerate() {
                                let s = &r.submission;
                                let sel = self.result_selected == Some(i);
                                if ui.add(egui::Button::new(RichText::new(&s.student_name).font(theme::semibold(14.0)).color(if sel { p.accent } else { p.text })).frame(false)).on_hover_text(format!("ID {} · {}", s.student_id, s.machine)).clicked() {
                                    self.result_selected = if sel { None } else { Some(i) };
                                }
                                ui.label(theme::figure(format!("{} / {}", theme::fmt_points(r.total()), theme::fmt_points(s.max_score)), 13.0, p.text));
                                let pct = r.percent();
                                ui.label(theme::figure(format!("{pct:.0}%"), 13.0, if pct >= 50.0 { p.success } else { p.danger }));
                                if s.violations.is_empty() {
                                    ui.label(theme::muted("—"));
                                } else {
                                    theme::pill(ui, &format!("{}  {}", icon::WARNING, s.violations.len()), p.danger);
                                }
                                ui.label(theme::figure(storage::chrono_like(s.submitted_at), 11.5, p.muted));
                                ui.end_row();
                            }
                        });
                    });
                    ui.add_space(10.0);
                    // Per-question difficulty.
                    theme::card(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::eyebrow("Per question"));
                        ui.label(theme::muted("Average share of points earned").size(12.5));
                        for (i, q) in self.project.exam.questions.iter().enumerate() {
                            let got: Vec<f32> = self.project.results.iter().filter(|r| r.submission.results.iter().any(|x| x.qid == q.id)).map(|r| if q.points > 0.0 { r.earned(q.id) / q.points } else { 0.0 }).collect();
                            if got.is_empty() {
                                continue;
                            }
                            let frac = got.iter().sum::<f32>() / got.len() as f32;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("Q{}", i + 1)).font(theme::semibold(13.0)));
                                ui.allocate_ui(egui::vec2(ui.available_width() - 60.0, 10.0), |ui| theme::progress_bar(ui, frac, 8.0, if frac >= 0.5 { p.success } else { p.warning }));
                                ui.label(theme::muted(format!("{:.0}%", frac * 100.0)).size(12.5));
                            });
                        }
                    });
                });
            });
            if let Some(ri) = self.result_selected.filter(|i| *i < self.project.results.len()) {
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ui.available_height()), Layout::top_down(Align::Min), |ui| {
                    theme::card_frame().inner_margin(Margin::same(14)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height() - 30.0);
                        let s = self.project.results[ri].submission.clone();
                        let total = self.project.results[ri].total();
                        ui.horizontal(|ui| {
                            ui.label(theme::heading(&s.student_name, 20.0));
                            ui.label(theme::muted(format!("{} · {}", s.student_id, s.machine)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if theme::icon_button(ui, icon::CANCEL, "Close").clicked() {
                                    self.result_selected = None;
                                }
                                if theme::icon_button(ui, icon::DELETE, "Delete this result").clicked() {
                                    self.project.results.remove(ri);
                                    self.result_selected = None;
                                    self.dirty = true;
                                }
                            });
                        });
                        if self.result_selected.is_none() {
                            return;
                        }
                        ui.label(RichText::new(format!("{} / {} points", theme::fmt_points(total), theme::fmt_points(s.max_score))).font(theme::semibold(16.0)));
                        if s.forced {
                            ui.label(RichText::new("Submitted automatically (timer / teacher / proctor)").color(p.warning));
                        }
                        if !s.violations.is_empty() {
                            egui::CollapsingHeader::new(RichText::new(format!("{}  {} alert(s)", icon::WARNING, s.violations.len())).color(p.danger)).id_salt(("rv", ri)).show(ui, |ui| {
                                for v in &s.violations {
                                    ui.label(theme::muted(format!("{}  {}", storage::chrono_like(v.at), v.kind)).size(12.0));
                                }
                            });
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().id_salt(("result-detail", ri)).auto_shrink([false, false]).show(ui, |ui| {
                            for (i, qid) in s.question_order.iter().enumerate() {
                                let Some(q) = self.project.exam.questions.iter().find(|q| q.id == *qid).cloned() else { continue };
                                let a = s.answers.get(qid).cloned().unwrap_or_default();
                                self.answer_view(ui, &s.attempt_id, i, &q, &a, Some(ri));
                            }
                        });
                    });
                });
            }
        });
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let p = pal();
        for (i, t) in self.toasts.iter().enumerate() {
            egui::Area::new(egui::Id::new(("toast", i)))
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-20.0, -20.0 - i as f32 * 58.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0_f32, if t.error { p.danger } else { p.success }))
                        .corner_radius(CornerRadius::same(theme::R_PANEL))
                        .inner_margin(Margin::symmetric(16, 12))
                        .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(80) })
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(RichText::new(format!("{}  {}", if t.error { icon::WARNING } else { icon::CHECK }, t.text)).color(if t.error { p.danger } else { p.text }));
                        });
                });
        }
    }
}

#[cfg(debug_assertions)]
impl Studio {
    /// Debug builds: open the sample exam on a given page with demo data, for screenshots.
    fn shot_script(&mut self, ctx: &egui::Context) {
        let page = std::env::var("HDJLABS_SHOT_PAGE").unwrap_or_default();
        if page.is_empty() || page == "home" || self.project.exam.title != "Untitled exam" {
            return;
        }
        self.project.exam = sample_exam();
        self.project.exam.author = "Mr. Hussein · CS101".into();
        self.project.last_export = Some(std::env::temp_dir().join("demo.exe"));
        self.selected = std::env::var("HDJLABS_SHOT_Q").ok().and_then(|v| v.parse().ok()).or(Some(0));
        let e = self.project.exam.clone();
        let demo = |name: &str, sid: &str, correct: usize, v: usize| {
            let mut answers = std::collections::BTreeMap::new();
            answers.insert(1, Answer::Choice(vec![1]));
            answers.insert(2, Answer::Choice(vec![0, 2]));
            answers.insert(4, Answer::Text("def".into()));
            answers.insert(5, Answer::Code { lang: Lang::Python, source: "n = int(input())\ntotal = 0\nfor i in range(1, n + 1):\n    total += i\nprint(total)\n".into() });
            let results: Vec<QuestionResult> = e
                .questions
                .iter()
                .enumerate()
                .map(|(i, q)| QuestionResult { qid: q.id, earned: if i < correct { q.points } else { 0.0 }, max: q.points, status: if i < correct { Status::Correct } else { Status::Wrong }, tests_passed: 0, tests_total: 0 })
                .collect();
            Submission {
                exam_id: e.id.clone(),
                exam_title: e.title.clone(),
                attempt_id: format!("demo-{sid}"),
                student_name: name.into(),
                student_id: sid.into(),
                machine: format!("LAB-PC-{}", &sid[2..]),
                started_at: 1_793_000_000,
                submitted_at: 1_793_001_500,
                question_order: e.questions.iter().map(|q| q.id).collect(),
                answers,
                score: results.iter().map(|r| r.earned).sum(),
                results,
                max_score: e.total_points(),
                violations: (0..v).map(|k| Violation { at: 1_793_000_100 + k as u64 * 60, kind: "Blocked system shortcut ×1".into() }).collect(),
                forced: false,
            }
        };
        let subs = vec![demo("Sara Ali", "S-104", 6, 0), demo("Omar Khalid", "S-117", 4, 2), demo("Lina Haddad", "S-122", 5, 0), demo("Yusuf Kareem", "S-131", 2, 1)];
        match page.as_str() {
            "build" => self.page = Page::Build,
            "settings" => self.page = Page::Settings,
            "results" => {
                for s in subs {
                    self.project.upsert_result(s);
                }
                self.result_selected = Some(1);
                self.page = Page::Results;
            }
            "live" => {
                let c = ctx.clone();
                let server = LiveServer::start(e.id.clone(), self.project.teacher_secret.clone(), "T".into(), 47999, None, move || c.request_repaint());
                {
                    let mut st = server.state.lock().unwrap();
                    for (i, sub) in subs.into_iter().enumerate() {
                        let done = i == 0;
                        st.students.push(net::StudentLive {
                            attempt_id: sub.attempt_id.clone(),
                            name: sub.student_name.clone(),
                            student_id: sub.student_id.clone(),
                            machine: sub.machine.clone(),
                            addr: format!("192.168.1.{}", 20 + i),
                            connected: i != 3,
                            last_seen: Instant::now(),
                            status: Some(net::LiveStatus { phase: if done { Phase::Submitted } else { Phase::Working }, current: 4, answered: 2 + i, total: 6, remaining_secs: 1260 - i as i64 * 75, violations: sub.violations.len() as u32 }),
                            answers: sub.answers.clone(),
                            order: sub.question_order.clone(),
                            events: sub.violations.iter().map(|v| (v.at, v.kind.clone())).collect(),
                            submission: if done { Some(sub) } else { None },
                        });
                    }
                }
                self.live_selected = Some("demo-S-117".into());
                self.live = Some(server);
                self.page = Page::Live;
            }
            _ => {}
        }
    }
}

/// Question types share one neutral tag colour; only code is called out.
fn kind_color(k: &Kind) -> Color32 {
    let p = pal();
    match k {
        Kind::Code(_) => p.info,
        _ => p.muted,
    }
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    egui::Frame::new().stroke(Stroke::new(1.0_f32, pal().border)).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::symmetric(14, 9)).show(ui, |ui| {
        ui.set_min_width(104.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::eyebrow(label));
            ui.label(theme::figure(value, 22.0, color));
        });
    });
}

pub fn sample_exam() -> Exam {
    let mut e = Exam::new();
    e.title = "Intro to Programming — Quiz 1".into();
    e.author = "HDJLabs sample".into();
    e.duration_min = 30;
    e.settings.exam_password = "start".into();
    e.settings.exit_password = "proctor".into();
    e.settings.show_review = true;
    e.questions = vec![
        Question { id: 1, prompt: "Which data structure works in a First-In, First-Out order?".into(), points: 1.0, kind: Kind::Choice { options: vec!["Stack".into(), "Queue".into(), "Binary tree".into(), "Hash map".into()], correct: vec![1], multiple: false } },
        Question { id: 2, prompt: "Select every language that is compiled ahead of time to native code by default.".into(), points: 2.0, kind: Kind::Choice { options: vec!["C".into(), "Python".into(), "Rust".into(), "JavaScript".into()], correct: vec![0, 2], multiple: true } },
        Question { id: 3, prompt: "In Python, `len(\"hello\")` returns 5.".into(), points: 1.0, kind: Kind::TrueFalse { answer: true } },
        Question { id: 4, prompt: "What keyword defines a function in Python?".into(), points: 1.0, kind: Kind::Short { accepted: vec!["def".into()], case_sensitive: true } },
        Question {
            id: 5,
            prompt: "Read an integer n and print the sum 1 + 2 + … + n.".into(),
            points: 5.0,
            kind: Kind::Code(CodeSpec {
                lang: Lang::Python,
                starter: "n = int(input())\n# print the sum of 1..n\n".into(),
                tests: vec![
                    TestCase { input: "3".into(), expected: "6".into(), hidden: false },
                    TestCase { input: "10".into(), expected: "55".into(), hidden: false },
                    TestCase { input: "1000".into(), expected: "500500".into(), hidden: true },
                ],
                reference: "n = int(input())\nprint(n * (n + 1) // 2)\n".into(),
            }),
        },
        Question {
            id: 6,
            prompt: "Read a line of text and print it reversed.".into(),
            points: 3.0,
            kind: Kind::Code(CodeSpec {
                lang: Lang::JavaScript,
                starter: "const line = readline();\n// print the reversed line\n".into(),
                tests: vec![TestCase { input: "abc".into(), expected: "cba".into(), hidden: false }, TestCase { input: "HDJLabs".into(), expected: "sbaLJDH".into(), hidden: true }],
                reference: "const line = readline();\nconsole.log(line.split('').reverse().join(''));\n".into(),
            }),
        },
    ];
    e
}

pub fn run() -> i32 {
    let viewport = egui::ViewportBuilder::default().with_title("HDJLabs Studio").with_inner_size([1360.0, 860.0]).with_min_inner_size([1040.0, 640.0]).with_icon(crate::ui::app_icon());
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let r = eframe::run_native(
        "HDJLabs Studio",
        options,
        Box::new(|cc| {
            theme::install_fonts(&cc.egui_ctx);
            theme::apply_style(&cc.egui_ctx);
            Ok(Box::new(Studio::new()))
        }),
    );
    if r.is_ok() { 0 } else { crate::EXIT_UI_FAILED }
}
