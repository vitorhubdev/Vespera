//! Windows taskbar badge for unread messages.
//!
//! Policy: counts unread *messages* (sum of `chat.unread`) for chats that
//! are neither archived nor muted, following the same policy as desktop
//! notifications (cleared when unread is zero, archived, or muted). Linux
//! and macOS notifications are untouched; this module is a no-op there.
//!
//! Updates happen on chat events and local read clears, never by polling.
//! Zero clears the overlay; window recreation restores it via `restore()`.
//! Explorer restarts are covered by a `TaskbarButtonCreated` watcher that
//! re-applies the desired count (Windows only).
//!
//! Integration follows Microsoft Learn `ITaskbarList3::SetOverlayIcon`:
//! overlay on the taskbar button, `NULL` icon clears it, and the caller
//! frees the icon after the call. The success cache only records real OS
//! results, never the intent.
//!
//! Visual confirmation on a real desktop remains pending; see the batch
//! report for what was executed versus what still needs eyes.

use crate::model::Chat;

/// Total unread messages for badge display, excluding archived and muted
/// chats at `now` (Unix seconds, same clock as `Chat::muted`).
pub fn count(chats: &[Chat], now: i64) -> u64 {
    chats
        .iter()
        .filter(|chat| !chat.archived && !chat.muted(now))
        .map(|chat| u64::from(chat.unread))
        .sum()
}

/// Text drawn on the overlay icon. `None` clears. Counts above 99 collapse
/// to `99+`: a 16 px overlay cannot carry more digits legibly.
pub fn overlay_label(count: u64) -> Option<String> {
    if count == 0 {
        None
    } else if count > 99 {
        Some("99+".to_owned())
    } else {
        Some(count.to_string())
    }
}

/// Overlay bitmap size in pixels. 32 px scales down cleanly to the 16 px
/// overlay slot on 100% and 200% displays.
pub const BADGE_PX: usize = 32;

/// Renders a 32x32 BGRA overlay bitmap (red disc, white glyphs) for `label`.
/// Pure Rust so tests can verify pixels without a desktop or a device.
/// Background pixels are fully transparent (alpha 0).
pub fn render_badge_rgba(label: &str) -> Vec<u8> {
    const CENTER: f32 = 16.0;
    const RADIUS: f32 = 15.0;
    let mut rgba = vec![0u8; BADGE_PX * BADGE_PX * 4];
    for y in 0..BADGE_PX {
        for x in 0..BADGE_PX {
            let dx = x as f32 + 0.5 - CENTER;
            let dy = y as f32 + 0.5 - CENTER;
            if dx * dx + dy * dy <= RADIUS * RADIUS {
                let at = (y * BADGE_PX + x) * 4;
                // Windows taskbar red, opaque.
                rgba[at] = 0x23;
                rgba[at + 1] = 0x11;
                rgba[at + 2] = 0xE8;
                rgba[at + 3] = 0xFF;
            }
        }
    }
    // 3x5 glyphs, scaled to fit the disc.
    let scale = match label.len() {
        0 => return rgba,
        1 => 4,
        2 => 3,
        _ => 2,
    };
    let glyph_w = 3 * scale;
    let glyph_h = 5 * scale;
    let gap = scale;
    let total_w = label.len() * glyph_w + label.len().saturating_sub(1) * gap;
    let mut cursor = (BADGE_PX.saturating_sub(total_w)) / 2;
    let top = (BADGE_PX.saturating_sub(glyph_h)) / 2;
    for ch in label.chars() {
        blit_glyph(&mut rgba, ch, cursor, top, scale);
        cursor += glyph_w + gap;
    }
    rgba
}

/// 3x5 bitmap font for the overlay digits and `+`.
fn glyph_rows(ch: char) -> [u8; 5] {
    match ch {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        _ => [0b000, 0b000, 0b000, 0b000, 0b000],
    }
}

/// Paints one scaled white glyph into the bitmap.
fn blit_glyph(rgba: &mut [u8], ch: char, left: usize, top: usize, scale: usize) {
    let rows = glyph_rows(ch);
    for (gy, row) in rows.iter().enumerate() {
        for gx in 0..3 {
            if row & (1 << (2 - gx)) == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = left + gx * scale + dx;
                    let y = top + gy * scale + dy;
                    if x < BADGE_PX && y < BADGE_PX {
                        let at = (y * BADGE_PX + x) * 4;
                        rgba[at] = 0xFF;
                        rgba[at + 1] = 0xFF;
                        rgba[at + 2] = 0xFF;
                        rgba[at + 3] = 0xFF;
                    }
                }
            }
        }
    }
}

