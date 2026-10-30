//! The student exam app: sign-in → exam (question rail, answer area, IDE for code) → results.

use crate::grading::{self, TestOutcome};
use crate::lockdown;
use crate::model::*;
use crate::net::{Conn, LanClient, LiveStatus, Phase, S2C};
use crate::payload::Package;
use crate::runner::{self, RunOutput};
use crate::storage;
use crate::ui::{editor, rtl};
use crate::ui::theme::{self, icon, pal};
use egui::{Align, Color32, CornerRadius, Key, Layout, Margin, RichText, Stroke};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Running on the private exam desktop.
    Locked,
    /// Lockdown inside the normal desktop (private desktop unavailable).
    Fallback,
    /// Teacher preview / practice exam — normal window, no restrictions.
    Windowed,
}

#[derive(Clone, Serialize, Deserialize)]
struct Attempt {
    id: String,
    name: String,
    sid: String,
    order: Vec<u32>,
    option_orders: BTreeMap<u32, Vec<usize>>,
    answers: BTreeMap<u32, Answer>,
    flags: BTreeSet<u32>,
    current: usize,
    started_at: u64,
    deadline: u64,
    violations: Vec<Violation>,
    submitted: Option<Submission>,
    /// The teacher finished grading and sent the final score.
    #[serde(default)]
    graded: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum BottomTab {
    Output,
    Input,
    Tests,
}

struct CodeUi {
    stdin: String,
    output: Option<RunOutput>,
    tests: Option<Vec<TestOutcome>>,
    running: Option<Receiver<RunResult>>,
    tab: BottomTab,
    line: usize,
    col: usize,
}

enum RunResult {
    Run(RunOutput),
    Tests(Vec<TestOutcome>),
}

impl Default for CodeUi {
    fn default() -> Self {
        Self { stdin: String::new(), output: None, tests: None, running: None, tab: BottomTab::Output, line: 1, col: 1 }
    }
}

enum Screen {
    Welcome,
    Exam,
    Grading,
    Finished,
}

pub struct Player {
    pkg: Package,
    mode: Mode,
    window_title: String,
    screen: Screen,
    name: String,
    sid: String,
    password: String,
    error: Option<String>,
    unlocking: Option<Receiver<Result<(Exam, [u8; 32]), String>>>,
    failures: u32,
    cooldown_until: Option<Instant>,
    manual_addr: Arc<Mutex<String>>,
    manual_edit: String,
    show_net: bool,
    lan: LanClient,
    exam: Option<Exam>,
    key: [u8; 32],
    at: Option<Attempt>,
    code: HashMap<u32, CodeUi>,
    confirm_submit: bool,
    proctor: Option<(String, Option<String>)>,
    teacher_msg: Option<String>,
    upload_error: Option<String>,
    grading: Option<Receiver<Submission>>,
    saved_paths: Vec<std::path::PathBuf>,
    window: Option<lockdown::Window>,
    harden_frames: u32,
    last_push: Instant,
    last_save: Instant,
    dirty: bool,
    focus_lost: bool,
    font: f32,
    allow_close: bool,
    web: crate::ui::web::WebPreview,
    /// Teacher asked to delete the exam file from this PC (done when the exam closes).
    remove_requested: bool,
    close_countdown: Option<Instant>,
    finalized: bool,
    #[cfg(debug_assertions)]
    selftest_started: Instant,
    #[cfg(debug_assertions)]
    selftest_frames: u64,
}

impl Player {
    pub fn new(pkg: Package, mode: Mode) -> Self {
        let manual_addr = Arc::new(Mutex::new(String::new()));
        let h = &pkg.header;
        let lan = LanClient::start(h.exam_id.clone(), h.teacher_pub.clone(), h.teacher_hints.clone(), h.lan_port, manual_addr.clone());
        let window_title = format!("HDJLabs Exam {}", &h.exam_id[..8.min(h.exam_id.len())]);
        Self {
            pkg,
            mode,
            window_title,
            screen: Screen::Welcome,
            name: String::new(),
            sid: String::new(),
            password: String::new(),
            error: None,
            unlocking: None,
            failures: 0,
            cooldown_until: None,
            manual_addr,
            manual_edit: String::new(),
            show_net: false,
            lan,
            exam: None,
            key: [0; 32],
            at: None,
            code: HashMap::new(),
            confirm_submit: false,
            proctor: None,
            teacher_msg: None,
            upload_error: None,
            grading: None,
            saved_paths: vec![],
            window: None,
            harden_frames: 0,
            last_push: Instant::now(),
            last_save: Instant::now(),
            dirty: false,
            focus_lost: false,
            font: 15.0,
            allow_close: mode == Mode::Windowed,
            // The CPU renderer runs on PCs without a graphics driver, where WebView2 doesn't draw
            // and its window can't be layered under egui's dialogs.
            web: if crate::ui::software_requested() {
                crate::ui::web::WebPreview::disabled("Live preview isn't available on this PC (no graphics driver). Your page is still saved and submitted, and your teacher sees it.")
            } else {
                Default::default()
            },
            remove_requested: false,
            close_countdown: None,
            finalized: false,
            #[cfg(debug_assertions)]
            selftest_started: Instant::now(),
            #[cfg(debug_assertions)]
            selftest_frames: 0,
        }
    }


    fn locked(&self) -> bool {
        self.mode != Mode::Windowed
    }

    fn violation(&mut self, kind: &str) {
        if let Some(at) = self.at.as_mut() {
            if at.submitted.is_none() {
                at.violations.push(Violation { at: now_unix(), kind: kind.into() });
                self.dirty = true;
            }
        }
        self.lan.event(kind);
    }

    fn remaining(&self) -> i64 {
        self.at.as_ref().map(|a| a.deadline as i64 - now_unix() as i64).unwrap_or(0)
    }

    // ───────────── start / resume ─────────────

