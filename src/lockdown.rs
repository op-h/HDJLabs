//! Exam lockdown for Windows.
//!
//! Layers (strongest first):
//! 1. A private Win32 desktop. The exam runs on its own desktop with no Explorer, taskbar,
//!    Start menu, notifications, other windows, or shell gestures — the same idea as
//!    Safe Exam Browser's "Create New Desktop" kiosk mode.
//! 2. A low-level keyboard hook: Win key, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4, Ctrl+Shift+Esc,
//!    Print Screen, app-launch keys, and any *injected* (synthetic) keystrokes are swallowed.
//! 3. Per-user policies for the Ctrl+Alt+Del screen: no Task Manager, lock, sign-out, password
//!    change or power menu. Three/four-finger touchpad gestures are switched off. All values are
//!    backed up first and restored on exit — or on the next launch if the PC lost power.
//! 4. The exam window is excluded from screenshots / screen recording / screen sharing,
//!    opts out of touch edge swipes, and stays topmost and fullscreen.
//! 5. Accessibility hotkeys (Sticky/Filter/Toggle Keys dialogs — a classic kiosk escape) are
//!    disabled for the duration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

pub const DESKTOP_PREFIX: &str = "HDJLabsExam";
const RUNNING_MUTEX: &str = "Local\\HDJLabs.ExamRunning";

static BLOCKED: AtomicU32 = AtomicU32::new(0);

/// Shortcuts swallowed by the keyboard hook since the last call.
pub fn take_blocked() -> u32 {
    BLOCKED.swap(0, Ordering::Relaxed)
}

fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let d = base.join("HDJLabs");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn backup_path() -> PathBuf {
    data_dir().join("lockdown-restore.json")
}

#[derive(Serialize, Deserialize, Default)]
struct Backup {
    values: Vec<(String, String, Option<u32>)>,
    access_flags: Option<[u32; 3]>,
}

const SYS: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\System";
const EXP: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
const PTP: &str = r"Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad";

const POLICIES: &[(&str, &str, u32)] = &[
    (SYS, "DisableTaskMgr", 1),
    (SYS, "DisableLockWorkstation", 1),
    (SYS, "DisableChangePassword", 1),
    (EXP, "NoLogoff", 1),
    (EXP, "NoClose", 1),
    (PTP, "ThreeFingerSlideEnabled", 0),
    (PTP, "FourFingerSlideEnabled", 0),
    (PTP, "ThreeFingerTapEnabled", 0),
    (PTP, "FourFingerTapEnabled", 0),
    (PTP, "EnableEdgy", 0),
];

/// Applies the per-user policies; restores them when dropped.
pub struct SystemGuard {
    owned: bool,
}

impl SystemGuard {
    pub fn apply() -> SystemGuard {
        // If a backup already exists, another guard (the launcher) owns the original values.
        if backup_path().exists() {
            return SystemGuard { owned: false };
        }
        let mut b = Backup::default();
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        for (path, name, _) in POLICIES {
            let prev = hkcu.open_subkey(path).ok().and_then(|k| k.get_value::<u32, _>(name).ok());
            b.values.push((path.to_string(), name.to_string(), prev));
        }
        b.access_flags = win::access_hotkeys_get();
        if std::fs::write(backup_path(), serde_json::to_vec(&b).unwrap_or_default()).is_err() {
            return SystemGuard { owned: false }; // never change settings we could not back up
        }
        for (path, name, value) in POLICIES {
            if let Ok((k, _)) = hkcu.create_subkey(path) {
                let _ = k.set_value(name, value);
            }
        }
        win::access_hotkeys_disable();
        win::clear_clipboard();
        SystemGuard { owned: true }
    }
}

impl Drop for SystemGuard {
    fn drop(&mut self) {
        if self.owned {
            restore();
        }
    }
}

fn restore() {
    let Ok(data) = std::fs::read(backup_path()) else { return };
    let Ok(b) = serde_json::from_slice::<Backup>(&data) else {
        let _ = std::fs::remove_file(backup_path());
        return;
    };
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    for (path, name, prev) in &b.values {
        if let Ok((k, _)) = hkcu.create_subkey(path) {
            let _ = match prev {
                Some(v) => k.set_value(name, v),
                None => k.delete_value(name),
            };
        }
    }
    if let Some(f) = b.access_flags {
        win::access_hotkeys_set(f);
    }
    win::clear_clipboard();
    let _ = std::fs::remove_file(backup_path());
}

