//! Compiles and runs student code in a Windows Job Object sandbox.
//!
//! The run step gets: one process max (no `os.system("explorer")` escapes from the exam
//! desktop), no console, a memory cap, no clipboard / desktop / global-atom access, a hard
//! timeout, and kill-on-close so nothing outlives the exam.

use crate::model::Lang;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const RUN_TIMEOUT: Duration = Duration::from_secs(6);
const NODE_PRELUDE: &str = "let __in=null,__at=0;globalThis.readline=()=>{if(__in===null){try{__in=require('fs').readFileSync(0,'utf8').split(String.fromCharCode(10)).map(l=>l.endsWith(String.fromCharCode(13))?l.slice(0,-1):l);if(__in[__in.length-1]==='')__in.pop()}catch(e){__in=[]}}return __at<__in.length?__in[__at++]:null};globalThis.prompt=globalThis.readline;globalThis.print=(...a)=>console.log(...a);";
const COMPILE_TIMEOUT: Duration = Duration::from_secs(90);
const OUTPUT_CAP: usize = 128 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub compile_failed: bool,
    pub unavailable: bool,
    pub millis: u128,
}

impl RunOutput {
    pub fn ok(&self) -> bool {
        !self.timed_out && !self.compile_failed && !self.unavailable && self.exit_code == Some(0)
    }

    pub fn summary(&self) -> String {
        if self.unavailable {
            "Toolchain not available on this computer".into()
        } else if self.compile_failed {
            "Compilation failed".into()
        } else if self.timed_out {
            format!("Stopped: time limit exceeded ({} s)", RUN_TIMEOUT.as_secs())
        } else {
            match self.exit_code {
                Some(0) => format!("Finished in {} ms", self.millis),
                Some(c) => format!("Exited with code {c} after {} ms", self.millis),
                None => "Terminated".into(),
            }
        }
    }

    fn failed(msg: &str, unavailable: bool) -> Self {
        Self { stderr: msg.into(), unavailable, ..Default::default() }
    }
}

