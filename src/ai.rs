//! AI question builder (off by default). The teacher's own files (PDF, Word, PowerPoint, text)
//! are read on this PC; their text goes to the AI provider the teacher configured, which returns
//! questions as JSON. Claude is called through Anthropic's Messages API; every other provider
//! through its OpenAI-compatible chat API (OpenAI, Gemini, OpenRouter, Groq, DeepSeek, Mistral,
//! Ollama, LM Studio, or any custom endpoint). Code questions come with a reference solution that
//! HDJLabs runs to fill in the expected outputs, so a wrong expected output from the model can't
//! slip in. The API key is stored with Windows DPAPI (readable only by this Windows user).

use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Provider {
    #[default]
    Anthropic,
    OpenAi,
    Gemini,
    OpenRouter,
    Groq,
    DeepSeek,
    Mistral,
    Ollama,
    LmStudio,
    Custom,
}

impl Provider {
    pub const ALL: [Provider; 10] = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini, Provider::OpenRouter, Provider::Groq, Provider::DeepSeek, Provider::Mistral, Provider::Ollama, Provider::LmStudio, Provider::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic (Claude)",
            Provider::OpenAi => "OpenAI",
            Provider::Gemini => "Google Gemini",
            Provider::OpenRouter => "OpenRouter",
            Provider::Groq => "Groq",
            Provider::DeepSeek => "DeepSeek",
            Provider::Mistral => "Mistral",
            Provider::Ollama => "Ollama (on this PC)",
            Provider::LmStudio => "LM Studio (on this PC)",
            Provider::Custom => "Other (OpenAI-compatible)",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
            Provider::Groq => "https://api.groq.com/openai/v1",
            Provider::DeepSeek => "https://api.deepseek.com/v1",
            Provider::Mistral => "https://api.mistral.ai/v1",
            Provider::Ollama => "http://localhost:11434/v1",
            Provider::LmStudio => "http://localhost:1234/v1",
            Provider::Custom => "",
        }
    }

    /// Model filled in when the provider is picked (others: choose with "Load models").
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            _ => "",
        }
    }

    pub fn needs_key(self) -> bool {
        !matches!(self, Provider::Ollama | Provider::LmStudio)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AiConfig {
    pub enabled: bool,
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    /// The key itself is never written to disk in the clear (see `key_protected`).
    #[serde(skip)]
    pub api_key: String,
    #[serde(default)]
    key_protected: String,
}

fn config_path() -> std::path::PathBuf {
    let d = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HDJLabs");
    let _ = std::fs::create_dir_all(&d);
    d.join("ai.json")
}

impl AiConfig {
    pub fn load() -> AiConfig {
        let mut c: AiConfig = std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if let Some(k) = crate::crypto::unb64(&c.key_protected).ok().and_then(|b| dpapi::unprotect(&b)).and_then(|b| String::from_utf8(b).ok()) {
            c.api_key = k;
        }
        if c.base_url.is_empty() {
            c.base_url = c.provider.default_url().into();
        }
        c
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.key_protected = if self.api_key.is_empty() { String::new() } else { dpapi::protect(self.api_key.as_bytes()).map(|b| crate::crypto::b64(&b)).ok_or("Windows couldn't protect the API key.")? };
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(config_path(), data).map_err(|e| e.to_string())
    }

    pub fn ready(&self) -> bool {
        self.enabled && !self.model.trim().is_empty() && !self.base_url.trim().is_empty() && (!self.provider.needs_key() || !self.api_key.trim().is_empty())
    }
}

// ───────────── reading the teacher's files ─────────────

/// Text the model reads, at most this many characters across all files (about 75K tokens).
pub const MAX_CHARS: usize = 300_000;

pub fn extract_text(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let text = match ext.as_str() {
        "pdf" => {
            let mut doc = lopdf::Document::load_mem(&bytes).map_err(|e| format!("Not a readable PDF ({e})"))?;
            undo_ascii85(&mut doc);
            let mut out = String::new();
            for (n, _) in doc.get_pages() {
                if let Ok(t) = doc.extract_text(&[n]) {
                    out.push_str(&t);
                    out.push('\n');
                }
            }
            out
        }
        "docx" => xml_text(&zip_entry(&bytes, "word/document.xml")?, "w:t", "w:p"),
        "pptx" => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
            let mut slides: Vec<(u32, String)> = archive.file_names().filter_map(|n| n.ok()).filter_map(|n| n.strip_prefix("ppt/slides/slide").and_then(|r| r.strip_suffix(".xml")).and_then(|k| k.parse().ok()).map(|k| (k, n.to_string()))).collect();
            slides.sort();
            let mut out = String::new();
            for (k, name) in slides {
                let mut s = String::new();
                use std::io::Read;
                archive.by_name(&name).map_err(|e| e.to_string())?.read_to_string(&mut s).map_err(|e| e.to_string())?;
                out.push_str(&format!("[Slide {k}]\n{}\n", xml_text(&s, "a:t", "a:p")));
            }
            out
        }
        _ => {
            let head = &bytes[..bytes.len().min(4096)];
            if head.contains(&0) {
                return Err("Only PDF, Word (.docx), PowerPoint (.pptx) and text files can be read.".into());
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(if ext == "pdf" { "No text found (a scanned PDF has only pictures of pages).".into() } else { "No text found in this file.".into() });
    }
    Ok(text)
}

/// lopdf can't decode ASCII85 streams (ReportLab and some printers write them): decode that
/// layer here so the Flate layer under it can be read.
fn undo_ascii85(doc: &mut lopdf::Document) {
    use lopdf::Object;
    for obj in doc.objects.values_mut() {
        let Object::Stream(st) = obj else { continue };
        let filters: Vec<Object> = match st.dict.get(b"Filter") {
            Ok(Object::Array(a)) => a.clone(),
            Ok(Object::Name(n)) => vec![Object::Name(n.clone())],
            _ => continue,
        };
        if filters.first().and_then(|f| f.as_name().ok()) != Some(b"ASCII85Decode".as_slice()) {
            continue;
        }
        let Some(raw) = ascii85(&st.content) else { continue };
        st.content = raw;
        let rest = filters[1..].to_vec();
        if rest.is_empty() {
            st.dict.remove(b"Filter");
        } else {
            st.dict.set("Filter", Object::Array(rest));
        }
    }
}

fn ascii85(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let (mut group, mut n) = (0u64, 0);
    let body = data.strip_prefix(b"<~").unwrap_or(data);
    for &c in body {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group = group * 85 + (c - b'!') as u64;
                n += 1;
                if n == 5 {
                    out.extend_from_slice(&(group as u32).to_be_bytes());
                    (group, n) = (0, 0);
                }
            }
            c if c.is_ascii_whitespace() => {}
            _ => return None,
        }
    }
    if n > 0 {
        for _ in n..5 {
            group = group * 85 + 84;
        }
        out.extend_from_slice(&(group as u32).to_be_bytes()[..n - 1]);
    }
    Some(out)
}

