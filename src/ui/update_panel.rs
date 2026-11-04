//! Studio updates: checked at start and every few hours; waits while a live session runs.

use crate::ui::theme::{self, icon, pal};
use crate::update::{self, Notice};
use egui::{Align, Layout, Margin, RichText};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const RECHECK: Duration = Duration::from_secs(4 * 3600);

enum Install {
    Progress(f32),
    Done(Result<(), String>),
}

pub struct UpdatePanel {
    check: Option<Receiver<Result<Option<Notice>, String>>>,
    last_check: Option<Instant>,
    notice: Option<Notice>,
    /// Optional request postponed for this run.
    later: bool,
    install: Option<Receiver<Install>>,
    progress: f32,
    error: Option<String>,
}

impl UpdatePanel {
    pub fn new() -> Self {
        UpdatePanel { check: None, last_check: None, notice: None, later: false, install: None, progress: 0.0, error: None }
    }

    #[cfg(debug_assertions)]
    pub fn shot(&mut self) {
        if let (Ok(kind), 1) = (std::env::var("HDJLABS_SHOT_UPDATE"), crate::ui::shot::frame()) {
            self.last_check = Some(Instant::now());
            self.notice = Some(Notice {
                version: "1.0.1-beta".into(),
                mandatory: kind == "mandatory",
                title: "Internet link and AI questions".into(),
                notes: "Students can now join from home through an internet link.\nCreate questions from your PDF, Word and PowerPoint files with any AI model.\nFixes for Arabic names in results.".into(),
                url: "https://github.com/op-h/HDJLabs/releases/download/v1.0.1-beta/HDJLabs-Setup.exe".into(),
                sha256: "0".repeat(64),
                size: 18_000_000,
                issued: 0,
            });
            if kind == "installing" {
                let (tx, rx) = mpsc::channel();
                std::mem::forget(tx);
                self.install = Some(rx);
                self.progress = 0.42;
            }
        }
    }

    /// Runs the checks and shows the request. `busy`: a live session is running.
    pub fn show(&mut self, ctx: &egui::Context, busy: bool) {
        #[cfg(debug_assertions)]
        if crate::ui::shot::enabled() && self.last_check.is_none() && std::env::var_os("HDJLABS_UPDATE_URL").is_none() {
            self.last_check = Some(Instant::now());
        }
        if self.check.is_none() && self.last_check.is_none_or(|t| t.elapsed() > RECHECK) {
            self.last_check = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            let c = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(update::check());
                c.request_repaint();
            });
            self.check = Some(rx);
        }
        if let Some(rx) = &self.check {
            if let Ok(r) = rx.try_recv() {
                self.check = None;
                if let Ok(n) = r {
                    // A different request (e.g. now mandatory) asks again even after "Later".
                    if n.as_ref() != self.notice.as_ref() {
                        self.later = false;
                    }
                    self.notice = n;
                }
            }
        }
        if let Some(rx) = &self.install {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Install::Progress(f) => self.progress = f,
                    Install::Done(Ok(())) => {
                        // The installer replaces this program and opens it again.
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Install::Done(Err(e)) => {
                        self.install = None;
                        self.error = Some(e);
                        break;
                    }
                }
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let Some(n) = self.notice.clone() else { return };
        if (busy && self.install.is_none()) || (self.later && !n.mandatory) {
            return;
        }
        self.dialog(ctx, &n);
    }

    fn dialog(&mut self, ctx: &egui::Context, n: &Notice) {
        let p = pal();
        // Mandatory: nothing behind it can be used; Escape and clicks outside do nothing.
        let modal = egui::Modal::new(egui::Id::new("update-request")).frame(theme::card_frame().inner_margin(Margin::same(26)));
        let modal = if n.mandatory { modal.backdrop_color(p.bg.gamma_multiply(0.97)) } else { modal };
        modal.show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(theme::eyebrow(if n.mandatory { "Required update" } else { "Update available" }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(theme::figure(format!("v{} → v{}", env!("CARGO_PKG_VERSION"), n.version), 11.5, p.muted));
                });
            });
            ui.add_space(4.0);
            ui.add(egui::Label::new(theme::heading(if n.title.trim().is_empty() { format!("HDJLabs {}", n.version) } else { n.title.clone() }, 20.0)).wrap());
            ui.add_space(6.0);
            for line in n.notes.lines().filter(|l| !l.trim().is_empty()) {
                ui.horizontal_top(|ui| {
                    ui.label(RichText::new("·").color(p.accent));
                    ui.add(egui::Label::new(RichText::new(line.trim()).size(13.5)).wrap());
                });
            }
            if n.mandatory {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(format!("{}  This update is required to keep using HDJLabs Studio. Your projects and results are kept.", icon::INFO)).size(12.5).color(p.warning)).wrap());
            }
            if let Some(e) = &self.error {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(format!("{}  {e}", icon::WARNING)).size(12.5).color(p.danger)).wrap());
            }
            ui.add_space(14.0);
            if self.install.is_some() {
                ui.label(theme::muted(format!("Downloading the update… {:.0}%", self.progress * 100.0)).size(13.0));
                ui.add_space(4.0);
                theme::progress_bar(ui, self.progress, 4.0, p.accent);
                ui.add_space(4.0);
                ui.label(theme::muted("Studio closes and reopens by itself when it's installed.").size(12.0));
                return;
            }
            ui.horizontal(|ui| {
                if n.mandatory {
                    if theme::ghost(ui, "Close HDJLabs").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else if theme::ghost(ui, "Later").on_hover_text("Ask again next time Studio starts").clicked() {
                    self.later = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::primary(ui, &format!("{}  Update now", icon::DOWNLOAD)).clicked() {
                        self.start_install(ui.ctx(), n.clone());
                    }
                });
            });
        });
    }

    fn start_install(&mut self, ctx: &egui::Context, n: Notice) {
        let (tx, rx) = mpsc::channel();
        let c = ctx.clone();
        self.error = None;
        self.progress = 0.0;
        std::thread::spawn(move || {
            let t2 = tx.clone();
            let c2 = c.clone();
            let r = update::install(&n, move |f| {
                let _ = t2.send(Install::Progress(f));
                c2.request_repaint();
            });
            let _ = tx.send(Install::Done(r));
            c.request_repaint();
        });
        self.install = Some(rx);
    }
}