#[derive(Clone, Debug)]
pub enum Engine {
    /// Interpreter compiled into HDJLabs itself.
    Builtin,
    /// A toolchain found on this computer (primary executable path).
    System(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Tool {
    pub engine: Engine,
    pub label: String,
}

fn extra_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        r"C:\msys64\ucrt64\bin",
        r"C:\msys64\mingw64\bin",
        r"C:\MinGW\bin",
        r"C:\mingw64\bin",
        r"C:\TDM-GCC-64\bin",
        r"C:\Program Files\LLVM\bin",
        r"C:\Program Files\nodejs",
        r"C:\Program Files\Go\bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Ok(home) = std::env::var("USERPROFILE") {
        v.push(Path::new(&home).join(r".cargo\bin"));
    }
    // JDKs install into versioned folders.
    for root in [r"C:\Program Files\Java", r"C:\Program Files\Eclipse Adoptium", r"C:\Program Files\Microsoft"] {
        if let Ok(rd) = std::fs::read_dir(root) {
            for e in rd.flatten() {
                v.push(e.path().join("bin"));
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if let Ok(rd) = std::fs::read_dir(Path::new(&local).join(r"Programs\Python")) {
            for e in rd.flatten() {
                v.push(e.path());
            }
        }
    }
    v
}

fn find_exe(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.exe");
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(extra_dirs())
        // The WindowsApps "python.exe" is a Store installer stub, not Python.
        .filter(|d| !d.to_string_lossy().to_ascii_lowercase().contains("windowsapps"))
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

fn detect(lang: Lang) -> Option<Tool> {
    let sys = |names: &[&str]| names.iter().find_map(|n| find_exe(n).map(|p| (p, n.to_string())));
    match lang {
        Lang::Python => Some(match sys(&["python", "py"]) {
            Some((p, _)) => Tool { engine: Engine::System(p), label: "Python (system)".into() },
            None => Tool { engine: Engine::Builtin, label: "Python (built-in)".into() },
        }),
        Lang::JavaScript => Some(match sys(&["node"]) {
            Some((p, _)) => Tool { engine: Engine::System(p), label: "Node.js".into() },
            None => Tool { engine: Engine::Builtin, label: "JavaScript (built-in)".into() },
        }),
        Lang::C => sys(&["gcc", "clang"]).map(|(p, n)| Tool { engine: Engine::System(p), label: n }),
        Lang::Cpp => sys(&["g++", "clang++"]).map(|(p, n)| Tool { engine: Engine::System(p), label: n }),
        Lang::Java => sys(&["javac"]).map(|(p, _)| Tool { engine: Engine::System(p), label: "JDK".into() }),
        Lang::Go => sys(&["go"]).map(|(p, _)| Tool { engine: Engine::System(p), label: "Go".into() }),
        Lang::Rust => sys(&["rustc"]).map(|(p, _)| Tool { engine: Engine::System(p), label: "rustc".into() }),
    }
}

pub fn tool(lang: Lang) -> Option<Tool> {
    static CACHE: OnceLock<HashMap<Lang, Option<Tool>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Lang::ALL.iter().map(|l| (*l, detect(*l))).collect())
        .get(&lang)
        .cloned()
        .flatten()
}

struct Step {
    program: PathBuf,
    args: Vec<String>,
    /// Compiler steps may spawn helpers (cc1, as, ld); student programs may not.
    compile: bool,
}

fn plan(lang: Lang, tool: &Tool, dir: &Path) -> Vec<Step> {
    let src = dir.join(lang.file_name()).to_string_lossy().into_owned();
    let exe = dir.join("main.exe").to_string_lossy().into_owned();
    let run_exe = Step { program: PathBuf::from(&exe), args: vec![], compile: false };
    let me = std::env::current_exe().unwrap_or_default();
    let path = match &tool.engine {
        Engine::System(p) => p.clone(),
        Engine::Builtin => {
            let which = if lang == Lang::Python { "python" } else { "js" };
            return vec![Step { program: me, args: vec!["--exec".into(), which.into(), src], compile: false }];
        }
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match lang {
        Lang::Python if path.file_stem().is_some_and(|n| n == "py") => {
            vec![Step { program: path, args: s(&["-3", "-u", &src]), compile: false }]
        }
        Lang::Python => vec![Step { program: path, args: s(&["-u", &src]), compile: false }],
        Lang::JavaScript => vec![Step { program: path, args: vec![src], compile: false }],
        Lang::C => vec![Step { program: path, args: s(&[&src, "-O2", "-std=c11", "-o", &exe, "-lm"]), compile: true }, run_exe],
        Lang::Cpp => vec![Step { program: path, args: s(&[&src, "-O2", "-std=c++17", "-o", &exe]), compile: true }, run_exe],
        Lang::Go => vec![Step { program: path, args: s(&["build", "-o", &exe, &src]), compile: true }, run_exe],
        Lang::Rust => vec![Step { program: path, args: s(&[&src, "-o", &exe]), compile: true }, run_exe],
        Lang::Java => {
            let java = path.with_file_name("java.exe");
            vec![
                Step { program: path, args: s(&["-encoding", "UTF-8", &src]), compile: true },
                Step { program: java, args: s(&["-Xmx256m", "-Xss8m", "-cp", &dir.to_string_lossy(), "Main"]), compile: false },
            ]
        }
    }
}

/// Compiles (if needed) and runs `source` with `stdin`. Blocking — call from a worker thread.
pub fn run(lang: Lang, source: &str, stdin: &str, timeout: Duration) -> RunOutput {
    let Some(tool) = tool(lang) else {
        return RunOutput::failed(&format!("No {} toolchain was found on this computer.", lang.name()), true);
    };
    let dir = std::env::temp_dir().join("HDJLabs").join(format!("run-{}", crate::crypto::random_hex(6)));
    // Node lacks the browser-style input helpers the built-in engine provides; add them on the
    // first line (no newline) so error line numbers still match the student's editor.
    let source = match (&tool.engine, lang) {
        (Engine::System(_), Lang::JavaScript) => format!("{NODE_PRELUDE}{source}"),
        _ => source.to_string(),
    };
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(lang.file_name()), &source)) {
        return RunOutput::failed(&format!("Cannot prepare workspace: {e}"), false);
    }
    let started = Instant::now();
    let mut out = RunOutput::default();
    for step in plan(lang, &tool, &dir) {
        let r = exec(&step, &dir, if step.compile { "" } else { stdin }, if step.compile { COMPILE_TIMEOUT } else { timeout });
        if step.compile {
            if !r.ok() {
                out = RunOutput { compile_failed: true, stderr: format!("{}{}", r.stdout, r.stderr), ..r };
                break;
            }
        } else {
            out = r;
        }
    }
    out.millis = started.elapsed().as_millis();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn read_capped(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut truncated = false;
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if buf.len() < OUTPUT_CAP {
                buf.extend_from_slice(&chunk[..n.min(OUTPUT_CAP - buf.len())]);
            } else {
                truncated = true; // keep draining so the child never blocks on a full pipe
            }
        }
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            s.push_str("\n… output truncated …");
        }
        s
    })
}