fn zip_entry(bytes: &[u8], name: &str) -> Result<String, String> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "Not a valid Office file.".to_string())?;
    let mut s = String::new();
    archive.by_name(name).map_err(|_| "Not a valid Office file.".to_string())?.read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Text runs (`<w:t>`/`<a:t>`) of an Office XML part, one line per paragraph.
fn xml_text(xml: &str, run: &str, para: &str) -> String {
    let (open, close, end_para) = (format!("<{run}"), format!("</{run}>"), format!("</{para}>"));
    let mut out = String::new();
    let mut rest = xml;
    loop {
        let next_run = rest.find(&open).filter(|i| rest[i + open.len()..].starts_with(['>', ' ']));
        let next_para = rest.find(&end_para);
        match (next_run, next_para) {
            (Some(r), p) if p.is_none_or(|p| r < p) => {
                let after = &rest[r..];
                let Some(gt) = after.find('>') else { break };
                let body = &after[gt + 1..];
                let Some(endi) = body.find(&close) else { break };
                out.push_str(&unescape(&body[..endi]));
                rest = &body[endi + close.len()..];
            }
            (_, Some(p)) => {
                out.push('\n');
                rest = &rest[p + end_para.len()..];
            }
            _ => break,
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

// ───────────── the request ─────────────

#[derive(Clone, Debug, Default)]
pub struct Counts {
    pub choice: u32,
    pub multi: u32,
    pub truefalse: u32,
    pub short: u32,
    pub code: u32,
}

impl Counts {
    pub fn total(&self) -> u32 {
        self.choice + self.multi + self.truefalse + self.short + self.code
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    /// (file name, extracted text)
    pub sources: Vec<(String, String)>,
    pub counts: Counts,
    pub code_lang: Lang,
    pub difficulty: String,
    pub language: String,
    pub notes: String,
}

/// Languages the builder writes code questions in (their tests are plain stdin → stdout).
pub const CODE_LANGS: [Lang; 7] = [Lang::Python, Lang::JavaScript, Lang::C, Lang::Cpp, Lang::Java, Lang::Go, Lang::Rust];

fn schema() -> Value {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["questions"],
        "properties": {"questions": {"type": "array", "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["type", "prompt", "points", "options", "correct", "answer", "accepted", "starter", "solution", "tests"],
            "properties": {
                "type": {"type": "string", "enum": ["choice", "multi", "truefalse", "short", "code"]},
                "prompt": {"type": "string"},
                "points": {"type": "number"},
                "options": strings,
                "correct": {"type": "array", "items": {"type": "integer"}},
                "answer": {"type": "boolean"},
                "accepted": strings,
                "starter": {"type": "string"},
                "solution": {"type": "string"},
                "tests": {"type": "array", "items": {
                    "type": "object", "additionalProperties": false, "required": ["input", "expected"],
                    "properties": {"input": {"type": "string"}, "expected": {"type": "string"}}
                }}
            }
        }}}
    })
}

const SYSTEM: &str = "You write exam questions for a teacher, using only the course material the teacher provides. \
Every question must be answerable from that material; do not test trivia from outside it. \
Write clear, unambiguous questions at the requested difficulty, in the requested language. \
Return only the JSON object described, with exactly the requested number of questions of each type.";

fn instructions(r: &Request) -> String {
    let c = &r.counts;
    let lang = r.code_lang.name();
    let read = match r.code_lang {
        Lang::Python => "input()",
        Lang::JavaScript => "readline() (returns one line) and print with console.log()",
        Lang::C => "scanf",
        Lang::Cpp => "cin",
        Lang::Java => "a Scanner on System.in inside public class Main",
        Lang::Go => "fmt.Scan",
        _ => "std::io::stdin",
    };
    format!(
        "Write these questions from the material above:\n\
- {} multiple choice (\"choice\"): 4 options, exactly one correct; \"correct\" holds its 0-based index.\n\
- {} multiple answer (\"multi\"): 4-5 options, two or more correct; \"correct\" lists their 0-based indexes.\n\
- {} true/false (\"truefalse\"): a statement; \"answer\" is true or false.\n\
- {} short answer (\"short\"): one word, number or short phrase; \"accepted\" lists every acceptable spelling.\n\
- {} code (\"code\") in {lang}: the program reads input with {read} and prints the answer. \"starter\" is a short starting file (reading the input, a comment where the student's code goes, never the answer); \"solution\" is a complete correct program; \"tests\" has 3 cases with the exact stdin \"input\" and the exact \"expected\" output.\n\
Difficulty: {}. Write the questions in {}. Points: 1 for choice, true/false and short answer, 2 for multiple answer, 5 for code.\n\
Fill fields a type doesn't use with empty values ([] , \"\" or false).{}",
        c.choice,
        c.multi,
        c.truefalse,
        c.short,
        c.code,
        r.difficulty,
        r.language,
        if r.notes.trim().is_empty() { String::new() } else { format!("\nTeacher's notes: {}", r.notes.trim()) }
    )
}

/// The material, each file in its own tag, up to `MAX_CHARS` in total. Returns the text and
/// whether anything had to be left out (the UI says so; nothing is cut silently).
pub fn material(sources: &[(String, String)]) -> (String, bool) {
    let mut out = String::new();
    let mut cut = false;
    for (name, text) in sources {
        let room = MAX_CHARS.saturating_sub(out.len());
        if room < 200 {
            cut = true;
            break;
        }
        let body: String = if text.len() > room {
            cut = true;
            text.chars().take(room).collect()
        } else {
            text.clone()
        };
        out.push_str(&format!("<document name=\"{}\">\n{}\n</document>\n", name.replace('"', "'"), body));
    }
    (out, cut)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(600)).timeout_write(Duration::from_secs(120)).build()
}

