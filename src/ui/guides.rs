//! "How to write a good question" guides for each code language: a short screen recording
//! (GIF shipped next to the Studio by the installer) plus a written checklist that always works.

use crate::model::Lang;
use crate::ui::theme::{self, pal};
use egui::RichText;

fn guide_file(lang: Lang) -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("guides");
    let p = dir.join(format!("{}.gif", lang.ext()));
    p.is_file().then_some(p)
}

pub fn online_url(lang: Lang) -> String {
    format!("https://github.com/op-h/HDJLabs/blob/main/docs/guides/{}.gif", lang.ext())
}

fn checklist(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Sql => &[
            "Create the tables and rows in the Database box; students see it next to the question.",
            "Ask for one result: say which columns, and in what order (ORDER BY), so the output is predictable.",
            "Write your own query under Your solution and press Run — the expected output fills itself in.",
            "For extra safety switch to Several test cases and add hidden tests with different rows.",
        ],
        Lang::Web => &[
            "Describe what the page must contain: headings, elements, colours, what a button does.",
            "Give students a starter page so they only fill in the important parts.",
            "Students see their page live while they type.",
            "Grade it in Results: open each student's page and read their code.",
        ],
        _ => &[
            "State exactly what to print — including spacing and capitalisation — and show an example.",
            "If the program reads input, say what each line contains.",
            "Write your own solution under Your solution and press Run: the expected output fills itself in.",
            "Prefer Several test cases with one or two hidden tests, so answers can't be hard-coded.",
            "Press Preview to try the question as a student before exporting.",
        ],
    }
}

/// Collapsible guide shown in the code-question editor.
pub fn show(ui: &mut egui::Ui, lang: Lang) {
    let p = pal();
    egui::CollapsingHeader::new(RichText::new(format!("Watch how to write a good {} question", lang.name())).color(p.accent).font(theme::semibold(13.5)))
        .id_salt(("guide", lang.ext()))
        .default_open(cfg!(debug_assertions) && std::env::var_os("HDJLABS_SHOT_GUIDE_OPEN").is_some())
        .show(ui, |ui| {
            match guide_file(lang) {
                Some(path) => {
                    let uri = format!("file://{}", path.display());
                    ui.add(egui::Image::new(uri).max_width(ui.available_width().min(760.0)).corner_radius(theme::R_PANEL));
                }
                None => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(theme::muted("The video guide is installed with HDJLabs Studio. Online copy:").size(12.5));
                        ui.hyperlink_to(RichText::new(format!("{} guide", lang.name())).size(12.5), online_url(lang));
                    });
                }
            }
            ui.add_space(4.0);
            for (i, line) in checklist(lang).iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(theme::figure(format!("{:02}", i + 1), 12.0, p.accent));
                    ui.label(RichText::new(*line).size(13.0));
                });
            }
        });
}
