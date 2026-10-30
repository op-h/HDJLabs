//! Debug-build visual QA: `HDJLABS_SHOT_OUT=<file>` renders the UI, captures a frame after
//! `HDJLABS_SHOT_FRAMES` frames, writes raw RGBA (`u32 w, u32 h, pixels`) and closes the app.
//! `HDJLABS_SHOT_SEQ=period:offset:count` captures `count` frames instead (`<out>.N`), used to
//! record the teacher guide animations.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES: AtomicU64 = AtomicU64::new(0);
static CAPTURED: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    std::env::var_os("HDJLABS_SHOT_OUT").is_some()
}

pub fn frame() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}

fn sequence() -> Option<(u64, u64, u64)> {
    let v = std::env::var("HDJLABS_SHOT_SEQ").ok()?;
    let mut it = v.split(':').filter_map(|x| x.parse::<u64>().ok());
    Some((it.next()?, it.next()?, it.next()?))
}

pub fn tick(ctx: &egui::Context) {
    let Some(out) = std::env::var_os("HDJLABS_SHOT_OUT") else { return };
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    if n == 0 {
        if let Some(z) = std::env::var("HDJLABS_SHOT_ZOOM").ok().and_then(|v| v.parse::<f32>().ok()) {
            ctx.set_zoom_factor(z);
        }
    }
    ctx.request_repaint();
    let seq = sequence();
    let capture_now = match seq {
        Some((period, offset, count)) => n >= offset && (n - offset) % period == 0 && (n - offset) / period < count,
        None => n == std::env::var("HDJLABS_SHOT_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40),
    };
    if capture_now {
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
        match seq {
            Some((_, _, count)) => {
                let k = CAPTURED.fetch_add(1, Ordering::Relaxed);
                let mut path = out.clone();
                path.push(format!(".{k:02}"));
                let _ = std::fs::write(path, data);
                if k + 1 >= count {
                    std::process::exit(0);
                }
            }
            None => {
                let _ = std::fs::write(out, data);
                std::process::exit(0);
            }
        }
    }
}
