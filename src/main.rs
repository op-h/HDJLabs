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
        let mut seen = 0;
        while std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let st = server.state.lock().unwrap();
            for l in st.log.iter().skip(seen) {
                println!("log: {l}");
            }
            seen = st.log.len();
            if let Some(s) = st.students.iter().find(|s| s.submission.is_some()) {
                let sub = s.submission.as_ref().unwrap();
                println!("SUBMISSION from {} ({}) score {}/{} answers={} events={:?}", sub.student_name, sub.student_id, sub.score, sub.max_score, sub.answers.len(), s.events);
                return;
            }
        }
        println!("no submission received");
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
        std::process::exit(ui::studio::run());
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
    match lockdown::run_on_private_desktop("--exam-child") {
        Ok(c) if c as i32 == EXIT_UI_FAILED && started.elapsed().as_secs() < 20 => run_fallback(),
        Ok(c) => c as i32,
        Err(_) => run_fallback(),
    }
}

/// Private desktop unavailable (rare drivers / policies): lock down inside the normal desktop.
fn run_fallback() -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    std::process::Command::new(exe).arg("--exam-fallback").status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
}