    fn begin_unlock(&mut self) {
        if self.name.trim().is_empty() {
            self.error = Some("Enter your full name.".into());
            return;
        }
        if let Some(t) = self.cooldown_until {
            if Instant::now() < t {
                return;
            }
        }
        self.error = None;
        let pkg = self.pkg.clone();
        let pw = self.password.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = pkg.unlock(&pw).map(|e| (e, pkg.attempt_key(&pw))).map_err(|_| "That password is not correct.".to_string());
            let _ = tx.send(r);
        });
        self.unlocking = Some(rx);
    }

    fn on_unlocked(&mut self, exam: Exam, key: [u8; 32]) {
        self.key = key;
        let name = self.name.trim().to_string();
        let sid = self.sid.trim().to_string();
        let saved: Option<Attempt> = storage::attempt_load(&exam.id, &key);
        let mut at = match saved {
            Some(a) if a.sid == sid && a.name.eq_ignore_ascii_case(&name) => a,
            _ => new_attempt(&exam, name, sid),
        };
        let finished = at.submitted.is_some();
        if !finished && at.started_at + 5 < now_unix() {
            // The exam was closed mid-attempt (crash, power, proctor) — make that visible.
            at.violations.push(Violation { at: now_unix(), kind: "Exam was closed and resumed".into() });
            self.lan.event("Exam was closed and resumed");
        }
        self.lan.hello(&at.id, &at.name, &at.sid);
        self.at = Some(at);
        self.exam = Some(exam);
        self.password.clear();
        self.dirty = true;
        if finished {
            // Already submitted from this PC: resend and show the result again.
            if let Some(sub) = self.at.as_ref().and_then(|a| a.submitted.clone()) {
                self.deliver(sub);
            }
            self.screen = Screen::Finished;
        } else {
            self.screen = Screen::Exam;
        }
    }

    fn question(&self, idx: usize) -> Option<&Question> {
        let at = self.at.as_ref()?;
        let qid = *at.order.get(idx)?;
        self.exam.as_ref()?.questions.iter().find(|q| q.id == qid)
    }

    // ───────────── submit ─────────────

    fn submit(&mut self, forced: bool) {
        let (Some(exam), Some(at)) = (self.exam.clone(), self.at.clone()) else { return };
        if at.submitted.is_some() || self.grading.is_some() {
            return;
        }
        self.confirm_submit = false;
        self.screen = Screen::Grading;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut results = Vec::new();
            for qid in &at.order {
                if let Some(q) = exam.questions.iter().find(|q| q.id == *qid) {
                    let a = at.answers.get(qid).cloned().unwrap_or_default();
                    results.push(grading::grade(q, &a));
                }
            }
            let score = results.iter().map(|r| r.earned).sum();
            let max_score = results.iter().map(|r| r.max).sum();
            let _ = tx.send(Submission {
                exam_id: exam.id.clone(),
                exam_title: exam.title.clone(),
                attempt_id: at.id.clone(),
                student_name: at.name.clone(),
                student_id: at.sid.clone(),
                machine: machine_name(),
                started_at: at.started_at,
                submitted_at: now_unix(),
                question_order: at.order.clone(),
                answers: at.answers.clone(),
                results,
                score,
                max_score,
                violations: at.violations.clone(),
                forced,
            });
        });
        self.grading = Some(rx);
    }

    fn deliver(&mut self, sub: Submission) {
        let h = &self.pkg.header;
        if let Ok(sealed) = crate::crypto::seal_for(&h.teacher_pub, &h.exam_id, &serde_json::to_vec(&sub).unwrap_or_default()) {
            self.lan.submit(sealed.clone());
            self.saved_paths = storage::save_result_file(&sealed, &h.title, &sub.student_name, &sub.student_id);
        }
    }

    // ───────────── background housekeeping ─────────────

    /// Debug builds only: close after N seconds and write a report, so lockdown can be verified
    /// on a developer machine without getting stuck on the exam desktop.
    #[cfg(debug_assertions)]
    fn selftest(&mut self, ctx: &egui::Context) {
        let Some(secs) = std::env::var("HDJLABS_SELFTEST_EXIT").ok().and_then(|v| v.parse::<u64>().ok()) else { return };
        self.selftest_frames += 1;
        if self.selftest_started.elapsed().as_secs() >= secs && !self.allow_close {
            let report = format!(
                "desktop={}\nmode={:?}\nwindow_found={}\nframes={}\nblocked_shortcuts_logged={}\n",
                lockdown::current_desktop_name(),
                self.mode,
                self.window.is_some(),
                self.selftest_frames,
                self.at.as_ref().map(|a| a.violations.len()).unwrap_or(0)
            );
            let _ = std::fs::write(std::env::temp_dir().join("hdjlabs-selftest.txt"), report);
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Debug builds: drive the UI into a state for screenshots (`HDJLABS_SHOT_SCRIPT`).
    #[cfg(debug_assertions)]
    fn shot_script(&mut self) {
        let script = std::env::var("HDJLABS_SHOT_SCRIPT").unwrap_or_default();
        if script.is_empty() || script == "welcome" {
            if self.name.is_empty() {
                self.name = "Sara Ali".into();
                self.sid = "S-104".into();
            }
            return;
        }
        match self.screen {
            Screen::Welcome if self.unlocking.is_none() && self.exam.is_none() => {
                self.name = "Sara Ali".into();
                self.sid = std::env::var("HDJLABS_SHOT_SID").unwrap_or_else(|_| "S-104".into());
                self.password = "start".into();
                self.begin_unlock();
            }
            Screen::Exam => {
                let at = self.at.as_mut().unwrap();
                if !at.answers.is_empty() {
                    return;
                }
                let ids: Vec<u32> = at.order.clone();
                let exam = self.exam.as_ref().unwrap();
                for id in ids.iter().take(2) {
                    let q = exam.questions.iter().find(|q| q.id == *id).unwrap();
                    let a = match &q.kind {
                        Kind::Choice { correct, .. } => Answer::Choice(correct.clone()),
                        Kind::TrueFalse { .. } => Answer::Bool(true),
                        Kind::Short { .. } => Answer::Text("def".into()),
                        Kind::Code(c) => Answer::Code { lang: c.lang, source: c.starter.clone() },
                        Kind::Upload { .. } => Answer::Empty,
                    };
                    at.answers.insert(*id, a);
                }
                if let Some(id) = ids.get(3) {
                    at.flags.insert(*id);
                }
                let want = script.split(':').nth(1).unwrap_or("choice").to_string();
                let pos = ids.iter().position(|id| {
                    let q = exam.questions.iter().find(|q| q.id == *id).unwrap();
                    matches!((&q.kind, want.as_str()), (Kind::Short { accepted, .. }, "flag") if accepted.iter().any(|a| a.contains('{'))) || matches!((&q.kind, want.as_str()), (Kind::Choice { multiple: false, .. }, "choice") | (Kind::Code(CodeSpec { lang: Lang::Python, .. }), "code") | (Kind::Code(CodeSpec { lang: Lang::Sql, .. }), "sql") | (Kind::Code(CodeSpec { lang: Lang::Web, .. }), "web") | (Kind::Short { .. }, "short") | (Kind::Upload { .. }, "upload"))
                });
                at.current = pos.unwrap_or(0);
                let qid = ids[at.current];
                if let Ok(prompt) = std::env::var("HDJLABS_SHOT_PROMPT") {
                    let e = self.exam.as_mut().unwrap();
                    if let Some(q) = e.questions.iter_mut().find(|q| q.id == qid) {
                        q.prompt = prompt;
                        if let Kind::Choice { options, .. } = &mut q.kind {
                            for (o, t) in options.iter_mut().zip(["مكدس (Stack)", "طابور (Queue)", "جدول تجزئة", "شجرة ثنائية"]) {
                                *o = t.into();
                            }
                        }
                    }
                }
                let exam = self.exam.as_ref().unwrap();
                let q = exam.questions.iter().find(|q| q.id == qid).unwrap().clone();
                if let Kind::Code(c) = &q.kind {
                    let src = if c.lang == Lang::Sql { "SELECT name
FROM students
WHERE grade > 80
ORDER BY grade DESC;
".to_string() } else if c.lang == Lang::Python { "n = int(input())\n\n# add up 1..n\ntotal = 0\nfor i in range(1, n + 1):\n    total += i\n\nprint(total)\n".to_string() } else { c.starter.clone() };
                    at.answers.insert(qid, Answer::Code { lang: c.lang, source: src.clone() });
                    let cu = self.code.entry(qid).or_default();
                    cu.stdin = "10".into();
                    let (tx, rx) = mpsc::channel();
                    let spec = c.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(RunResult::Tests(grading::run_tests(&spec, spec.lang, &src, false)));
                    });
                    cu.running = Some(rx);
                }
                if want == "flag" {
                    let id = ids[at.current];
                    at.answers.insert(id, Answer::Text("HDJ{l0c".into()));
                }
                if script == "submit" {
                    self.confirm_submit = true;
                }
                if script == "proctor" {
                    self.proctor = Some((String::new(), Some("Incorrect password.".into())));
                }
                if script == "message" {
                    self.teacher_msg = Some("Ten minutes left. Remember to press Run tests before you submit.".into());
                }
                if script == "grading" {
                    self.screen = Screen::Grading;
                }
                if script.starts_with("finished") {
                    if let Some(e) = self.exam.as_mut() {
                        e.settings.hold_scores = script == "finished-wait";
                    }
                    self.submit(false);
                    self.remove_requested = script == "finished-remove";
                    if script == "finished-graded" {
                        self.at.as_mut().unwrap().graded = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        #[cfg(debug_assertions)]
        self.selftest(ctx);
        if let Some(rx) = &self.unlocking {
            if let Ok(r) = rx.try_recv() {
                self.unlocking = None;
                match r {
                    Ok((exam, key)) => self.on_unlocked(exam, key),
                    Err(e) => {
                        self.failures += 1;
                        self.lan.event("Wrong exam password");
                        if self.failures >= 3 {
                            let secs = 2u64.pow(self.failures.min(6)).min(60);
                            self.cooldown_until = Some(Instant::now() + Duration::from_secs(secs));
                        }
                        self.error = Some(e);
                    }
                }
            }
        }
        if let Some(rx) = &self.grading {
            if let Ok(sub) = rx.try_recv() {
                self.grading = None;
                if let Some(at) = self.at.as_mut() {
                    at.submitted = Some(sub.clone());
                }
                self.deliver(sub);
                self.dirty = true;
                self.screen = Screen::Finished;
            }
        }
        while let Ok(msg) = self.lan.inbox.try_recv() {
            match msg {
                S2C::Message { text } => self.teacher_msg = Some(text.chars().take(2000).collect()),
                S2C::AddTime { secs } => {
                    if let Some(at) = self.at.as_mut() {
                        at.deadline = (at.deadline as i64 + secs.clamp(-3600, 4 * 3600)).max(0) as u64;
                        self.dirty = true;
                    }
                }
                S2C::ForceSubmit => {
                    if matches!(self.screen, Screen::Exam) {
                        self.submit(true);
                    }
                }
                S2C::Graded { results, score } => {
                    if let Some(sub) = self.at.as_mut().and_then(|a| a.submitted.as_mut()) {
                        sub.results = results.into_iter().filter(|r| sub.question_order.contains(&r.qid)).collect();
                        sub.score = score.clamp(0.0, sub.max_score);
                        self.at.as_mut().unwrap().graded = true;
                        if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
                            storage::attempt_save(&exam.id, &self.key, at);
                        }
                        self.dirty = true;
                    }
                }
                S2C::RemoveExam => {
                    if !self.remove_requested {
                        self.remove_requested = true;
                        self.lan.event("Exam file will be removed when it closes");
                    }
                }
                _ => {}
            }
        }
        for c in self.code.values_mut() {
            if let Some(rx) = &c.running {
                if let Ok(r) = rx.try_recv() {
                    c.running = None;
                    match r {
                        RunResult::Run(o) => {
                            c.output = Some(o);
                            c.tab = BottomTab::Output;
                        }
                        RunResult::Tests(t) => {
                            c.tests = Some(t);
                            c.tab = BottomTab::Tests;
                        }
                    }
                }
            }
        }

        if matches!(self.screen, Screen::Exam) && self.remaining() <= 0 {
            self.submit(true);
        }

        // Lockdown upkeep.
        if self.locked() {
            if self.window.is_none() {
                self.window = lockdown::find_window(&self.window_title);
            }
            if let Some(w) = self.window {
                if self.harden_frames < 5 {
                    lockdown::harden_window(w);
                    self.harden_frames += 1;
                }
                if lockdown::enforce_foreground(w) && matches!(self.screen, Screen::Exam) {
                    self.violation("Exam window lost focus");
                }
                if self.mode == Mode::Fallback {
                    lockdown::clip_cursor(w);
                }
            }
            let blocked = lockdown::take_blocked();
            if blocked > 0 && matches!(self.screen, Screen::Exam) {
                self.violation(&format!("Blocked system shortcut ×{blocked}"));
            }
        } else {
            let focused = ctx.input(|i| i.focused);
            if !focused && !self.focus_lost && matches!(self.screen, Screen::Exam) {
                self.violation("Switched away from the exam");
            }
            self.focus_lost = !focused;
        }

        if self.last_push.elapsed() > Duration::from_secs(2) {
            self.last_push = Instant::now();
            let total = self.at.as_ref().map(|a| a.order.len()).unwrap_or(self.pkg.header.question_count);
            let status = LiveStatus {
                phase: match self.screen {
                    Screen::Welcome => Phase::Waiting,
                    Screen::Exam | Screen::Grading => Phase::Working,
                    Screen::Finished => Phase::Submitted,
                },
                current: self.at.as_ref().map(|a| a.current).unwrap_or(0),
                answered: self.at.as_ref().map(|a| a.answers.values().filter(|x| x.is_answered()).count()).unwrap_or(0),
                total,
                remaining_secs: self.remaining(),
                violations: self.at.as_ref().map(|a| a.violations.len() as u32).unwrap_or(0),
            };
            self.lan.status(status);
            if self.dirty {
                if let Some(at) = &self.at {
                    self.lan.answers(at.answers.clone(), at.order.clone());
                }
            }
        }
        if self.dirty && self.last_save.elapsed() > Duration::from_secs(3) {
            if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
                storage::attempt_save(&exam.id, &self.key, at);
            }
            self.last_save = Instant::now();
            self.dirty = false;
        }
    }
}

fn new_attempt(exam: &Exam, name: String, sid: String) -> Attempt {
    let mut rng = rand::thread_rng();
    let mut order: Vec<u32> = exam.questions.iter().map(|q| q.id).collect();
    if exam.settings.shuffle_questions || exam.settings.pick > 0 {
        if exam.settings.pick > 0 {
            order.shuffle(&mut rng);
            order.truncate(exam.delivered_count());
            // Keep the teacher's order inside the random subset unless shuffling was asked for.
            if !exam.settings.shuffle_questions {
                order.sort_by_key(|id| exam.questions.iter().position(|q| q.id == *id));
            }
        } else {
            order.shuffle(&mut rng);
        }
    }
    let mut option_orders = BTreeMap::new();
    for q in &exam.questions {
        if let Kind::Choice { options, .. } = &q.kind {
            let mut idx: Vec<usize> = (0..options.len()).collect();
            if exam.settings.shuffle_options {
                idx.shuffle(&mut rng);
            }
            option_orders.insert(q.id, idx);
        }
    }
    let now = now_unix();
    Attempt {
        id: crate::crypto::random_hex(12),
        name,
        sid,
        order,
        option_orders,
        answers: BTreeMap::new(),
        flags: BTreeSet::new(),
        current: 0,
        started_at: now,
        deadline: now + exam.duration_min as u64 * 60,
        violations: vec![],
        submitted: None,
        graded: false,
    }
}

// ───────────────────────────── rendering ─────────────────────────────

impl crate::ui::Root for Player {
    fn root(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        self.web.begin();
        #[cfg(debug_assertions)]
        if crate::ui::shot::enabled() {
            self.shot_script();
            crate::ui::shot::tick(ctx);
        }
        self.tick(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.locked() {
                self.violation("Tried to close the exam");
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(Key::F12)) {
            self.proctor = Some((String::new(), None));
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal().bg)).show_inside(ui, |ui| match self.screen {
            Screen::Welcome => self.welcome(ui),
            Screen::Exam => self.exam_screen(ui),
            Screen::Grading => grading_screen(ui),
            Screen::Finished => self.finished(ui),
        });

        self.modals(ctx);
        if self.confirm_submit || self.proctor.is_some() || self.teacher_msg.is_some() {
            self.web.begin(); // dialogs must not sit under the native preview window
        }
        let title = self.window_title.clone();
        self.web.end(&title, ctx);
        ctx.request_repaint_after(Duration::from_millis(if self.locked() { 250 } else { 500 }));
    }

    fn exit(&mut self) {
        self.finalize();
    }
}

impl Player {
    /// Saves the attempt and, if requested, schedules the exam file's removal. Safe to call twice.
    fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
            storage::attempt_save(&exam.id, &self.key, at);
        }
        let auto = self.exam.as_ref().is_some_and(|e| e.settings.remove_after_submit) && self.at.as_ref().is_some_and(|a| a.submitted.is_some());
        if self.remove_requested || auto {
            if let Some(exam) = &self.exam {
                storage::attempt_remove(&exam.id);
            }
            let exe = std::env::current_exe().unwrap_or_default();
            lockdown::request_removal(&exe);
            if self.mode == Mode::Windowed {
                lockdown::perform_requested_removal();
            }
        }
        lockdown::release_cursor();
    }
}