/// Desired count (what the UI asked for) versus applied count (what the OS
/// confirmed). `u64::MAX` means "no request yet". Success is only recorded
/// after the real OS result, never before.
static DESIRED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
static APPLIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// Taskbar backend behind an adapter so tests can verify calls, failures,
/// and re-application without a desktop. Counts stay unit-tested apart
/// from integration.
pub trait TaskbarBackend: Send + Sync {
    /// Correct taskbar window handle, or `None` when no window exists yet.
    fn resolve_hwnd(&self) -> Option<isize>;
    /// Sets (`Some` bitmap) or clears (`None`) the overlay. Returns whether
    /// the OS accepted the call.
    fn set_overlay(&self, hwnd: isize, rgba: Option<&[u8]>, label: &str) -> bool;
}

/// Policy driver shared by production and tests: desired versus applied,
/// zero clears, failures never poison the cache.
pub struct Applier<B: TaskbarBackend> {
    backend: B,
    desired: Option<u64>,
    applied: Option<u64>,
}

impl<B: TaskbarBackend> Applier<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            desired: None,
            applied: None,
        }
    }

    /// Applies `count`. Returns the real backend result.
    pub fn apply(&mut self, count: u64) -> bool {
        self.desired = Some(count);
        let ok = self.push(count);
        if ok {
            self.applied = Some(count);
        }
        ok
    }

    /// Re-applies the last desired count (window recreation, Explorer
    /// restart). Returns `false` when nothing was ever desired.
    pub fn restore(&mut self) -> bool {
        let Some(count) = self.desired else {
            return false;
        };
        let ok = self.push(count);
        if ok {
            self.applied = Some(count);
        }
        ok
    }

    fn push(&self, count: u64) -> bool {
        let Some(hwnd) = self.backend.resolve_hwnd() else {
            return false;
        };
        match overlay_label(count) {
            None => self.backend.set_overlay(hwnd, None, ""),
            Some(label) => {
                let rgba = render_badge_rgba(&label);
                self.backend.set_overlay(hwnd, Some(&rgba), &label)
            }
        }
    }

    #[cfg(test)]
    fn applied(&self) -> Option<u64> {
        self.applied
    }
}

/// Production backend: the real taskbar button via `ITaskbarList3`.
#[cfg(target_os = "windows")]
struct RealBackend;

#[cfg(target_os = "windows")]
impl TaskbarBackend for RealBackend {
    fn resolve_hwnd(&self) -> Option<isize> {
        win::app_hwnd()
    }

    fn set_overlay(&self, hwnd: isize, rgba: Option<&[u8]>, label: &str) -> bool {
        win::set_overlay_icon(hwnd, rgba, label)
    }
}

