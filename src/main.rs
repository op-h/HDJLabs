//! HDJLabs — secure offline exams for Windows.
//!
//! One executable, three roles:
//! * no exam attached        → HDJLabs Studio (teacher)
//! * exam attached           → the exam (student), inside lockdown
//! * `--exec <lang> <file>`  → built-in Python / JavaScript interpreter used by the code runner

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod crypto;
mod grading;
mod interp;
mod lockdown;
mod model;
mod net;
mod payload;
mod runner;
mod storage;
mod ui;

use ui::player::Mode;

/// The exam UI could not create its window (e.g. no GPU driver on the private desktop).
pub const EXIT_UI_FAILED: i32 = 3;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = args.get(1).map(String::as_str);
    if flag == Some("--exec") {
        std::process::exit(interp::exec_main(&args[2..]));
    }
    lockdown::recover_stale();

    #[cfg(debug_assertions)]
    if flag == Some("--dev-export-sample") {
        // Developer helper: write the sample exam as an .exe (args: <out.exe> <lockdown 0|1>).
        let (secret, public) = crypto::new_keypair();
        let mut exam = ui::studio::sample_exam();
        let mode = args.get(3).map(String::as_str).unwrap_or("0");
        exam.settings.lockdown = mode != "0";
        exam.settings.block_remote_tools = mode == "1";
        exam.settings.keep_answers_off_pcs = std::env::var_os("HDJLABS_DEV_HIDE_ANSWERS").is_some();
        if mode == "0" && std::env::var_os("HDJLABS_DEV_UPLOAD").is_some() {
            let id = exam.questions.iter().map(|q| q.id).max().unwrap_or(0) + 1;
            let mut q = model::Question::new(id, model::Kind::Upload { extensions: vec!["pdf".into(), "docx".into(), "zip".into()], max_mb: 2.0 });
            q.prompt = "Upload your lab report (PDF or Word).".into();
            q.points = 4.0;
            exam.questions.insert(0, q);
        }
        let out = std::path::PathBuf::from(&args[2]);
        payload::export(&exam, &public, false, vec!["127.0.0.1".into()], &out).expect("export");
        let mut project = storage::Project::new();
        project.exam = exam;
        project.teacher_secret = secret;
        project.teacher_public = public;
        project.last_export = Some(out.clone());
        project.save(&out.with_extension("hdjx")).expect("save project");
        return;
    }

    #[cfg(debug_assertions)]
    if flag == Some("--dev-serve") {
        // Developer helper: headless teacher server for a project (args: <project.hdjx> <seconds>).
        let project = storage::Project::load(std::path::Path::new(&args[2])).expect("project");
        let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(60);
        let server = net::LiveServer::start(project.exam.id.clone(), project.teacher_secret.clone(), "dev".into(), project.exam.settings.lan_port, project.last_export.clone().map(|p| (p, project.exam.title.clone())), || {});
        println!("serving errors={:?} http={:?}", server.errors, server.http_url);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let expect: usize = std::env::var("HDJLABS_DEV_EXPECT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let remove = std::env::var("HDJLABS_DEV_REMOVE").is_ok();
        // Like the Studio: re-grade each submission here and send the student their score.
        let release = std::env::var("HDJLABS_DEV_RELEASE").is_ok();
        let mut released = std::collections::HashSet::new();
        let (mut seen_log, mut last_summary, mut removed) = (0, String::new(), std::collections::HashSet::new());
        let mut events_seen = std::collections::HashMap::<String, usize>::new();
        while std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut st = server.state.lock().unwrap();
            for l in st.log.iter().skip(seen_log) {
                println!("log: {l}");
            }
            seen_log = st.log.len();
            for s in &st.students {
                let n = events_seen.entry(s.attempt_id.clone()).or_default();
                for (_, e) in s.events.iter().skip(*n) {
                    println!("event: {} — {e}", s.name);
                }
                *n = s.events.len();
            }
            let dl: Vec<String> = st.downloads.iter().map(|d| format!("{} {}%{}", d.ip, if d.total > 0 { d.sent * 100 / d.total } else { 0 }, if d.finished.is_some() { if d.failed { " FAILED" } else { " done" } } else { "" })).collect();
            let students: Vec<String> = st
                .students
                .iter()
                .map(|s| {
                    let stt = s.status.as_ref().map(|x| format!("{:?} {}/{} q{} {}s", x.phase, x.answered, x.total, x.current + 1, x.remaining_secs)).unwrap_or_default();
                    format!("{} [{}] {} {} {}", s.name, s.addr, if s.connected { "online" } else { "OFFLINE" }, stt, s.submission.as_ref().map(|x| format!("SUBMITTED {}/{}", x.score, x.max_score)).unwrap_or_default())
                })
                .collect();
            let summary = format!("waiting={:?} downloads={:?}\n  students={:#?}", st.waiting_pcs(), dl, students);
            if summary != last_summary {
                println!("{summary}");
                last_summary = summary;
            }
            let subs: Vec<String> = st.students.iter().filter(|s| s.submission.is_some()).map(|s| s.attempt_id.clone()).collect();
            if release {
                for s in st.students.clone().iter().filter(|s| s.submission.is_some()) {
                    if released.insert(s.attempt_id.clone()) {
                        let sub = s.submission.clone().unwrap();
                        let results = grading::regrade(&project.exam, &sub);
                        let score: f32 = results.iter().map(|r| r.earned).sum();
                        println!("re-graded {}: student PC said {}/{}, teacher says {score}/{} — sending", s.name, sub.score, sub.max_score, sub.max_score);
                        st.release_grade(&s.attempt_id, results, score);
                    }
                }
            }
            if remove {
                for id in &subs {
                    if removed.insert(id.clone()) {
                        st.send(id, net::S2C::RemoveExam);
                        println!("sent RemoveExam to {id}");
                    }
                }
            }
            if subs.len() >= expect {
                drop(st);
                std::thread::sleep(std::time::Duration::from_secs(20));
                println!("all {expect} submissions received");
                return;
            }
        }
        println!("time limit reached");
        return;
    }

    let pkg = match payload::read_self() {
        Ok(p) => p,
        Err(e) => {
            lockdown::message_box("HDJLabs", &format!("Cannot read this program file: {e}"), false);
            std::process::exit(1);
        }
    };
    let Some(pkg) = pkg else {
        // Double-clicking a .hdjx project opens it in the Studio.
        let project = args.get(1).map(std::path::PathBuf::from).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("hdjx")) && p.is_file());
        std::process::exit(ui::studio::run(project));
    };

    let code = match flag {
        // Exam UI launched by the lockdown host. If it is not really on our private desktop
        // (someone ran it by hand), it still locks down — in-window, with its own guard.
        Some("--exam-child") if lockdown::current_desktop_name().starts_with(lockdown::DESKTOP_PREFIX) => ui::player::run(pkg, Mode::Locked),
        Some("--exam-child") | Some("--exam-fallback") if pkg.header.lockdown => {
            let _mutex = lockdown::hold_running_mutex();
            let _guard = lockdown::SystemGuard::apply();
            ui::player::run(pkg, Mode::Fallback)
        }
        _ if !pkg.header.lockdown => ui::player::run(pkg, Mode::Windowed),
        _ => host_lockdown(&pkg),
    };
    std::process::exit(code);
}

