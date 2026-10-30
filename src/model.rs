//! Exam data model shared by the studio, the exam runtime and the LAN protocol.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Lang {
    Python,
    JavaScript,
    C,
    Cpp,
    Java,
    Go,
    Rust,
    /// SQLite, built in. The teacher's setup script creates the tables first.
    Sql,
    /// One HTML document with its CSS and JavaScript, shown in a live preview.
    Web,
}

impl Lang {
    pub const ALL: [Lang; 9] = [
        Lang::Python,
        Lang::JavaScript,
        Lang::Sql,
        Lang::Web,
        Lang::C,
        Lang::Cpp,
        Lang::Java,
        Lang::Go,
        Lang::Rust,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "Python",
            Lang::JavaScript => "JavaScript",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::Java => "Java",
            Lang::Go => "Go",
            Lang::Rust => "Rust",
            Lang::Sql => "SQL",
            Lang::Web => "HTML / CSS / JS",
        }
    }

    /// Extension used both for the source file and for syntax highlighting.
    pub fn ext(self) -> &'static str {
        match self {
            Lang::Python => "py",
            Lang::JavaScript => "js",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Java => "java",
            Lang::Go => "go",
            Lang::Rust => "rs",
            Lang::Sql => "sql",
            Lang::Web => "html",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Lang::Java => "Main.java",
            Lang::Python => "main.py",
            Lang::JavaScript => "main.js",
            Lang::C => "main.c",
            Lang::Cpp => "main.cpp",
            Lang::Go => "main.go",
            Lang::Rust => "main.rs",
            Lang::Sql => "query.sql",
            Lang::Web => "index.html",
        }
    }

    pub fn template(self) -> &'static str {
        match self {
            Lang::Python => "# Read input with input(), print the answer\nname = input()\nprint(\"Hello,\", name)\n",
            Lang::JavaScript => "// Read input with prompt() / readline(), print with console.log\nconst name = readline();\nconsole.log(\"Hello, \" + name);\n",
            Lang::C => "#include <stdio.h>\n\nint main(void) {\n    char name[64];\n    scanf(\"%63s\", name);\n    printf(\"Hello, %s\\n\", name);\n    return 0;\n}\n",
            Lang::Cpp => "#include <iostream>\n#include <string>\nusing namespace std;\n\nint main() {\n    string name;\n    cin >> name;\n    cout << \"Hello, \" << name << endl;\n    return 0;\n}\n",
            Lang::Java => "import java.util.Scanner;\n\npublic class Main {\n    public static void main(String[] args) {\n        Scanner in = new Scanner(System.in);\n        String name = in.next();\n        System.out.println(\"Hello, \" + name);\n    }\n}\n",
            Lang::Go => "package main\n\nimport \"fmt\"\n\nfunc main() {\n    var name string\n    fmt.Scan(&name)\n    fmt.Println(\"Hello,\", name)\n}\n",
            Lang::Rust => "use std::io::stdin;\n\nfn main() {\n    let mut name = String::new();\n    stdin().read_line(&mut name).unwrap();\n    println!(\"Hello, {}\", name.trim());\n}\n",
            Lang::Sql => "-- Write your query below\nSELECT name, grade\nFROM students\nORDER BY grade DESC;\n",
            Lang::Web => "<!doctype html>\n<html>\n<head>\n  <style>\n    body { font-family: sans-serif; margin: 24px; }\n    h1 { color: #2d6a4f; }\n  </style>\n</head>\n<body>\n  <h1>Hello</h1>\n  <button id=\"btn\">Click me</button>\n  <p id=\"out\"></p>\n\n  <script>\n    document.getElementById(\"btn\").onclick = () => {\n      document.getElementById(\"out\").textContent = \"Clicked!\";\n    };\n  </script>\n</body>\n</html>\n",
        }
    }

    /// Default database for new SQL questions.
    pub fn sql_setup_template() -> &'static str {
        "CREATE TABLE students (id INTEGER PRIMARY KEY, name TEXT, grade INTEGER);\nINSERT INTO students (name, grade) VALUES\n  ('Sara', 91), ('Omar', 78), ('Lina', 85);\n"
    }

    /// Languages whose answers are graded by reading/previewing rather than by tests.
    pub fn is_preview_only(self) -> bool {
        self == Lang::Web
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestCase {
    pub input: String,
    pub expected: String,
    /// Hidden tests only run at submission time; students never see them.
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CodeSpec {
    pub lang: Lang,
    pub starter: String,
    pub tests: Vec<TestCase>,
    /// Teacher's own solution. Kept in the project file only — stripped on export.
    #[serde(default)]
    pub reference: String,
    /// SQL only: script that creates and fills the tables before the student's query runs.
    #[serde(default)]
    pub setup: String,
    /// Simple mode: one expected output, no stdin, no hidden tests.
    #[serde(default)]
    pub simple: bool,
}

impl CodeSpec {
    /// Graded by the teacher: no tests, a page preview, or tests that don't say what to expect
    /// (scoring those automatically would give everyone zero).
    pub fn teacher_graded(&self) -> bool {
        self.lang.is_preview_only() || self.tests.iter().all(|t| t.expected.trim().is_empty())
    }

    pub fn new(lang: Lang) -> Self {
        CodeSpec {
            lang,
            starter: lang.template().into(),
            tests: if lang.is_preview_only() { vec![] } else { vec![TestCase { input: String::new(), expected: String::new(), hidden: false }] },
            reference: String::new(),
            setup: if lang == Lang::Sql { Lang::sql_setup_template().into() } else { String::new() },
            simple: true,
        }
    }

    /// What the program receives on stdin for a test input (SQL gets the database setup first).
    pub fn stdin_for(&self, input: &str) -> String {
        if self.lang == Lang::Sql {
            format!("{}\n{}", self.setup, input)
        } else {
            input.to_string()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Kind {
    Choice {
        options: Vec<String>,
        correct: Vec<usize>,
        multiple: bool,
    },
    TrueFalse {
        answer: bool,
    },
    Short {
        accepted: Vec<String>,
        case_sensitive: bool,
        /// Show the answer's shape in the box, TryHackMe style: `***{****_***}`.
        #[serde(default)]
        show_format: bool,
    },
    Code(CodeSpec),
    /// The student attaches a file (report, project zip…). Only allowed with lockdown off, since
    /// a locked exam never opens a file window. Graded by the teacher.
    Upload {
        /// Allowed extensions without the dot (`pdf`, `zip`); empty accepts any file.
        #[serde(default)]
        extensions: Vec<String>,
        /// Largest file accepted, in MB (at most `MAX_UPLOAD_MB`).
        max_mb: f32,
    },
}

/// Upper limit for one uploaded file, and for all of a student's files together, so the
/// submission still fits through the classroom network in one piece.
pub const MAX_UPLOAD_MB: f32 = 3.0;

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Choice { multiple: true, .. } => "Multiple answer",
            Kind::Choice { .. } => "Multiple choice",
            Kind::TrueFalse { .. } => "True / False",
            Kind::Short { .. } => "Short answer",
            Kind::Code(_) => "Code",
            Kind::Upload { .. } => "File upload",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub id: u32,
    pub prompt: String,
    pub points: f32,
    pub kind: Kind,
}

impl Question {
    pub fn new(id: u32, kind: Kind) -> Self {
        Self { id, prompt: String::new(), points: 1.0, kind }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub exam_password: String,
    pub exit_password: String,
    pub shuffle_questions: bool,
    pub shuffle_options: bool,
    /// Random subset size per student; 0 = all questions.
    pub pick: u32,
    pub show_score: bool,
    pub show_review: bool,
    pub lockdown: bool,
    pub block_vm: bool,
    pub block_remote_tools: bool,
    pub lan_port: u16,
    /// Delete the exam .exe from the student's PC once they have submitted.
    #[serde(default)]
    pub remove_after_submit: bool,
    /// Students see their score only after the teacher finishes grading and sends it.
    #[serde(default)]
    pub hold_scores: bool,
    /// Ship the exam without correct answers or hidden tests; the teacher's Studio grades.
    #[serde(default)]
    pub keep_answers_off_pcs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            exam_password: String::new(),
            exit_password: String::new(),
            shuffle_questions: true,
            shuffle_options: true,
            pick: 0,
            show_score: true,
            show_review: false,
            lockdown: true,
            block_vm: false,
            block_remote_tools: true,
            lan_port: crate::net::DEFAULT_TCP_PORT,
            remove_after_submit: false,
            hold_scores: false,
            keep_answers_off_pcs: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Exam {
    pub id: String,
    pub title: String,
    pub author: String,
    pub instructions: String,
    pub duration_min: u32,
    pub questions: Vec<Question>,
    pub settings: Settings,
}

impl Exam {
    pub fn new() -> Self {
        Self {
            id: crate::crypto::random_hex(16),
            title: "Untitled exam".into(),
            author: String::new(),
            instructions: "Answer every question. Your work is saved automatically.".into(),
            duration_min: 45,
            questions: Vec::new(),
            settings: Settings::default(),
        }
    }

    pub fn next_question_id(&self) -> u32 {
        self.questions.iter().map(|q| q.id).max().unwrap_or(0) + 1
    }

    pub fn total_points(&self) -> f32 {
        self.questions.iter().map(|q| q.points).sum()
    }

    /// Number of questions each student actually gets.
    pub fn delivered_count(&self) -> usize {
        let n = self.questions.len();
        match self.settings.pick as usize {
            0 => n,
            p => p.min(n),
        }
    }

    /// Copy that is safe to ship inside an exam .exe (no teacher-only data).
    pub fn for_students(&self) -> Exam {
        let mut e = self.clone();
        e.settings.exam_password.clear();
        e.settings.exit_password.clear();
        let strip = e.settings.keep_answers_off_pcs;
        for q in &mut e.questions {
            match &mut q.kind {
                Kind::Code(c) => {
                    c.reference.clear();
                    if strip {
                        c.tests.retain(|t| !t.hidden);
                    }
                }
                Kind::Choice { correct, .. } if strip => correct.clear(),
                Kind::TrueFalse { answer } if strip => *answer = false,
                // Only the answer's shape (`***{****}`) travels, for the format hint.
                Kind::Short { accepted, show_format, .. } if strip => {
                    *accepted = if *show_format { accepted.iter().find(|a| !a.trim().is_empty()).map(|a| vec![answer_format(a)]).unwrap_or_default() } else { vec![] };
                }
                _ => {}
            }
        }
        e
    }

    /// Problems that would make the exported exam unusable.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.title.trim().is_empty() {
            out.push("The exam needs a title.".into());
        }
        if self.questions.is_empty() {
            out.push("Add at least one question.".into());
        }
        if self.settings.lockdown && self.settings.exit_password.trim().len() < 4 {
            out.push("Lockdown exams need a proctor exit password (4+ characters).".into());
        }
        for (i, q) in self.questions.iter().enumerate() {
            let n = i + 1;
            if q.prompt.trim().is_empty() {
                out.push(format!("Question {n} has no prompt."));
            }
            match &q.kind {
                Kind::Choice { options, correct, .. } => {
                    if options.iter().filter(|o| !o.trim().is_empty()).count() < 2 {
                        out.push(format!("Question {n} needs at least two options."));
                    }
                    if correct.is_empty() {
                        out.push(format!("Question {n} has no correct option marked."));
                    }
                }
                Kind::Short { accepted, .. } => {
                    if accepted.iter().all(|a| a.trim().is_empty()) {
                        out.push(format!("Question {n} has no accepted answer."));
                    }
                }
                Kind::Upload { .. } if self.settings.lockdown => {
                    out.push(format!("Question {n} is a file upload: turn off lockdown in Settings (a locked exam never opens a file window)."));
                }
                _ => {}
            }
        }
        out
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum Answer {
    #[default]
    Empty,
    Choice(Vec<usize>),
    Bool(bool),
    Text(String),
    Code { lang: Lang, source: String },
    /// An uploaded file. `data` is base64; it is left out of live progress updates.
    File { name: String, size: u64, data: String },
}

impl Answer {
    pub fn is_answered(&self) -> bool {
        match self {
            Answer::Empty => false,
            Answer::Choice(v) => !v.is_empty(),
            Answer::Bool(_) => true,
            Answer::Text(t) => !t.trim().is_empty(),
            Answer::Code { source, .. } => !source.trim().is_empty(),
            Answer::File { name, .. } => !name.is_empty(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    Correct,
    Partial,
    Wrong,
    Unanswered,
    /// Needs a teacher's eye (code without tests).
    Review,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Correct => "Correct",
            Status::Partial => "Partial",
            Status::Wrong => "Incorrect",
            Status::Unanswered => "Unanswered",
            Status::Review => "Needs review",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct QuestionResult {
    pub qid: u32,
    pub earned: f32,
    pub max: f32,
    pub status: Status,
    pub tests_passed: u32,
    pub tests_total: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Violation {
    pub at: u64,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Submission {
    pub exam_id: String,
    pub exam_title: String,
    pub attempt_id: String,
    pub student_name: String,
    pub student_id: String,
    pub machine: String,
    pub started_at: u64,
    pub submitted_at: u64,
    pub question_order: Vec<u32>,
    pub answers: BTreeMap<u32, Answer>,
    pub results: Vec<QuestionResult>,
    pub score: f32,
    pub max_score: f32,
    pub violations: Vec<Violation>,
    /// Submitted by the timer or by the teacher rather than the student.
    pub forced: bool,
}

/// TryHackMe-style answer format: letters and digits become `*`, everything else is shown as is.
pub fn answer_format(answer: &str) -> String {
    answer.trim().chars().map(|c| if c.is_alphanumeric() { '*' } else { c }).collect()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn machine_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into())
}