/// Applies the badge for `count`. Zero clears the overlay. Records success
/// only from the real OS result.
pub fn apply(count: u64) {
    DESIRED.store(count, std::sync::atomic::Ordering::SeqCst);
    if apply_now(count) {
        APPLIED.store(count, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Re-applies the last desired badge (e.g. after window recreation).
pub fn restore() {
    let count = DESIRED.load(std::sync::atomic::Ordering::SeqCst);
    if count != u64::MAX && apply_now(count) {
        APPLIED.store(count, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Resets the caches. Tests only.
#[cfg(test)]
fn reset_for_tests() {
    DESIRED.store(u64::MAX, std::sync::atomic::Ordering::SeqCst);
    APPLIED.store(u64::MAX, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(not(target_os = "windows"))]
fn apply_now(_count: u64) -> bool {
    true
}

#[cfg(target_os = "windows")]
fn apply_now(count: u64) -> bool {
    // Headless `cargo test` has no taskbar button. The Explorer watcher
    // opens a window on a background thread; badge unit tests use a mock
    // backend instead. Set VESPERA_BADGE_WATCHER=1 to force the real watcher.
    if cfg!(test) && std::env::var_os("VESPERA_BADGE_WATCHER").is_none() {
        return false;
    }
    win::ensure_watcher();
    let applier_backend = RealBackend;
    let Some(hwnd) = applier_backend.resolve_hwnd() else {
        return false;
    };
    match overlay_label(count) {
        None => applier_backend.set_overlay(hwnd, None, ""),
        Some(label) => {
            let rgba = render_badge_rgba(&label);
            applier_backend.set_overlay(hwnd, Some(&rgba), &label)
        }
    }
}

/// Real Windows integration: HWND lookup, numeric `HICON`, `ITaskbarList3`,
/// Explorer-restart watcher, COM/GDI lifetime handling.
#[cfg(target_os = "windows")]
mod win {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyIcon, GetMessageW, HICON,
        ICONINFO, MSG, RegisterClassW, RegisterWindowMessageW, WINDOW_STYLE, WNDCLASSW,
        WS_EX_TOOLWINDOW,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// First window of this process whose title is ours. The badge must sit
    /// on the taskbar button of the real window, not on a guessed handle.
    pub(super) fn app_hwnd() -> Option<isize> {
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
        };
        use windows::core::BOOL;
        struct Found {
            hwnd: isize,
        }
        unsafe extern "system" fn find(hwnd: HWND, param: LPARAM) -> BOOL {
            let found = unsafe { &mut *(param.0 as *mut Found) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != std::process::id() {
                return true.into();
            }
            let mut title = [0u16; 64];
            let length = unsafe { GetWindowTextW(hwnd, &mut title) };
            if length == 0 {
                return true.into();
            }
            let title = String::from_utf16_lossy(&title[..length as usize]);
            if !title.starts_with("Vespera") {
                return true.into();
            }
            found.hwnd = hwnd.0 as isize;
            false.into()
        }
        let mut found = Found { hwnd: 0 };
        let _ = unsafe { EnumWindows(Some(find), LPARAM(&mut found as *mut Found as isize)) };
        if found.hwnd == 0 {
            // No window yet (link runs headless, window recreating): the
            // caller keeps the desired count and `restore()` retries later.
            None
        } else {
            Some(found.hwnd)
        }
    }

    /// Builds a color `HICON` from a 32x32 BGRA bitmap. The 1-bit mask marks
    /// fully transparent pixels so the disc keeps its round shape.
    fn hicon_from_rgba(rgba: &[u8]) -> Option<HICON> {
        if rgba.len() != super::BADGE_PX * super::BADGE_PX * 4 {
            return None;
        }
        unsafe {
            let hdc = CreateCompatibleDC(None);
            if hdc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: super::BADGE_PX as i32,
                    biHeight: -(super::BADGE_PX as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [Default::default(); 1],
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let color =
                CreateDIBSection(Some(hdc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), bits as *mut u8, rgba.len());
            // 1-bit mask, DWORD-aligned rows: 32 px -> 4 bytes per row.
            let mut mask = [0xFFu8; super::BADGE_PX * 4];
            for y in 0..super::BADGE_PX {
                for x in 0..super::BADGE_PX {
                    let alpha = rgba[(y * super::BADGE_PX + x) * 4 + 3];
                    if alpha != 0 {
                        mask[y * 4 + x / 8] &= !(0x80 >> (x % 8));
                    }
                }
            }
            let mask_bitmap = CreateBitmap(
                super::BADGE_PX as i32,
                super::BADGE_PX as i32,
                1,
                1,
                Some(mask.as_ptr() as *const core::ffi::c_void),
            );
            let icon = CreateIconIndirect(&ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask_bitmap,
                hbmColor: color,
            })
            .ok();
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask_bitmap.into());
            let _ = DeleteDC(hdc);
            icon
        }
    }

    /// Sets or clears the taskbar overlay icon. Returns the real OS result.
    /// The icon is freed after the call, per Microsoft Learn: the taskbar
    /// copies what it needs and the caller owns the `HICON` lifetime.
    pub(super) fn set_overlay_icon(hwnd_raw: isize, rgba: Option<&[u8]>, label: &str) -> bool {
        // COM for this call only; S_FALSE means already initialized there.
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        let result = (|| {
            let taskbar: ITaskbarList3 = unsafe {
                CoCreateInstance(
                    &TaskbarList,
                    Option::<&windows::core::IUnknown>::None,
                    CLSCTX_INPROC_SERVER,
                )
                .ok()?
            };
            unsafe { taskbar.HrInit().ok()? };
            let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
            match rgba {
                None => unsafe {
                    taskbar
                        .SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null())
                        .ok()?;
                },
                Some(bytes) => {
                    let icon = hicon_from_rgba(bytes)?;
                    let desc = HSTRING::from(format!("{label} unread"));
                    let status = unsafe { taskbar.SetOverlayIcon(hwnd, icon, &desc) };
                    // Caller-owned lifetime: free even when the call failed.
                    let _ = unsafe { DestroyIcon(icon) };
                    status.ok()?;
                }
            }
            Some(())
        })();
        if initialized {
            unsafe { CoUninitialize() };
        }
        result.is_some()
    }

    /// `TaskbarButtonCreated` is delivered by `SendNotifyMessage` to the
    /// window procedure. It does not come back as the `MSG` from `GetMessageW`.
    static RECREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Window procedure for the watcher window.
    /// `DefWindowProcW` itself is a Rust-ABI wrapper in this projection,
    /// so the class needs its own `system` procedure that forwards to it.
    unsafe extern "system" fn watch_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let recreated = RECREATED.load(std::sync::atomic::Ordering::Relaxed);
        if recreated != 0 && msg == recreated {
            super::restore();
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Watches for Explorer restarts (`TaskbarButtonCreated`) and re-applies
    /// the desired count. Spawned once; the thread lives with the process.
    pub(super) fn ensure_watcher() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("taskbar-recreated".into())
                .spawn(|| {
                    unsafe {
                        // Explorer broadcasts this registered message after a
                        // restart; re-apply the desired overlay then.
                        let name: Vec<u16> = "TaskbarButtonCreated\0".encode_utf16().collect();
                        let recreated = RegisterWindowMessageW(PCWSTR(name.as_ptr()));
                        RECREATED.store(recreated, std::sync::atomic::Ordering::Relaxed);
                        // Explorer re-broadcasts after a restart; restore then.
                        let class_name: Vec<u16> = "VesperaTaskbarWatch"
                            .encode_utf16()
                            .chain(std::iter::once(0))
                            .collect();
                        let class = WNDCLASSW {
                            lpfnWndProc: Some(watch_proc),
                            lpszClassName: PCWSTR(class_name.as_ptr()),
                            ..Default::default()
                        };
                        // Best effort: without a window the next badge change
                        // still restores the overlay through apply()/restore().
                        if RegisterClassW(&class) == 0 {
                            return;
                        }
                        // A message-only window (parent HWND_MESSAGE) does not
                        // receive HWND_BROADCAST, which is how Explorer sends
                        // TaskbarButtonCreated. This is a hidden top-level
                        // tool window: no taskbar button, no Alt+Tab entry,
                        // and it does get the broadcast in `watch_proc`.
                        let Ok(window) = CreateWindowExW(
                            WS_EX_TOOLWINDOW,
                            PCWSTR(class_name.as_ptr()),
                            PCWSTR::null(),
                            WINDOW_STYLE::default(),
                            0,
                            0,
                            0,
                            0,
                            None,
                            None,
                            None,
                            None,
                        ) else {
                            return;
                        };
                        let _ = window;
                        let mut msg = MSG::default();
                        // Sent broadcasts are dispatched inside GetMessageW to
                        // `watch_proc`. This loop only has to keep running.
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
                    }
                });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn chat(id: &str, unread: u32, archived: bool, muted_until: Option<i64>) -> Chat {
        let mut chat = Chat::new(id.into(), id.into());
        chat.unread = unread;
        chat.archived = archived;
        chat.muted_until = muted_until;
        chat
    }

    #[test]
    fn counts_messages_excluding_archived_and_muted() {
        reset_for_tests();
        let now = 1_000_000;
        let chats = vec![
            chat("a@s.whatsapp.net", 3, false, None),
            chat("b@s.whatsapp.net", 2, false, None),
            chat("c@s.whatsapp.net", 5, true, None),
            chat("d@s.whatsapp.net", 7, false, Some(now + 100)),
            chat("e@s.whatsapp.net", 0, false, None),
        ];
        assert_eq!(count(&chats, now), 5);
    }

    #[test]
    fn overlay_label_collapses_high_counts() {
        assert_eq!(overlay_label(0), None);
        assert_eq!(overlay_label(1).as_deref(), Some("1"));
        assert_eq!(overlay_label(99).as_deref(), Some("99"));
        assert_eq!(overlay_label(100).as_deref(), Some("99+"));
        assert_eq!(overlay_label(10_000).as_deref(), Some("99+"));
    }

    #[test]
    fn rendered_bitmap_is_a_red_disc_with_white_glyphs() {
        let rgba = render_badge_rgba("7");
        assert_eq!(rgba.len(), BADGE_PX * BADGE_PX * 4);
        // Corners stay transparent, the center is opaque red.
        assert_eq!(rgba[3], 0);
        let center = (16 * BADGE_PX + 16) * 4;
        assert_eq!(rgba[center + 3], 0xFF);
        // Some pixel is pure white (a glyph), distinct from the red disc.
        let white = (0..BADGE_PX * BADGE_PX)
            .any(|pixel| rgba[pixel * 4..pixel * 4 + 4] == [0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(white, "digits must paint");
        // Empty label keeps the disc but paints no glyphs.
        let plain = render_badge_rgba("");
        assert!(
            !(0..BADGE_PX * BADGE_PX)
                .any(|pixel| plain[pixel * 4..pixel * 4 + 4] == [0xFF, 0xFF, 0xFF, 0xFF]),
            "no label means no glyphs"
        );
    }

    /// Recording backend: verifies calls, injected failures, re-application.
    /// A count test alone never proves the OS received anything.
    type Call = (isize, Option<Vec<u8>>, String);

    #[derive(Default)]
    struct Mock {
        hwnd: Option<isize>,
        fail: bool,
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl TaskbarBackend for Mock {
        fn resolve_hwnd(&self) -> Option<isize> {
            self.hwnd
        }

        fn set_overlay(&self, hwnd: isize, rgba: Option<&[u8]>, label: &str) -> bool {
            self.calls.lock().unwrap().push((
                hwnd,
                rgba.map(|bytes| bytes.to_vec()),
                label.to_owned(),
            ));
            !self.fail
        }
    }

    #[test]
    fn adapter_reports_calls_failures_and_reapplication() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut applier = Applier::new(Mock {
            hwnd: Some(0x1234),
            calls: Arc::clone(&calls),
            ..Default::default()
        });
        assert!(applier.apply(5));
        assert_eq!(applier.applied(), Some(5));
        // Zero clears with no bitmap.
        assert!(applier.apply(0));
        assert_eq!(applier.applied(), Some(0));
        // No window: failure, applied cache keeps the last success.
        let mut missing = Applier::new(Mock {
            hwnd: None,
            calls: Arc::clone(&calls),
            ..Default::default()
        });
        assert!(!missing.apply(3));
        assert_eq!(missing.applied(), None);
        // Injected failure: desired moves, applied does not.
        let mut failing = Applier::new(Mock {
            hwnd: Some(0x1234),
            fail: true,
            calls: Arc::clone(&calls),
        });
        assert!(!failing.apply(9));
        assert_eq!(failing.applied(), None);
        assert!(!failing.restore());
        // Wait: restore on a failing backend returns false too.
        assert_eq!(failing.applied(), None);
        // Success path restores after window recreation.
        assert!(applier.restore());
        let calls = calls.lock().unwrap();
        assert!(calls.len() >= 4, "calls must be recorded");
        assert!(
            calls.iter().any(|(_, rgba, _)| rgba.is_none()),
            "clear must call with no bitmap"
        );
    }

    /// Headless tests must not open the Explorer watcher. The diagnostic
    /// workflow sets `VESPERA_BADGE_WATCHER` when it wants that path.
    #[test]
    fn headless_apply_skips_the_real_taskbar_without_the_diagnostic_env() {
        if std::env::var_os("VESPERA_BADGE_WATCHER").is_some() {
            return;
        }
        reset_for_tests();
        apply(4);
        #[cfg(target_os = "windows")]
        assert_eq!(
            APPLIED.load(std::sync::atomic::Ordering::SeqCst),
            u64::MAX,
            "a headless test must not record a real taskbar result"
        );
    }

    #[test]
    fn failing_restore_does_not_claim_success() {
        let mut failing = Applier::new(Mock {
            hwnd: Some(1),
            fail: true,
            ..Default::default()
        });
        assert!(!failing.apply(4));
        assert!(!failing.restore());
        assert_eq!(failing.applied(), None);
    }
}