fn http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body).ok().and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from)).unwrap_or_else(|| body.chars().take(300).collect());
            match code {
                401 | 403 => format!("The provider rejected the API key ({code}). {msg}"),
                404 => format!("Not found ({code}): check the address and model name. {msg}"),
                429 => format!("Rate limited or out of credit ({code}). {msg}"),
                _ => format!("The provider returned an error ({code}). {msg}"),
            }
        }
        ureq::Error::Transport(t) => format!("Couldn't reach the provider: {t}"),
    }
}

fn base(c: &AiConfig) -> String {
    c.base_url.trim().trim_end_matches('/').to_string()
}

/// Models the provider offers, for the settings dropdown.
pub fn list_models(c: &AiConfig) -> Result<Vec<String>, String> {
    let req = match c.provider {
        Provider::Anthropic => agent().get(&format!("{}/v1/models?limit=100", base(c))).set("x-api-key", c.api_key.trim()).set("anthropic-version", "2026-10-21"),
        _ => {
            let r = agent().get(&format!("{}/models", base(c)));
            if c.api_key.trim().is_empty() { r } else { r.set("Authorization", &format!("Bearer {}", c.api_key.trim())) }
        }
    };
    let v: Value = req.call().map_err(http_error)?.into_json().map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default();
    ids.sort();
    if ids.is_empty() {
        return Err("The provider listed no models.".into());
    }
    Ok(ids)
}

