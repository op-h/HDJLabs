pub mod editor;
pub mod player;
pub mod studio;
pub mod theme;
#[cfg(debug_assertions)]
pub mod shot;

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
