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
        (Kind::Short { accepted, case_sensitive }, Answer::Text(t)) => {
            let got = normalize_text(t, *case_sensitive);
            if accepted.iter().any(|acc| !acc.trim().is_empty() && normalize_text(acc, *case_sensitive) == got) {
                result(q, q.points, Status::Correct)
            } else {
                result(q, 0.0, Status::Wrong)
            }
        }
        (Kind::Code(_), _) => result(q, 0.0, Status::Review),
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
            let out = runner::run(lang, source, &t.input, runner::RUN_TIMEOUT);
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
    if spec.tests.is_empty() {
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
        Question { id: 1, prompt: "p".into(), points: 4.0, kind }
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
        let s = q(Kind::Short { accepted: vec!["Responsive  Design".into(), "rwd".into()], case_sensitive: false });
        assert_eq!(grade_static(&s, &Answer::Text("  responsive design ".into())).status, Status::Correct);
        assert_eq!(grade_static(&s, &Answer::Text("RWD".into())).status, Status::Correct);
        assert_eq!(grade_static(&s, &Answer::Text("design".into())).status, Status::Wrong);
        let cs = q(Kind::Short { accepted: vec!["NaCl".into()], case_sensitive: true });
        assert_eq!(grade_static(&cs, &Answer::Text("nacl".into())).status, Status::Wrong);
    }

    #[test]
    fn output_normalization() {
        assert!(outputs_match("1\r\n2  \r\n\r\n", "1\n2"));
        assert!(!outputs_match("1 2", "1\n2"));
    }
}
