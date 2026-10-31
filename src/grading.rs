//! Scoring. Choice / true-false / short answers are graded instantly; code is graded by running
//! the teacher's test cases (stdin -> expected stdout) through the sandboxed runner.

use crate::model::*;
use crate::runner::{self, RunOutput};

pub fn normalize_output(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
    lines.join("\n").trim_end_matches('\n').trim_end().to_string()
}

pub fn outputs_match(actual: &str, expected: &str) -> bool {
    normalize_output(actual) == normalize_output(expected)
}

fn normalize_text(s: &str, case_sensitive: bool) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if case_sensitive { collapsed } else { collapsed.to_lowercase() }
}

fn result(q: &Question, earned: f32, status: Status) -> QuestionResult {
    QuestionResult { qid: q.id, earned, max: q.points, status, tests_passed: 0, tests_total: 0 }
}

/// Grades everything except code questions, which need `grade_code`.
pub fn grade_static(q: &Question, a: &Answer) -> QuestionResult {
    if !a.is_answered() {
        return result(q, 0.0, Status::Unanswered);
    }
    match (&q.kind, a) {
        (Kind::Choice { correct, multiple, options }, Answer::Choice(picked)) => {
            let mut want = correct.clone();
            want.sort_unstable();
            let mut got = picked.clone();
            got.sort_unstable();
            got.dedup();
            if got == want {
                return result(q, q.points, Status::Correct);
            }
            if *multiple && !correct.is_empty() {
                // Partial credit: +1 per right pick, -1 per wrong pick, floored at zero.
                let right = got.iter().filter(|i| want.contains(i)).count() as f32;
                let wrong = got.iter().filter(|i| !want.contains(i) && **i < options.len()).count() as f32;
                let frac = ((right - wrong) / want.len() as f32).max(0.0);
                if frac > 0.0 {
                    return result(q, (q.points * frac * 100.0).round() / 100.0, Status::Partial);
                }
            }
            result(q, 0.0, Status::Wrong)
        }
        (Kind::TrueFalse { answer }, Answer::Bool(b)) => {
            if answer == b { result(q, q.points, Status::Correct) } else { result(q, 0.0, Status::Wrong) }
        }
        (Kind::Short { accepted, case_sensitive, .. }, Answer::Text(t)) => {
            let got = normalize_text(t, *case_sensitive);
            if accepted.iter().any(|acc| !acc.trim().is_empty() && normalize_text(acc, *case_sensitive) == got) {
                result(q, q.points, Status::Correct)
            } else {
                result(q, 0.0, Status::Wrong)
            }
        }
        (Kind::Code(_), _) => result(q, 0.0, Status::Review),
        (Kind::Upload { .. }, Answer::File { .. }) => result(q, 0.0, Status::Review),
        _ => result(q, 0.0, Status::Wrong),
    }
}

#[derive(Clone, Debug)]
pub struct TestOutcome {
    pub passed: bool,
    pub hidden: bool,
    pub input: String,
    pub expected: String,
    pub output: RunOutput,
}

pub fn run_tests(spec: &CodeSpec, lang: Lang, source: &str, include_hidden: bool) -> Vec<TestOutcome> {
    spec.tests
        .iter()
        .filter(|t| include_hidden || !t.hidden)
        .map(|t| {
            let out = runner::run(lang, source, &spec.stdin_for(&t.input), runner::RUN_TIMEOUT);
            TestOutcome {
                passed: out.ok() && outputs_match(&out.stdout, &t.expected),
                hidden: t.hidden,
                input: t.input.clone(),
                expected: t.expected.clone(),
                output: out,
            }
        })
        .collect()
}