fn grading_screen(ui: &mut egui::Ui) {
    let p = pal();
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.38);
        ui.add(egui::Spinner::new().size(28.0).color(p.accent));
        ui.add_space(14.0);
        ui.label(theme::heading("Submitting", 20.0));
        ui.label(theme::muted("Running the hidden tests for your code answers."));
    });
}

/// Connection state. `compact` is the short form for the top bar; details are in the tooltip.
fn conn_status(ui: &mut egui::Ui, c: &Conn, compact: bool) -> egui::Response {
    let p = pal();
    let (col, text, busy) = if compact {
        match c {
            Conn::Searching => (p.warning, "Searching…".to_string(), false),
            Conn::Found(_) => (p.success, "Teacher found".to_string(), false),
            Conn::Connecting(_) => (p.warning, "Connecting…".to_string(), false),
            Conn::Connected(_) => (p.success, "Connected".to_string(), false),
            Conn::Offline(_) => (p.muted, "Offline".to_string(), false),
        }
    } else {
        match c {
        Conn::Searching => (p.warning, "Looking for your teacher…".to_string(), true),
        Conn::Found(ip) => (p.success, format!("Teacher found · {ip}"), false),
        Conn::Connecting(ip) => (p.warning, format!("Connecting to {ip}…"), true),
        Conn::Connected(ip) => (p.success, format!("Connected · {ip}"), false),
        Conn::Offline(_) => (p.muted, "Offline · saved on this PC".to_string(), false),
        }
    };
    let r = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if busy {
            ui.add(egui::Spinner::new().size(14.0).color(col));
            ui.label(RichText::new(&text).font(theme::mono(12.0)).color(col));
        } else {
            theme::status_dot(ui, col, &text);
        }
    });
    r.response.on_hover_text(match c {
        Conn::Searching => "Searching this network for the teacher's HDJLabs Studio. You can start without waiting — results are saved on this PC too.".to_string(),
        Conn::Found(_) => "Your teacher's Studio is on this network. You'll join the live session as soon as you start the exam.".to_string(),
        Conn::Connecting(ip) => format!("Opening an encrypted connection to {ip}."),
        Conn::Connected(_) => "Your teacher can see your progress, and your result is sent automatically.".to_string(),
        Conn::Offline(e) => format!("Not connected: {e}. Keep working — your result is saved to a file your teacher can collect."),
    })
}

