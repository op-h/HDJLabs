//! Debug-build visual QA: `HDJLABS_SHOT_OUT=<file>` renders the UI, captures a frame after
//! `HDJLABS_SHOT_FRAMES` frames, writes raw RGBA (`u32 w, u32 h, pixels`) and closes the app.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    std::env::var_os("HDJLABS_SHOT_OUT").is_some()
}

pub fn tick(ctx: &egui::Context) {
    let Some(out) = std::env::var_os("HDJLABS_SHOT_OUT") else { return };
    let wait: u64 = std::env::var("HDJLABS_SHOT_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    ctx.request_repaint();
    if n == wait {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    let shot = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = shot {
        let mut data = Vec::with_capacity(8 + img.pixels.len() * 4);
        data.extend_from_slice(&(img.size[0] as u32).to_le_bytes());
        data.extend_from_slice(&(img.size[1] as u32).to_le_bytes());
        for p in &img.pixels {
            data.extend_from_slice(&p.to_array());
        }
        let _ = std::fs::write(out, data);
        std::process::exit(0);
    }
}
