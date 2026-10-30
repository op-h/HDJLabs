//! Files on disk: teacher projects (.hdjx), result files (.hdjr), and the student's encrypted
//! in-progress autosave (so a crash or power cut never loses an attempt).

use crate::crypto;
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredResult {
    pub submission: Submission,
    /// Teacher's manual points per question id (overrides the automatic score).
    #[serde(default)]
    pub overrides: BTreeMap<u32, f32>,
}

impl StoredResult {
    pub fn earned(&self, qid: u32) -> f32 {
        if let Some(v) = self.overrides.get(&qid) {
            return *v;
        }
        self.submission.results.iter().find(|r| r.qid == qid).map(|r| r.earned).unwrap_or(0.0)
    }

    pub fn total(&self) -> f32 {
        self.submission.results.iter().map(|r| self.earned(r.qid)).sum()
    }

    pub fn percent(&self) -> f32 {
        if self.submission.max_score <= 0.0 { 0.0 } else { self.total() / self.submission.max_score * 100.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub exam: Exam,
    pub teacher_secret: String,
    pub teacher_public: String,
    #[serde(default)]
    pub last_export: Option<PathBuf>,
    #[serde(default)]
    pub results: Vec<StoredResult>,
}

impl Project {
    pub fn new() -> Self {
        let (s, p) = crypto::new_keypair();
        Project { version: 1, exam: Exam::new(), teacher_secret: s, teacher_public: p, last_export: None, results: vec![] }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let p: Project = serde_json::from_slice(&data).map_err(|e| format!("Not an HDJLabs project: {e}"))?;
        if crypto::public_from_secret(&p.teacher_secret).ok().as_deref() != Some(p.teacher_public.as_str()) {
            return Err("The project's key pair is damaged.".into());
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("hdjx.tmp");
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Adds or replaces a submission (same attempt) — keeps any manual grading already done.
    pub fn upsert_result(&mut self, sub: Submission) -> bool {
        if sub.exam_id != self.exam.id {
            return false;
        }
        match self.results.iter_mut().find(|r| r.submission.attempt_id == sub.attempt_id) {
            Some(r) if r.submission == sub => false,
            Some(r) => {
                r.submission = sub;
                true
            }
            None => {
                self.results.push(StoredResult { submission: sub, overrides: BTreeMap::new() });
                true
            }
        }
    }

    pub fn import_result_file(&mut self, path: &Path) -> Result<bool, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let sb: crypto::SealedBox = serde_json::from_slice(&data).map_err(|_| "Not an HDJLabs result file".to_string())?;
        if sb.exam_id != self.exam.id {
            return Err("This result belongs to a different exam.".into());
        }
        let plain = crypto::open_sealed(&self.teacher_secret, &sb).map_err(|_| "Result file is damaged or was altered.".to_string())?;
        let sub: Submission = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
        Ok(self.upsert_result(sub))
    }
}

pub fn documents() -> PathBuf {
    dirs::document_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_else(std::env::temp_dir))
}

pub fn projects_dir() -> PathBuf {
    let d = documents().join("HDJLabs").join("Exams");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn app_data() -> PathBuf {
    let d = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HDJLabs");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn recent_load() -> Vec<PathBuf> {
    std::fs::read(app_data().join("recent.json"))
        .ok()
        .and_then(|d| serde_json::from_slice::<Vec<PathBuf>>(&d).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

pub fn recent_push(path: &Path) {
    let mut v = recent_load();
    v.retain(|p| p != path);
    v.insert(0, path.to_path_buf());
    v.truncate(8);
    let _ = std::fs::write(app_data().join("recent.json"), serde_json::to_vec(&v).unwrap_or_default());
}

// ─────────────── student attempt autosave ───────────────

fn attempt_path(exam_id: &str) -> PathBuf {
    let d = app_data().join("attempts");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{}.bin", &exam_id[..exam_id.len().min(32)]))
}

pub fn attempt_save<T: Serialize>(exam_id: &str, key: &[u8; 32], value: &T) {
    let Ok(plain) = serde_json::to_vec(value) else { return };
    let nonce = crypto::random_bytes::<12>();
    let mut out = nonce.to_vec();
    out.extend(crypto::seal(key, &nonce, &plain, exam_id.as_bytes()));
    let path = attempt_path(exam_id);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub fn attempt_load<T: for<'de> Deserialize<'de>>(exam_id: &str, key: &[u8; 32]) -> Option<T> {
    let data = std::fs::read(attempt_path(exam_id)).ok()?;
    if data.len() < 12 {
        return None;
    }
    let nonce: [u8; 12] = data[..12].try_into().ok()?;
    let plain = crypto::open(key, &nonce, &data[12..], exam_id.as_bytes()).ok()?;
    serde_json::from_slice(&plain).ok()
}

/// Writes the sealed result to Documents\HDJLabs Results and, when possible, next to the exam
/// (handy for collecting results from a USB stick). Returns every path written.
pub fn save_result_file(sealed: &crypto::SealedBox, title: &str, name: &str, sid: &str) -> Vec<PathBuf> {
    let file = format!("{} - {} {}.hdjr", crate::net::sanitize_file_name(title), crate::net::sanitize_file_name(name), crate::net::sanitize_file_name(sid));
    let data = serde_json::to_vec_pretty(sealed).unwrap_or_default();
    let mut dirs_ = vec![documents().join("HDJLabs Results")];
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        dirs_.push(d.join("HDJLabs Results"));
    }
    let mut written = Vec::new();
    for d in dirs_ {
        if std::fs::create_dir_all(&d).is_ok() && std::fs::write(d.join(&file), &data).is_ok() {
            written.push(d.join(&file));
        }
    }
    written
}

pub fn results_csv(project: &Project) -> String {
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let qs = &project.exam.questions;
    let mut out = String::from("Student,Student ID,Machine,Submitted,Score,Max,Percent,Violations,Forced");
    for (i, _) in qs.iter().enumerate() {
        out.push_str(&format!(",Q{}", i + 1));
    }
    out.push('\n');
    for r in &project.results {
        let s = &r.submission;
        let when = chrono_like(s.submitted_at);
        out.push_str(&format!(
            "{},{},{},{},{:.2},{:.2},{:.1},{},{}",
            esc(&s.student_name),
            esc(&s.student_id),
            esc(&s.machine),
            esc(&when),
            r.total(),
            s.max_score,
            r.percent(),
            s.violations.len(),
            s.forced
        ));
        for q in qs {
            if s.results.iter().any(|x| x.qid == q.id) {
                out.push_str(&format!(",{:.2}", r.earned(q.id)));
            } else {
                out.push(',');
            }
        }
        out.push('\n');
    }
    out
}

/// "YYYY-MM-DD HH:MM" in UTC without pulling in a date crate.
pub fn chrono_like(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let secs = unix % 86400;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", secs / 3600, (secs / 60) % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format() {
        assert_eq!(chrono_like(0), "1970-01-01 00:00 UTC");
        assert_eq!(chrono_like(1_798_675_200), "2026-12-31 00:00 UTC");
    }

    #[test]
    fn project_roundtrip_and_result_import() {
        let mut p = Project::new();
        p.exam.questions.push(Question::new(1, Kind::TrueFalse { answer: true }));
        let path = std::env::temp_dir().join(format!("hdj-{}.hdjx", crypto::random_hex(4)));
        p.save(&path).unwrap();
        let mut q = Project::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(q.exam, p.exam);

        let sub = Submission {
            exam_id: q.exam.id.clone(),
            exam_title: "t".into(),
            attempt_id: "a".into(),
            student_name: "N".into(),
            student_id: "1".into(),
            machine: "m".into(),
            started_at: 0,
            submitted_at: 0,
            question_order: vec![1],
            answers: Default::default(),
            results: vec![QuestionResult { qid: 1, earned: 1.0, max: 1.0, status: Status::Correct, tests_passed: 0, tests_total: 0 }],
            score: 1.0,
            max_score: 1.0,
            violations: vec![],
            forced: false,
        };
        let sb = crypto::seal_for(&q.teacher_public, &q.exam.id, &serde_json::to_vec(&sub).unwrap()).unwrap();
        let rpath = std::env::temp_dir().join(format!("hdj-{}.hdjr", crypto::random_hex(4)));
        std::fs::write(&rpath, serde_json::to_vec(&sb).unwrap()).unwrap();
        assert!(q.import_result_file(&rpath).unwrap());
        assert!(!q.import_result_file(&rpath).unwrap(), "duplicate import is a no-op");
        let _ = std::fs::remove_file(&rpath);
        q.results[0].overrides.insert(1, 0.5);
        assert_eq!(q.results[0].total(), 0.5);
        assert!(results_csv(&q).contains("\"N\""));
    }
}