/// Runs every test (including hidden ones). Code without tests is left for manual review.
pub fn grade_code(q: &Question, a: &Answer) -> QuestionResult {
    let Kind::Code(spec) = &q.kind else { return grade_static(q, a) };
    let Answer::Code { lang, source } = a else { return result(q, 0.0, Status::Unanswered) };
    if source.trim().is_empty() || source.trim() == spec.starter.trim() {
        return result(q, 0.0, Status::Unanswered);
    }
    if spec.teacher_graded() {
        return result(q, 0.0, Status::Review);
    }
    let outcomes = run_tests(spec, *lang, source, true);
    let passed = outcomes.iter().filter(|o| o.passed).count() as u32;
    let total = outcomes.len() as u32;
    let earned = (q.points * passed as f32 / total as f32 * 100.0).round() / 100.0;
    let status = match passed {
        0 => Status::Wrong,
        p if p == total => Status::Correct,
        _ => Status::Partial,
    };
    QuestionResult { qid: q.id, earned, max: q.points, status, tests_passed: passed, tests_total: total }
}

/// Re-grades a submission on the teacher's PC from the student's answers alone, so a score the
/// student's PC reported can't be trusted blindly. Code whose compiler isn't on this PC keeps the
/// student's result for that question rather than scoring zero.
pub fn regrade(exam: &Exam, sub: &Submission) -> Vec<QuestionResult> {
    sub.question_order
        .iter()
        .filter_map(|qid| {
            let q = exam.questions.iter().find(|q| q.id == *qid)?;
            let a = sub.answers.get(qid).cloned().unwrap_or_default();
            if let (Kind::Code(c), Answer::Code { lang, .. }) = (&q.kind, &a) {
                if !c.lang.is_preview_only() && !c.tests.is_empty() && runner::tool(*lang).is_none() {
                    if let Some(theirs) = sub.results.iter().find(|r| r.qid == *qid) {
                        return Some(theirs.clone());
                    }
                }
            }
            Some(grade(q, &a))
        })
        .collect()
}