/// Called at every start: undo a lockdown that was interrupted (power loss, crash).
pub fn recover_stale() {
    if backup_path().exists() && !win::mutex_exists(RUNNING_MUTEX) {
        restore();
    }
}

pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::StationsAndDesktops::*;
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::Accessibility::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    pub struct RunningMutex(HANDLE);
    unsafe impl Send for RunningMutex {}
    impl Drop for RunningMutex {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn hold_running_mutex() -> Option<RunningMutex> {
        unsafe { CreateMutexW(None, false, &HSTRING::from(RUNNING_MUTEX)).ok().map(RunningMutex) }
    }

    pub fn mutex_exists(name: &str) -> bool {
        unsafe {
            match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) {
                Ok(h) => {
                    let _ = CloseHandle(h);
                    true
                }
                Err(_) => false,
            }
        }
    }

    pub fn clear_clipboard() {
        unsafe {
            if OpenClipboard(None).is_ok() {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
    }

    pub fn access_hotkeys_get() -> Option<[u32; 3]> {
        unsafe {
            let mut sk = STICKYKEYS { cbSize: std::mem::size_of::<STICKYKEYS>() as u32, ..Default::default() };
            let mut fk = FILTERKEYS { cbSize: std::mem::size_of::<FILTERKEYS>() as u32, ..Default::default() };
            let mut tk = TOGGLEKEYS { cbSize: std::mem::size_of::<TOGGLEKEYS>() as u32, ..Default::default() };
            SystemParametersInfoW(SPI_GETSTICKYKEYS, sk.cbSize, Some(&mut sk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).ok()?;
            SystemParametersInfoW(SPI_GETFILTERKEYS, fk.cbSize, Some(&mut fk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).ok()?;
            SystemParametersInfoW(SPI_GETTOGGLEKEYS, tk.cbSize, Some(&mut tk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).ok()?;
            Some([sk.dwFlags.0, fk.dwFlags, tk.dwFlags])
        }
    }

    pub fn access_hotkeys_set(f: [u32; 3]) {
        unsafe {
            let mut sk = STICKYKEYS { cbSize: std::mem::size_of::<STICKYKEYS>() as u32, dwFlags: STICKYKEYS_FLAGS(f[0]) };
            let mut fk = FILTERKEYS { cbSize: std::mem::size_of::<FILTERKEYS>() as u32, ..Default::default() };
            let mut tk = TOGGLEKEYS { cbSize: std::mem::size_of::<TOGGLEKEYS>() as u32, dwFlags: f[2] };
            // FILTERKEYS carries timing fields — read them first so only the flags change.
            let _ = SystemParametersInfoW(SPI_GETFILTERKEYS, fk.cbSize, Some(&mut fk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            fk.dwFlags = f[1];
            let _ = SystemParametersInfoW(SPI_SETSTICKYKEYS, sk.cbSize, Some(&mut sk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            let _ = SystemParametersInfoW(SPI_SETFILTERKEYS, fk.cbSize, Some(&mut fk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            let _ = SystemParametersInfoW(SPI_SETTOGGLEKEYS, tk.cbSize, Some(&mut tk as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
    }

    pub fn access_hotkeys_disable() {
        if let Some([s, f, t]) = access_hotkeys_get() {
            access_hotkeys_set([s & !SKF_HOTKEYACTIVE.0, f & !FKF_HOTKEYACTIVE, t & !TKF_HOTKEYACTIVE]);
        }
    }

    pub fn is_remote_session() -> bool {
        unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
    }

    pub fn is_virtual_machine() -> bool {
        let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
        let Ok(bios) = hklm.open_subkey(r"HARDWARE\DESCRIPTION\System\BIOS") else { return false };
        let read = |n: &str| bios.get_value::<String, _>(n).unwrap_or_default().to_ascii_lowercase();
        let s = format!("{} {} {}", read("SystemManufacturer"), read("SystemProductName"), read("BIOSVendor"));
        ["vmware", "virtualbox", "qemu", "kvm", "xen", "parallels", "virtual machine", "bochs", "innotek"].iter().any(|m| s.contains(m))
    }

    /// Screen-sharing / remote-control software that would let someone else see or drive the exam.
    pub fn forbidden_processes() -> Vec<String> {
        const BAD: &[&str] = &[
            "teamviewer.exe", "anydesk.exe", "rustdesk.exe", "remoting_host.exe", "winvnc.exe", "tvnserver.exe",
            "vncserver.exe", "uvnc_service.exe", "parsecd.exe", "ultraviewer_desktop.exe", "supremo.exe",
            "obs64.exe", "obs32.exe", "screenconnect.windowsclient.exe", "ammyy_admin.exe", "zoom.exe",
            "discord.exe", "ms-teams.exe", "msteams.exe", "skype.exe", "sharex.exe", "bdcam.exe",
        ];
        let mut found = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return found };
            let mut pe = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut pe).is_ok() {
                loop {
                    let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
                    let name = String::from_utf16_lossy(&pe.szExeFile[..len]);
                    if BAD.contains(&name.to_ascii_lowercase().as_str()) && !found.contains(&name) {
                        found.push(name);
                    }
                    if Process32NextW(snap, &mut pe).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
        found
    }

    /// Native dialog (used by the launcher before any UI exists). Returns true for OK/Retry.
    pub fn message_box(title: &str, text: &str, retry: bool) -> bool {
        unsafe {
            let style = if retry { MB_RETRYCANCEL | MB_ICONWARNING } else { MB_OK | MB_ICONINFORMATION };
            let r = MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), style | MB_TOPMOST | MB_SETFOREGROUND);
            r == IDOK || r == IDRETRY
        }
    }

    pub fn current_desktop_name() -> String {
        unsafe {
            let Ok(d) = GetThreadDesktop(GetCurrentThreadId()) else { return String::new() };
            let mut buf = [0u16; 256];
            let mut needed = 0u32;
            if GetUserObjectInformationW(HANDLE(d.0), UOI_NAME, Some(buf.as_mut_ptr() as *mut _), (buf.len() * 2) as u32, Some(&mut needed)).is_err() {
                return String::new();
            }
            let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
            String::from_utf16_lossy(&buf[..len])
        }
    }

    /// Creates a private desktop, runs `exe args` on it, switches the screen to it and waits.
    /// The original desktop is always restored afterwards. Returns the child's exit code.
    pub fn run_on_private_desktop(args: &str) -> Result<u32, String> {
        unsafe {
            let original = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP).map_err(|e| format!("OpenInputDesktop: {e}"))?;
            let name = format!("{DESKTOP_PREFIX}-{}", crate::crypto::random_hex(4));
            let hname = HSTRING::from(name.as_str());
            let desk = CreateDesktopW(&hname, PCWSTR::null(), None, DESKTOP_CONTROL_FLAGS(0), GENERIC_ALL.0, None).map_err(|e| format!("CreateDesktop: {e}"))?;

            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let mut cmdline: Vec<u16> = format!("\"{}\" {args}", exe.display()).encode_utf16().chain([0]).collect();
            let mut desktop_w: Vec<u16> = format!("WinSta0\\{name}").encode_utf16().chain([0]).collect();
            let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, lpDesktop: PWSTR(desktop_w.as_mut_ptr()), ..Default::default() };
            let mut pi = PROCESS_INFORMATION::default();
            let spawned = CreateProcessW(PCWSTR::null(), PWSTR(cmdline.as_mut_ptr()), None, None, false, PROCESS_CREATION_FLAGS(0), None, PCWSTR::null(), &si, &mut pi);
            if let Err(e) = spawned {
                let _ = CloseDesktop(desk);
                let _ = CloseDesktop(original);
                return Err(format!("CreateProcess: {e}"));
            }
            let _ = SwitchDesktop(desk);
            // Debug builds: never leave a developer stuck on a blank desktop.
            let limit = if cfg!(debug_assertions) {
                std::env::var("HDJLABS_SELFTEST_EXIT").ok().and_then(|v| v.parse::<u32>().ok()).map(|s| (s + 20) * 1000).unwrap_or(INFINITE)
            } else {
                INFINITE
            };
            if WaitForSingleObject(pi.hProcess, limit) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(pi.hProcess, 99);
                let _ = WaitForSingleObject(pi.hProcess, 5000);
            }
            let mut code = 1u32;
            let _ = GetExitCodeProcess(pi.hProcess, &mut code);
            let _ = SwitchDesktop(original);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseDesktop(desk);
            let _ = CloseDesktop(original);
            Ok(code)
        }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if should_block(k.vkCode, k.flags) {
                if wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN {
                    BLOCKED.fetch_add(1, Ordering::Relaxed);
                }
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    fn should_block(vk: u32, flags: KBDLLHOOKSTRUCT_FLAGS) -> bool {
        if flags.0 & LLKHF_INJECTED.0 != 0 {
            return true; // macro tools, auto-typers, remote input
        }
        let alt = flags.0 & LLKHF_ALTDOWN.0 != 0;
        let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
        let vk16 = vk as u16;
        match VIRTUAL_KEY(vk16) {
            VK_LWIN | VK_RWIN | VK_APPS | VK_SNAPSHOT | VK_SLEEP => true,
            VK_TAB | VK_F4 | VK_SPACE if alt => true,
            VK_ESCAPE if alt || ctrl => true,
            // Browser keys and app-launch keys (volume/media keys stay usable).
            _ if (0xA6..=0xAC).contains(&vk16) || (0xB4..=0xB7).contains(&vk16) => true,
            _ => false,
        }
    }

    /// Installs the low-level keyboard hook on a dedicated thread with its own message loop.
    pub fn start_keyboard_hook() {
        std::thread::spawn(|| unsafe {
            let module = GetModuleHandleW(None).unwrap_or_default();
            let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), HINSTANCE(module.0), 0) else { return };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        });
    }

    #[derive(Clone, Copy)]
    pub struct Window(pub HWND);
    unsafe impl Send for Window {}

    pub fn find_window(title: &str) -> Option<Window> {
        unsafe { FindWindowW(PCWSTR::null(), &HSTRING::from(title)).ok().filter(|h| !h.0.is_null()).map(Window) }
    }

    /// Screenshot/recording exclusion, no touch edge swipes, topmost over the whole primary screen.
    pub fn harden_window(w: Window) {
        unsafe {
            let _ = SetWindowDisplayAffinity(w.0, WDA_EXCLUDEFROMCAPTURE);
            disable_edge_gestures(w.0);
            let (cx, cy) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let _ = SetWindowPos(w.0, HWND_TOPMOST, 0, 0, cx, cy, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(w.0);
        }
    }

    fn disable_edge_gestures(hwnd: HWND) {
        use windows::core::PROPVARIANT;
        use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow, PROPERTYKEY};
        // System.EdgeGesture.DisableTouchWhenFullscreen
        let key = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x32CE38B2_2C9A_41B1_9BC5_B3784394AA44), pid: 2 };
        unsafe {
            if let Ok(store) = SHGetPropertyStoreForWindow::<_, IPropertyStore>(hwnd) {
                let _ = store.SetValue(&key, &PROPVARIANT::from(true));
            }
        }
    }

    /// Returns true if focus had escaped and was pulled back.
    pub fn enforce_foreground(w: Window) -> bool {
        unsafe {
            let fg = GetForegroundWindow();
            if fg != w.0 {
                let _ = SetWindowPos(w.0, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                let _ = SetForegroundWindow(w.0);
                return !fg.0.is_null();
            }
            false
        }
    }

    /// Keeps the pointer on the exam window (fallback mode, where other monitors show the real desktop).
    pub fn clip_cursor(w: Window) {
        unsafe {
            let mut r = RECT::default();
            if GetWindowRect(w.0, &mut r).is_ok() {
                let _ = ClipCursor(Some(&r));
            }
        }
    }

    pub fn release_cursor() {
        unsafe {
            let _ = ClipCursor(None);
        }
    }
}