impl Player {
    fn welcome(&mut self, ui: &mut egui::Ui) {
        let h = self.pkg.header.clone();
        theme::paint_grid(ui, ui.max_rect());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let w = ui.available_width();
            // Side by side on normal screens; stacked (form first) on small ones like 800×600.
            let narrow = w < 900.0;
            let margin = if narrow { 24.0 } else { (w * 0.08).clamp(24.0, 120.0) };
            let form_w: f32 = if narrow { (w - 2.0 * margin).min(420.0) } else { 380.0 };
            let gap = if narrow { 28.0 } else { 56.0 };
            let left_w = if narrow { w - 2.0 * margin } else { (w - 2.0 * margin - form_w - gap).clamp(300.0, 620.0) };
            ui.add_space(if narrow { 20.0 } else { (ui.available_height() * 0.07).max(28.0) });
            let layout = if narrow { Layout::top_down(Align::Min) } else { Layout::left_to_right(Align::Min) };
            egui::Frame::new().inner_margin(Margin { left: margin as i8, right: 0, top: 0, bottom: 24 }).show(ui, |ui| ui.with_layout(layout, |ui| {
                if narrow {
                    self.welcome_form(ui, &h, form_w, narrow);
                    ui.add_space(gap);
                    self.welcome_brief(ui, &h, left_w);
                } else {
                    self.welcome_brief(ui, &h, left_w);
                    ui.add_space(gap);
                    self.welcome_form(ui, &h, form_w, narrow);
                }
            }));
        });
    }

    fn welcome_brief(&mut self, ui: &mut egui::Ui, h: &crate::payload::Header, left_w: f32) {
        let p = pal();
                // Exam brief.
                ui.allocate_ui_with_layout(egui::vec2(left_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    theme::brand(ui, "Secure exam");
                    ui.add_space(40.0);
                    ui.label(theme::eyebrow(if self.locked() { "Locked exam session" } else { "Practice session" }));
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(theme::heading(&rtl::line(&h.title), 34.0)).wrap());
                    if !h.author.is_empty() {
                        ui.label(RichText::new(&h.author).size(15.0).color(p.muted));
                    }
                    ui.add_space(22.0);
                    egui::Frame::new().stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(theme::R_PANEL)).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::Grid::new("spec").num_columns(2).spacing([28.0, 6.0]).min_row_height(22.0).striped(false).show(ui, |ui| {
                            for (k, v) in [
                                ("Duration", format!("{} min", h.duration_min)),
                                ("Questions", h.question_count.to_string()),
                                ("Points", theme::fmt_points(h.total_points)),
                            ] {
                                ui.label(theme::eyebrow(k));
                                ui.label(theme::figure(v, 14.0, p.text));
                                ui.end_row();
                            }
                            ui.label(theme::eyebrow("Network"));
                            conn_status(ui, &self.lan.conn(), false);
                            ui.end_row();
                        });
                    });
                    if !h.instructions.trim().is_empty() {
                        ui.add_space(22.0);
                        ui.label(theme::eyebrow("Instructions"));
                        ui.add(egui::Label::new(RichText::new(&h.instructions).size(15.0).color(p.text)).wrap());
                    }
                    if self.locked() {
                        ui.add_space(22.0);
                        ui.label(theme::eyebrow("While the exam runs"));
                        for line in [
                            "Other apps, the taskbar and notifications are hidden.",
                            "System shortcuts and touchpad gestures are switched off.",
                            "Screenshots and screen sharing capture a blank window.",
                            "Everything returns to normal when you submit.",
                        ] {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("—").font(theme::mono(13.0)).color(p.faint));
                                ui.label(RichText::new(line).color(p.muted));
                            });
                        }
                    }
                });
    }

    fn welcome_form(&mut self, ui: &mut egui::Ui, h: &crate::payload::Header, form_w: f32, narrow: bool) {
        let p = pal();
                // Sign-in panel.
                ui.allocate_ui_with_layout(egui::vec2(form_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    if !narrow {
                        ui.add_space(70.0);
                    }
                    theme::card_frame().inner_margin(Margin::same(24)).show(ui, |ui| {
                        ui.set_width(form_w - 50.0);
                        ui.label(theme::eyebrow("Sign in"));
                        ui.label(theme::heading("Start the exam", 21.0));
                        ui.add_space(10.0);
                        let busy = self.unlocking.is_some();
                        theme::field_label(ui, "Full name");
                        let name_resp = theme::text_field(ui, &mut self.name, "As on the class list", f32::INFINITY);
                        if self.name.is_empty() && !busy && ui.memory(|m| m.focused().is_none()) {
                            name_resp.request_focus();
                        }
                        theme::field_label(ui, "Student ID");
                        theme::text_field(ui, &mut self.sid, "Optional", f32::INFINITY);
                        let mut enter = false;
                        if h.password_required {
                            theme::field_label(ui, "Exam password");
                            let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text("Given by your teacher").desired_width(theme::fill(&ui, 10)).margin(Margin::symmetric(10, 8)));
                            enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        }
                        if let Some(e) = &self.error {
                            ui.label(RichText::new(e).color(p.danger));
                        }
                        let cooldown = self.cooldown_until.map(|t| t.saturating_duration_since(Instant::now()).as_secs()).filter(|s| *s > 0);
                        if let Some(s) = cooldown {
                            ui.label(RichText::new(format!("Too many attempts. Try again in {s} s.")).color(p.warning));
                        }
                        ui.add_space(8.0);
                        let label = if busy { "Checking password…".to_string() } else { format!("Start exam   {}", icon::FORWARD) };
                        let start = ui.add_enabled(
                            !busy && cooldown.is_none(),
                            egui::Button::new(RichText::new(label).font(theme::semibold(15.0)).color(p.on_accent)).fill(theme::accent_fill(ui)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(ui.available_width(), 40.0)),
                        );
                        if (start.clicked() || enter) && !busy {
                            self.begin_unlock();
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(theme::muted(format!("{}  Teacher address", icon::NETWORK)).size(13.0)).frame_when_inactive(false)).clicked() {
                                self.show_net = !self.show_net;
                                self.manual_edit = self.manual_addr.lock().unwrap().clone();
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(theme::muted("Leave").size(13.0)).frame_when_inactive(false)).on_hover_text("Close without starting the exam").clicked() {
                                    self.allow_close = true;
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                            });
                        });
                        if self.show_net {
                            ui.separator();
                            ui.label(theme::muted("Found automatically on the same network. If not, type the address shown on the teacher's Live screen.").size(12.5));
                            ui.horizontal(|ui| {
                                theme::text_field(ui, &mut self.manual_edit, "192.168.1.20", 200.0);
                                if theme::ghost(ui, "Connect").clicked() {
                                    *self.manual_addr.lock().unwrap() = self.manual_edit.trim().to_string();
                                }
                            });
                        }
                    });
                });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let remaining = self.remaining();
        let title = self.exam.as_ref().map(|e| e.title.clone()).unwrap_or_default();
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).inner_margin(Margin::symmetric(18, 9)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(260.0, 34.0), Layout::left_to_right(Align::Center), |ui| theme::brand(ui, &title));
                ui.add_space(12.0);
                let tcol = if remaining < 60 { p.danger } else if remaining < 300 { p.warning } else { p.text };
                egui::Frame::new().stroke(Stroke::new(1.0_f32, if remaining < 300 { tcol.gamma_multiply(0.7) } else { p.border })).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 5)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::CLOCK).size(13.0).color(p.muted));
                        ui.label(theme::figure(theme::fmt_secs(remaining), 19.0, tcol));
                        ui.label(theme::eyebrow("left"));
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::primary(ui, &format!("{}  Submit", icon::SEND)).clicked() {
                        self.confirm_submit = true;
                    }
                    ui.add_space(4.0);
                    if self.locked() && theme::icon_button(ui, icon::LOCK, "Proctor (teacher only) — Ctrl+Shift+F12").clicked() {
                        self.proctor = Some((String::new(), None));
                    }
                    let light = theme::is_light();
                    if theme::icon_button(ui, if light { icon::MOON } else { icon::SUN }, "Switch theme").clicked() {
                        theme::set_light(ui.ctx(), !light);
                    }
                    ui.separator();
                    if let Some(at) = &self.at {
                        ui.label(RichText::new(&at.name).color(p.text));
                    }
                    ui.separator();
                    conn_status(ui, &self.lan.conn(), true);
                });
            });
        });
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let Some(at) = self.at.as_ref() else { return };
        let total = at.order.len();
        let answered = at.order.iter().filter(|id| at.answers.get(id).is_some_and(|a| a.is_answered())).count();
        let mut goto = None;
        ui.add_space(16.0);
        ui.label(theme::eyebrow("Questions"));
        ui.horizontal(|ui| {
            ui.label(theme::figure(format!("{answered}/{total}"), 18.0, p.text));
            ui.label(theme::muted("answered").size(13.0));
        });
        theme::progress_bar(ui, answered as f32 / total.max(1) as f32, 3.0, p.accent);
        ui.add_space(10.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 70.0).show(ui, |ui| {
            let per_row = ((ui.available_width() + 6.0) / 42.0).floor().max(1.0) as usize;
            for chunk in at.order.chunks(per_row).enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (j, qid) in chunk.1.iter().enumerate() {
                        let idx = chunk.0 * per_row + j;
                        let done = at.answers.get(qid).is_some_and(|a| a.is_answered());
                        let flagged = at.flags.contains(qid);
                        let current = idx == at.current;
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::click());
                        let hov = resp.hovered();
                        let (fill, stroke, text) = if current {
                            (if hov { p.accent_hover } else { p.accent }, p.accent, p.on_accent)
                        } else if done {
                            (if hov { p.hover } else { p.accent_soft }, if hov { p.accent } else { p.accent.gamma_multiply(0.45) }, p.accent)
                        } else if hov {
                            (p.hover, p.accent.gamma_multiply(0.7), p.text)
                        } else {
                            (Color32::TRANSPARENT, p.border, p.muted)
                        };
                        let painter = ui.painter();
                        painter.rect(rect, CornerRadius::same(theme::R_CONTROL), fill, Stroke::new(1.0_f32, stroke), egui::StrokeKind::Inside);
                        painter.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{:02}", idx + 1), theme::mono(13.0), text);
                        if flagged {
                            painter.rect_filled(egui::Rect::from_min_size(rect.right_top() + egui::vec2(-9.0, 3.0), egui::vec2(6.0, 6.0)), CornerRadius::same(1), p.warning);
                        }
                        let label = format!("Question {}{}{}", idx + 1, if done { " — answered" } else { "" }, if flagged { " — flagged" } else { "" });
                        if resp.on_hover_text(label).clicked() {
                            goto = Some(idx);
                        }
                    }
                });
            }
        });
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                legend(ui, p.accent, "Answered");
                legend(ui, p.warning, "Flagged");
            });
        });
        if let Some(i) = goto {
            self.at.as_mut().unwrap().current = i;
            self.dirty = true;
        }
    }

    fn exam_screen(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        egui::Panel::top("exam-top").frame(egui::Frame::new()).show_inside(ui, |ui| self.top_bar(ui));
        egui::Panel::left("exam-rail").exact_size(200.0).resizable(false).frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::symmetric(16, 0)).stroke(Stroke::new(1.0_f32, p.border))).show_inside(ui, |ui| self.rail(ui));

        let Some(at) = self.at.as_ref() else { return };
        let idx = at.current.min(at.order.len().saturating_sub(1));
        let total = at.order.len();
        let Some(q) = self.question(idx).cloned() else { return };
        let flagged = at.flags.contains(&q.id);

        egui::Panel::bottom("exam-nav").frame(egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).inner_margin(Margin::symmetric(24, 10))).show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(idx > 0, egui::Button::new(format!("{}  Previous", icon::BACK)).fill(theme::clear_fill(ui)).min_size(egui::vec2(0.0, 34.0))).clicked() {
                    self.at.as_mut().unwrap().current = idx - 1;
                }
                let flag_txt = if flagged { format!("{}  Flagged", icon::FLAG) } else { format!("{}  Flag for review", icon::FLAG) };
                let b = egui::Button::new(RichText::new(flag_txt).color(if flagged { p.warning } else { p.text })).fill(if flagged { p.warning.gamma_multiply(0.10) } else { theme::clear_fill(ui) }).stroke(theme::outline(ui, if flagged { p.warning.gamma_multiply(0.6) } else { p.border })).min_size(egui::vec2(0.0, 34.0));
                if ui.add(b).clicked() {
                    let f = &mut self.at.as_mut().unwrap().flags;
                    if !f.remove(&q.id) {
                        f.insert(q.id);
                    }
                    self.dirty = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if idx + 1 < total {
                        if theme::primary(ui, &format!("Next   {}", icon::FORWARD)).clicked() {
                            self.at.as_mut().unwrap().current = idx + 1;
                        }
                    } else if theme::primary(ui, &format!("{}  Review and submit", icon::CHECK)).clicked() {
                        self.confirm_submit = true;
                    }
                    ui.label(theme::figure(format!("{:02} / {:02}", idx + 1, total), 13.0, p.muted));
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(28, 22))).show_inside(ui, |ui| {
            if let Kind::Code(spec) = &q.kind {
                self.code_question(ui, &q, spec, idx);
            } else {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.set_max_width(ui.available_width().min(820.0));
                        ui.add_space(8.0);
                        question_header(ui, &q, idx);
                        ui.add_space(8.0);
                        rtl::label(ui, &q.prompt, egui::FontId::proportional(19.0), p.text);
                        ui.add_space(20.0);
                        self.answer_widget(ui, &q);
                    });
                });
            }
        });
    }

    fn answer_widget(&mut self, ui: &mut egui::Ui, q: &Question) {
        let p = pal();
        let locked = self.pkg.header.lockdown;
        let at = self.at.as_mut().unwrap();
        let mut changed = false;
        match &q.kind {
            Kind::Choice { options, multiple, .. } => {
                if *multiple {
                    ui.label(theme::muted("Select all that apply."));
                }
                let order = at.option_orders.get(&q.id).cloned().unwrap_or_else(|| (0..options.len()).collect());
                let mut picked = match at.answers.get(&q.id) {
                    Some(Answer::Choice(v)) => v.clone(),
                    _ => vec![],
                };
                for (pos, &oi) in order.iter().enumerate() {
                    let Some(text) = options.get(oi) else { continue };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let on = picked.contains(&oi);
                    let letter = (b'A' + pos as u8) as char;
                    if option_row(ui, letter, text, on, *multiple).clicked() {
                        if *multiple {
                            if on {
                                picked.retain(|x| *x != oi);
                            } else {
                                picked.push(oi);
                            }
                        } else {
                            picked = vec![oi];
                        }
                        changed = true;
                    }
                }
                if changed {
                    at.answers.insert(q.id, Answer::Choice(picked));
                }
            }
            Kind::TrueFalse { .. } => {
                let cur = match at.answers.get(&q.id) {
                    Some(Answer::Bool(b)) => Some(*b),
                    _ => None,
                };
                ui.horizontal(|ui| {
                    for (val, label) in [(true, "True"), (false, "False")] {
                        let on = cur == Some(val);
                        let (fill, stroke, col) = if on { (p.accent, p.accent, p.on_accent) } else if theme::will_hover(ui) { (p.hover, p.border_strong, p.text) } else { (p.surface, p.border_strong, p.text) };
                        let b = egui::Button::new(RichText::new(label).font(theme::semibold(16.0)).color(col)).fill(fill).stroke(theme::outline(ui, stroke)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(160.0, 52.0));
                        if ui.add(b).clicked() {
                            at.answers.insert(q.id, Answer::Bool(val));
                            changed = true;
                        }
                    }
                });
            }
            Kind::Short { accepted, show_format, .. } => {
                let mut text = match at.answers.get(&q.id) {
                    Some(Answer::Text(t)) => t.clone(),
                    _ => String::new(),
                };
                let format = show_format.then(|| accepted.iter().find(|a| !a.trim().is_empty()).map(|a| answer_format(a))).flatten();
                let font = theme::mono(16.0);
                let hint = if format.is_some() { String::new() } else { "Type your answer".into() };
                let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(RichText::new(hint).font(font.clone()).color(p.faint)).font(font.clone()).desired_width(theme::fill(&ui, 14)).margin(Margin::symmetric(14, 12)));
                if let Some(f) = &format {
                    // TryHackMe style: every typed character uses up one slot of the pattern and the
                    // rest stays visible right after the cursor — "TH" over "***{****}" shows "TH*{****}".
                    let typed = text.chars().count();
                    let rest: String = f.chars().skip(typed).collect();
                    if !rest.is_empty() {
                        let typed_w = ui.painter().layout_no_wrap(text.clone(), font.clone(), p.text).size().x;
                        let pos = egui::pos2(r.rect.left() + 14.0 + typed_w, r.rect.center().y);
                        ui.painter().with_clip_rect(r.rect.shrink(2.0)).text(pos, egui::Align2::LEFT_CENTER, rest, font.clone(), p.faint);
                    }
                }
                if r.changed() {
                    at.answers.insert(q.id, Answer::Text(text.chars().take(2000).collect()));
                    changed = true;
                }
            }
            Kind::Upload { extensions, max_mb } => {
                let cur = match at.answers.get(&q.id) {
                    Some(Answer::File { name, size, .. }) => Some((name.clone(), *size)),
                    _ => None,
                };
                let types = if extensions.is_empty() { "Any file".to_string() } else { extensions.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join("  ") };
                let limit = max_mb.min(MAX_UPLOAD_MB);
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::same(16)).show(ui, |ui| {
                    ui.set_width(theme::fill(ui, 0));
                    if locked {
                        ui.label(RichText::new("File uploads are turned off while the exam is locked. Ask your teacher.").color(p.warning));
                        return;
                    }
                    match &cur {
                        Some((name, size)) => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon::DOC).size(20.0).color(p.accent));
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(name).font(theme::semibold(15.0)));
                                    ui.label(theme::muted(fmt_size(*size)).size(12.5));
                                });
                            });
                        }
                        None => {
                            ui.label(theme::muted("No file attached yet."));
                        }
                    }
                    ui.label(theme::muted(format!("{types} · up to {} MB", theme::fmt_points(limit))).size(12.5));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if theme::primary(ui, &format!("{}  {}", icon::OPEN, if cur.is_some() { "Replace file" } else { "Choose file" })).clicked() {
                            let mut dlg = rfd::FileDialog::new().set_title("Attach a file");
                            if !extensions.is_empty() {
                                dlg = dlg.add_filter("Allowed files", &extensions.iter().map(String::as_str).collect::<Vec<_>>());
                            }
                            if let Some(path) = dlg.pick_file() {
                                let others: u64 = at.answers.iter().filter(|(k, _)| **k != q.id).map(|(_, a)| if let Answer::File { size, .. } = a { *size } else { 0 }).sum();
                                let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                let mb = |b: u64| b as f32 / (1024.0 * 1024.0);
                                self.upload_error = match std::fs::metadata(&path) {
                                    Err(e) => Some(e.to_string()),
                                    Ok(_) if !extensions.is_empty() && !extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) => Some(format!("That file type isn't allowed here ({types}).")),
                                    Ok(m) if mb(m.len()) > limit => Some(format!("{name} is {} — the limit is {} MB.", fmt_size(m.len()), theme::fmt_points(limit))),
                                    Ok(m) if mb(m.len() + others) > MAX_UPLOAD_MB => Some(format!("All your files together must stay under {} MB.", theme::fmt_points(MAX_UPLOAD_MB))),
                                    Ok(_) => match std::fs::read(&path) {
                                        Ok(bytes) => {
                                            at.answers.insert(q.id, Answer::File { name, size: bytes.len() as u64, data: crate::crypto::b64(&bytes) });
                                            changed = true;
                                            None
                                        }
                                        Err(e) => Some(e.to_string()),
                                    },
                                };
                            }
                        }
                        if cur.is_some() && theme::ghost(ui, &format!("{}  Remove", icon::DELETE)).clicked() {
                            at.answers.remove(&q.id);
                            changed = true;
                        }
                    });
                    if let Some(e) = &self.upload_error {
                        ui.label(RichText::new(e).color(p.danger));
                    }
                });
            }
            Kind::Code(_) => {}
        }
        if changed {
            self.dirty = true;
        }
    }

    fn code_question(&mut self, ui: &mut egui::Ui, q: &Question, spec: &CodeSpec, idx: usize) {
        let p = pal();
        let at = self.at.as_mut().unwrap();
        let mut source = match at.answers.get(&q.id) {
            Some(Answer::Code { source, .. }) => source.clone(),
            _ => spec.starter.clone(),
        };
        let cu = self.code.entry(q.id).or_default();
        let running = cu.running.is_some();
        let tool = runner::tool(spec.lang);
        let mut run_req = false;
        let mut tests_req = false;
        let avail = ui.available_size();
        let left_w = (avail.x * 0.34).clamp(260.0, 460.0);

        ui.horizontal_top(|ui| {
            // Problem statement.
            ui.allocate_ui_with_layout(egui::vec2(left_w, avail.y), Layout::top_down(Align::Min), |ui| {
                theme::card_frame().show(ui, |ui| {
                    ui.set_min_height(avail.y - 42.0);
                    ui.set_width(left_w - 42.0);
                    egui::ScrollArea::vertical().id_salt("prompt").auto_shrink([false, false]).show(ui, |ui| {
                        question_header(ui, q, idx);
                        ui.add_space(4.0);
                        rtl::label(ui, &q.prompt, egui::FontId::proportional(16.0), p.text);
                        if spec.lang == Lang::Sql && !spec.setup.trim().is_empty() {
                            ui.add_space(12.0);
                            ui.label(theme::eyebrow("Database"));
                            let rows = spec.setup.lines().count().clamp(3, 14) as f32;
                            ui.allocate_ui(egui::vec2(ui.available_width(), rows * 18.0 + 20.0), |ui| {
                                let mut setup = spec.setup.clone();
                                editor::code_editor(ui, ("setup-view", q.id), &mut setup, Lang::Sql, 12.5, true);
                            });
                        }
                        if spec.lang == Lang::Web {
                            ui.add_space(12.0);
                            ui.add(egui::Label::new(theme::muted("Your page appears live under the editor as you type. Your teacher grades the finished page.").size(13.0)).wrap());
                        }
                        let visible: Vec<&TestCase> = spec.tests.iter().filter(|t| !t.hidden && !t.expected.trim().is_empty()).collect();
                        if !visible.is_empty() {
                            ui.add_space(12.0);
                            ui.label(theme::eyebrow(if visible.len() == 1 { "Expected result" } else { "Examples" }));
                            for (i, t) in visible.iter().enumerate() {
                                egui::Frame::new().fill(p.code_bg).stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(theme::R_CONTROL)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    if !t.input.trim().is_empty() {
                                        ui.label(theme::eyebrow(if visible.len() > 1 { format!("Input {}", i + 1) } else { "Input".into() }));
                                        ui.add(egui::Label::new(RichText::new(&t.input).font(theme::mono(13.0))).wrap());
                                        ui.add_space(4.0);
                                    }
                                    ui.label(theme::eyebrow("Output"));
                                    ui.add(egui::Label::new(RichText::new(&t.expected).font(theme::mono(13.0))).wrap());
                                });
                            }
                        }
                        let hidden = spec.tests.iter().filter(|t| t.hidden).count();
                        if hidden > 0 {
                            ui.add_space(6.0);
                            ui.label(theme::muted(format!("{}  {hidden} hidden test(s) also run when you submit.", icon::INFO)).size(12.5));
                        }
                    });
                });
            });

            // IDE.
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), avail.y), Layout::top_down(Align::Min), |ui| {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0_f32, p.border)).corner_radius(CornerRadius::same(14)).inner_margin(Margin::same(10)).show(ui, |ui| {
                    ui.set_min_height(avail.y - 22.0);
                    // Toolbar. Secondary items drop out when space is short, so nothing overlaps.
                    let bar_w = ui.available_width();
                    ui.horizontal(|ui| {
                        ui.label(theme::figure(spec.lang.file_name(), 13.0, p.text));
                        if bar_w > 640.0 {
                            ui.label(theme::muted(tool.as_ref().map(|t| t.label.clone()).unwrap_or_else(|| "not installed".into())).size(12.5));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if spec.lang.is_preview_only() {
                                if ui.add(egui::Button::new(format!("{}  Reload preview", icon::PLAY)).min_size(egui::vec2(0.0, 30.0))).clicked() {
                                    self.web.reload();
                                }
                            } else {
                                let run = ui.add_enabled(!running && tool.is_some(), egui::Button::new(RichText::new(format!("{}  Run", icon::PLAY)).font(theme::semibold(14.0)).color(p.on_accent)).fill(theme::accent_fill(ui)).corner_radius(CornerRadius::same(theme::R_CONTROL)).min_size(egui::vec2(80.0, 30.0))).on_hover_text("Run your code  (F5)");
                                run_req |= run.clicked();
                            }
                            let has_tests = spec.tests.iter().any(|t| !t.hidden) && !spec.lang.is_preview_only();
                            if has_tests && ui.add_enabled(!running && tool.is_some(), egui::Button::new(format!("{}  Run tests", icon::TESTS)).fill(theme::clear_fill(ui)).min_size(egui::vec2(0.0, 30.0))).on_hover_text("Check against the examples  (Ctrl+Enter)").clicked() {
                                tests_req = true;
                            }
                            if running {
                                ui.add(egui::Spinner::new().color(p.accent));
                            }
                            if bar_w > 520.0 {
                                ui.separator();
                                if theme::icon_button(ui, icon::UNDO, "Reset to starter code").clicked() {
                                    source = spec.starter.clone();
                                    at.answers.insert(q.id, Answer::Code { lang: spec.lang, source: source.clone() });
                                }
                                if theme::icon_button(ui, icon::ZOOM_IN, "Larger text").clicked() {
                                    self.font = (self.font + 1.0).min(26.0);
                                }
                                if theme::icon_button(ui, icon::ZOOM_OUT, "Smaller text").clicked() {
                                    self.font = (self.font - 1.0).max(10.0);
                                }
                            }
                        });
                    });
                    run_req |= ui.input(|i| i.key_pressed(Key::F5));
                    tests_req |= ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Enter));

                    let panel_h = if spec.lang.is_preview_only() { (ui.available_height() * 0.5).max(200.0) } else { 190.0 };
                    let editor_h = (ui.available_height() - panel_h - 34.0).max(160.0);
                    ui.allocate_ui(egui::vec2(ui.available_width(), editor_h), |ui| {
                        ui.set_min_height(editor_h);
                        let out = editor::code_editor(ui, ("code", q.id), &mut source, spec.lang, self.font, false);
                        if out.changed {
                            at.answers.insert(q.id, Answer::Code { lang: spec.lang, source: source.clone() });
                            self.dirty = true;
                        }
                        cu.line = out.line;
                        cu.col = out.col;
                    });

                    if spec.lang.is_preview_only() {
                        ui.horizontal(|ui| {
                            ui.label(theme::eyebrow("Live preview"));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(theme::figure(format!("Ln {}, Col {}", cu.line, cu.col), 11.5, p.muted));
                            });
                        });
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), (ui.available_height() - 4.0).max(120.0)), egui::Sense::hover());
                        ui.painter().rect(rect, CornerRadius::same(theme::R_CONTROL), egui::Color32::WHITE, Stroke::new(1.0_f32, p.border), egui::StrokeKind::Outside);
                        if let Some(err) = self.web.request(rect, &source) {
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Preview unavailable on this PC", theme::semibold(14.0), egui::Color32::from_rgb(60, 60, 60));
                            ui.put(egui::Rect::from_min_size(rect.min + egui::vec2(16.0, rect.height() / 2.0 + 16.0), egui::vec2(rect.width() - 32.0, 60.0)), egui::Label::new(RichText::new(err).size(12.0).color(egui::Color32::from_rgb(90, 90, 90))).wrap());
                        }
                    } else {
                    let tabs: &[(BottomTab, &str)] = if spec.lang == Lang::Sql { &[(BottomTab::Output, "Result"), (BottomTab::Tests, "Tests")] } else { &[(BottomTab::Output, "Output"), (BottomTab::Input, "Input (stdin)"), (BottomTab::Tests, "Tests")] };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 18.0;
                        for &(tab, label) in tabs {
                            let on = cu.tab == tab;
                            let txt = RichText::new(label.to_uppercase()).font(theme::mono(11.5)).extra_letter_spacing(0.8).color(if on { p.text } else { p.muted });
                            let r = ui.add(egui::Button::new(txt).frame_when_inactive(false));
                            if on {
                                let rr = r.rect;
                                ui.painter().line_segment([rr.left_bottom() + egui::vec2(4.0, 2.0), rr.right_bottom() + egui::vec2(-4.0, 2.0)], Stroke::new(2.0_f32, p.accent));
                            }
                            if r.clicked() {
                                cu.tab = tab;
                            }
                        }
                        if ui.available_width() > 260.0 {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(theme::figure(format!("Ln {}, Col {}", cu.line, cu.col), 11.5, p.muted));
                            });
                        }
                    });
                    egui::Frame::new().fill(p.code_bg).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(10)).show(ui, |ui| {
                        ui.set_min_height(panel_h - 40.0);
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical().id_salt(("bottom", q.id)).max_height(panel_h - 40.0).auto_shrink([false, false]).stick_to_bottom(cu.tab == BottomTab::Output).show(ui, |ui| match cu.tab {
                            BottomTab::Input => {
                                ui.add(egui::TextEdit::multiline(&mut cu.stdin).hint_text("Text your program reads from input() / stdin").font(egui::FontId::monospace(13.5)).desired_width(theme::fill(&ui, 4)).desired_rows(6).frame(egui::Frame::NONE));
                            }
                            BottomTab::Output => match &cu.output {
                                None => {
                                    ui.label(theme::muted(format!("Press  {}  Run  (F5)  to execute your program.", icon::PLAY)));
                                }
                                Some(o) => run_output(ui, o),
                            },
                            BottomTab::Tests => match &cu.tests {
                                None => {
                                    ui.label(theme::muted("Run tests to compare your output with the examples."));
                                }
                                Some(t) => test_report(ui, t),
                            },
                        });
                    });
                    }
                });
            });
        });

        if (run_req || tests_req) && !running && tool.is_some() {
            let (tx, rx) = mpsc::channel();
            let lang = spec.lang;
            let src = source.clone();
            if tests_req {
                let spec = spec.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(RunResult::Tests(grading::run_tests(&spec, lang, &src, false)));
                });
            } else {
                let input = spec.stdin_for(&cu.stdin);
                std::thread::spawn(move || {
                    let _ = tx.send(RunResult::Run(runner::run(lang, &src, &input, runner::RUN_TIMEOUT)));
                });
            }
            cu.running = Some(rx);
        }
    }

    fn finished(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        theme::paint_grid(ui, ui.max_rect());
        let Some(sub) = self.at.as_ref().and_then(|a| a.submitted.clone()) else { return };
        let exam = self.exam.clone();
        let show_score = exam.as_ref().is_some_and(|e| e.settings.show_score);
        let show_review = exam.as_ref().is_some_and(|e| e.settings.show_review);
        let acked = self.lan.submit_acked();
        let graded = self.at.as_ref().is_some_and(|a| a.graded);
        // Hold the marks back while the teacher still has answers to grade (or asked to release them).
        let hold = (show_score || show_review) && !graded && (exam.as_ref().is_some_and(|e| e.settings.hold_scores || e.settings.keep_answers_off_pcs) || sub.results.iter().any(|r| r.status == Status::Review));
        let (show_score, show_review) = (show_score && !hold, show_review && !hold);
        if hold {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.set_max_width(640.0);
                theme::card(ui, |ui| {
                    ui.set_width(600.0);
                    ui.vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
                        ui.painter().rect(rect.shrink(8.0), CornerRadius::same(theme::R_PANEL), p.accent_soft, Stroke::new(1.0_f32, p.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon::CHECK, egui::FontId::proportional(22.0), p.accent);
                        ui.add_space(8.0);
                        ui.label(theme::heading("Exam submitted", 26.0));
                        ui.label(theme::muted(format!("{} · {}", rtl::line(&sub.student_name), rtl::line(&sub.exam_title))));
                        if sub.forced {
                            ui.label(RichText::new("Submitted automatically (time is up or the teacher ended the exam).").color(p.warning));
                        }
                        let auto = exam.as_ref().is_some_and(|e| e.settings.remove_after_submit);
                        if self.remove_requested || auto {
                            let left = match self.close_countdown {
                                Some(t) => 10i64 - t.elapsed().as_secs() as i64,
                                None if self.remove_requested => {
                                    self.close_countdown = Some(Instant::now());
                                    10
                                }
                                None => 99,
                            };
                            ui.label(RichText::new(if left <= 10 { format!("Your teacher is removing this exam from the PC. Closing in {} s.", left.max(0)) } else { "This exam file is removed from the PC when you close it.".into() }).color(p.warning));
                            if left <= 0 {
                                if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
                                    storage::attempt_save(&exam.id, &self.key, at);
                                }
                                self.allow_close = true;
                                self.finalize();
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            ui.ctx().request_repaint_after(Duration::from_millis(500));
                        }

                        if hold {
                            ui.add_space(18.0);
                            let online = matches!(self.lan.conn(), Conn::Connected(_));
                            ui.add(egui::Spinner::new().size(30.0).color(p.accent));
                            ui.add_space(6.0);
                            ui.label(RichText::new("Waiting for your teacher to finish grading").font(theme::semibold(16.0)).color(p.text));
                            ui.label(theme::muted(if online {
                                "Your score appears here as soon as it's ready. Keep this window open."
                            } else {
                                "Your teacher will share your score. You can close the exam now."
                            }));
                        }
                        if show_score {
                            ui.add_space(14.0);
                            if graded {
                                ui.label(RichText::new(format!("{}  Graded by your teacher", icon::CHECK)).font(theme::semibold(13.5)).color(p.success));
                            }
                            let pct = if sub.max_score > 0.0 { sub.score / sub.max_score * 100.0 } else { 0.0 };
                            ui.label(theme::figure(format!("{} / {}", theme::fmt_points(sub.score), theme::fmt_points(sub.max_score)), 40.0, p.text));
                            ui.label(theme::figure(format!("{pct:.0}%"), 14.0, p.muted));
                            if !graded && sub.results.iter().any(|r| r.status == Status::Review) {
                                ui.label(theme::muted("Some answers will be reviewed by your teacher — your final score may change."));
                            }
                        }
                    });
                    if show_review {
                        if let Some(exam) = &exam {
                            ui.add_space(12.0);
                            for (i, r) in sub.results.iter().enumerate() {
                                let q = exam.questions.iter().find(|q| q.id == r.qid);
                                let col = match r.status {
                                    Status::Correct => p.success,
                                    Status::Partial | Status::Review => p.warning,
                                    Status::Unanswered => p.muted,
                                    Status::Wrong => p.danger,
                                };
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(egui::vec2(116.0, 28.0), Layout::left_to_right(Align::Center), |ui| theme::pill(ui, r.status.label(), col));
                                    let prompt = q.map(|q| rtl::line(q.prompt.lines().next().unwrap_or(""))).unwrap_or_default();
                                    let w = (ui.available_width() - 70.0).max(60.0);
                                    ui.allocate_ui_with_layout(egui::vec2(w, 28.0), Layout::left_to_right(Align::Center), |ui| {
                                        ui.add(egui::Label::new(format!("Q{}  {prompt}", i + 1)).truncate());
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(theme::muted(format!("{} / {}", theme::fmt_points(r.earned), theme::fmt_points(r.max))));
                                    });
                                });
                            }
                        }
                    }
                    ui.add_space(16.0);
                    ui.separator();
                    if acked {
                        ui.label(RichText::new(format!("{}  Delivered to your teacher.", icon::CHECK)).color(p.success));
                    } else {
                        ui.label(RichText::new(format!("{}  Waiting to reach the teacher over the network…", icon::NETWORK)).color(p.warning));
                    }
                    if let Some(path) = self.saved_paths.first() {
                        ui.add(egui::Label::new(theme::muted(format!("A copy was saved to {}", path.parent().map(|d| d.display().to_string()).unwrap_or_default())).size(12.5)).wrap());
                    }
                    ui.add_space(10.0);
                    ui.vertical_centered(|ui| {
                        if theme::primary(ui, &format!("{}  Close exam", icon::CHECK)).clicked() {
                            if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
                                storage::attempt_save(&exam.id, &self.key, at);
                            }
                            self.allow_close = true;
                            self.finalize();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });
        });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        let p = pal();
        if self.confirm_submit {
            if let Some(at) = &self.at {
                let unanswered: Vec<usize> = at.order.iter().enumerate().filter(|(_, id)| !at.answers.get(id).is_some_and(|a| a.is_answered())).map(|(i, _)| i + 1).collect();
                let flagged: Vec<usize> = at.order.iter().enumerate().filter(|(_, id)| at.flags.contains(id)).map(|(i, _)| i + 1).collect();
                let total = at.order.len();
                let mut close = false;
                let mut go = false;
                egui::Modal::new(egui::Id::new("confirm-submit")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.label(theme::heading("Submit your exam?", 22.0));
                    ui.label(theme::muted("You can't change answers after submitting."));
                    ui.add_space(8.0);
                    ui.label(format!("Answered: {} of {total}", total - unanswered.len()));
                    if !unanswered.is_empty() {
                        ui.label(RichText::new(format!("Unanswered: {}", join_nums(&unanswered))).color(p.warning));
                    }
                    if !flagged.is_empty() {
                        ui.label(RichText::new(format!("Flagged: {}", join_nums(&flagged))).color(p.warning));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::ghost(ui, "Keep working").clicked() {
                            close = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::primary(ui, &format!("{}  Submit now", icon::SEND)).clicked() {
                                go = true;
                            }
                        });
                    });
                });
                if close || ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.confirm_submit = false;
                }
                if go {
                    self.submit(false);
                }
            }
        }

        if let Some(text) = self.teacher_msg.clone() {
            egui::Modal::new(egui::Id::new("teacher-msg")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(theme::heading(format!("{}  Message from your teacher", icon::MESSAGE), 20.0));
                ui.add_space(6.0);
                ui.label(RichText::new(text).size(16.0));
                ui.add_space(10.0);
                if theme::primary(ui, "OK").clicked() {
                    self.teacher_msg = None;
                }
            });
        }

        if let Some((mut pw, err)) = self.proctor.take() {
            let mut keep = true;
            let mut action = None;
            egui::Modal::new(egui::Id::new("proctor")).frame(theme::card_frame().inner_margin(Margin::same(24))).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(theme::heading(format!("{}  Proctor access", icon::LOCK), 20.0));
                ui.label(theme::muted("Teacher only. Enter the exit password set for this exam."));
                ui.add_space(6.0);
                let r = ui.add(egui::TextEdit::singleline(&mut pw).password(true).desired_width(theme::fill(&ui, 12)).margin(Margin::symmetric(12, 9)));
                r.request_focus();
                if let Some(e) = &err {
                    ui.label(RichText::new(e).color(p.danger));
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if theme::ghost(ui, "Cancel").clicked() {
                        keep = false;
                    }
                    if self.at.is_some() && matches!(self.screen, Screen::Exam) && theme::primary(ui, "Submit & close").clicked() {
                        action = Some(true);
                    }
                    if theme::danger(ui, "Close without submitting").clicked() {
                        action = Some(false);
                    }
                });
            });
            if let Some(submit) = action {
                if crate::crypto::verify_password(&pw, &self.pkg.header.exit_hash) {
                    keep = false;
                    self.lan.event(if submit { "Proctor: submitted and closed" } else { "Proctor: closed without submitting" });
                    if submit {
                        self.submit(true);
                    } else {
                        if let (Some(at), Some(exam)) = (&self.at, &self.exam) {
                            storage::attempt_save(&exam.id, &self.key, at);
                        }
                        self.allow_close = true;
                        self.finalize();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else {
                    self.violation("Wrong proctor password");
                    self.proctor = Some((String::new(), Some("Incorrect password.".into())));
                    return;
                }
            }
            if keep {
                self.proctor = Some((pw, err));
            }
        }
    }
}