pub fn grade(q: &Question, a: &Answer) -> QuestionResult {
    match q.kind {
        Kind::Code(_) => grade_code(q, a),
        _ => grade_static(q, a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(kind: Kind) -> Question {
        Question { attachments: vec![], id: 1, prompt: "p".into(), points: 4.0, kind }
    }

    #[test]
    fn choice_single_and_multi() {
        let single = q(Kind::Choice { options: vec!["a".into(), "b".into(), "c".into()], correct: vec![2], multiple: false });
        assert_eq!(grade_static(&single, &Answer::Choice(vec![2])).status, Status::Correct);
        assert_eq!(grade_static(&single, &Answer::Choice(vec![1])).earned, 0.0);
        assert_eq!(grade_static(&single, &Answer::Choice(vec![])).status, Status::Unanswered);

        let multi = q(Kind::Choice { options: vec!["a".into(), "b".into(), "c".into(), "d".into()], correct: vec![0, 2], multiple: true });
        assert_eq!(grade_static(&multi, &Answer::Choice(vec![2, 0])).earned, 4.0);
        assert_eq!(grade_static(&multi, &Answer::Choice(vec![0])).earned, 2.0);
        // Selecting everything must not earn credit.
        assert_eq!(grade_static(&multi, &Answer::Choice(vec![0, 1, 2, 3])).earned, 0.0);
    }

    #[test]
    fn short_answer_normalizes() {
        let s = q(Kind::Short { accepted: vec!["Responsive  Design".into(), "rwd".into()], case_sensitive: false, show_format: false });
        assert_eq!(grade_static(&s, &Answer::Text("  responsive design ".into())).status, Status::Correct);
        assert_eq!(grade_static(&s, &Answer::Text("RWD".into())).status, Status::Correct);
        assert_eq!(grade_static(&s, &Answer::Text("design".into())).status, Status::Wrong);
        let cs = q(Kind::Short { accepted: vec!["NaCl".into()], case_sensitive: true, show_format: true });
        assert_eq!(answer_format("THM{h3ll0_w0rld!} ok"), "***{*****_*****!} **");
        assert_eq!(grade_static(&cs, &Answer::Text("nacl".into())).status, Status::Wrong);
    }

    #[test]
    fn teacher_regrade_ignores_forged_score_and_answers_stay_home() {
        let mut exam = Exam::new();
        exam.questions = vec![
            Question { attachments: vec![], id: 1, prompt: "p".into(), points: 2.0, kind: Kind::Choice { options: vec!["a".into(), "b".into()], correct: vec![1], multiple: false } },
            Question { attachments: vec![], id: 2, prompt: "p".into(), points: 3.0, kind: Kind::Short { accepted: vec!["HDJ{ok_1}".into()], case_sensitive: true, show_format: true } },
            Question { attachments: vec![], id: 3, prompt: "p".into(), points: 1.0, kind: Kind::TrueFalse { answer: true } },
        ];
        let mut answers = std::collections::BTreeMap::new();
        answers.insert(1, Answer::Choice(vec![0]));
        answers.insert(2, Answer::Text("HDJ{ok_1}".into()));
        let forged: Vec<QuestionResult> = exam.questions.iter().map(|q| QuestionResult { qid: q.id, earned: q.points, max: q.points, status: Status::Correct, tests_passed: 0, tests_total: 0 }).collect();
        let sub = Submission {
            exam_id: exam.id.clone(),
            exam_title: String::new(),
            attempt_id: "a".into(),
            student_name: "s".into(),
            student_id: "1".into(),
            machine: "m".into(),
            started_at: 0,
            submitted_at: 0,
            question_order: vec![1, 2, 3],
            answers,
            results: forged,
            score: 6.0,
            max_score: 6.0,
            violations: vec![],
            forced: false,
        };
        let re = regrade(&exam, &sub);
        let earned: Vec<f32> = re.iter().map(|r| r.earned).collect();
        assert_eq!(earned, vec![0.0, 3.0, 0.0], "only the short answer was right");
        assert_eq!(re[2].status, Status::Unanswered);

        exam.settings.keep_answers_off_pcs = true;
        let shipped = exam.for_students();
        let json = serde_json::to_string(&shipped).unwrap();
        assert!(!json.contains("HDJ{ok_1}"), "short answer must not ship");
        assert!(matches!(&shipped.questions[0].kind, Kind::Choice { correct, .. } if correct.is_empty()));
        assert!(matches!(&shipped.questions[1].kind, Kind::Short { accepted, .. } if accepted == &["***{**_*}".to_string()]));
    }

    #[test]
    fn uploads_wait_for_the_teacher_and_need_lockdown_off() {
        let up = q(Kind::Upload { extensions: vec!["pdf".into()], max_mb: 2.0 });
        assert_eq!(grade_static(&up, &Answer::Empty).status, Status::Unanswered);
        assert_eq!(grade_static(&up, &Answer::File { name: "r.pdf".into(), size: 10, data: "AAAA".into() }).status, Status::Review);
        let mut exam = Exam::new();
        exam.questions = vec![Question { attachments: vec![], id: 1, prompt: "Upload".into(), points: 1.0, kind: Kind::Upload { extensions: vec![], max_mb: 1.0 } }];
        exam.settings.lockdown = true;
        assert!(exam.validate().iter().any(|e| e.contains("lockdown")), "a locked exam must refuse uploads");
        exam.settings.lockdown = false;
        assert!(!exam.validate().iter().any(|e| e.contains("lockdown")));
    }

    #[test]
    fn code_without_expected_output_waits_for_the_teacher() {
        let mut spec = CodeSpec::new(Lang::Python);
        assert!(spec.teacher_graded(), "a new question has no expected output yet");
        let code = q(Kind::Code(spec.clone()));
        let answer = Answer::Code { lang: Lang::Python, source: "print(42)".into() };
        assert_eq!(grade_code(&code, &answer).status, Status::Review);
        spec.tests[0].expected = "42".into();
        assert!(!spec.teacher_graded());
        spec.tests.clear();
        assert!(spec.teacher_graded(), "\"I'll grade it myself\"");
    }

    #[test]
    fn output_normalization() {
        assert!(outputs_match("1\r\n2  \r\n\r\n", "1\n2"));
        assert!(!outputs_match("1 2", "1\n2"));
    }
}
