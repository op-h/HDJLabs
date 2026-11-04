//! Home-screen "Create questions with AI": provider setup and the generator. Off until the
//! teacher connects a provider; nothing is sent anywhere before that.

use crate::ai::{self, AiConfig, Counts, Progress, Provider, Request};
use crate::model::*;
use crate::ui::rtl;
use crate::ui::theme::{self, icon, pal};
use egui::{Align, Layout, Margin, RichText};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

enum Source {
    Reading(Receiver<Result<String, String>>),
    Ready(String),
    Failed(String),
}

struct SourceFile {
    path: PathBuf,
    state: Source,
}

const DIFFICULTY: [&str; 4] = ["Mixed", "Easy", "Medium", "Hard"];
const LANGUAGES: [&str; 3] = ["English", "Arabic", "the same language as the material"];
/// Questions per request: enough for an exam, small enough for one answer.
const MAX_QUESTIONS: u32 = 40;

pub struct AiPanel {
    cfg: AiConfig,
    /// Settings dialog open.
    setup: bool,
    /// Generator dialog open.
    open: bool,
    models: Vec<String>,
    models_job: Option<Receiver<Result<Vec<String>, String>>>,
    setup_msg: Option<(String, bool)>,
    files: Vec<SourceFile>,
    counts: Counts,
    code_lang: Lang,
    difficulty: usize,
    language: usize,
    notes: String,
    job: Option<Receiver<Progress>>,
    status: String,
    error: Option<String>,
    /// Generated questions, each with "add it" ticked.
    results: Vec<(bool, Question)>,
    remarks: Vec<String>,
}

impl AiPanel {
    pub fn new() -> Self {
        AiPanel {
            cfg: AiConfig::load(),
            setup: false,
            open: false,
            models: vec![],
            models_job: None,
            setup_msg: None,
            files: vec![],
            counts: Counts { choice: 5, multi: 2, truefalse: 3, short: 2, code: 0 },
            code_lang: Lang::Python,
            difficulty: 0,
            language: 0,
            notes: String::new(),
            job: None,
            status: String::new(),
            error: None,
            results: vec![],
            remarks: vec![],
        }
    }

    /// Debug captures (HDJLABS_SHOT_AI = off | on | setup | gen | busy | results).
    #[cfg(debug_assertions)]
    pub fn shot(&mut self) {
        let Ok(mode) = std::env::var("HDJLABS_SHOT_AI") else { return };
        if crate::ui::shot::frame() != 1 {
            return;
        }
        self.cfg = AiConfig::default();
        if mode != "off" {
            self.cfg.enabled = true;
            self.cfg.provider = Provider::Anthropic;
            self.cfg.base_url = Provider::Anthropic.default_url().into();
            self.cfg.model = "claude-opus-5-5".into();
            self.cfg.api_key = "sk-ant-demo".into();
        }
        self.setup = mode == "setup";
        self.open = matches!(mode.as_str(), "gen" | "busy" | "results");
        if self.open {
            for (n, k) in [("Chapter 3 - Networking.pdf", 48_200), ("Python loops.docx", 12_900), ("Lecture 5 (scanned).pdf", 0)] {
                self.files.push(SourceFile { path: PathBuf::from(n), state: if k > 0 { Source::Ready("x".repeat(k)) } else { Source::Failed("No text found (a scanned PDF has only pictures of pages).".into()) } });
            }
            self.counts.code = 1;
        }
        if mode == "busy" {
            let (_tx, rx) = mpsc::channel();
            std::mem::forget(_tx);
            self.job = Some(rx);
            self.status = "Asking Anthropic (Claude) (claude-opus-5-5)…".into();
        }
        if mode == "results" {
            self.results = crate::ui::studio::sample_exam().questions.into_iter().filter(|q| !matches!(q.kind, Kind::Upload { .. })).map(|q| (true, q)).collect();
            self.results[1].0 = false;
            self.remarks = vec!["Question 5: expected outputs are the AI's (no Java on this PC to check them).".into()];
        }
    }