fn exec(step: &Step, dir: &Path, stdin: &str, timeout: Duration) -> RunOutput {
    let mut cmd = Command::new(&step.program);
    cmd.args(&step.args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONDONTWRITEBYTECODE", "1");
    let sandbox = sandbox::Job::new(!step.compile);
    let mut child = match sandbox::spawn_suspended(&mut cmd, !step.compile) {
        Ok(c) => c,
        Err(e) => return RunOutput::failed(&format!("Could not start {}: {e}", step.program.display()), false),
    };
    if let Some(job) = &sandbox {
        job.assign(&child);
    }
    sandbox::resume(&child);

    let out_h = read_capped(child.stdout.take().unwrap());
    let err_h = read_capped(child.stderr.take().unwrap());
    if let Some(mut si) = child.stdin.take() {
        let data = stdin.to_string();
        std::thread::spawn(move || {
            let _ = si.write_all(data.as_bytes());
            if !data.ends_with('\n') && !data.is_empty() {
                let _ = si.write_all(b"\n");
            }
        });
    }
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                match &sandbox {
                    Some(job) => job.kill(),
                    None => {
                        let _ = child.kill();
                    }
                }
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    // Kill anything left in the job (compilers' helpers) before joining the pipe readers.
    drop(sandbox);
    RunOutput {
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        ..Default::default()
    }
}

#[cfg(windows)]
mod sandbox {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, DETACHED_PROCESS, THREAD_SUSPEND_RESUME};

    pub struct Job(HANDLE);

    unsafe impl Send for Job {}

    impl Job {
        pub fn new(strict: bool) -> Option<Job> {
            unsafe {
                let h = CreateJobObjectW(None, None).ok()?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION | JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.JobMemoryLimit = if strict { 1 << 30 } else { 3 << 30 };
                if strict {
                    flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                    info.BasicLimitInformation.ActiveProcessLimit = 1;
                }
                info.BasicLimitInformation.LimitFlags = flags;
                let _ = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                    UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                        | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                        | JOB_OBJECT_UILIMIT_EXITWINDOWS
                        | JOB_OBJECT_UILIMIT_GLOBALATOMS
                        | JOB_OBJECT_UILIMIT_HANDLES
                        | JOB_OBJECT_UILIMIT_READCLIPBOARD
                        | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                        | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
                };
                let _ = SetInformationJobObject(
                    h,
                    JobObjectBasicUIRestrictions,
                    &ui as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                );
                Some(Job(h))
            }
        }

        pub fn assign(&self, child: &Child) {
            unsafe {
                let _ = AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle()));
            }
        }

        pub fn kill(&self) {
            unsafe {
                let _ = TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = TerminateJobObject(self.0, 1);
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Starts suspended so the job is in place before the first student instruction runs.
    pub fn spawn_suspended(cmd: &mut Command, detached: bool) -> std::io::Result<Child> {
        // Detached = no console host, which also keeps the job at exactly one process.
        let console = if detached { DETACHED_PROCESS } else { CREATE_NO_WINDOW };
        cmd.creation_flags((CREATE_SUSPENDED | console).0);
        cmd.spawn()
    }

    pub fn resume(child: &Child) {
        unsafe {
            let pid = child.id();
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return };
            let mut te = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
            if Thread32First(snap, &mut te).is_ok() {
                loop {
                    if te.th32OwnerProcessID == pid {
                        if let Ok(t) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                            ResumeThread(t);
                            let _ = CloseHandle(t);
                        }
                    }
                    if Thread32Next(snap, &mut te).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
    }
}

#[cfg(not(windows))]
mod sandbox {
    use std::process::{Child, Command};
    pub struct Job;
    impl Job {
        pub fn new(_: bool) -> Option<Job> {
            None
        }
        pub fn assign(&self, _: &Child) {}
        pub fn kill(&self) {}
    }
    pub fn spawn_suspended(cmd: &mut Command, _: bool) -> std::io::Result<Child> {
        cmd.spawn()
    }
    pub fn resume(_: &Child) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(lang: Lang) -> &'static str {
        match lang {
            Lang::Python => "print('sum', sum(int(x) for x in input().split()))",
            Lang::JavaScript => "const a = require('fs').readFileSync(0,'utf8').trim().split(' ').map(Number); console.log('sum', a.reduce((x,y)=>x+y,0));",
            Lang::C => "#include <stdio.h>\nint main(){int a,b;scanf(\"%d %d\",&a,&b);printf(\"sum %d\\n\",a+b);}",
            Lang::Cpp => "#include <iostream>\nint main(){int a,b;std::cin>>a>>b;std::cout<<\"sum \"<<a+b<<std::endl;}",
            Lang::Java => "import java.util.*;\npublic class Main{public static void main(String[] s){Scanner in=new Scanner(System.in);System.out.println(\"sum \"+(in.nextInt()+in.nextInt()));}}",
            Lang::Go => "package main\nimport \"fmt\"\nfunc main(){var a,b int;fmt.Scan(&a,&b);fmt.Println(\"sum\",a+b)}",
            Lang::Rust => "fn main(){let mut s=String::new();std::io::stdin().read_line(&mut s).unwrap();let v:i32=s.split_whitespace().map(|x|x.parse::<i32>().unwrap()).sum();println!(\"sum {}\",v);}",
        }
    }

    #[test]
    fn system_toolchains_compile_and_run() {
        for lang in Lang::ALL {
            match tool(lang) {
                Some(Tool { engine: Engine::System(_), label }) => {
                    let o = run(lang, hello(lang), "2 3", Duration::from_secs(60));
                    assert!(o.ok(), "{lang:?} via {label} failed: {o:?}");
                    assert_eq!(o.stdout.trim(), "sum 5", "{lang:?}");
                }
                _ => eprintln!("skip {lang:?}: no system toolchain"),
            }
        }
    }

    #[test]
    fn javascript_template_runs_under_node() {
        if !matches!(tool(Lang::JavaScript), Some(Tool { engine: Engine::System(_), .. })) {
            return;
        }
        let o = run(Lang::JavaScript, Lang::JavaScript.template(), "World", Duration::from_secs(20));
        assert!(o.ok(), "{o:?}");
        assert_eq!(o.stdout.trim(), "Hello, World");
        let o = run(Lang::JavaScript, "let a = readline(); let b = readline(); print(a + b, readline());", "1\n2\n", Duration::from_secs(20));
        assert_eq!(o.stdout.trim(), "12 null", "{o:?}");
        let o = run(Lang::JavaScript, "throw new Error('x')", "", Duration::from_secs(20));
        assert!(o.stderr.contains("main.js:1"), "line numbers must be preserved: {o:?}");
    }

    #[test]
    fn infinite_loop_is_killed() {
        if !matches!(tool(Lang::Python), Some(Tool { engine: Engine::System(_), .. })) {
            return;
        }
        let t = Instant::now();
        let o = run(Lang::Python, "while True: pass", "", Duration::from_secs(2));
        assert!(o.timed_out && t.elapsed() < Duration::from_secs(6), "{o:?}");
    }

    #[test]
    fn sandbox_blocks_child_processes() {
        if !matches!(tool(Lang::Python), Some(Tool { engine: Engine::System(_), .. })) {
            return;
        }
        let code = "import subprocess\ntry:\n    subprocess.run(['cmd','/c','echo','ESCAPED'], check=True)\n    print('ESCAPED')\nexcept Exception as e:\n    print('blocked', type(e).__name__)";
        let o = run(Lang::Python, code, "", Duration::from_secs(10));
        assert!(!o.stdout.contains("ESCAPED"), "student code spawned a process: {o:?}");
        assert!(o.stdout.contains("blocked"), "{o:?}");
    }

    #[test]
    fn output_is_capped() {
        if !matches!(tool(Lang::Python), Some(Tool { engine: Engine::System(_), .. })) {
            return;
        }
        let o = run(Lang::Python, "print('x' * 10_000_000)", "", Duration::from_secs(10));
        assert!(o.stdout.len() <= OUTPUT_CAP + 64, "{}", o.stdout.len());
    }
}
