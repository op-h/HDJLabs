//! Live HTML/CSS/JS preview: an Edge WebView2 child window placed over a rectangle of the egui
//! UI. Locked down for exams — no navigation away, no popups, no downloads, no context menu,
//! no devtools or browser shortcuts (Ctrl+P's print dialog is a classic kiosk escape).
//!
//! Usage per frame: `begin()`, then `request(rect, html)` where the preview should be, then
//! `end(window_title)`. If nothing requested a preview this frame, it is hidden.

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use std::num::NonZeroIsize;
use std::time::{Duration, Instant};

const GUARD_SCRIPT: &str = r#"
(() => {
  const stop = e => { e.preventDefault(); e.stopPropagation(); };
  document.addEventListener('contextmenu', stop, true);
  document.addEventListener('dragstart', stop, true);
  document.addEventListener('keydown', e => {
    const k = (e.key || '').toLowerCase();
    if (e.key === 'F12' || e.key === 'F7' || e.key === 'F3' ||
        (e.ctrlKey && ['p','s','o','f','g','h','j','n','u','r','l','t','w'].includes(k)) ||
        (e.ctrlKey && e.shiftKey && ['i','j','c','s'].includes(k))) stop(e);
  }, true);
  // Pages without their own colours look as they do in a normal browser: black on white.
  // :where() has zero specificity, so any CSS the student writes still wins.
  const base = document.createElement('style');
  base.textContent = ':where(html){color-scheme:light;background:#fff;color:#000}';
  document.documentElement.prepend(base);
  window.print = () => {};
  window.open = () => null;
})();
"#;

struct Hwnd(isize);

impl HasWindowHandle for Hwnd {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = Win32WindowHandle::new(NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?);
        // SAFETY: the HWND belongs to our own top-level window, which outlives the preview.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
    }
}

#[derive(Default)]
pub struct WebPreview {
    view: Option<wry::WebView>,
    context: Option<wry::WebContext>,
    failed: Option<String>,
    shown_html: String,
    pending: Option<(egui::Rect, String)>,
    last_change: Option<Instant>,
    visible: bool,
}

impl WebPreview {
    pub fn begin(&mut self) {
        self.pending = None;
    }

    /// Show `html` in `rect` (egui points) this frame. Returns an error to display instead,
    /// if WebView2 isn't available on this PC.
    pub fn request(&mut self, rect: egui::Rect, html: &str) -> Option<String> {
        if self.failed.is_some() {
            return self.failed.clone();
        }
        if self.pending.as_ref().map(|(_, h)| h != html).unwrap_or(true) && html != self.shown_html {
            self.last_change.get_or_insert_with(Instant::now);
        }
        self.pending = Some((rect, html.to_string()));
        None
    }

    pub fn end(&mut self, window_title: &str, ctx: &egui::Context) {
        let Some((rect, html)) = self.pending.take() else {
            if self.visible {
                if let Some(v) = &self.view {
                    let _ = v.set_visible(false);
                }
                self.visible = false;
            }
            return;
        };
        let zoom = ctx.zoom_factor();
        let bounds = wry::Rect {
            position: wry::dpi::LogicalPosition::new((rect.min.x * zoom) as f64, (rect.min.y * zoom) as f64).into(),
            size: wry::dpi::LogicalSize::new((rect.width() * zoom).max(1.0) as f64, (rect.height() * zoom).max(1.0) as f64).into(),
        };
        if self.view.is_none() {
            let Some(win) = crate::lockdown::find_window(window_title) else { return };
            let dir = std::env::temp_dir().join("HDJLabs").join("webview");
            let _ = std::fs::create_dir_all(&dir);
            let context = self.context.get_or_insert_with(|| wry::WebContext::new(Some(dir)));
            use wry::WebViewBuilderExtWindows;
            let built = wry::WebViewBuilder::new_with_web_context(context)
                .with_html(&html)
                .with_bounds(bounds)
                .with_devtools(false)
                // Same default as a browser: unstyled pages render on white, whatever the OS theme.
                .with_background_color((255, 255, 255, 255))
                .with_initialization_script(GUARD_SCRIPT)
                .with_navigation_handler(|url| url.starts_with("data:") || url.starts_with("about:") || url.starts_with("http://localhost") || url.starts_with("https://localhost"))
                .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                .with_download_started_handler(|_, _| false)
                .with_browser_accelerator_keys(false)
                // A dark OS theme would make WebView2 auto-darken the student's page.
                .with_theme(wry::Theme::Light)
                .with_default_context_menus(false)
                .build_as_child(&Hwnd(win.0 .0 as isize));
            match built {
                Ok(v) => {
                    self.view = Some(v);
                    self.shown_html = html;
                    self.last_change = None;
                    self.visible = true;
                }
                Err(e) => {
                    self.failed = Some(format!("Live preview needs Microsoft Edge WebView2, which isn't available on this PC ({e}). Your code is still saved and submitted."));
                }
            }
            return;
        }
        let v = self.view.as_ref().unwrap();
        let _ = v.set_bounds(bounds);
        if !self.visible {
            let _ = v.set_visible(true);
            self.visible = true;
        }
        // Debounce typing so the page doesn't reload on every keystroke.
        if html != self.shown_html && self.last_change.is_some_and(|t| t.elapsed() > Duration::from_millis(450)) {
            let _ = v.load_html(&html);
            self.shown_html = html;
            self.last_change = None;
        } else if html != self.shown_html {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    /// Force a reload of the current page (e.g. a "Reload" button).
    pub fn reload(&mut self) {
        if let Some(v) = &self.view {
            let _ = v.load_html(&self.shown_html);
        }
    }
}