fn join_nums(v: &[usize]) -> String {
    let s: Vec<String> = v.iter().take(30).map(|n| n.to_string()).collect();
    s.join(", ") + if v.len() > 30 { " …" } else { "" }
}

fn legend(ui: &mut egui::Ui, c: Color32, label: &str) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(1), c);
    ui.label(RichText::new(label).size(12.0).color(pal().muted));
}

fn question_header(ui: &mut egui::Ui, q: &Question, idx: usize) {
    let p = pal();
    ui.horizontal(|ui| {
        ui.label(theme::figure(format!("Q{:02}", idx + 1), 13.0, p.accent));
        ui.label(theme::eyebrow(q.kind.label()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(theme::figure(format!("{} pt{}", theme::fmt_points(q.points), if (q.points - 1.0).abs() < 0.01 { "" } else { "s" }), 12.5, p.muted));
        });
    });
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{:.0} KB", (bytes as f32 / 1024.0).max(1.0)) } else { format!("{:.1} MB", bytes as f32 / (1024.0 * 1024.0)) }
}

fn option_row(ui: &mut egui::Ui, letter: char, text: &str, on: bool, multiple: bool) -> egui::Response {
    let p = pal();
    let id = ui.next_auto_id();
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let (fill, stroke) = if on {
        (p.accent_soft, p.accent)
    } else if hovered {
        (p.hover, p.accent.gamma_multiply(0.7))
    } else {
        (p.surface, p.border)
    };
    let resp = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, stroke))
        .corner_radius(CornerRadius::same(theme::R_CONTROL))
        .inner_margin(Margin::symmetric(14, 11))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                let painter = ui.painter();
                let radius = CornerRadius::same(if multiple { theme::R_TAG } else { 12 });
                painter.rect(r, radius, if on { p.accent } else { Color32::TRANSPARENT }, Stroke::new(1.0_f32, if on { p.accent } else { p.border_strong }), egui::StrokeKind::Inside);
                if multiple && on {
                    painter.text(r.center(), egui::Align2::CENTER_CENTER, icon::CHECK, egui::FontId::proportional(12.0), p.on_accent);
                } else {
                    painter.text(r.center(), egui::Align2::CENTER_CENTER, letter, theme::mono(12.5), if on { p.on_accent } else { p.muted });
                }
                ui.add_space(4.0);
                rtl::label(ui, text, egui::FontId::proportional(15.5), p.text);
            });
        })
        .response;
    let resp = ui.interact(resp.rect, id, egui::Sense::click());
    ui.add_space(2.0);
    resp
}

