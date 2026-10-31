//! Results reports for the teacher: an Excel workbook and a printable PDF, alongside the CSV.

use crate::model::Status;
use crate::storage::{self, Project};
use crate::ui::studio::{duration, Analytics};

fn mmss(secs: u64) -> String {
    if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60) } else { format!("{:02}:{:02}", secs / 60, secs % 60) }
}

fn grading_status(r: &storage::StoredResult) -> &'static str {
    match r.pending_review() {
        0 if r.released => "Score sent",
        0 => "Graded",
        _ => "Needs grading",
    }
}

/// Excel workbook: Grades (one row per student), Questions, Summary.
pub fn xlsx(project: &Project) -> Result<Vec<u8>, String> {
    use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook};
    let e = |x: rust_xlsxwriter::XlsxError| x.to_string();
    let a = Analytics::of(project);
    let mut wb = Workbook::new();
    let head = Format::new().set_bold().set_font_color(Color::RGB(0x0B0E0D)).set_background_color(Color::RGB(0xA8E33A)).set_border_bottom(FormatBorder::Thin).set_align(FormatAlign::Center);
    let pct = Format::new().set_num_format("0%");
    let num = Format::new().set_num_format("0.##");
    let title = Format::new().set_bold().set_font_size(14);

    let ws = wb.add_worksheet().set_name("Grades").map_err(e)?;
    let qs = &project.exam.questions;
    let mut headers: Vec<String> = ["Student", "Student ID", "PC", "Score", "Max", "Percent", "Time", "Alerts", "Grading", "Submitted"].iter().map(|s| s.to_string()).collect();
    headers.extend(qs.iter().enumerate().map(|(i, _)| format!("Q{}", i + 1)));
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, h, &head).map_err(e)?;
    }
    let mut rows: Vec<&storage::StoredResult> = project.results.iter().collect();
    rows.sort_by(|x, y| y.percent().total_cmp(&x.percent()));
    for (i, r) in rows.iter().enumerate() {
        let row = i as u32 + 1;
        let s = &r.submission;
        ws.write_string(row, 0, &s.student_name).map_err(e)?;
        ws.write_string(row, 1, &s.student_id).map_err(e)?;
        ws.write_string(row, 2, &s.machine).map_err(e)?;
        ws.write_number_with_format(row, 3, r.total() as f64, &num).map_err(e)?;
        ws.write_number_with_format(row, 4, s.max_score as f64, &num).map_err(e)?;
        ws.write_number_with_format(row, 5, (r.percent() / 100.0) as f64, &pct).map_err(e)?;
        ws.write_string(row, 6, duration(s).map(mmss).unwrap_or_default()).map_err(e)?;
        ws.write_number(row, 7, s.violations.len() as f64).map_err(e)?;
        ws.write_string(row, 8, grading_status(r)).map_err(e)?;
        ws.write_string(row, 9, storage::chrono_like(s.submitted_at)).map_err(e)?;
        for (qi, q) in qs.iter().enumerate() {
            if r.auto(q.id).is_some() {
                ws.write_number_with_format(row, 10 + qi as u16, r.earned(q.id) as f64, &num).map_err(e)?;
            }
        }
    }
    for (c, w) in [24.0, 12.0, 14.0, 8.0, 7.0, 9.0, 9.0, 7.0, 15.0, 22.0].iter().enumerate() {
        ws.set_column_width(c as u16, *w).map_err(e)?;
    }
    ws.set_freeze_panes(1, 1).map_err(e)?;
    ws.autofilter(0, 0, rows.len() as u32, (headers.len() - 1) as u16).map_err(e)?;

    let ws = wb.add_worksheet().set_name("Questions").map_err(e)?;
    for (c, h) in ["Question", "Type", "Points", "Average", "Left blank", "Note", "Prompt"].iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &head).map_err(e)?;
    }
    for (i, qa) in a.questions.iter().enumerate() {
        let row = i as u32 + 1;
        let q = &qs[qa.index];
        ws.write_string(row, 0, format!("Q{}", qa.index + 1)).map_err(e)?;
        ws.write_string(row, 1, q.kind.label()).map_err(e)?;
        ws.write_number_with_format(row, 2, q.points as f64, &num).map_err(e)?;
        ws.write_number_with_format(row, 3, qa.avg as f64, &pct).map_err(e)?;
        ws.write_number_with_format(row, 4, qa.blank as f64, &pct).map_err(e)?;
        let note = if Some(qa.index) == a.hardest { "Hardest" } else if Some(qa.index) == a.easiest { "Easiest" } else { "" };
        ws.write_string(row, 5, note).map_err(e)?;
        ws.write_string(row, 6, &q.prompt).map_err(e)?;
    }
    for (c, w) in [10.0, 16.0, 8.0, 10.0, 11.0, 10.0, 80.0].iter().enumerate() {
        ws.set_column_width(c as u16, *w).map_err(e)?;
    }

    let ws = wb.add_worksheet().set_name("Summary").map_err(e)?;
    ws.write_string_with_format(0, 0, &project.exam.title, &title).map_err(e)?;
    let summary: Vec<(&str, String)> = vec![
        ("Students", project.results.len().to_string()),
        ("Average", format!("{:.1}%", a.avg)),
        ("Median", format!("{:.1}%", a.median)),
        ("Highest", format!("{:.1}%", a.hi)),
        ("Lowest", format!("{:.1}%", a.lo)),
        ("Average time", a.avg_time.map(mmss).unwrap_or_default()),
        ("Fastest", a.fastest.as_ref().map(|(n, t)| format!("{n} ({})", mmss(*t))).unwrap_or_default()),
        ("Still to grade", a.to_grade.to_string()),
        ("Hardest question", a.hardest.map(|i| format!("Q{}", i + 1)).unwrap_or_default()),
        ("Easiest question", a.easiest.map(|i| format!("Q{}", i + 1)).unwrap_or_default()),
    ];
    for (i, (k, v)) in summary.iter().enumerate() {
        ws.write_string_with_format(i as u32 + 2, 0, *k, &Format::new().set_bold()).map_err(e)?;
        ws.write_string(i as u32 + 2, 1, v).map_err(e)?;
    }
    ws.write_string_with_format(14, 0, "Score band", &head).map_err(e)?;
    ws.write_string_with_format(14, 1, "Students", &head).map_err(e)?;
    for (i, c) in a.bins.iter().enumerate() {
        ws.write_string(15 + i as u32, 0, format!("{}–{}%", i * 10, if i == 9 { 100 } else { i * 10 + 9 })).map_err(e)?;
        ws.write_number(15 + i as u32, 1, *c as f64).map_err(e)?;
    }
    ws.set_column_width(0, 20.0).map_err(e)?;
    ws.set_column_width(1, 28.0).map_err(e)?;
    wb.save_to_buffer().map_err(e)
}