/// Claude: Messages API with structured JSON output.
fn call_anthropic(c: &AiConfig, user: &str) -> Result<String, String> {
    let model = c.model.trim();
    let mut body = json!({
        "model": model,
        "max_tokens": 32000,
        "system": SYSTEM,
        "messages": [{"role": "user", "content": user}],
        "output_config": {"format": {"type": "json_schema", "schema": schema()}},
    });
    // Current models: thoughtful questions at high effort, and a server-side fallback if a
    // classifier declines the request.
    let current = ["claude-opus-5-5", "claude-opus-5", "claude-fable-5-1", "claude-sonnet-5-5"].contains(&model);
    let mut req = agent().post(&format!("{}/v1/messages", base(c))).set("x-api-key", c.api_key.trim()).set("anthropic-version", "2026-10-21").set("content-type", "application/json");
    if current {
        body["output_config"]["effort"] = json!("high");
        body["fallbacks"] = json!("default");
        req = req.set("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let v: Value = req.send_json(body).map_err(http_error)?.into_json().map_err(|e| e.to_string())?;
    match v["stop_reason"].as_str() {
        Some("refusal") => return Err("The model declined to write these questions. Try different material or wording.".into()),
        Some("max_tokens") => return Err("The answer was cut off. Ask for fewer questions at a time.".into()),
        _ => {}
    }
    let text: String = v["content"].as_array().map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default();
    Ok(text)
}

/// Every other provider: OpenAI-compatible chat completions, JSON mode when supported.
fn call_openai_compatible(c: &AiConfig, user: &str) -> Result<String, String> {
    let send = |json_mode: bool| -> Result<Value, ureq::Error> {
        let mut body = json!({
            "model": c.model.trim(),
            "messages": [
                {"role": "system", "content": format!("{SYSTEM}\nReply with JSON matching this schema:\n{}", schema())},
                {"role": "user", "content": user}
            ]
        });
        if json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        let mut req = agent().post(&format!("{}/chat/completions", base(c))).set("content-type", "application/json");
        if !c.api_key.trim().is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", c.api_key.trim()));
        }
        req.send_json(body)?.into_json().map_err(|e| ureq::Error::from(e))
    };
    let v = match send(true) {
        // Some servers don't know JSON mode: ask again without it (the prompt still asks for JSON).
        Err(ureq::Error::Status(400, _)) => send(false).map_err(http_error)?,
        r => r.map_err(http_error)?,
    };
    let choice = &v["choices"][0];
    if choice["finish_reason"] == "length" {
        return Err("The answer was cut off. Ask for fewer questions at a time.".into());
    }
    Ok(choice["message"]["content"].as_str().unwrap_or_default().to_string())
}

#[derive(Deserialize)]
struct RawQuestion {
    #[serde(rename = "type")]
    kind: String,
    prompt: String,
    #[serde(default)]
    points: f32,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    correct: Vec<Value>,
    #[serde(default)]
    answer: bool,
    #[serde(default)]
    accepted: Vec<String>,
    #[serde(default)]
    starter: String,
    #[serde(default)]
    solution: String,
    #[serde(default)]
    tests: Vec<RawTest>,
}

#[derive(Deserialize)]
struct RawTest {
    #[serde(default)]
    input: String,
    #[serde(default)]
    expected: String,
}

/// The JSON object in a reply (some models wrap it in prose or a code fence).
fn json_object(text: &str) -> Option<Value> {
    serde_json::from_str(text.trim()).ok().or_else(|| {
        let (a, b) = (text.find('{')?, text.rfind('}')?);
        serde_json::from_str(&text[a..=b]).ok()
    })
}

fn convert(raw: RawQuestion, lang: Lang, notes: &mut Vec<String>, n: usize) -> Option<Question> {
    let prompt = raw.prompt.trim().to_string();
    if prompt.is_empty() {
        return None;
    }
    let pts = |d: f32| if raw.points > 0.0 && raw.points <= 100.0 { raw.points } else { d };
    // Indexes as asked; smaller models also send flags per option or the option's text.
    let idx = |v: &[Value], opts: &[String]| -> Vec<usize> {
        if !v.is_empty() && v.iter().all(Value::is_boolean) {
            return v.iter().enumerate().filter(|(_, b)| b.as_bool() == Some(true)).map(|(i, _)| i).collect();
        }
        v.iter()
            .filter_map(|c| match c {
                Value::Number(n) => n.as_u64().map(|n| n as usize),
                Value::String(t) => t.trim().parse().ok().or_else(|| opts.iter().position(|o| o.trim() == t.trim())),
                _ => None,
            })
            .filter(|i| *i < opts.len())
            .collect()
    };
    let (kind, points) = match raw.kind.as_str() {
        "choice" | "multi" if raw.options.len() >= 2 => {
            let mut correct = idx(&raw.correct, &raw.options);
            correct.sort_unstable();
            correct.dedup();
            if correct.is_empty() {
                notes.push(format!("Question {n}: no correct option was marked; tick it before exporting."));
            }
            let multiple = raw.kind == "multi" && correct.len() > 1;
            (Kind::Choice { options: raw.options, correct, multiple }, pts(if multiple { 2.0 } else { 1.0 }))
        }
        "truefalse" => (Kind::TrueFalse { answer: raw.answer }, pts(1.0)),
        "short" => {
            let accepted: Vec<String> = raw.accepted.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
            if accepted.is_empty() {
                return None;
            }
            (Kind::Short { accepted, case_sensitive: false, show_format: false }, pts(1.0))
        }
        "code" => {
            let mut spec = CodeSpec::new(lang);
            if !raw.starter.trim().is_empty() {
                spec.starter = raw.starter;
            }
            spec.reference = raw.solution.clone();
            spec.tests = raw.tests.into_iter().map(|t| TestCase { input: t.input, expected: t.expected, hidden: false }).collect();
            spec.simple = spec.tests.len() <= 1;
            // Expected outputs come from running the solution, not from the model's guess.
            if !raw.solution.trim().is_empty() && crate::runner::tool(lang).is_some() {
                let mut failed = 0;
                for t in &mut spec.tests {
                    let out = crate::runner::run(lang, &raw.solution, &t.input, crate::runner::RUN_TIMEOUT);
                    if out.ok() {
                        t.expected = crate::grading::normalize_output(&out.stdout);
                    } else {
                        failed += 1;
                    }
                }
                if failed > 0 {
                    notes.push(format!("Question {n}: the AI's solution failed on {failed} test(s); check its expected output."));
                }
            } else if !spec.tests.is_empty() {
                notes.push(format!("Question {n}: expected outputs are the AI's (no {} on this PC to check them).", lang.name()));
            }
            if spec.tests.len() > 1 {
                spec.tests.last_mut().unwrap().hidden = true;
            }
            (Kind::Code(spec), pts(5.0))
        }
        _ => return None,
    };
    let mut q = Question::new(0, kind);
    q.prompt = prompt;
    q.points = points;
    Some(q)
}

pub enum Progress {
    Status(String),
    Done(Result<(Vec<Question>, Vec<String>), String>),
}

/// Asks the provider for questions and turns the answer into HDJLabs questions plus notes for
/// the teacher (anything to double-check).
pub fn generate(c: &AiConfig, r: &Request, status: impl Fn(String)) -> Result<(Vec<Question>, Vec<String>), String> {
    let (text, _) = material(&r.sources);
    let user = format!("{text}\n{}", instructions(r));
    status(format!("Asking {} ({})…", c.provider.label(), c.model.trim()));
    let reply = match c.provider {
        Provider::Anthropic => call_anthropic(c, &user)?,
        _ => call_openai_compatible(c, &user)?,
    };
    let v = json_object(&reply).ok_or("The reply wasn't the JSON that was asked for. Try again, or pick a stronger model.")?;
    let items = v["questions"].as_array().ok_or("The reply had no list of questions.")?;
    let raws: Vec<Option<RawQuestion>> = items.iter().map(|q| serde_json::from_value(q.clone()).ok()).collect();
    status("Checking the questions and running code solutions…".into());
    let mut notes = Vec::new();
    let qs: Vec<Question> = raws.into_iter().enumerate().filter_map(|(i, q)| convert(q?, r.code_lang, &mut notes, i + 1)).collect();
    if qs.is_empty() {
        return Err("No usable questions came back.".into());
    }
    let want = r.counts.total() as usize;
    if qs.len() != want {
        notes.push(format!("Asked for {want} questions, got {} usable ones.", qs.len()));
    }
    Ok((qs, notes))
}

// ───────────── Windows DPAPI ─────────────

mod dpapi {
    #[cfg(windows)]
    fn call(data: &[u8], protect: bool) -> Option<Vec<u8>> {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, windows::core::w!("HDJLabs AI key"), None, None, None, 0, &mut out).is_ok()
            } else {
                CryptUnprotectData(&input, None, None, None, None, 0, &mut out).is_ok()
            }
        };
        if !ok || out.pbData.is_null() {
            return None;
        }
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe {
            let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        }
        Some(v)
    }

    #[cfg(not(windows))]
    fn call(data: &[u8], _: bool) -> Option<Vec<u8>> {
        Some(data.to_vec())
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        call(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        call(data, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_text_and_reply_parsing() {
        let xml = "<w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space=\"preserve\"> world &amp; more</w:t></w:r></w:p><w:p><w:r><w:t>Line 2</w:t></w:r></w:p></w:body>";
        assert_eq!(xml_text(xml, "w:t", "w:p").trim(), "Hello world & more\nLine 2");
        assert!(dpapi::unprotect(&dpapi::protect(b"sk-test").unwrap()).unwrap() == b"sk-test");
        let reply = "Here you go:\n```json\n{\"questions\":[{\"type\":\"choice\",\"prompt\":\"2+2?\",\"points\":1,\"options\":[\"3\",\"4\",\"5\",\"6\"],\"correct\":[1],\"answer\":false,\"accepted\":[],\"starter\":\"\",\"solution\":\"\",\"tests\":[]},{\"type\":\"code\",\"prompt\":\"Print n*2\",\"points\":5,\"options\":[],\"correct\":[],\"answer\":false,\"accepted\":[],\"starter\":\"n = int(input())\\n\",\"solution\":\"print(int(input())*2)\",\"tests\":[{\"input\":\"4\",\"expected\":\"WRONG\"},{\"input\":\"10\",\"expected\":\"20\"}]}]}\n```";
        let v = json_object(reply).unwrap();
        let raws: Vec<RawQuestion> = serde_json::from_value(v["questions"].clone()).unwrap();
        let mut notes = vec![];
        let qs: Vec<Question> = raws.into_iter().enumerate().filter_map(|(i, q)| convert(q, Lang::Python, &mut notes, i + 1)).collect();
        assert!(matches!(&qs[0].kind, Kind::Choice { correct, multiple: false, .. } if correct == &vec![1]));
        let Kind::Code(spec) = &qs[1].kind else { panic!() };
        // The model's wrong expected output is replaced by what the solution really prints.
        assert_eq!(spec.tests[0].expected, "8");
        assert!(spec.tests[1].hidden);
        let (m, cut) = material(&[("a.txt".into(), "x".repeat(MAX_CHARS + 10))]);
        assert!(cut && m.len() <= MAX_CHARS + 100);
    }
}
