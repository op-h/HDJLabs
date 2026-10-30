pub mod editor;
pub mod guides;
pub mod player;
pub mod rtl;
pub mod studio;
pub mod text_field;
pub mod theme;
pub mod web;
#[cfg(debug_assertions)]
pub mod shot;

/// Implemented by the Studio and the exam: one frame of UI, plus cleanup on exit.
pub trait Root {
    fn root(&mut self, ui: &mut egui::Ui);
    fn exit(&mut self) {}
}

struct GlApp<T>(T);

impl<T: Root> eframe::App for GlApp<T> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.root(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.0.exit();
    }
    /// Debug captures: `HDJLABS_SHOT_POINTER=x,y` parks the pointer there to show hover states.
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some((x, y)) = std::env::var("HDJLABS_SHOT_POINTER").ok().and_then(|v| v.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))) {
            raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
        }
        shot::scripted_input(raw);
    }
}

struct CpuApp<T>(T);

impl<T: Root> egui_software_backend::App for CpuApp<T> {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut egui_software_backend::SoftwareBackend) {
        self.0.root(ui);
    }

    fn on_exit(&mut self, _ctx: &egui::Context) {
        self.0.exit();
    }
}

fn setup(ctx: &egui::Context) {
    theme::install_fonts(ctx);
    #[cfg(debug_assertions)]
    if std::env::var_os("HDJLABS_SHOT_LIGHT").is_some() {
        theme::set_light(ctx, true);
    }
    theme::apply_style(ctx);
    egui_extras::install_image_loaders(ctx);
}

fn renderer_marker() -> std::path::PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HDJLabs").join("renderer.txt")
}

/// CPU rendering: forced by `HDJLABS_RENDERER=software`, or remembered after OpenGL failed here.
pub(crate) fn software_requested() -> bool {
    match std::env::var("HDJLABS_RENDERER").as_deref() {
        Ok("software") => true,
        Ok("opengl") => false,
        _ => std::fs::read_to_string(renderer_marker()).is_ok_and(|s| s.trim() == "software"),
    }
}

/// Opens the window and runs `make`'s app until it closes. Uses OpenGL when the PC supports it
/// and otherwise a CPU renderer, so HDJLabs also works on PCs with no graphics driver
/// ("Microsoft Basic Display Adapter"). Returns the process exit code.
pub fn launch<T: Root + 'static>(name: &str, viewport: egui::ViewportBuilder, mut make: impl FnMut(&egui::Context) -> T + 'static) -> i32 {
    #[cfg(debug_assertions)]
    let viewport = match std::env::var("HDJLABS_SHOT_SIZE").ok().and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))) {
        Some((w, h)) => viewport.with_inner_size([w, h]).with_min_inner_size([200.0, 200.0]),
        None => viewport,
    };
    if software_requested() {
        let cfg = egui_software_backend::SoftwareBackendAppConfiguration::new().viewport_builder(viewport);
        return match egui_software_backend::run_app_with_software_backend(cfg, move |ctx| {
            setup(&ctx);
            CpuApp(make(&ctx))
        }) {
            Ok(()) => 0,
            Err(_) => crate::EXIT_UI_FAILED,
        };
    }
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = started.clone();
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        name,
        options,
        Box::new(move |cc| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            setup(&cc.egui_ctx);
            Ok(Box::new(GlApp(make(&cc.egui_ctx))))
        }),
    );
    match result {
        Ok(()) => 0,
        // OpenGL is unusable on this PC. winit allows one event loop per process, so start a
        // fresh copy of ourselves with the CPU renderer (it inherits the exam desktop too).
        Err(_) if !started.load(std::sync::atomic::Ordering::SeqCst) => {
            let marker = renderer_marker();
            let _ = std::fs::create_dir_all(marker.parent().unwrap());
            let _ = std::fs::write(&marker, "software");
            let exe = std::env::current_exe().unwrap_or_default();
            std::process::Command::new(exe)
                .args(std::env::args().skip(1))
                .env("HDJLABS_RENDERER", "software")
                .status()
                .ok()
                .and_then(|s| s.code())
                .unwrap_or(crate::EXIT_UI_FAILED)
        }
        Err(_) => crate::EXIT_UI_FAILED,
    }
}

/// Clamps a window's default and minimum size to the screen, so small displays (800×600 lab
/// PCs) never get a window bigger than the screen.
pub fn fit_to_screen(size: [f32; 2], min: [f32; 2]) -> ([f32; 2], [f32; 2]) {
    #[cfg(windows)]
    let (sw, sh) = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXMAXIMIZED, SM_CYMAXIMIZED};
        (GetSystemMetrics(SM_CXMAXIMIZED) as f32, GetSystemMetrics(SM_CYMAXIMIZED) as f32)
    };
    #[cfg(not(windows))]
    let (sw, sh) = (size[0], size[1]);
    if sw < 200.0 || sh < 200.0 {
        return (size, min);
    }
    let fit = |v: [f32; 2], f: f32| [v[0].min(sw * f), v[1].min(sh * f)];
    (fit(size, 0.95), fit(min, 0.9))
}

/// Window icon, drawn in code so the binary needs no image decoder:
/// a graphite tile with a lime "H" and cursor bar.
pub fn app_icon() -> egui::IconData {
    let n = 64u32;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = 10.0f32;
            let dx = (r - fx).max(fx - (n as f32 - r)).max(0.0);
            let dy = (r - fy).max(fy - (n as f32 - r)).max(0.0);
            let alpha = (r - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            let edge = fx < 3.0 || fy < 3.0 || fx > 61.0 || fy > 61.0;
            let lime = ((17.0..24.0).contains(&fx) || (40.0..47.0).contains(&fx)) && (12.0..42.0).contains(&fy)
                || (17.0..47.0).contains(&fx) && (24.0..30.0).contains(&fy)
                || (22.0..42.0).contains(&fx) && (48.0..53.0).contains(&fy);
            let c: [u8; 3] = if lime { [168, 227, 58] } else if edge { [56, 67, 62] } else { [17, 22, 21] };
            rgba.extend_from_slice(&[c[0], c[1], c[2], (alpha * 255.0) as u8]);
        }
    }
    egui::IconData { rgba, width: n, height: n }
}

/// True if Windows Firewall already has an inbound allow rule for this executable.
pub fn firewall_allows_self() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().unwrap_or_default().to_string_lossy().to_lowercase();
        let out = std::process::Command::new("netsh")
            .args(["advfirewall", "firewall", "show", "rule", "name=all", "dir=in", "verbose"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();
        let Ok(out) = out else { return false };
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        // Rules are blank-line separated blocks; look for one that allows this program inbound.
        text.split("\r\n\r\n").chain(text.split("\n\n")).any(|block| block.contains(&exe) && block.contains("allow") && block.contains("yes"))
    }
    #[cfg(not(windows))]
    true
}

/// Adds an inbound Windows Firewall rule for this executable (prompts for elevation).
pub fn allow_firewall() {
    #[cfg(windows)]
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        let exe = std::env::current_exe().unwrap_or_default();
        let args = format!(
            "advfirewall firewall add rule name=\"HDJLabs Studio\" dir=in action=allow program=\"{}\" enable=yes profile=any",
            exe.display()
        );
        ShellExecuteW(None, &HSTRING::from("runas"), &HSTRING::from("netsh"), &HSTRING::from(args), None, SW_HIDE);
    }
}