/// A4 report: summary, score distribution, students, questions.
pub fn pdf(project: &Project) -> Result<Vec<u8>, String> {
    use printpdf::*;
    let a = Analytics::of(project);
    let (w, h) = (Mm(210.0), Mm(297.0));
    let (doc, p1, l1) = PdfDocument::new(format!("{} — results", project.exam.title), w, h, "Layer 1");
    let fonts = std::env::var("WINDIR").map(|d| std::path::PathBuf::from(d).join("Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let load = |names: &[&str]| -> Result<IndirectFontRef, String> {
        for n in names {
            if let Ok(f) = std::fs::File::open(fonts.join(n)) {
                return doc.add_external_font_with_subsetting(f, true).map_err(|e| e.to_string());
            }
        }
        doc.add_builtin_font(BuiltinFont::Helvetica).map_err(|e| e.to_string())
    };
    // Segoe UI / Tahoma carry Arabic presentation forms, so names print joined.
    let regular = load(&["segoeui.ttf", "tahoma.ttf", "arial.ttf"])?;
    let bold = load(&["segoeuib.ttf", "tahomabd.ttf", "arialbd.ttf"])?;
    let shown = |s: &str| crate::ui::rtl::line(s);
    let cut = |s: &str, n: usize| if s.chars().count() > n { format!("{}…", s.chars().take(n - 1).collect::<String>()) } else { s.to_string() };

    let mut layer = doc.get_page(p1).get_layer(l1);
    let mut y = 280.0;
    let ink = Color::Rgb(Rgb::new(0.07, 0.09, 0.08, None));
    let muted = Color::Rgb(Rgb::new(0.4, 0.44, 0.42, None));
    let accent = Color::Rgb(Rgb::new(0.25, 0.44, 0.04, None));
    let text = |l: &PdfLayerReference, s: &str, size: f32, x: f32, y: f32, f: &IndirectFontRef, c: &Color| {
        l.set_fill_color(c.clone());
        l.use_text(s, size, Mm(x), Mm(y), f);
    };
    let fill_rect = |l: &PdfLayerReference, x0: f32, y0: f32, x1: f32, y1: f32, c: Color| {
        l.set_fill_color(c);
        l.add_rect(Rect::new(Mm(x0), Mm(y0), Mm(x1), Mm(y1)).with_mode(path::PaintMode::Fill));
    };

    text(&layer, &shown(&project.exam.title), 18.0, 18.0, y, &bold, &ink);
    y -= 7.0;
    text(&layer, &format!("Results report · {} student(s) · {}", project.results.len(), storage::chrono_like(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))), 9.5, 18.0, y, &regular, &muted);
    y -= 12.0;

    // Summary tiles.
    let tiles = [
        ("AVERAGE", format!("{:.0}%", a.avg)),
        ("MEDIAN", format!("{:.0}%", a.median)),
        ("HIGHEST", format!("{:.0}%", a.hi)),
        ("LOWEST", format!("{:.0}%", a.lo)),
        ("AVG TIME", a.avg_time.map(mmss).unwrap_or_else(|| "—".into())),
        ("TO GRADE", a.to_grade.to_string()),
    ];
    for (i, (k, v)) in tiles.iter().enumerate() {
        let x = 18.0 + i as f32 * 29.0;
        fill_rect(&layer, x, y - 14.0, x + 27.0, y + 4.0, Color::Rgb(Rgb::new(0.94, 0.95, 0.92, None)));
        text(&layer, k, 7.0, x + 2.5, y - 1.0, &bold, &muted);
        text(&layer, v, 14.0, x + 2.5, y - 10.0, &bold, &ink);
    }
    y -= 26.0;

    // Distribution.
    text(&layer, "Score distribution", 11.0, 18.0, y, &bold, &ink);
    y -= 4.0;
    let max = (*a.bins.iter().max().unwrap_or(&1)).max(1) as f32;
    let (base, plot_h) = (y - 32.0, 28.0);
    for (i, c) in a.bins.iter().enumerate() {
        let x = 18.0 + i as f32 * 17.0;
        let bh = plot_h * (*c as f32 / max);
        if *c > 0 {
            fill_rect(&layer, x, base, x + 14.0, base + bh, if i >= 5 { Color::Rgb(Rgb::new(0.55, 0.78, 0.16, None)) } else { Color::Rgb(Rgb::new(0.86, 0.6, 0.25, None)) });
            text(&layer, &c.to_string(), 8.0, x + 5.5, base + bh + 1.5, &regular, &ink);
        }
        text(&layer, &format!("{}%", i * 10), 7.0, x + 3.0, base - 4.5, &regular, &muted);
    }
    y = base - 14.0;

    // Students.
    let header = |l: &PdfLayerReference, y: f32, cols: &[(&str, f32)]| {
        fill_rect(l, 18.0, y - 2.0, 192.0, y + 4.5, Color::Rgb(Rgb::new(0.66, 0.89, 0.23, None)));
        for (t, x) in cols {
            l.set_fill_color(Color::Rgb(Rgb::new(0.04, 0.05, 0.05, None)));
            l.use_text(*t, 8.0, Mm(*x), Mm(y), &bold);
        }
    };
    let student_cols = [("Student", 20.0), ("ID", 78.0), ("Score", 102.0), ("%", 124.0), ("Time", 138.0), ("Alerts", 156.0), ("Grading", 170.0)];
    text(&layer, "Students", 11.0, 18.0, y, &bold, &ink);
    y -= 7.0;
    header(&layer, y, &student_cols);
    y -= 6.5;
    let mut rows: Vec<&storage::StoredResult> = project.results.iter().collect();
    rows.sort_by(|x, z| z.percent().total_cmp(&x.percent()));
    let mut page = 1;
    let mut new_page = |layer: &mut PdfLayerReference, y: &mut f32| {
        page += 1;
        let (p, l) = doc.add_page(w, h, format!("Layer {page}"));
        *layer = doc.get_page(p).get_layer(l);
        *y = 280.0;
    };
    for (i, r) in rows.iter().enumerate() {
        if y < 20.0 {
            new_page(&mut layer, &mut y);
            header(&layer, y, &student_cols);
            y -= 6.5;
        }
        if i % 2 == 1 {
            fill_rect(&layer, 18.0, y - 1.8, 192.0, y + 4.0, Color::Rgb(Rgb::new(0.96, 0.97, 0.95, None)));
        }
        let s = &r.submission;
        let pct = r.percent();
        let cells = [
            cut(&shown(&s.student_name), 30),
            cut(&s.student_id, 12),
            format!("{} / {}", crate::ui::theme::fmt_points(r.total()), crate::ui::theme::fmt_points(s.max_score)),
            format!("{pct:.0}%"),
            duration(s).map(mmss).unwrap_or_else(|| "—".into()),
            s.violations.len().to_string(),
            grading_status(r).to_string(),
        ];
        for (c, (_, x)) in cells.iter().zip(student_cols.iter()) {
            text(&layer, c, 8.5, *x, y, &regular, &ink);
        }
        y -= 6.0;
    }
    y -= 6.0;

    // Questions.
    if y < 50.0 {
        new_page(&mut layer, &mut y);
    }
    text(&layer, "Questions", 11.0, 18.0, y, &bold, &ink);
    y -= 7.0;
    let qcols = [("Q", 20.0), ("Type", 30.0), ("Average", 62.0), ("Blank", 80.0), ("", 96.0), ("Prompt", 116.0)];
    header(&layer, y, &qcols);
    y -= 6.5;
    for qa in &a.questions {
        if y < 20.0 {
            new_page(&mut layer, &mut y);
            header(&layer, y, &qcols);
            y -= 6.5;
        }
        let q = &project.exam.questions[qa.index];
        let note = if Some(qa.index) == a.hardest { "Hardest" } else if Some(qa.index) == a.easiest { "Easiest" } else { "" };
        let cells = [format!("Q{}", qa.index + 1), q.kind.label().to_string(), format!("{:.0}%", qa.avg * 100.0), format!("{:.0}%", qa.blank * 100.0), note.to_string(), cut(&shown(&qa.prompt), 48)];
        for (i, (c, (_, x))) in cells.iter().zip(qcols.iter()).enumerate() {
            text(&layer, c, 8.5, *x, y, if i == 4 { &bold } else { &regular }, if i == 4 { &accent } else { &ink });
        }
        y -= 6.0;
    }
    let _ = Status::Correct;
    doc.save_to_bytes().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn reports_build() {
        let mut p = Project::new();
        p.exam.questions.push(Question::new(1, Kind::TrueFalse { answer: true }));
        let sub = Submission {
            exam_id: p.exam.id.clone(),
            exam_title: "t".into(),
            attempt_id: "a1".into(),
            student_name: "سارة علي".into(),
            student_id: "S-1".into(),
            machine: "PC".into(),
            started_at: 100,
            submitted_at: 700,
            question_order: vec![1],
            answers: Default::default(),
            results: vec![QuestionResult { qid: 1, earned: 1.0, max: 1.0, status: Status::Correct, tests_passed: 0, tests_total: 0 }],
            score: 1.0,
            max_score: 1.0,
            violations: vec![],
            forced: false,
        };
        p.upsert_result(sub.clone());
        for (i, name) in ["Omar Khalid", "Lina Haddad", "يوسف كريم", "Sara Ali"].iter().enumerate() {
            let mut s2 = sub.clone();
            s2.attempt_id = format!("a{}", i + 2);
            s2.student_name = name.to_string();
            s2.submitted_at = 700 + i as u64 * 400;
            s2.results[0].earned = (i % 2) as f32;
            p.upsert_result(s2);
        }
        let x = xlsx(&p).expect("xlsx");
        if let Ok(dir) = std::env::var("HDJLABS_REPORT_OUT") {
            std::fs::write(format!("{dir}/sample.xlsx"), &x).unwrap();
            std::fs::write(format!("{dir}/sample.pdf"), pdf(&p).unwrap()).unwrap();
        }
        assert_eq!(&x[..2], b"PK", "xlsx is a zip");
        let d = pdf(&p).expect("pdf");
        assert_eq!(&d[..5], b"%PDF-");
    }
}