/// Pre-flight checks, then runs the exam on a private desktop with system policies applied.
fn host_lockdown(pkg: &payload::Package) -> i32 {
    let h = &pkg.header;
    let title = format!("HDJLabs — {}", h.title);
    if lockdown::mutex_exists("Local\\HDJLabs.ExamRunning") {
        lockdown::message_box(&title, "An HDJLabs exam is already running on this computer.", false);
        return 1;
    }
    if lockdown::is_remote_session() {
        lockdown::message_box(&title, "This exam cannot be taken over Remote Desktop. Sit at the computer and open it again.", false);
        return 1;
    }
    if h.block_vm && lockdown::is_virtual_machine() {
        lockdown::message_box(&title, "This exam cannot run inside a virtual machine.", false);
        return 1;
    }
    if h.block_remote_tools {
        loop {
            let found = lockdown::forbidden_processes();
            if found.is_empty() {
                break;
            }
            let msg = format!("Close these programs before starting the exam:\n\n  • {}\n\nThen press Retry.", found.join("\n  • "));
            if !lockdown::message_box(&title, &msg, true) {
                return 1;
            }
        }
    }

    let _mutex = lockdown::hold_running_mutex();
    let _guard = lockdown::SystemGuard::apply();
    let started = std::time::Instant::now();
    let code = match lockdown::run_on_private_desktop("--exam-child") {
        Ok(c) if c as i32 == EXIT_UI_FAILED && started.elapsed().as_secs() < 20 => run_fallback(),
        Ok(c) => c as i32,
        Err(_) => run_fallback(),
    };
    lockdown::perform_requested_removal();
    code
}

/// Private desktop unavailable (rare drivers / policies): lock down inside the normal desktop.
fn run_fallback() -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    std::process::Command::new(exe).arg("--exam-fallback").status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
}