    /// The card on the Home page.
    pub fn card(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        egui::Frame::new().stroke(egui::Stroke::new(1.0_f32, p.border)).corner_radius(egui::CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::same(20)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::eyebrow("Create questions with AI"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (c, t) = if self.cfg.ready() { (p.success, "On") } else { (p.faint, "Off") };
                    ui.label(theme::figure(t, 11.0, c));
                });
            });
            ui.add_space(6.0);
            if self.cfg.ready() {
                ui.add(egui::Label::new(theme::muted("Give it your PDF, Word, PowerPoint or text files and say how many questions of each type you want. You review every question before it's added.").size(13.0)).wrap());
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::primary(ui, &format!("{}  Create from files", icon::DOC)).clicked() {
                        self.open = true;
                    }
                    if ui.add(egui::Button::new(theme::muted(format!("{}  {}", icon::SETTINGS, self.cfg.model.trim()))).frame_when_inactive(false)).on_hover_text(format!("{} · change the AI provider", self.cfg.provider.label())).clicked() {
                        self.setup = true;
                    }
                });
            } else {
                ui.add(egui::Label::new(theme::muted("Off. Connect an AI model with your own API key (Claude, OpenAI, Gemini, a model on this PC, or any compatible service) to turn your course files into questions.").size(13.0)).wrap());
                ui.add_space(10.0);
                if theme::ghost(ui, &format!("{}  Set up", icon::SETTINGS)).clicked() {
                    self.setup = true;
                }
            }
        });
    }

    /// Dialogs and background work. Returns questions the teacher chose to add.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Vec<Question>> {
        self.poll(ctx);
        if self.setup {
            self.setup_dialog(ctx);
        }
        if self.open {
            return self.generator_dialog(ctx);
        }
        None
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut busy = false;
        for f in &mut self.files {
            if let Source::Reading(rx) = &f.state {
                busy = true;
                if let Ok(r) = rx.try_recv() {
                    f.state = match r {
                        Ok(t) => Source::Ready(t),
                        Err(e) => Source::Failed(e),
                    };
                }
            }
        }
        if let Some(rx) = &self.models_job {
            busy = true;
            if let Ok(r) = rx.try_recv() {
                self.models_job = None;
                match r {
                    Ok(m) => {
                        self.setup_msg = Some((format!("Connected. {} model(s) available.", m.len()), false));
                        if self.cfg.model.trim().is_empty() || !m.contains(&self.cfg.model) {
                            if let Some(first) = m.iter().find(|x| x.as_str() == self.cfg.provider.default_model()).or(m.first()) {
                                if self.cfg.model.trim().is_empty() {
                                    self.cfg.model = first.clone();
                                }
                            }
                        }
                        self.models = m;
                    }
                    Err(e) => self.setup_msg = Some((e, true)),
                }
            }
        }
        while let Some(rx) = &self.job {
            busy = true;
            match rx.try_recv() {
                Ok(Progress::Status(s)) => self.status = s,
                Ok(Progress::Done(r)) => {
                    self.job = None;
                    match r {
                        Ok((qs, notes)) => {
                            self.results = qs.into_iter().map(|q| (true, q)).collect();
                            self.remarks = notes;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.error = Some("The request stopped unexpectedly.".into());
                }
            }
        }
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    fn setup_dialog(&mut self, ctx: &egui::Context) {
        let p = pal();
        let mut close = false;
        egui::Modal::new(egui::Id::new("ai-setup")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
            ui.set_width(500.0);
            ui.label(theme::heading("AI provider", 20.0));
            ui.add(egui::Label::new(theme::muted("Your course files are sent to the provider you choose when you create questions. Nothing is sent before that, and students' PCs never use it. The key is stored encrypted for your Windows account only.").size(13.0)).wrap());
            ui.add_space(12.0);
            ui.checkbox(&mut self.cfg.enabled, "Turn on AI question building");
            ui.add_enabled_ui(self.cfg.enabled, |ui| {
                ui.add_space(8.0);
                egui::Grid::new("ai-grid").striped(false).num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Provider");
                    let before = self.cfg.provider;
                    egui::ComboBox::from_id_salt("ai-provider").width(300.0).selected_text(self.cfg.provider.label()).show_ui(ui, |ui| {
                        for pr in Provider::ALL {
                            ui.selectable_value(&mut self.cfg.provider, pr, pr.label());
                        }
                    });
                    if before != self.cfg.provider {
                        self.cfg.base_url = self.cfg.provider.default_url().into();
                        self.cfg.model = self.cfg.provider.default_model().into();
                        self.models.clear();
                        self.setup_msg = None;
                    }
                    ui.end_row();
                    ui.label("Address");
                    theme::text_field(ui, &mut self.cfg.base_url, "https://…/v1", 300.0);
                    ui.end_row();
                    ui.label("API key");
                    let hint = if self.cfg.provider.needs_key() { "Paste your key" } else { "Not needed for a model on this PC" };
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.api_key).password(true).hint_text(hint).desired_width(300.0).margin(Margin::symmetric(10, 8)));
                    ui.end_row();
                    ui.label("Model");
                    ui.horizontal(|ui| {
                        if self.models.is_empty() {
                            theme::text_field(ui, &mut self.cfg.model, "model name", 190.0);
                        } else {
                            egui::ComboBox::from_id_salt("ai-model").width(190.0).selected_text(self.cfg.model.clone()).height(320.0).show_ui(ui, |ui| {
                                for m in &self.models {
                                    ui.selectable_value(&mut self.cfg.model, m.clone(), m);
                                }
                            });
                        }
                        let busy = self.models_job.is_some();
                        if ui.add_enabled(!busy, egui::Button::new(if busy { "Connecting…" } else { "Load models" })).on_hover_text("Connects with this key and lists the models you can use").clicked() {
                            let (tx, rx) = mpsc::channel();
                            let cfg = self.cfg.clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(ai::list_models(&cfg));
                            });
                            self.models_job = Some(rx);
                            self.setup_msg = None;
                        }
                    });
                    ui.end_row();
                });
            });
            if let Some((m, err)) = &self.setup_msg {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(m).size(13.0).color(if *err { p.danger } else { p.success })).wrap());
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if theme::ghost(ui, "Cancel").clicked() {
                    self.cfg = AiConfig::load();
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::primary(ui, &format!("{}  Save", icon::SAVE)).clicked() {
                        match self.cfg.save() {
                            Ok(()) => close = true,
                            Err(e) => self.setup_msg = Some((e, true)),
                        }
                    }
                });
            });
        });
        if close {
            self.setup = false;
        }
    }

    fn add_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new().set_title("Course material").add_filter("Course material", &["pdf", "docx", "pptx", "txt", "md", "csv", "html", "py", "js", "c", "cpp", "java", "sql"]).add_filter("All files", &["*"]).pick_files() else { return };
        for path in paths {
            if self.files.iter().any(|f| f.path == path) {
                continue;
            }
            let (tx, rx) = mpsc::channel();
            let p2 = path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(ai::extract_text(&p2));
            });
            self.files.push(SourceFile { path, state: Source::Reading(rx) });
        }
    }

    fn generator_dialog(&mut self, ctx: &egui::Context) -> Option<Vec<Question>> {
        let p = pal();
        let mut out = None;
        let mut close = false;
        let busy = self.job.is_some();
        egui::Modal::new(egui::Id::new("ai-generate")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
            let w = (ctx.content_rect().width() - 120.0).clamp(420.0, 760.0);
            ui.set_width(w);
            ui.label(theme::heading("Create questions with AI", 20.0));
            ui.label(theme::muted(format!("{} · {}", self.cfg.provider.label(), self.cfg.model.trim())).size(12.5));
            ui.add_space(10.0);
            let max_h = (ctx.content_rect().height() - 220.0).max(240.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                if !self.results.is_empty() {
                    self.results_view(ui);
                    return;
                }
                ui.add_enabled_ui(!busy, |ui| self.request_form(ui));
            });
            if let Some(e) = &self.error {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(format!("{}  {e}", icon::WARNING)).size(13.0).color(p.danger)).wrap());
            }
            if busy {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::muted(&self.status).size(13.0));
                });
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("Close").min_size(egui::vec2(0.0, 34.0))).clicked() {
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.results.is_empty() {
                        let n = self.results.iter().filter(|r| r.0).count();
                        if ui.add_enabled(n > 0, egui::Button::new(RichText::new(format!("{}  Add {n} question(s)", icon::ADD)).font(theme::semibold(14.0)).color(p.on_accent)).fill(theme::accent_fill(ui)).min_size(egui::vec2(0.0, 34.0))).clicked() {
                            out = Some(self.results.drain(..).filter(|r| r.0).map(|r| r.1).collect());
                            close = true;
                        }
                        if theme::ghost(ui, &format!("{}  Start over", icon::BACK)).clicked() {
                            self.results.clear();
                            self.remarks.clear();
                        }
                    } else {
                        let ready: Vec<(String, String)> = self.files.iter().filter_map(|f| match &f.state {
                            Source::Ready(t) => Some((f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), t.clone())),
                            _ => None,
                        }).collect();
                        let total = self.counts.total();
                        let can = !busy && !ready.is_empty() && total > 0 && total <= MAX_QUESTIONS;
                        let r = ui.add_enabled(can, egui::Button::new(RichText::new(format!("{}  Create {total} question(s)", icon::PLAY)).font(theme::semibold(14.0)).color(p.on_accent)).fill(theme::accent_fill(ui)).min_size(egui::vec2(0.0, 34.0)));
                        let r = if ready.is_empty() { r.on_disabled_hover_text("Add at least one readable file first") } else { r };
                        if r.clicked() {
                            self.start(ready);
                        }
                    }
                });
            });
        });
        if close {
            self.open = false;
        }
        out
    }

    fn request_form(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.label(theme::eyebrow("Material"));
        ui.add_space(4.0);
        let mut remove = None;
        let mut total_chars = 0;
        for (i, f) in self.files.iter().enumerate() {
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::DOC).color(p.muted));
                ui.add(egui::Label::new(RichText::new(rtl::line(&name)).font(theme::semibold(13.5))).truncate());
                match &f.state {
                    Source::Reading(_) => {
                        ui.spinner();
                    }
                    Source::Ready(t) => {
                        total_chars += t.len();
                        ui.label(theme::figure(format!("{}K chars", t.len().div_ceil(1000)), 11.5, p.faint));
                    }
                    Source::Failed(e) => {
                        ui.add(egui::Label::new(RichText::new(e).size(12.5).color(p.danger)).truncate());
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::icon_button(ui, icon::DELETE, "Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.files.remove(i);
        }
        if self.files.is_empty() {
            ui.label(theme::muted("PDF, Word (.docx), PowerPoint (.pptx) or text files. Scanned PDFs (pictures of pages) have no text to read.").size(13.0));
        }
        if total_chars > ai::MAX_CHARS {
            ui.add(egui::Label::new(RichText::new(format!("{}  That's {}K characters; only the first {}K are read. Remove a file or split it to cover the rest.", icon::WARNING, total_chars / 1000, ai::MAX_CHARS / 1000)).size(12.5).color(p.warning)).wrap());
        }
        ui.add_space(4.0);
        if theme::ghost(ui, &format!("{}  Add files", icon::ADD)).clicked() {
            self.add_files();
        }

        ui.add_space(14.0);
        ui.label(theme::eyebrow("How many of each"));
        ui.add_space(4.0);
        egui::Grid::new("ai-counts").striped(false).num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            for (label, n) in [("Multiple choice", &mut self.counts.choice), ("Multiple answer", &mut self.counts.multi), ("True / False", &mut self.counts.truefalse), ("Short answer", &mut self.counts.short), ("Code", &mut self.counts.code)] {
                ui.label(label);
                ui.add(egui::DragValue::new(n).range(0..=MAX_QUESTIONS).speed(0.1));
                ui.end_row();
            }
            if self.counts.code > 0 {
                ui.label("Code language");
                egui::ComboBox::from_id_salt("ai-code-lang").width(160.0).selected_text(self.code_lang.name()).show_ui(ui, |ui| {
                    for l in ai::CODE_LANGS {
                        ui.selectable_value(&mut self.code_lang, l, l.name());
                    }
                });
                ui.end_row();
            }
            ui.label("Difficulty");
            egui::ComboBox::from_id_salt("ai-diff").width(160.0).selected_text(DIFFICULTY[self.difficulty]).show_ui(ui, |ui| {
                for (i, d) in DIFFICULTY.iter().enumerate() {
                    ui.selectable_value(&mut self.difficulty, i, *d);
                }
            });
            ui.end_row();
            ui.label("Written in");
            egui::ComboBox::from_id_salt("ai-lang").width(160.0).selected_text(if self.language == 2 { "Same as the material" } else { LANGUAGES[self.language] }).show_ui(ui, |ui| {
                for (i, l) in ["English", "Arabic", "Same as the material"].iter().enumerate() {
                    ui.selectable_value(&mut self.language, i, *l);
                }
            });
            ui.end_row();
        });
        let total = self.counts.total();
        if total > MAX_QUESTIONS {
            ui.label(RichText::new(format!("Up to {MAX_QUESTIONS} questions at a time; run it again for more.")).size(12.5).color(p.warning));
        }
        if self.counts.code > 0 && crate::runner::tool(self.code_lang).is_none() {
            ui.add(egui::Label::new(RichText::new(format!("{} isn't installed on this PC, so the AI's expected outputs can't be checked by running its solution.", self.code_lang.name())).size(12.5).color(p.warning)).wrap());
        }
        ui.add_space(10.0);
        ui.label(theme::eyebrow("Notes for the AI (optional)"));
        ui.add_space(4.0);
        ui.add(crate::ui::text_field::TextField::multiline(&mut self.notes).hint_text("e.g. Focus on chapters 3-4. Avoid questions about dates.").desired_width(ui.available_width()).margin(Margin::symmetric(10, 8)));
    }

    fn start(&mut self, sources: Vec<(String, String)>) {
        let req = Request { sources, counts: self.counts.clone(), code_lang: self.code_lang, difficulty: DIFFICULTY[self.difficulty].to_lowercase(), language: LANGUAGES[self.language].into(), notes: self.notes.clone() };
        let cfg = self.cfg.clone();
        let (tx, rx) = mpsc::channel();
        self.error = None;
        self.status = "Starting…".into();
        std::thread::spawn(move || {
            let t2 = tx.clone();
            let r = ai::generate(&cfg, &req, move |s| {
                let _ = t2.send(Progress::Status(s));
            });
            let _ = tx.send(Progress::Done(r));
        });
        self.job = Some(rx);
    }

    fn results_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        for r in &self.remarks {
            ui.add(egui::Label::new(RichText::new(format!("{}  {r}", icon::INFO)).size(12.5).color(p.warning)).wrap());
        }
        if !self.remarks.is_empty() {
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            ui.label(theme::muted(format!("{} question(s). Untick any you don't want; you can edit them all afterwards.", self.results.len())).size(13.0));
        });
        ui.add_space(6.0);
        for (i, (keep, q)) in self.results.iter_mut().enumerate() {
            if i > 0 {
                ui.separator();
            }
            ui.horizontal_top(|ui| {
                ui.checkbox(keep, "");
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(theme::figure(format!("{:02} · {} · {} pts", i + 1, q.kind.label(), theme::fmt_points(q.points)), 11.5, p.accent));
                    ui.add(egui::Label::new(RichText::new(rtl::line(&q.prompt)).size(14.0)).wrap());
                    match &q.kind {
                        Kind::Choice { options, correct, .. } => {
                            for (k, o) in options.iter().enumerate() {
                                let ok = correct.contains(&k);
                                ui.label(RichText::new(format!("{} {}", if ok { icon::CHECK } else { "·" }, rtl::line(o))).size(13.0).color(if ok { p.success } else { p.muted }));
                            }
                        }
                        Kind::TrueFalse { answer } => {
                            ui.label(RichText::new(format!("{} {}", icon::CHECK, if *answer { "True" } else { "False" })).size(13.0).color(p.success));
                        }
                        Kind::Short { accepted, .. } => {
                            ui.label(RichText::new(format!("{} {}", icon::CHECK, rtl::line(&accepted.join("  |  ")))).size(13.0).color(p.success));
                        }
                        Kind::Code(spec) => {
                            ui.label(theme::muted(format!("{} · {} test(s){}", spec.lang.name(), spec.tests.len(), if spec.reference.trim().is_empty() { "" } else { " · reference solution included" })).size(12.5));
                        }
                        _ => {}
                    }
                });
            });
        }
    }
}
