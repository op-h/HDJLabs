//! HDJLabs Studio — the teacher app: build an exam, export it as a locked-down .exe, run a live
//! classroom session over the LAN, and grade results.

use crate::grading::{self, TestOutcome};
use crate::model::*;
use crate::net::{self, LiveServer, Phase, S2C};
use crate::payload;
use crate::runner;
use crate::storage::{self, Project};
use crate::ui::text_field::TextField;
use crate::ui::{editor, rtl};
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
    firewall_ok: Option<bool>,
    live_started: Option<Instant>,
    confirm_remove_all: bool,
    confirm_reset_results: bool,
    /// Teacher-side re-checks of submissions, by attempt id.
    regrade_jobs: HashMap<String, Receiver<Vec<QuestionResult>>>,
    /// Debug guide recordings: fixed scroll position and caption.
    guide_scroll: Option<f32>,
    guide_caption: Option<String>,
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
            firewall_ok: None,
            live_started: None,
            confirm_remove_all: false,
            confirm_reset_results: false,
            regrade_jobs: HashMap::new(),
            guide_scroll: None,
            guide_caption: None,
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
        self.tick_regrades();
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

impl crate::ui::Root for Studio {
    fn root(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
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
        egui::Panel::left("nav").exact_size(232.0).resizable(false).frame(egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).inner_margin(Margin::symmetric(16, 18))).show_inside(ui, |ui| self.nav(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(28, 22))).show_inside(ui, |ui| match self.page {
            Page::Home => self.home(ui),
            Page::Build => self.build(ui),
            Page::Settings => self.settings(ui),
            Page::Live => self.live_page(ui),
            Page::Results => self.results(ui),
        });
        self.draw_toasts(ctx);
        if let Some(c) = &self.guide_caption {
            let p = pal();
            egui::Area::new(egui::Id::new("guide-caption")).anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::new().fill(p.accent).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(18, 10)).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(c).font(theme::semibold(17.0)).color(p.on_accent)).extend());
                });
            });
        }
    }

    fn exit(&mut self) {
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
        painter.rect_filled(rect, CornerRadius::same(theme::R_CONTROL), p.hover);
    }
    let col = if active || resp.hovered() { p.text } else { p.muted };
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
            if ui.add(egui::Button::new(theme::muted(format!("{}  {} theme", if light { icon::MOON } else { icon::SUN }, if light { "Dark" } else { "Light" }))).frame_when_inactive(false)).clicked() {
                theme::set_light(ui.ctx(), !light);
            }
            ui.add_space(8.0);
            ui.separator();
            let state = if self.path.is_none() { "Not saved yet" } else if self.dirty { "Unsaved changes" } else { "All changes saved" };
            theme::status_dot(ui, if self.dirty || self.path.is_none() { p.warning } else { p.success }, state);
            ui.add(egui::Label::new(RichText::new(rtl::line(&self.project.exam.title)).font(theme::semibold(14.0))).truncate());
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
                if ui.add(egui::Button::new(theme::muted(format!("{}  Save", icon::SAVE))).frame_when_inactive(false)).on_hover_text("Ctrl+S").clicked() {
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
                        if ui.add(egui::Button::new(RichText::new(format!("{}  New exam", icon::ADD)).font(theme::semibold(14.5)).color(p.on_accent)).fill(theme::accent_fill(ui)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(0.0, 38.0))).clicked() {
                            self.new_project();
                        }
                        if ui.add(egui::Button::new(RichText::new(format!("{}  Open project", icon::OPEN)).font(theme::semibold(14.5))).fill(theme::clear_fill(ui)).stroke(theme::outline(ui, p.border_strong)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(0.0, 38.0))).clicked() {
                            self.open_dialog();
                        }
                        if ui.add(egui::Button::new(RichText::new("Load the sample exam").color(p.accent)).frame_when_inactive(false)).on_hover_text("Six questions covering every type, including two code questions").clicked() {
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
                            painter.rect_filled(rect, CornerRadius::same(theme::R_CONTROL), p.hover);
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
                ui.horizontal(|ui| {
                    let id = self.project.exam.next_question_id();
                    let mut add = None;
                    let p = pal();
                    let menu_btn = egui::Button::new(RichText::new(format!("{}  Add question", icon::ADD)).font(theme::semibold(14.0)).color(p.on_accent)).fill(theme::accent_fill(ui)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(list_w, 36.0));
                    egui::containers::menu::MenuButton::from_button(menu_btn).ui(ui, |ui| {
                        ui.set_min_width(list_w - 8.0);
                        for (label, hint, kind) in [
                            ("Multiple choice", "One or several correct options", 0),
                            ("True / False", "A statement to judge", 1),
                            ("Short answer", "A word, number or flag — TryHackMe style", 2),
                            ("Code", "Python, JavaScript, SQL, HTML/CSS/JS, C, C++, Java, Go, Rust", 3),
                            ("File upload", "Students attach a file you grade. Needs lockdown off", 4),
                        ] {
                            let r = ui.add(egui::Button::new(RichText::new(label).font(theme::semibold(14.0))).frame_when_inactive(false).min_size(egui::vec2(ui.available_width(), 24.0))).on_hover_text(hint);
                            ui.label(theme::muted(hint).size(12.0));
                            ui.add_space(4.0);
                            if r.clicked() {
                                add = Some(match kind {
                                    0 => Kind::Choice { options: vec![String::new(), String::new(), String::new(), String::new()], correct: vec![0], multiple: false },
                                    1 => Kind::TrueFalse { answer: true },
                                    2 => Kind::Short { accepted: vec![String::new()], case_sensitive: false, show_format: true },
                                    4 => Kind::Upload { extensions: vec!["pdf".into(), "docx".into(), "zip".into()], max_mb: 2.0 },
                                    _ => Kind::Code(CodeSpec::new(Lang::Python)),
                                });
                            }
                        }
                    });
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
                                let first_line = rtl::line(q.prompt.lines().next().unwrap_or(""));
                                let first = first_line.as_str();
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
                        let card = fr.response.interact(egui::Sense::click());
                        if card.hovered() && !sel {
                            ui.painter().rect_stroke(card.rect, CornerRadius::same(theme::R_PANEL), Stroke::new(1.0_f32, p.accent.gamma_multiply(0.7)), egui::StrokeKind::Inside);
                        }
                        if card.clicked() {
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
                let mut scroll = egui::ScrollArea::vertical().id_salt("qedit").auto_shrink([false, false]);
                if let Some(off) = self.guide_scroll {
                    scroll = scroll.vertical_scroll_offset(off);
                }
                scroll.show(ui, |ui| match self.selected.filter(|i| *i < n) {
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
        let locked = self.project.exam.settings.lockdown;
        let mut unlock = false;
        let mut ref_run = false;
        // Guide recordings show the solution section open.
        let force_solution_open = self.guide_scroll.is_some() && matches!(&q.kind, Kind::Code(c) if !c.reference.is_empty());
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
            ui.add(TextField::multiline(&mut q.prompt).hint_text("Write the question…").desired_rows(4).desired_width(theme::fill(&ui, 12)).margin(Margin::same(12)));
            ui.add_space(8.0);
            match &mut q.kind {
                Kind::Choice { options, correct, multiple } => {
                    theme::field_label(ui, "Options — tick the correct answer(s)");
                    if ui.checkbox(multiple, "Allow several correct answers").changed() && !*multiple && correct.len() > 1 {
                        correct.truncate(1);
                    }
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
                            ui.add(TextField::singleline(opt).hint_text(format!("Option {letter}")).desired_width(field_w).margin(Margin::symmetric(10, 7)));
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
                Kind::Upload { extensions, max_mb } => {
                    if locked {
                        egui::Frame::new().fill(p.warning.gamma_multiply(0.10)).stroke(Stroke::new(1.0_f32, p.warning.gamma_multiply(0.6))).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(egui::Label::new(RichText::new(format!("{}  File uploads need lockdown off: a locked exam never opens a file window. This exam can't be exported until lockdown is off.", icon::WARNING)).color(p.warning)).wrap());
                            if theme::ghost(ui, "Turn lockdown off").clicked() {
                                unlock = true;
                            }
                        });
                    }
                    theme::field_label(ui, "Allowed file types (comma separated, empty = any)");
                    let mut list = extensions.join(", ");
                    if ui.add(egui::TextEdit::singleline(&mut list).hint_text("pdf, docx, zip").desired_width(theme::fill(&ui, 10)).margin(Margin::symmetric(10, 7))).changed() {
                        *extensions = list.split([',', ' ', ';']).map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
                    }
                    theme::field_label(ui, "Largest file");
                    ui.add(egui::DragValue::new(max_mb).range(0.1..=MAX_UPLOAD_MB).speed(0.1).suffix(" MB"));
                    ui.label(theme::muted(format!("Up to {} MB per student in total, so it travels through the classroom network with the submission. Grade it in Results.", theme::fmt_points(MAX_UPLOAD_MB))).size(12.5));
                }
                Kind::TrueFalse { answer } => {
                    theme::field_label(ui, "Correct answer");
                    ui.horizontal(|ui| {
                        ui.radio_value(answer, true, "True");
                        ui.radio_value(answer, false, "False");
                    });
                }
                Kind::Short { accepted, case_sensitive, show_format } => {
                    theme::field_label(ui, "Accepted answers (extra spaces are ignored)");
                    let mut remove = None;
                    let field_w = ui.available_width() - 48.0;
                    for (ai, a) in accepted.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(TextField::singleline(a).hint_text("Accepted answer").desired_width(field_w).margin(Margin::symmetric(10, 7)));
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
                        ui.checkbox(show_format, "Show answer format").on_hover_text("TryHackMe style: each letter or digit shows as *, symbols like _ { } ! @ # appear as they are");
                    });
                    if *show_format {
                        let first = accepted.iter().find(|a| !a.trim().is_empty()).cloned().unwrap_or_default();
                        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(theme::eyebrow("Students see"));
                            ui.label(theme::figure(if first.is_empty() { "(type an accepted answer)".to_string() } else { answer_format(&first) }, 15.0, p.text));
                        });
                    }
                }
                Kind::Code(spec) => {
                    // 1 · Language
                    ui.label(theme::eyebrow("1 · Language"));
                    ui.horizontal(|ui| {
                        let old = spec.lang;
                        egui::ComboBox::from_id_salt(("lang", q.id)).width(200.0).selected_text(spec.lang.name()).show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                ui.selectable_value(&mut spec.lang, l, l.name());
                            }
                        });
                        if spec.lang != old {
                            if spec.starter.trim().is_empty() || spec.starter == old.template() {
                                spec.starter = spec.lang.template().into();
                            }
                            if spec.lang == Lang::Sql && spec.setup.trim().is_empty() {
                                spec.setup = Lang::sql_setup_template().into();
                            }
                            if spec.lang.is_preview_only() {
                                spec.tests.clear();
                            }
                        }
                        match runner::tool(spec.lang) {
                            Some(t) => theme::pill(ui, &t.label, p.success),
                            None => theme::pill(ui, "Students' PCs need this compiler", p.warning),
                        };
                    });
                    ui.add(egui::Label::new(theme::muted(lang_tip(spec.lang)).size(13.0)).wrap());
                    crate::ui::guides::show(ui, spec.lang);
                    ui.add_space(10.0);

                    // 2 · What students start with
                    ui.label(theme::eyebrow("2 · What students start with"));
                    ui.allocate_ui(egui::vec2(ui.available_width(), 170.0), |ui| {
                        ui.set_min_height(160.0);
                        editor::code_editor(ui, ("starter", q.id), &mut spec.starter, spec.lang, self.font, false);
                    });
                    if spec.lang == Lang::Sql {
                        ui.add_space(6.0);
                        ui.label(theme::eyebrow("Database (runs before every query — students can read it)"));
                        ui.allocate_ui(egui::vec2(ui.available_width(), 150.0), |ui| {
                            ui.set_min_height(140.0);
                            editor::code_editor(ui, ("setup", q.id), &mut spec.setup, Lang::Sql, self.font, false);
                        });
                    }
                    ui.add_space(10.0);

                    // 3 · How it's checked
                    ui.label(theme::eyebrow("3 · How it's checked"));
                    if spec.lang.is_preview_only() {
                        ui.add(egui::Label::new(theme::muted("Students see their page live while they work. You grade it in Results, where you can open each student's page.").size(13.0)).wrap());
                    } else {
                        #[derive(PartialEq, Clone, Copy)]
                        enum Mode {
                            Output,
                            Tests,
                            Manual,
                        }
                        let mode = if spec.tests.is_empty() { Mode::Manual } else if spec.simple { Mode::Output } else { Mode::Tests };
                        let mut new_mode = mode;
                        ui.horizontal_wrapped(|ui| {
                            ui.radio_value(&mut new_mode, Mode::Output, "Compare the printed output");
                            ui.radio_value(&mut new_mode, Mode::Tests, "Several test cases");
                            ui.radio_value(&mut new_mode, Mode::Manual, "I'll grade it myself");
                        });
                        if new_mode != mode {
                            match new_mode {
                                Mode::Manual => spec.tests.clear(),
                                Mode::Output => {
                                    spec.simple = true;
                                    if spec.tests.is_empty() {
                                        spec.tests.push(TestCase { input: String::new(), expected: String::new(), hidden: false });
                                    }
                                    spec.tests.truncate(1);
                                    spec.tests[0].hidden = false;
                                }
                                Mode::Tests => {
                                    spec.simple = false;
                                    if spec.tests.is_empty() {
                                        spec.tests.push(TestCase { input: String::new(), expected: String::new(), hidden: false });
                                    }
                                }
                            }
                        }
                        match new_mode {
                            Mode::Manual => {
                                ui.label(theme::muted("No automatic score. Every answer shows up in Results for you to read, run and mark.").size(13.0));
                            }
                            Mode::Output => {
                                let t = &mut spec.tests[0];
                                ui.label(theme::muted("Full marks when the program prints exactly this (spacing at line ends is ignored).").size(13.0));
                                ui.add(egui::TextEdit::multiline(&mut t.expected).hint_text("e.g. 55").font(theme::mono(13.5)).desired_rows(3).desired_width(theme::fill(&ui, 4)));
                                if spec.lang != Lang::Sql {
                                    ui.horizontal(|ui| {
                                        ui.label(theme::muted("Input typed into the program (optional)").size(12.5));
                                        ui.add(egui::TextEdit::singleline(&mut t.input).hint_text("e.g. 10").font(theme::mono(13.0)).desired_width(ui.available_width()));
                                    });
                                }
                            }
                            Mode::Tests => {
                                ui.label(theme::muted("Each test gives the program an input and checks what it prints. Score = tests passed ÷ tests × points. Hidden tests run only at submission, so answers can't be hard-coded.").size(13.0));
                                let mut remove = None;
                                for (ti, t) in spec.tests.iter_mut().enumerate() {
                                    egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(10)).show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(theme::figure(format!("TEST {}", ti + 1), 12.0, p.text));
                                            ui.checkbox(&mut t.hidden, "Hidden");
                                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                if theme::icon_button(ui, icon::DELETE, "Remove test").clicked() {
                                                    remove = Some(ti);
                                                }
                                            });
                                        });
                                        ui.columns(2, |cols| {
                                            cols[0].label(theme::eyebrow(if spec.lang == Lang::Sql { "Extra SQL for this test" } else { "Input" }));
                                            cols[0].add(egui::TextEdit::multiline(&mut t.input).font(theme::mono(13.0)).desired_rows(2).desired_width(theme::fill(&cols[0], 4)));
                                            cols[1].label(theme::eyebrow("Expected output"));
                                            cols[1].add(egui::TextEdit::multiline(&mut t.expected).font(theme::mono(13.0)).desired_rows(2).desired_width(theme::fill(&cols[1], 4)));
                                        });
                                    });
                                }
                                if let Some(r) = remove {
                                    spec.tests.remove(r);
                                }
                                if ui.button(format!("{}  Add test", icon::ADD)).clicked() {
                                    spec.tests.push(TestCase { input: String::new(), expected: String::new(), hidden: false });
                                }
                            }
                        }

                        if new_mode != Mode::Manual && spec.tests.iter().all(|t| t.expected.trim().is_empty()) {
                            ui.add(egui::Label::new(RichText::new(format!("{}  No expected output yet, so you'll grade this one yourself: students see \"waiting for your teacher\" until you send their score.", icon::WARNING)).color(p.warning).size(13.0)).wrap());
                        }

                        // Optional: the teacher's own solution fills the expected outputs.
                        if new_mode != Mode::Manual {
                            ui.add_space(6.0);
                            egui::CollapsingHeader::new(RichText::new("Your solution (optional) — fill the expected output automatically").font(theme::semibold(13.5)))
                                .id_salt(("solution", q.id))
                                .default_open(!spec.reference.trim().is_empty())
                                .open(force_solution_open.then_some(true))
                                .show(ui, |ui| {
                                    ui.label(theme::muted("Never shipped to students. Write it, press the button, and HDJLabs runs it to fill in what students must print.").size(12.5));
                                    ui.allocate_ui(egui::vec2(ui.available_width(), 160.0), |ui| {
                                        ui.set_min_height(150.0);
                                        editor::code_editor(ui, ("ref", q.id), &mut spec.reference, spec.lang, self.font, false);
                                    });
                                    ui.horizontal(|ui| {
                                        let busy = self.ref_job.is_some();
                                        if ui.add_enabled(!busy && !spec.reference.trim().is_empty() && runner::tool(spec.lang).is_some(), egui::Button::new(format!("{}  Run my solution and fill expected output", icon::PLAY))).clicked() {
                                            ref_run = true;
                                        }
                                        if busy {
                                            ui.add(egui::Spinner::new());
                                        }
                                    });
                                });
                        }
                    }
                }
            }
        });
        if ref_run {
            if let Kind::Code(spec) = &q.kind {
                let (tx, rx) = mpsc::channel();
                let spec = spec.clone();
                std::thread::spawn(move || {
                    let outs = spec.tests.iter().map(|t| runner::run(spec.lang, &spec.reference, &spec.stdin_for(&t.input), runner::RUN_TIMEOUT)).collect();
                    let _ = tx.send(outs);
                });
                self.ref_job = Some((q.id, rx));
            }
        }
        if q != before {
            self.exam().questions[i] = q;
        }
        if unlock {
            self.exam().settings.lockdown = false;
            self.toast("Lockdown is off for this exam. Students can use other apps while it's open.", false);
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
                    ui.add(TextField::multiline(&mut e.instructions).desired_rows(3).desired_width(theme::fill(&ui, 10)).margin(Margin::same(10)));
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
                    ui.checkbox(&mut e.settings.keep_answers_off_pcs, "Keep the answer key off student PCs").on_hover_text("The exported exam carries no correct answers or hidden tests. Your Studio grades each submission and sends the score back over the classroom network, so keep the Live classroom running.");
                    ui.checkbox(&mut e.settings.hold_scores, "Students wait for me to finish grading before they see a score").on_hover_text("Scores always wait when an answer needs your grading (code without tests, HTML). Turn this on to hold every score until you press \"Finish grading & send score\" in Results.");
                });
                theme::card(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::heading("Access", 18.0));
                    theme::field_label(ui, "Exam password — tell students when the exam starts");
                    ui.add(egui::TextEdit::singleline(&mut e.settings.exam_password).hint_text("Leave empty for no password").desired_width(theme::fill(&ui, 12)).margin(Margin::symmetric(12, 9)));
                    ui.label(theme::muted("The questions are encrypted with this password, so nobody can read them before you announce it.").size(12.5));
                    theme::field_label(ui, "Proctor exit password — only for you");
                    ui.add(egui::TextEdit::singleline(&mut e.settings.exit_password).hint_text("Unlocks a student's PC without submitting").desired_width(theme::fill(&ui, 12)).margin(Margin::symmetric(12, 9)));
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
                    ui.checkbox(&mut e.settings.remove_after_submit, "Delete the exam file from each student's PC after they submit");
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
        let export = self.project.last_export.clone().filter(|p| p.exists());
        if self.firewall_ok.is_none() {
            self.firewall_ok = Some(crate::ui::firewall_allows_self());
        }
        let mut start = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(theme::heading("Live classroom", 24.0));
                ui.label(theme::muted("Students on the same Wi-Fi, switch or hotspot join automatically. No internet needed."));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.live.is_some() {
                    if theme::danger(ui, &format!("{}  Stop session", icon::STOP)).clicked() {
                        self.live = None;
                    }
                } else if theme::primary(ui, &format!("{}  Start session", icon::PLAY)).clicked() {
                    start = true;
                }
            });
        });
        ui.add_space(12.0);
        if start {
            if self.firewall_ok != Some(true) {
                // One-time Windows prompt so students' PCs can reach this one.
                crate::ui::allow_firewall();
                self.toast("Windows will ask once for permission so students' PCs can reach this one.", false);
                self.firewall_ok = None; // re-check next frame
            }
            let share = export.clone().map(|p| (p, self.project.exam.title.clone()));
            let ctx = ui.ctx().clone();
            let server = LiveServer::start(self.project.exam.id.clone(), self.project.teacher_secret.clone(), self.project.exam.author.clone(), self.project.exam.settings.lan_port, share, move || ctx.request_repaint());
            for e in &server.errors {
                self.toasts.push(Toast { text: e.clone(), at: Instant::now(), error: true });
            }
            {
                // Scores sent in an earlier session reach students whose exam reconnects now.
                let mut st = server.state.lock().unwrap();
                for r in self.project.results.iter().filter(|r| r.released) {
                    st.release_grade(&r.submission.attempt_id, r.final_results(), r.total());
                }
            }
            self.live = Some(server);
            self.live_started = Some(Instant::now());
        }

        let ips = net::local_ipv4_named();
        let Some(live) = &self.live else {
            // Readiness checklist, evaluated live.
            theme::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::eyebrow("Ready to start"));
                ui.add_space(4.0);
                let check = |ui: &mut egui::Ui, ok: bool, title: &str, detail: &str| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if ok { icon::CHECK } else { icon::WARNING }).size(15.0).color(if ok { p.success } else { p.warning }));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(title).font(theme::semibold(14.0)));
                            ui.add(egui::Label::new(theme::muted(detail).size(12.5)).wrap());
                        });
                    });
                    ui.add_space(4.0);
                };
                if ips.is_empty() {
                    check(ui, false, "No network", "Connect this PC to the classroom Wi-Fi, switch or hotspot. Internet isn't needed.");
                } else {
                    let list = ips.iter().map(|(n, ip)| format!("{n}: {ip}")).collect::<Vec<_>>().join("   ·   ");
                    check(ui, true, "Network connected", &list);
                }
                match &export {
                    Some(path) => {
                        let mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
                        check(ui, true, "Exam file ready to share", &format!("{} · {mb:.0} MB", path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()));
                    }
                    None => {
                        check(ui, false, "Exam not exported yet", "Students download the exported .exe from this PC. Export it first in Settings & export.");
                        if ui.button(format!("{}  Export now", icon::EXPORT)).clicked() {
                            self.export();
                        }
                    }
                }
                match self.firewall_ok {
                    Some(true) => check(ui, true, "Firewall allows students in", "HDJLabs is allowed through Windows Firewall."),
                    _ => check(ui, false, "Firewall permission needed", "When you press Start session, Windows asks once for permission. Accept it so students' PCs can connect."),
                }
                ui.add_space(4.0);
                ui.label(theme::muted("Students then open the address shown here in any browser, run the exam, and appear in the list below.").size(12.5));
            });
            return;
        };

        // Session card: where students go, and what's happening on the network right now.
        let http = live.http_url.clone();
        let port = live.tcp_port;
        let (waiting, downloads) = {
            let st = live.state.lock().unwrap();
            (st.waiting_pcs(), st.downloads.clone())
        };
        let since = self.live_started.map(|t| t.elapsed().as_secs() as i64).unwrap_or(0);
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Spinner::new().size(12.0).color(p.success));
                ui.label(RichText::new("Session running").font(theme::semibold(15.0)).color(p.success));
                ui.label(theme::figure(theme::fmt_secs(since), 12.5, p.muted));
                ui.separator();
                match &http {
                    Some(u) => {
                        ui.label(theme::muted("Students download from"));
                        ui.label(RichText::new(u).font(theme::mono(15.0)).color(p.accent));
                        if theme::icon_button(ui, icon::COPY, "Copy link").clicked() {
                            ui.ctx().copy_text(u.clone());
                            self.toasts.push(Toast { text: "Link copied".into(), at: Instant::now(), error: false });
                        }
                    }
                    None => {
                        ui.label(RichText::new("Download page off — export the exam, then restart the session.").color(p.warning));
                    }
                }
            });
            ui.label(theme::muted(format!(
                "This PC: {}   ·   exam port {port}",
                ips.iter().map(|(n, ip)| format!("{ip} ({n})")).collect::<Vec<_>>().join(", ")
            ))
            .size(12.5));
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::eyebrow("Exam open, not signed in"));
                if waiting.is_empty() {
                    ui.label(theme::muted("none").size(12.5));
                } else {
                    ui.label(theme::figure(waiting.len().to_string(), 13.0, p.warning)).on_hover_text(waiting.join("\n"));
                    ui.label(theme::muted(waiting.iter().take(6).cloned().collect::<Vec<_>>().join(", ")).size(12.5));
                }
            });
            let active: Vec<&net::Download> = downloads.iter().filter(|d| d.finished.is_none()).collect();
            let done = downloads.iter().filter(|d| d.finished.is_some() && !d.failed).count();
            let failed = downloads.iter().filter(|d| d.failed).count();
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::eyebrow("Downloads"));
                ui.label(theme::muted(format!("{} in progress · {done} complete{}", active.len(), if failed > 0 { format!(" · {failed} interrupted") } else { String::new() })).size(12.5));
            });
            for d in active.iter().take(8) {
                ui.horizontal(|ui| {
                    ui.label(theme::figure(&d.ip, 12.5, p.text));
                    let frac = if d.total > 0 { d.sent as f32 / d.total as f32 } else { 0.0 };
                    ui.allocate_ui(egui::vec2(180.0, 10.0), |ui| theme::progress_bar(ui, frac, 6.0, p.accent));
                    let secs = d.started.elapsed().as_secs_f64().max(0.1);
                    ui.label(theme::muted(format!("{:.0}% · {:.1} of {:.0} MB · {:.1} MB/s", frac * 100.0, d.sent as f64 / 1_048_576.0, d.total as f64 / 1_048_576.0, d.sent as f64 / 1_048_576.0 / secs)).size(12.0));
                });
            }
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
            stat(ui, "Signed in", &students.len().to_string(), p.muted);
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new(format!("{}  Remove exam from PCs", icon::DELETE)).color(p.danger)).fill(theme::clear_fill(ui)).min_size(egui::vec2(0.0, 34.0))).on_hover_text("Deletes the exam file from every connected student PC when its exam closes. Students still working are not interrupted.").clicked() {
                    self.confirm_remove_all = true;
                }
                if theme::ghost(ui, &format!("{}  +5 min all", icon::CLOCK)).clicked() {
                    send.push((None, S2C::AddTime { secs: 300 }));
                }
                if ui.add_enabled(!self.broadcast.trim().is_empty(), egui::Button::new(format!("{}  Send to all", icon::SEND))).clicked() {
                    send.push((None, S2C::Message { text: self.broadcast.trim().to_string() }));
                    self.broadcast.clear();
                }
                theme::text_field(ui, &mut self.broadcast, "Message to all students…", f32::INFINITY);
            });
        });
        ui.add_space(10.0);

        let total_q = self.project.exam.delivered_count();
        ui.horizontal_top(|ui| {
            let table_w = if self.live_selected.is_some() { ui.available_width() * 0.6 } else { ui.available_width() };
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
                    egui::ScrollArea::both().id_salt("roster").auto_shrink([false, false]).show(ui, |ui| {
                        egui::Grid::new("roster-grid").num_columns(6).striped(true).spacing([12.0, 10.0]).min_col_width(40.0).show(ui, |ui| {
                            for h in ["Student", "Status", "Progress", "Time left", "Alerts", "Score"] {
                                ui.label(theme::eyebrow(h));
                            }
                            ui.end_row();
                            for s in &students {
                                let sel = self.live_selected.as_deref() == Some(s.attempt_id.as_str());
                                let name = ui.add(egui::Button::new(RichText::new(&s.name).font(theme::semibold(14.0)).color(if sel { p.accent } else { p.text })).frame_when_inactive(false)).on_hover_text(format!("ID {} · {} · {}", s.student_id, s.machine, s.addr));
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
                                    ui.set_width(104.0);
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
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(egui::Label::new(theme::heading(&s.name, 19.0)).truncate());
                                    ui.add(egui::Label::new(theme::figure(format!("{} · {} · {}", s.student_id, s.machine, s.addr), 11.5, p.muted)).truncate());
                                });
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    if theme::icon_button(ui, icon::CANCEL, "Close").clicked() {
                                        self.live_selected = None;
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                theme::text_field(ui, &mut self.direct_msg, "Message to this student…", (ui.available_width() - 230.0).max(120.0));
                                if ui.add_enabled(!self.direct_msg.trim().is_empty(), egui::Button::new(icon::SEND)).clicked() {
                                    send.push((Some(sel.clone()), S2C::Message { text: self.direct_msg.trim().to_string() }));
                                    self.direct_msg.clear();
                                }
                                if ui.button("+5 min").clicked() {
                                    send.push((Some(sel.clone()), S2C::AddTime { secs: 300 }));
                                }
                                if ui.add(egui::Button::new(RichText::new(icon::DELETE).color(p.danger))).on_hover_text("Remove the exam file from this student's PC when the exam closes").clicked() {
                                    send.push((Some(sel.clone()), S2C::RemoveExam));
                                    self.toasts.push(Toast { text: format!("{}'s PC will delete the exam file when it closes.", s.name), at: Instant::now(), error: false });
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

        if self.confirm_remove_all {
            let mut close = false;
            egui::Modal::new(egui::Id::new("confirm-remove")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ui.ctx(), |ui| {
                ui.set_width(440.0);
                ui.label(theme::heading("Remove the exam from students' PCs?", 20.0));
                ui.add(egui::Label::new(theme::muted("Every connected PC deletes the exam file when its exam closes. Students still working finish normally; students who have submitted see a 10-second countdown. Their encrypted result backup stays in Documents.")).wrap());
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::ghost(ui, "Cancel").clicked() {
                        close = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::danger(ui, &format!("{}  Remove from all PCs", icon::DELETE)).clicked() {
                            send.push((None, S2C::RemoveExam));
                            self.toasts.push(Toast { text: "Connected PCs will delete the exam file when it closes.".into(), at: Instant::now(), error: false });
                            close = true;
                        }
                    });
                });
            });
            if close {
                self.confirm_remove_all = false;
            }
        }
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
    /// The teacher's grading controls for one code answer: quick marks plus exact points.
    fn grade_row(&mut self, ui: &mut egui::Ui, ri: usize, q: &Question) {
        let p = pal();
        let r = &mut self.project.results[ri];
        let auto = r.auto(q.id).cloned();
        let manual = r.overrides.get(&q.id).copied();
        let needs = manual.is_none() && auto.as_ref().is_some_and(|a| a.status == Status::Review);
        let mut changed = false;
        ui.add_space(6.0);
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, if needs { p.warning.gamma_multiply(0.7) } else { p.border })).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::eyebrow("Grade"));
                match (manual, &auto) {
                    (Some(v), _) => theme::pill(ui, &format!("Graded {} / {}", theme::fmt_points(v), theme::fmt_points(q.points)), p.accent),
                    (None, Some(a)) if a.status == Status::Review => theme::pill(ui, "Needs grading", p.warning),
                    (None, Some(a)) => theme::pill(ui, &format!("Auto {} / {}{}", theme::fmt_points(a.earned), theme::fmt_points(q.points), if a.tests_total > 0 { format!("  ({}/{} tests)", a.tests_passed, a.tests_total) } else { String::new() }), p.muted),
                    (None, None) => theme::pill(ui, "Not answered", p.muted),
                };
                ui.add_space(6.0);
                let mut set = None;
                if theme::ghost(ui, &format!("{}  Full marks", icon::CHECK)).clicked() {
                    set = Some(q.points);
                }
                if theme::ghost(ui, "Half").clicked() {
                    set = Some((q.points * 50.0).round() / 100.0);
                }
                if theme::ghost(ui, "Zero").clicked() {
                    set = Some(0.0);
                }
                let mut v = manual.or(auto.as_ref().map(|a| a.earned)).unwrap_or(0.0);
                if ui.add(egui::DragValue::new(&mut v).range(0.0..=q.points).speed(0.25).suffix(format!(" / {} pts", theme::fmt_points(q.points)))).on_hover_text("Exact points").changed() {
                    set = Some(v);
                }
                if manual.is_some() && ui.add(egui::Button::new(theme::muted("Undo")).frame_when_inactive(false)).on_hover_text("Go back to the automatic result").clicked() {
                    r.overrides.remove(&q.id);
                    changed = true;
                }
                if let Some(v) = set {
                    r.overrides.insert(q.id, v.clamp(0.0, q.points));
                    changed = true;
                }
            });
        });
        if changed {
            r.released = false;
            self.dirty = true;
        }
    }

    fn answer_view(&mut self, ui: &mut egui::Ui, attempt: &str, idx: usize, q: &Question, a: &Answer, result_idx: Option<usize>) {
        let p = pal();
        let auto = if matches!(q.kind, Kind::Code(_)) { None } else { Some(grading::grade_static(q, a)) };
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Q{}", idx + 1)).font(theme::semibold(14.0)).color(p.accent));
                let prompt_w = (ui.available_width() - 150.0).max(60.0);
                ui.allocate_ui_with_layout(egui::vec2(prompt_w, 22.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(rtl::line(q.prompt.lines().next().unwrap_or(""))).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(ri) = result_idx {
                        let r = &mut self.project.results[ri];
                        let auto_pts = r.auto(q.id).map(|x| x.earned).unwrap_or(0.0);
                        let mut v = r.earned(q.id);
                        let changed = ui.add(egui::DragValue::new(&mut v).range(0.0..=q.points).speed(0.25).suffix(format!(" / {}", theme::fmt_points(q.points)))).on_hover_text("Override the points for this answer").changed();
                        if changed {
                            if (v - auto_pts).abs() < 0.001 && r.auto(q.id).is_some_and(|x| x.status != Status::Review) {
                                r.overrides.remove(&q.id);
                            } else {
                                r.overrides.insert(q.id, v);
                            }
                            r.released = false;
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
                    rtl::label(ui, t, theme::mono(13.5), p.text);
                }
                (_, Answer::File { name, size, data }) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::DOC).size(18.0).color(p.accent));
                        ui.label(RichText::new(name).font(theme::semibold(14.0)));
                        ui.label(theme::muted(format!("{:.1} KB", *size as f32 / 1024.0)).size(12.5));
                        if data.is_empty() {
                            ui.label(theme::muted("(arrives with the submission)").size(12.5));
                        } else if theme::ghost(ui, &format!("{}  Save file", icon::DOWNLOAD)).clicked() {
                            let suggested = net::sanitize_file_name(name);
                            if let Some(out) = rfd::FileDialog::new().set_file_name(suggested).save_file() {
                                match crate::crypto::unb64(data).map(|b| std::fs::write(&out, b)) {
                                    Ok(Ok(())) => self.toast(format!("Saved {}", out.display()), false),
                                    Ok(Err(e)) => self.toast(e.to_string(), true),
                                    Err(_) => self.toast("The file is damaged.", true),
                                }
                            }
                        }
                    });
                    if let Some(ri) = result_idx {
                        self.grade_row(ui, ri, q);
                    }
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
                    if let Some(ri) = result_idx {
                        self.grade_row(ui, ri, q);
                    }
                }
                _ => {
                    ui.label(theme::muted("(answer type does not match the question)"));
                }
            }
        });
        ui.add_space(6.0);
    }

    /// Re-checks every new submission with the full exam (a few at a time), then sends the score
    /// on its own when nothing needs the teacher and the answers never left this PC.
    fn tick_regrades(&mut self) {
        let finished: Vec<(String, Vec<QuestionResult>)> = self.regrade_jobs.iter().filter_map(|(k, rx)| rx.try_recv().ok().map(|v| (k.clone(), v))).collect();
        for (attempt, results) in finished {
            self.regrade_jobs.remove(&attempt);
            let Some(ri) = self.project.results.iter().position(|r| r.submission.attempt_id == attempt) else { continue };
            self.project.results[ri].regraded = Some(results);
            self.dirty = true;
            let s = &self.project.exam.settings;
            let r = &self.project.results[ri];
            if s.keep_answers_off_pcs && !s.hold_scores && r.pending_review() == 0 && !r.released {
                if let Some(live) = &self.live {
                    live.state.lock().unwrap().release_grade(&attempt, r.final_results(), r.total());
                    self.project.results[ri].released = true;
                }
            }
        }
        while self.regrade_jobs.len() < 4 {
            let Some(r) = self.project.results.iter().find(|r| r.regraded.is_none() && !self.regrade_jobs.contains_key(&r.submission.attempt_id)) else { break };
            let (tx, rx) = mpsc::channel();
            let (exam, sub) = (self.project.exam.clone(), r.submission.clone());
            self.regrade_jobs.insert(sub.attempt_id.clone(), rx);
            std::thread::spawn(move || {
                let _ = tx.send(grading::regrade(&exam, &sub));
            });
        }
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
                if !self.project.results.is_empty() && theme::danger(ui, &format!("{}  Reset results", icon::DELETE)).on_hover_text("Delete every result in this project").clicked() {
                    self.confirm_reset_results = true;
                }
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
        self.reset_results_modal(ui.ctx());
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
                egui::ScrollArea::both().id_salt("results-list").auto_shrink([false, false]).show(ui, |ui| {
                    theme::card_frame().inner_margin(Margin::same(12)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::Grid::new("results-grid").num_columns(6).striped(true).spacing([16.0, 10.0]).show(ui, |ui| {
                            for h in ["Student", "Score", "%", "Grading", "Alerts", "Submitted"] {
                                ui.label(theme::eyebrow(h));
                            }
                            ui.end_row();
                            for (i, r) in self.project.results.iter().enumerate() {
                                let s = &r.submission;
                                let sel = self.result_selected == Some(i);
                                if ui.add(egui::Button::new(RichText::new(rtl::line(&s.student_name)).font(theme::semibold(14.0)).color(if sel { p.accent } else { p.text })).frame_when_inactive(false)).on_hover_text(format!("ID {} · {}", s.student_id, s.machine)).clicked() {
                                    self.result_selected = if sel { None } else { Some(i) };
                                }
                                ui.label(theme::figure(format!("{} / {}", theme::fmt_points(r.total()), theme::fmt_points(s.max_score)), 13.0, p.text));
                                let pct = r.percent();
                                ui.label(theme::figure(format!("{pct:.0}%"), 13.0, if pct >= 50.0 { p.success } else { p.danger }));
                                match r.pending_review() {
                                    _ if r.regraded.is_none() => theme::pill(ui, "Checking…", p.muted),
                                    _ if r.mismatch() && !self.project.exam.settings.keep_answers_off_pcs => theme::pill(ui, &format!("{}  Score changed", icon::WARNING), p.danger).on_hover_text("The score the student's PC reported doesn't match this PC's re-check. The re-check counts."),
                                    0 if r.released => theme::pill(ui, "Sent", p.success),
                                    0 => theme::pill(ui, "Done", p.muted),
                                    n => theme::pill(ui, &format!("{n} to grade"), p.warning),
                                };
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
                            let got: Vec<f32> = self.project.results.iter().filter(|r| r.auto(q.id).is_some()).map(|r| if q.points > 0.0 { r.earned(q.id) / q.points } else { 0.0 }).collect();
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
                            ui.label(theme::heading(&rtl::line(&s.student_name), 20.0));
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
                        if self.project.results[ri].mismatch() && !self.project.exam.settings.keep_answers_off_pcs {
                            ui.label(RichText::new(format!("{}  The score this student's PC reported didn't match the re-check on your PC. The re-check counts; look at the alerts.", icon::WARNING)).color(p.danger));
                        }
                        self.grading_bar(ui, ri);
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

    /// Grading status for one student and the button that sends them their final score.
    fn grading_bar(&mut self, ui: &mut egui::Ui, ri: usize) {
        let p = pal();
        let (pending, released, name, attempt) = {
            let r = &self.project.results[ri];
            (r.pending_review(), r.released, r.submission.student_name.clone(), r.submission.attempt_id.clone())
        };
        let mut send = false;
        let mut reset = false;
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if pending > 0 {
                    theme::pill(ui, &format!("{pending} answer(s) to grade"), p.warning);
                } else if released {
                    theme::pill(ui, &format!("{}  Score sent", icon::CHECK), p.success);
                } else {
                    theme::pill(ui, "Graded, not sent yet", p.accent);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new(theme::muted("Reset grading")).frame_when_inactive(false)).on_hover_text("Clear every manual grade for this student").clicked() {
                        reset = true;
                    }
                });
            });
            let label = if released { format!("{}  Send again", icon::SEND) } else { format!("{}  Finish grading & send score", icon::SEND) };
            let b = if pending == 0 {
                let fill = theme::accent_fill(ui);
                ui.add(egui::Button::new(RichText::new(label).font(theme::semibold(14.0)).color(p.on_accent)).fill(fill).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(ui.available_width(), 34.0)))
            } else {
                ui.add_enabled(false, egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 34.0)))
            };
            if b.on_hover_text(if pending == 0 { "The student's exam shows the final score" } else { "Grade every answer marked \"Needs grading\" first" }).clicked() {
                send = true;
            }
        });
        if reset {
            let r = &mut self.project.results[ri];
            r.overrides.clear();
            r.released = false;
            self.dirty = true;
        }
        if send {
            let (results, score) = {
                let r = &self.project.results[ri];
                (r.final_results(), r.total())
            };
            match &self.live {
                Some(live) => {
                    let mut st = live.state.lock().unwrap();
                    let online = st.students.iter().any(|s| s.attempt_id == attempt && s.connected);
                    st.release_grade(&attempt, results, score);
                    drop(st);
                    self.project.results[ri].released = true;
                    self.dirty = true;
                    self.toast(if online { format!("Score sent to {name}.") } else { format!("{name} isn't connected; the score is sent when their exam reconnects.") }, false);
                }
                None => self.toast("Start the Live classroom session to send scores to students.", true),
            }
        }
    }

    fn reset_results_modal(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset_results {
            return;
        }
        let n = self.project.results.len();
        let mut close = false;
        let mut reset = false;
        egui::Modal::new(egui::Id::new("confirm-reset-results")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(theme::heading("Reset all results?", 20.0));
            ui.add(egui::Label::new(theme::muted(format!("This deletes {n} result(s) and your grading from this project. Students' sealed .hdjr backups in Documents are not touched, so you can import them again."))).wrap());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::ghost(ui, "Cancel").clicked() {
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::danger(ui, &format!("{}  Reset results", icon::DELETE)).clicked() {
                        reset = true;
                        close = true;
                    }
                });
            });
        });
        if reset {
            self.project.results.clear();
            self.result_selected = None;
            self.review_results.clear();
            if let Some(live) = &self.live {
                live.state.lock().unwrap().clear_submissions();
            }
            self.dirty = true;
            self.toast(format!("Deleted {n} result(s)."), false);
        }
        if close {
            self.confirm_reset_results = false;
        }
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
    /// Records the "how to write a good question" guide for one language, step by step.
    fn guide_script(&mut self, lang: Lang) {
        let step = crate::ui::shot::frame() / 30;
        if step == 0 && self.project.exam.questions.is_empty() {
            self.project.exam.title = "Week 3 quiz".into();
            self.project.exam.questions.push(Question { id: 1, prompt: String::new(), points: 5.0, kind: Kind::Code(CodeSpec::new(Lang::Python)) });
            self.selected = Some(0);
            self.page = Page::Build;
        }
        let (prompt, starter, reference, input) = guide_content(lang);
        let q = &mut self.project.exam.questions[0];
        let Kind::Code(spec) = &mut q.kind else { return };
        let (caption, scroll) = match step {
            0 => ("1 · Add question → Code", 0.0),
            1 => {
                if spec.lang != lang {
                    *spec = CodeSpec::new(lang);
                }
                ("2 · Pick the language", 0.0)
            }
            2 => {
                q.prompt = prompt.into();
                ("3 · Say exactly what to print", 0.0)
            }
            3 => {
                spec.starter = starter.into();
                (if lang == Lang::Sql { "4 · Starter query + the database students can read" } else { "4 · Give students a starting point" }, 150.0)
            }
            4 if lang.is_preview_only() => ("5 · Students see their page live — you grade it in Results", 380.0),
            4 => {
                spec.reference = reference.into();
                if let Some(t) = spec.tests.first_mut() {
                    t.input = input.into();
                }
                ("5 · Write your own solution (never shipped)", 520.0)
            }
            5 if lang.is_preview_only() => ("6 · Press Preview to try it as a student", 0.0),
            5 => {
                if spec.tests.first().is_some_and(|t| t.expected.is_empty()) {
                    let out = runner::run(spec.lang, &spec.reference, &spec.stdin_for(input), runner::RUN_TIMEOUT);
                    if let Some(t) = spec.tests.first_mut() {
                        t.expected = grading::normalize_output(&out.stdout);
                    }
                }
                ("6 · Run it — the expected output fills itself in", 470.0)
            }
            _ => ("7 · Preview as a student, then Export .exe", 0.0),
        };
        self.guide_caption = Some(caption.into());
        self.guide_scroll = Some(scroll);
    }

    fn shot_script(&mut self, ctx: &egui::Context) {
        if let Some(lang) = std::env::var("HDJLABS_SHOT_GUIDE").ok().and_then(|e| Lang::ALL.into_iter().find(|l| l.ext() == e)) {
            self.guide_script(lang);
            return;
        }
        let page = std::env::var("HDJLABS_SHOT_PAGE").unwrap_or_default();
        if page.is_empty() || page == "home" || self.project.exam.title != "Untitled exam" {
            return;
        }
        self.project.exam = sample_exam();
        self.project.exam.author = "Mr. Hussein · CS101".into();
        if std::env::var_os("HDJLABS_DEV_UPLOAD").is_some() {
            let mut q = Question::new(99, Kind::Upload { extensions: vec!["pdf".into(), "docx".into(), "zip".into()], max_mb: 2.0 });
            q.prompt = "Upload your lab report (PDF or Word).".into();
            self.project.exam.questions.insert(0, q);
        }
        self.project.last_export = Some(std::env::temp_dir().join("demo.exe"));
        self.selected = std::env::var("HDJLABS_SHOT_Q").ok().and_then(|v| v.parse().ok()).or(Some(0));
        let e = self.project.exam.clone();
        let demo = |name: &str, sid: &str, correct: usize, v: usize| {
            let mut answers = std::collections::BTreeMap::new();
            answers.insert(1, Answer::Choice(vec![1]));
            answers.insert(2, Answer::Choice(vec![0, 2]));
            answers.insert(4, Answer::Text("def".into()));
            if let Some(w) = e.questions.iter().find(|q| matches!(&q.kind, Kind::Code(c) if c.lang.is_preview_only())) {
                answers.insert(w.id, Answer::Code { lang: Lang::Web, source: "<h1>My profile</h1>
<p>Hi, I'm learning HTML.</p>
".into() });
            }
            answers.insert(5, Answer::Code { lang: Lang::Python, source: "n = int(input())\ntotal = 0\nfor i in range(1, n + 1):\n    total += i\nprint(total)\n".into() });
            let results: Vec<QuestionResult> = e
                .questions
                .iter()
                .enumerate()
                .map(|(i, q)| match &q.kind {
                    // Manually graded work (HTML pages) waits for the teacher.
                    Kind::Code(c) if c.lang.is_preview_only() => QuestionResult { qid: q.id, earned: 0.0, max: q.points, status: Status::Review, tests_passed: 0, tests_total: 0 },
                    _ => QuestionResult { qid: q.id, earned: if i < correct { q.points } else { 0.0 }, max: q.points, status: if i < correct { Status::Correct } else { Status::Wrong }, tests_passed: 0, tests_total: 0 },
                })
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
                // Code first so the grading controls are on screen in captures.
                question_order: e.questions.iter().filter(|q| matches!(q.kind, Kind::Code(_))).chain(e.questions.iter().filter(|q| !matches!(q.kind, Kind::Code(_)))).map(|q| q.id).collect(),
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
            "live-empty" => self.page = Page::Live,
            "live-remove" => {
                self.page = Page::Live;
                self.confirm_remove_all = true;
                let c = ctx.clone();
                self.live = Some(LiveServer::start(e.id.clone(), self.project.teacher_secret.clone(), "T".into(), 47998, None, move || c.request_repaint()));
            }
            "toasts" => {
                self.page = Page::Settings;
                self.toast("Exported C:\\Users\\Teacher\\Documents\\HDJLabs\\Exams\\Intro to Programming — Quiz 1.exe", false);
                self.toast("Fix 2 issue(s) before exporting.", true);
            }
            "settings" => self.page = Page::Settings,
            "results" => {
                for s in subs {
                    self.project.upsert_result(s);
                }
                // Demo data: treat the made-up scores as already re-checked.
                for r in &mut self.project.results {
                    r.regraded = Some(r.submission.results.clone());
                }
                self.project.results[0].released = true;
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
/// Example content for the guide recordings: (prompt, starter, solution, input).
#[cfg(debug_assertions)]
fn guide_content(l: Lang) -> (&'static str, &'static str, &'static str, &'static str) {
    match l {
        Lang::Python => ("Read a number n and print the sum 1 + 2 + … + n.", "n = int(input())\n# print the sum of 1..n\n", "n = int(input())\nprint(n * (n + 1) // 2)\n", "10"),
        Lang::JavaScript => ("Read a word and print it backwards.", "const word = readline();\n// print word reversed\n", "const word = readline();\nconsole.log(word.split('').reverse().join(''));\n", "exam"),
        Lang::Sql => ("List the names of students with a grade above 80, highest grade first.", "SELECT \n", "SELECT name FROM students WHERE grade > 80 ORDER BY grade DESC;\n", ""),
        Lang::Web => ("Make a page with the heading \"My profile\" and a button that turns it green.", "<!doctype html>\n<html>\n<body>\n  <h1 id=\"t\">My profile</h1>\n  <button>Make it green</button>\n  <script>\n    // your code\n  </script>\n</body>\n</html>\n", "", ""),
        Lang::C => ("Read two integers and print their product.", "#include <stdio.h>\n\nint main(void) {\n    int a, b;\n    scanf(\"%d %d\", &a, &b);\n    // print a * b\n    return 0;\n}\n", "#include <stdio.h>\nint main(void){int a,b;scanf(\"%d %d\",&a,&b);printf(\"%d\\n\",a*b);return 0;}\n", "6 7"),
        Lang::Cpp => ("Read two integers and print the larger one.", "#include <iostream>\nusing namespace std;\n\nint main() {\n    int a, b;\n    cin >> a >> b;\n    // print the larger\n}\n", "#include <iostream>\nusing namespace std;\nint main(){int a,b;cin>>a>>b;cout<<(a>b?a:b)<<endl;}\n", "4 9"),
        Lang::Java => ("Read a name and print \"Hello, <name>!\".", "import java.util.Scanner;\n\npublic class Main {\n    public static void main(String[] args) {\n        Scanner in = new Scanner(System.in);\n        // read and greet\n    }\n}\n", "import java.util.Scanner;\npublic class Main { public static void main(String[] a) { Scanner in = new Scanner(System.in); System.out.println(\"Hello, \" + in.next() + \"!\"); } }\n", "Sara"),
        Lang::Go => ("Read n and print n squared.", "package main\n\nimport \"fmt\"\n\nfunc main() {\n    var n int\n    fmt.Scan(&n)\n    // print n*n\n}\n", "package main\nimport \"fmt\"\nfunc main(){var n int;fmt.Scan(&n);fmt.Println(n*n)}\n", "12"),
        Lang::Rust => ("Read a line and print it in upper case.", "use std::io::stdin;\n\nfn main() {\n    let mut s = String::new();\n    stdin().read_line(&mut s).unwrap();\n    // print s in upper case\n}\n", "use std::io::stdin;\nfn main(){let mut s=String::new();stdin().read_line(&mut s).unwrap();println!(\"{}\",s.trim().to_uppercase());}\n", "hdjlabs"),
    }
}

fn lang_tip(l: Lang) -> &'static str {
    match l {
        Lang::Python => "Students read input with input() and print the answer. Built into the exam, so it works on every PC.",
        Lang::JavaScript => "Students read lines with readline() and print with console.log(). Built into the exam, so it works on every PC.",
        Lang::Sql => "Put CREATE TABLE and INSERT statements in the database box. Students write queries; results print as a header row followed by `value | value` rows. Built in (SQLite).",
        Lang::Web => "Students write one HTML page — CSS in <style>, JavaScript in <script> — and see it live beside the editor.",
        Lang::C => "Needs gcc or clang on the students' PCs (MinGW / MSYS2). Students read input with scanf.",
        Lang::Cpp => "Needs g++ or clang++ on the students' PCs (MinGW / MSYS2). Students read input with cin.",
        Lang::Java => "Needs a JDK on the students' PCs. The class must be called Main. Students read input with Scanner.",
        Lang::Go => "Needs Go on the students' PCs. Students read input with fmt.Scan.",
        Lang::Rust => "Needs Rust (rustc) on the students' PCs. Students read input from std::io::stdin.",
    }
}

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
        Question { id: 4, prompt: "What keyword defines a function in Python?".into(), points: 1.0, kind: Kind::Short { accepted: vec!["def".into()], case_sensitive: true, show_format: true } },
        Question { id: 9, prompt: "Capture the flag: what is printed by the lab server? (copy it exactly)".into(), points: 2.0, kind: Kind::Short { accepted: vec!["HDJ{l0ck3d_d0wn!}".into()], case_sensitive: true, show_format: true } },
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
                setup: String::new(),
                simple: false,
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
                setup: String::new(),
                simple: false,
            }),
        },
        Question {
            id: 7,
            prompt: "List the names of students with a grade above 80, highest grade first.".into(),
            points: 3.0,
            kind: Kind::Code(CodeSpec {
                lang: Lang::Sql,
                starter: "-- Write your query below\nSELECT \n".into(),
                tests: vec![TestCase { input: String::new(), expected: "name\nSara\nLina".into(), hidden: false }],
                reference: "SELECT name FROM students WHERE grade > 80 ORDER BY grade DESC;".into(),
                setup: Lang::sql_setup_template().into(),
                simple: true,
            }),
        },
        Question {
            id: 8,
            prompt: "Build a page with a heading \"My profile\", a short paragraph about you, and a button that changes the heading colour when clicked.".into(),
            points: 4.0,
            kind: Kind::Code(CodeSpec::new(Lang::Web)),
        },
    ];
    e
}

pub fn run(project: Option<PathBuf>) -> i32 {
    let (size, min) = crate::ui::fit_to_screen([1360.0, 860.0], [1040.0, 640.0]);
    let viewport = egui::ViewportBuilder::default().with_title("HDJLabs Studio").with_inner_size(size).with_min_inner_size(min).with_icon(crate::ui::app_icon());
    crate::ui::launch("HDJLabs Studio", viewport, move |_| {
        let mut studio = Studio::new();
        if let Some(p) = project.clone() {
            studio.open_path(p);
        }
        studio
    })
}