pub fn run_output(ui: &mut egui::Ui, o: &RunOutput) {
    let p = pal();
    let col = if o.ok() { p.success } else if o.timed_out || o.compile_failed || o.unavailable || o.exit_code.is_some_and(|c| c != 0) { p.danger } else { p.muted };
    ui.label(RichText::new(o.summary()).size(12.5).color(col));
    if !o.stdout.is_empty() {
        ui.label(RichText::new(&o.stdout).monospace().color(p.text));
    }
    if !o.stderr.is_empty() {
        ui.label(RichText::new(&o.stderr).monospace().color(p.danger));
    }
    if o.stdout.is_empty() && o.stderr.is_empty() && o.ok() {
        ui.label(theme::muted("(no output)"));
    }
}

pub fn test_report(ui: &mut egui::Ui, tests: &[TestOutcome]) {
    let p = pal();
    let passed = tests.iter().filter(|t| t.passed).count();
    ui.label(RichText::new(format!("{passed} / {} tests passed", tests.len())).font(theme::semibold(14.0)).color(if passed == tests.len() { p.success } else { p.warning }));
    for (i, t) in tests.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(if t.passed { icon::CHECK } else { icon::CANCEL }).color(if t.passed { p.success } else { p.danger }));
            ui.label(RichText::new(format!("Test {}{}", i + 1, if t.hidden { " (hidden)" } else { "" })).strong());
            ui.label(theme::muted(t.output.summary()).size(12.0));
        });
        if !t.passed && !t.hidden {
            egui::Grid::new(("t", i)).num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                ui.label(theme::muted("Input").size(12.0));
                ui.label(RichText::new(&t.input).monospace());
                ui.end_row();
                ui.label(theme::muted("Expected").size(12.0));
                ui.label(RichText::new(&t.expected).monospace().color(p.success));
                ui.end_row();
                ui.label(theme::muted("Got").size(12.0));
                let got = if t.output.stdout.is_empty() { t.output.stderr.clone() } else { t.output.stdout.clone() };
                ui.label(RichText::new(got).monospace().color(p.danger));
                ui.end_row();
            });
        }
    }
}

pub fn run(pkg: Package, mode: Mode) -> i32 {
    if mode != Mode::Windowed {
        lockdown::start_keyboard_hook();
    }
    let title = format!("HDJLabs Exam {}", &pkg.header.exam_id[..8.min(pkg.header.exam_id.len())]);
    let fit = crate::ui::fit_to_screen([1280.0, 820.0], [900.0, 600.0]);
    let mut viewport = egui::ViewportBuilder::default().with_title(title).with_inner_size(fit.0).with_min_inner_size(fit.1).with_icon(crate::ui::app_icon());
    if mode != Mode::Windowed {
        viewport = viewport.with_fullscreen(true).with_decorations(false).with_window_level(egui::WindowLevel::AlwaysOnTop).with_resizable(false);
    }
    let code = crate::ui::launch("HDJLabs Exam", viewport, move |_| Player::new(pkg.clone(), mode));
    lockdown::release_cursor();
    code
}
