use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::diagnose;
use crate::localization::{self, LanguageId, Strings};
use crate::models::AppUsageData;
use crate::native_interop::{
    self, Color, TIMER_COUNTDOWN, TIMER_ICON_ANIM, TIMER_POLL, TIMER_RESET_POLL, WM_APP_TRAY,
    WM_APP_USAGE_UPDATED,
};
use crate::poller;
use crate::theme;
use crate::tray_icon;

/// Wrapper to make HWND sendable across threads (safe for PostMessage usage)
#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Shared application state
struct AppState {
    hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    is_dark: bool,
    embedded: bool,
    language_override: Option<LanguageId>,
    language: LanguageId,

    session_percent: f64,
    session_text: String,
    weekly_percent: f64,
    weekly_text: String,
    codex_session_percent: f64,
    codex_session_text: String,
    codex_weekly_percent: f64,
    codex_weekly_text: String,
    antigravity_session_percent: f64,
    antigravity_session_text: String,
    antigravity_weekly_percent: f64,
    antigravity_weekly_text: String,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,

    data: Option<AppUsageData>,

    poll_interval_ms: u32,
    retry_count: u32,
    force_notify_auth_error: bool,
    auth_error_paused_polling: bool,
    auth_watch_mode: poller::CredentialWatchMode,
    auth_watch_snapshot: poller::CredentialWatchSnapshot,
    last_poll_ok: bool,

    taskbar_index: usize,
    tray_offset: i32,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_client_x: i32,
    drag_start_offset: i32,

    widget_visible: bool,
}

const RETRY_BASE_MS: u32 = 30_000; // 30 seconds

const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

// Menu item IDs for update frequency
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_START_WITH_WINDOWS: u16 = 20;
const IDM_RESET_POSITION: u16 = 30;
const IDM_LANG_SYSTEM: u16 = 40;
const IDM_LANG_ENGLISH: u16 = 41;
const IDM_LANG_DUTCH: u16 = 42;
const IDM_LANG_SPANISH: u16 = 43;
const IDM_LANG_FRENCH: u16 = 44;
const IDM_LANG_GERMAN: u16 = 45;
const IDM_LANG_JAPANESE: u16 = 46;
const IDM_LANG_KOREAN: u16 = 47;
const IDM_LANG_TRADITIONAL_CHINESE: u16 = 48;
const IDM_LANG_RUSSIAN: u16 = 49;
const IDM_LANG_PORTUGUESE_BRAZIL: u16 = 50;
const IDM_MODEL_CLAUDE_CODE: u16 = 60;
const IDM_MODEL_CODEX: u16 = 61;
const IDM_MODEL_ANTIGRAVITY: u16 = 62;
// Monitor picker: one id per taskbar, IDM_MONITOR_BASE + index (up to 16 monitors).
const IDM_MONITOR_BASE: u16 = 80;
const IDM_MONITOR_MAX_COUNT: u16 = 16;
const IDM_ABOUT: u16 = 99;

const WM_DPICHANGED_MSG: u32 = 0x02E0;
const TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS: u64 = 750;

/// How often the watchdog thread polls for an explorer.exe restart (which
/// recreates the taskbar and wipes our tray-icon registration).
const TASKBAR_WATCH_INTERVAL_SECS: u64 = 2;

static SUPPRESS_TRAY_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Current system DPI (96 = 100% scaling, 144 = 150%, 192 = 200%, etc.)
static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);

/// Advances the mascot's walk-cycle frame on each tick of TIMER_ICON_ANIM.
static ICON_ANIM_FRAME: AtomicU32 = AtomicU32::new(0);
const ICON_ANIM_INTERVAL_MS: u32 = 90;

/// Scale a base pixel value (designed at 96 DPI) to the current DPI.
fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

/// Re-query the monitor DPI for our window and update the cached value.
/// Uses GetDpiForWindow which returns the live DPI (unlike GetDpiForSystem
/// which is cached at process startup and never changes).
fn refresh_dpi() {
    let hwnd = {
        let state = lock_state();
        state.as_ref().map(|s| s.hwnd.to_hwnd())
    };
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

/// Spacing below which two relaunches are treated as a storm (e.g. explorer.exe
/// crash-looping); when detected we back off instead of spawning in a tight loop.
const RELAUNCH_THROTTLE_SECS: u64 = 10;
const RELAUNCH_BACKOFF_SECS: u64 = 30;
/// Environment flag set on a relaunched child so it waits for the previous
/// instance's single-instance mutex instead of exiting immediately.
const ENV_RELAUNCH: &str = "CCUM_RELAUNCH";
/// Unix timestamp (seconds) of the relaunch that spawned this process, passed to
/// the child so it can detect a relaunch storm.
const ENV_LAST_RELAUNCH_UNIX: &str = "CCUM_LAST_RELAUNCH_UNIX";

/// Relaunch the widget as a fresh process after explorer.exe has restarted.
///
/// When the shell restarts it destroys our embedded child window outright (the
/// window is gone, not merely orphaned - `IsWindow` returns false) and leaves
/// the UI thread parked in `GetMessage` with no window to recreate in place.
/// Spawning a clean new process - which re-embeds into the freshly created
/// taskbar - and exiting this one is the robust recovery. The child is flagged
/// via `ENV_RELAUNCH` so it waits for this instance's single-instance mutex to
/// be released before taking over (see the guard in `run`).
fn relaunch_self() {
    // Back off if we are relaunching very soon after the relaunch that spawned
    // us: that signals the shell is crash-looping, not a one-off restart.
    let now = now_unix_secs();
    let last = std::env::var(ENV_LAST_RELAUNCH_UNIX)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < RELAUNCH_THROTTLE_SECS {
        diagnose::log("relaunch storm detected; backing off before relaunching");
        std::thread::sleep(Duration::from_secs(RELAUNCH_BACKOFF_SECS));
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            diagnose::log_error("watchdog: unable to resolve current executable", error);
            return;
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe)
        .args(&args)
        .env(ENV_RELAUNCH, "1")
        .env(ENV_LAST_RELAUNCH_UNIX, now.to_string())
        .spawn()
    {
        Ok(_) => {
            diagnose::log("watchdog: relaunched fresh instance, exiting old one");
            std::process::exit(0);
        }
        Err(error) => {
            diagnose::log_error("watchdog: unable to spawn relaunched instance", error);
        }
    }
}

/// Detect explorer.exe restarts and recover from them.
///
/// Once explorer destroys the taskbar, our embedded child window is destroyed
/// and the UI message loop is dead, so recovery cannot happen in-process. This
/// dedicated thread (independent of the dead message loop) polls the taskbar
/// handle and, when it changes, relaunches the widget as a fresh process.
fn spawn_taskbar_watchdog() {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TASKBAR_WATCH_INTERVAL_SECS));
        let stored = {
            let state = lock_state();
            state.as_ref().and_then(|s| s.taskbar_hwnd)
        };
        // Only relevant once we have embedded into a taskbar at least once.
        let Some(old) = stored else {
            continue;
        };
        let taskbars = native_interop::find_taskbars();
        if !taskbars.is_empty() && !taskbars.iter().any(|taskbar| taskbar.hwnd == old) {
            let new = taskbars[0].hwnd;
            diagnose::log(format!(
                "watchdog: taskbar changed old={:?} new={:?} -> relaunching",
                old.0, new.0
            ));
            relaunch_self();
        }
    });
}

fn load_embedded_app_icons() -> (HICON, HICON) {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large_icon, small_icon)
        }
    }
}

unsafe impl Send for AppState {}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

/// Lock STATE safely, recovering from poisoned mutex
fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn settings_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata)
        .join("ClaudeCodeUsageMonitor")
        .join("settings.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    tray_offset: i32,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default = "default_widget_visible")]
    widget_visible: bool,
    #[serde(default = "default_show_claude_code")]
    show_claude_code: bool,
    #[serde(default = "default_show_codex")]
    show_codex: bool,
    #[serde(default = "default_show_antigravity")]
    show_antigravity: bool,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            tray_offset: 0,
            taskbar_index: 0,
            poll_interval_ms: default_poll_interval(),
            language: None,
            widget_visible: true,
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
        }
    }
}

fn default_poll_interval() -> u32 {
    POLL_15_MIN
}

fn default_widget_visible() -> bool {
    true
}

fn default_show_claude_code() -> bool {
    true
}

fn default_show_codex() -> bool {
    false
}

fn default_show_antigravity() -> bool {
    false
}

fn load_settings() -> SettingsFile {
    let content = match std::fs::read_to_string(settings_path()) {
        Ok(c) => c,
        Err(_) => return SettingsFile::default(),
    };
    let mut settings: SettingsFile = serde_json::from_str(&content).unwrap_or_default();
    if !settings.show_claude_code && !settings.show_codex && !settings.show_antigravity {
        settings.show_claude_code = true;
    }
    settings
}

fn save_settings(settings: &SettingsFile) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn save_state_settings() {
    let state = lock_state();
    if let Some(s) = state.as_ref() {
        save_settings(&SettingsFile {
            tray_offset: s.tray_offset,
            taskbar_index: s.taskbar_index,
            poll_interval_ms: s.poll_interval_ms,
            language: s
                .language_override
                .map(|language| language.code().to_string()),
            widget_visible: s.widget_visible,
            show_claude_code: s.show_claude_code,
            show_codex: s.show_codex,
            show_antigravity: s.show_antigravity,
        });
    }
}

fn tray_icon_data_from_state() -> Vec<tray_icon::TrayIconData> {
    let state = lock_state();
    match state.as_ref() {
        Some(s) if s.last_poll_ok => {
            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: Some(s.session_percent),
                    tooltip: format!(
                        "{} 5h: {} | 7d: {}",
                        s.language.strings().claude_code_model,
                        s.session_text,
                        s.weekly_text
                    ),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: Some(s.codex_session_percent),
                    tooltip: format!(
                        "{} 5h: {} | 7d: {}",
                        s.language.strings().codex_model,
                        s.codex_session_text,
                        s.codex_weekly_text
                    ),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: Some(s.antigravity_session_percent),
                    tooltip: format!(
                        "{} 5h: {} | 7d: {}",
                        s.language.strings().antigravity_model,
                        s.antigravity_session_text,
                        s.antigravity_weekly_text
                    ),
                });
            }
            icons
        }
        Some(s) => {
            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: None,
                    tooltip: s.language.strings().window_title.to_string(),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: None,
                    tooltip: s.language.strings().codex_window_title.to_string(),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: None,
                    tooltip: s.language.strings().antigravity_window_title.to_string(),
                });
            }
            icons
        }
        None => Vec::new(),
    }
}

fn sync_tray_icons(hwnd: HWND) {
    let icons = tray_icon_data_from_state();
    tray_icon::sync(hwnd, &icons);
}

fn toggle_widget_visibility(hwnd: HWND) {
    let new_visible = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            s.widget_visible = !s.widget_visible;
            s.widget_visible
        } else {
            return;
        }
    };
    save_state_settings();
    unsafe {
        if new_visible {
            position_at_taskbar();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            render_layered();
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn attach_to_taskbar(hwnd: HWND, requested_index: usize) -> bool {
    let taskbars = native_interop::find_taskbars();
    if taskbars.is_empty() {
        diagnose::log("taskbar not found; using fallback popup window");
        return false;
    }

    let index = requested_index.min(taskbars.len().saturating_sub(1));
    let taskbar = taskbars[index];
    diagnose::log(format!(
        "taskbar selected index={index} count={} hwnd={:?} rect=({}, {}, {}, {})",
        taskbars.len(),
        taskbar.hwnd,
        taskbar.rect.left,
        taskbar.rect.top,
        taskbar.rect.right,
        taskbar.rect.bottom
    ));

    let old_hook = {
        let mut state = lock_state();
        state.as_mut().and_then(|s| s.win_event_hook.take())
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }

    native_interop::embed_in_taskbar(hwnd, taskbar.hwnd);

    let tray_notify = native_interop::find_child_window(taskbar.hwnd, "TrayNotifyWnd");
    if tray_notify.is_some() {
        diagnose::log("TrayNotifyWnd found");
    } else {
        diagnose::log("TrayNotifyWnd not found");
    }

    let hook = tray_notify.and_then(|tray_hwnd| {
        let thread_id = native_interop::get_window_thread_id(tray_hwnd);
        native_interop::set_tray_event_hook(thread_id, on_tray_location_changed)
    });
    if hook.is_some() {
        diagnose::log("tray event hook installed");
    } else {
        diagnose::log("tray event hook could not be installed");
    }

    let mut state = lock_state();
    if let Some(s) = state.as_mut() {
        s.taskbar_hwnd = Some(taskbar.hwnd);
        s.tray_notify_hwnd = tray_notify;
        s.win_event_hook = hook;
        s.taskbar_index = index;
        s.embedded = true;
    }
    true
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

fn tray_left_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT) -> i32 {
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    tray_left
}

fn clamp_offset_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT, offset: i32) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let max_offset = (tray_left - taskbar_rect.left - total_widget_width()).max(0);
    offset.clamp(0, max_offset)
}

fn offset_for_drop_point(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    pt: POINT,
    drag_start_client_x: i32,
) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let desired_left = pt.x - taskbar_rect.left - drag_start_client_x;
    let offset = tray_left - taskbar_rect.left - total_widget_width() - desired_left;
    clamp_offset_for_taskbar(taskbar_hwnd, taskbar_rect, offset)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn refresh_usage_texts(state: &mut AppState) {
    if !state.last_poll_ok {
        return;
    }

    let strings = state.language.strings();
    let Some(data) = state.data.as_ref() else {
        return;
    };

    if let Some(claude_code) = data.claude_code.as_ref() {
        state.session_text = poller::format_line(&claude_code.session, strings);
        state.weekly_text = poller::format_line(&claude_code.weekly, strings);
    } else if state.show_claude_code {
        state.session_text = "!".to_string();
        state.weekly_text = "!".to_string();
    }

    if let Some(codex) = data.codex.as_ref() {
        state.codex_session_text = poller::format_line(&codex.session, strings);
        state.codex_weekly_text = poller::format_line(&codex.weekly, strings);
    } else if state.show_codex {
        state.codex_session_text = "!".to_string();
        state.codex_weekly_text = "!".to_string();
    }

    if let Some(antigravity) = data.antigravity.as_ref() {
        state.antigravity_session_text = poller::format_line(&antigravity.session, strings);
        state.antigravity_weekly_text =
            if antigravity.weekly.resets_at.is_none() && antigravity.weekly.percentage == 0.0 {
                "--".to_string()
            } else {
                poller::format_line(&antigravity.weekly, strings)
            };
    } else if state.show_antigravity {
        state.antigravity_session_text = "!".to_string();
        state.antigravity_weekly_text = "!".to_string();
    }
}

fn set_window_title(hwnd: HWND, strings: Strings) {
    unsafe {
        let title = native_interop::wide_str(strings.window_title);
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(title.as_ptr()));
    }
}

fn apply_language_to_state(state: &mut AppState, language_override: Option<LanguageId>) {
    state.language_override = language_override;
    state.language = localization::resolve_language(language_override);
    set_window_title(state.hwnd.to_hwnd(), state.language.strings());
    refresh_usage_texts(state);
}

fn update_language_change() -> bool {
    let mut state = lock_state();
    let Some(app_state) = state.as_mut() else {
        return false;
    };

    if app_state.language_override.is_some() {
        return false;
    }

    let new_language = localization::detect_system_language();
    if new_language == app_state.language {
        return false;
    }

    apply_language_to_state(app_state, None);
    true
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "ClaudeCodeUsageMonitor";

/// Returns true only if the startup registry value points to this executable.
fn is_startup_enabled() -> bool {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if result.is_err() {
            return false;
        }

        // Query the size of the value
        let mut data_size: u32 = 0;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_size),
        );
        if result.is_err() || data_size == 0 {
            let _ = RegCloseKey(hkey);
            return false;
        }

        // Read the value
        let mut buf = vec![0u8; data_size as usize];
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return false;
        }

        // Convert the registry value (UTF-16) to a string
        let wide_slice =
            std::slice::from_raw_parts(buf.as_ptr() as *const u16, data_size as usize / 2);
        let reg_value = String::from_utf16_lossy(wide_slice)
            .trim_end_matches('\0')
            .to_string();

        // Get the current executable path
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return false;
        }
        let current_exe = String::from_utf16_lossy(&exe_buf[..len]);

        // Case-insensitive comparison (Windows paths are case-insensitive)
        reg_value.eq_ignore_ascii_case(&current_exe)
    }
}

fn set_startup_enabled(enable: bool) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        if enable {
            let mut exe_buf = [0u16; 260];
            let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
            if len > 0 {
                // Write the wide string including null terminator
                let byte_len = ((len + 1) * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    PCWSTR::from_raw(key_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        exe_buf.as_ptr() as *const u8,
                        byte_len as usize,
                    )),
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
        }

        let _ = RegCloseKey(hkey);
    }
}

// Dimensions matching the C# version
const SEGMENT_W: i32 = 12;
const SEGMENT_H: i32 = 11;
const SEGMENT_GAP: i32 = 1;
const SEGMENT_COUNT: i32 = 10;
const CORNER_RADIUS: i32 = 0;

const ICON_LEFT_MARGIN: i32 = 8;
const ICON_W: i32 = 41;
const ICON_H: i32 = 21;
const ICON_RIGHT_MARGIN: i32 = 6;
const LABEL_WIDTH: i32 = 26;
const LABEL_RIGHT_MARGIN: i32 = 1;
const BAR_RIGHT_MARGIN: i32 = 1;
const TEXT_WIDTH: i32 = 62;
const MODEL_RIGHT_MARGIN: i32 = 3;
const RIGHT_MARGIN: i32 = 8;
const WIDGET_HEIGHT: i32 = 46;

/// Bounding box (x, y, w, h) of the mascot icon in client coordinates —
/// shared by painting and drag hit-testing so they never drift apart.
fn icon_bounds() -> (i32, i32, i32, i32) {
    let icon_w = sc(ICON_W);
    let icon_h = sc(ICON_H);
    let icon_x = sc(ICON_LEFT_MARGIN);
    let icon_y = (sc(WIDGET_HEIGHT) - icon_h) / 2;
    (icon_x, icon_y, icon_w, icon_h)
}

fn is_drag_handle_point(client_x: i32, client_y: i32) -> bool {
    let (icon_x, icon_y, icon_w, icon_h) = icon_bounds();
    client_x >= icon_x
        && client_x < icon_x + icon_w
        && client_y >= icon_y
        && client_y < icon_y + icon_h
}

fn cursor_is_on_drag_handle(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() || !ScreenToClient(hwnd, &mut pt).as_bool() {
            return false;
        }
        is_drag_handle_point(pt.x, pt.y)
    }
}

fn active_model_count(show_claude_code: bool, show_codex: bool, show_antigravity: bool) -> i32 {
    (show_claude_code as i32 + show_codex as i32 + show_antigravity as i32).max(1)
}

fn row_bar_segment_count(active_models: i32) -> i32 {
    match active_models {
        1 => SEGMENT_COUNT,
        2 => 5,
        _ => 4,
    }
}

fn total_widget_width_for(active_models: i32) -> i32 {
    let bar_segments = row_bar_segment_count(active_models);
    let model_width = (sc(SEGMENT_W) + sc(SEGMENT_GAP)) * bar_segments - sc(SEGMENT_GAP)
        + sc(BAR_RIGHT_MARGIN)
        + sc(TEXT_WIDTH);

    sc(ICON_LEFT_MARGIN)
        + sc(ICON_W)
        + sc(ICON_RIGHT_MARGIN)
        + sc(LABEL_WIDTH)
        + sc(LABEL_RIGHT_MARGIN)
        + model_width * active_models
        + sc(MODEL_RIGHT_MARGIN) * (active_models - 1)
        + sc(RIGHT_MARGIN)
}

fn total_widget_width_for_state(state: &AppState) -> i32 {
    total_widget_width_for(active_model_count(
        state.show_claude_code,
        state.show_codex,
        state.show_antigravity,
    ))
}

fn total_widget_width() -> i32 {
    let active_models = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| active_model_count(s.show_claude_code, s.show_codex, s.show_antigravity))
            .unwrap_or(1)
    };
    total_widget_width_for(active_models)
}

fn claude_accent_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#FFFFFF")
    } else {
        Color::from_hex("#D97757")
    }
}

fn codex_accent_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F5F5F5")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn antigravity_accent_color() -> Color {
    Color::from_hex("#4285F4")
}

fn claude_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#FFFFFF")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn codex_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F5F5F5")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn antigravity_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#8AB4F8")
    } else {
        Color::from_hex("#1967D2")
    }
}

pub fn run() {
    // Enable Per-Monitor DPI Awareness V2 for crisp rendering at any scale factor
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    // Single-instance guard: silently exit if another instance is running.
    // Exception: when relaunched after an explorer restart (ENV_RELAUNCH set),
    // wait for the previous instance to release the mutex, then take over.
    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok();
    let mutex_name = native_interop::wide_str("Global\\ClaudeCodeUsageMonitor");
    let _mutex = unsafe {
        let handle = CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr()));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if is_relaunch {
                        diagnose::log("relaunch: waiting for previous instance to exit");
                        let wait_result = WaitForSingleObject(h, 10_000);
                        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED {
                            diagnose::log(format!(
                                "startup aborted: previous instance did not exit cleanly ({wait_result:?})"
                            ));
                            return;
                        }
                    } else {
                        diagnose::log("startup aborted: another instance is already running");
                        return;
                    }
                }
                h
            }
            Err(error) => {
                diagnose::log_error(
                    "startup aborted: unable to create single-instance mutex",
                    error,
                );
                return;
            }
        }
    };

    let class_name = native_interop::wide_str("ClaudeCodeUsageMonitor");

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_embedded_app_icons();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            diagnose::log("RegisterClassExW returned 0");
        }

        let settings = load_settings();
        let language_override = settings.language.as_deref().and_then(LanguageId::from_code);
        let language = localization::resolve_language(language_override);

        // Create as layered popup (will be reparented into taskbar)
        let title = native_interop::wide_str(language.strings().window_title);
        let initial_model_count = active_model_count(
            settings.show_claude_code,
            settings.show_codex,
            settings.show_antigravity,
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            total_widget_width_for(initial_model_count),
            sc(WIDGET_HEIGHT),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .unwrap();

        if !large_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(large_icon.0 as isize),
            );
        }
        if !small_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small_icon.0 as isize),
            );
        }

        diagnose::log(format!("main window created hwnd={:?}", hwnd));

        let is_dark = theme::is_dark_mode();
        let mut embedded = false;

        {
            let mut state = lock_state();
            *state = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                is_dark,
                embedded: false,
                language_override,
                language,
                session_percent: 0.0,
                session_text: "--".to_string(),
                weekly_percent: 0.0,
                weekly_text: "--".to_string(),
                codex_session_percent: 0.0,
                codex_session_text: "--".to_string(),
                codex_weekly_percent: 0.0,
                codex_weekly_text: "--".to_string(),
                antigravity_session_percent: 0.0,
                antigravity_session_text: "--".to_string(),
                antigravity_weekly_percent: 0.0,
                antigravity_weekly_text: "--".to_string(),
                show_claude_code: settings.show_claude_code,
                show_codex: settings.show_codex,
                show_antigravity: settings.show_antigravity,
                data: None,
                poll_interval_ms: settings.poll_interval_ms,
                retry_count: 0,
                force_notify_auth_error: false,
                auth_error_paused_polling: false,
                auth_watch_mode: poller::CredentialWatchMode::ActiveSource,
                auth_watch_snapshot: Vec::new(),
                last_poll_ok: false,
                taskbar_index: settings.taskbar_index,
                tray_offset: settings.tray_offset,
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_client_x: 0,
                drag_start_offset: 0,
                widget_visible: settings.widget_visible,
            });
        }

        // Try to embed in taskbar
        if attach_to_taskbar(hwnd, settings.taskbar_index) {
            embedded = true;
        }

        // If not embedded, fall back to topmost popup with SetLayeredWindowAttributes
        if !embedded {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }

        // Register system tray icon(s)
        sync_tray_icons(hwnd);

        // Position and show (only if widget_visible preference is true)
        position_at_taskbar();
        if settings.widget_visible {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        diagnose::log("window shown");

        // Initial render via UpdateLayeredWindow (for embedded) or InvalidateRect (fallback)
        render_layered();

        // Poll timer: 15 minutes
        let initial_poll_ms = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| s.poll_interval_ms)
                .unwrap_or(POLL_15_MIN)
        };
        SetTimer(hwnd, TIMER_POLL, initial_poll_ms, None);

        // Icon walk animation: redraw at a steady cadence to advance the frame.
        SetTimer(hwnd, TIMER_ICON_ANIM, ICON_ANIM_INTERVAL_MS, None);

        // Watch for explorer.exe restarts so we can re-embed and re-add the tray
        // icon (the shell discards tray registrations when it restarts). This
        // runs on a dedicated thread, NOT a window timer: once explorer destroys
        // the taskbar, our embedded child window stops receiving all messages
        // (WM_TIMER included), so a timer would never fire again.
        spawn_taskbar_watchdog();

        // Initial poll
        let send_hwnd = SendHwnd::from_hwnd(hwnd);
        std::thread::spawn(move || {
            diagnose::log("initial poll thread started");
            do_poll(send_hwnd);
        });

        // Initial theme check
        check_theme_change();

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Formats the time left until `resets_at` as "H:MM" (e.g. "2:32"). Used for
/// the 5-hour limit line, where minute-level precision reads better than the
/// widget's own coarse "2h" countdown.
fn format_hm_remaining(resets_at: Option<SystemTime>) -> String {
    let Some(reset) = resets_at else {
        return "--".to_string();
    };
    let remaining_secs = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d.as_secs(),
        Err(_) => 0,
    };
    diagnose::log(format!(
        "format_hm_remaining: resets_at={:?} remaining_secs={}",
        reset
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        remaining_secs
    ));
    let remaining_mins = remaining_secs / 60;
    format!("{}:{:02}", remaining_mins / 60, remaining_mins % 60)
}

/// Formats the time left until `resets_at` as "N day(s)", pluralized. Used
/// for the weekly limit line.
fn format_days_remaining(resets_at: Option<SystemTime>, strings: Strings) -> String {
    let Some(reset) = resets_at else {
        return "--".to_string();
    };
    let remaining_days = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d.as_secs() / 86400,
        Err(_) => 0,
    };
    let word = if remaining_days == 1 {
        strings.day_word_singular
    } else {
        strings.day_word_plural
    };
    format!("{remaining_days} {word}")
}

/// Render widget content and push to the layered window via UpdateLayeredWindow.
/// Renders fully opaque with the actual taskbar background colour so that
/// ClearType sub-pixel font rendering can be used for crisp, OS-native text.
fn render_layered() {
    refresh_dpi();
    let (
        hwnd_val,
        is_dark,
        embedded,
        strings,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (
                s.hwnd,
                s.is_dark,
                s.embedded,
                s.language.strings(),
                s.session_percent,
                s.session_text.clone(),
                s.weekly_percent,
                s.weekly_text.clone(),
                s.codex_session_percent,
                s.codex_session_text.clone(),
                s.codex_weekly_percent,
                s.codex_weekly_text.clone(),
                s.antigravity_session_percent,
                s.antigravity_session_text.clone(),
                s.antigravity_weekly_percent,
                s.antigravity_weekly_text.clone(),
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
            ),
            None => return,
        }
    };

    let hwnd = hwnd_val.to_hwnd();

    // For non-embedded fallback, just invalidate and let WM_PAINT handle it
    if !embedded {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let width = total_widget_width();
    let height = sc(WIDGET_HEIGHT);

    let accent = claude_accent_color(is_dark);
    let codex_accent = codex_accent_color(is_dark);
    let antigravity_accent = antigravity_accent_color();
    let track = if is_dark {
        Color::from_hex("#444444")
    } else {
        Color::from_hex("#D6D6D6")
    };
    let text_color = if is_dark {
        Color::from_hex("#888888")
    } else {
        Color::from_hex("#404040")
    };
    let bg_color = if is_dark {
        Color::from_hex("#1C1C1C")
    } else {
        Color::from_hex("#F3F3F3")
    };

    unsafe {
        let screen_dc = GetDC(hwnd);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        let pixel_count = (width * height) as usize;

        // Render once with the actual taskbar background colour.
        // Using an opaque background lets us use CLEARTYPE_QUALITY for
        // sub-pixel font rendering that matches the rest of the OS.
        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            &bg_color,
            &text_color,
            &accent,
            &track,
            strings,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            &codex_accent,
            &antigravity_accent,
        );

        // Background pixels → alpha 1 (nearly invisible but still hittable for right-click).
        // Content pixels → fully opaque (preserves ClearType sub-pixel rendering).
        let bg_bgr = bg_color.to_colorref();
        let pixel_data = std::slice::from_raw_parts_mut(bits as *mut u32, pixel_count);
        for px in pixel_data.iter_mut() {
            let rgb = *px & 0x00FFFFFF;
            if rgb == bg_bgr {
                *px = 0x01000000;
            } else {
                *px = rgb | 0xFF000000;
            }
        }

        // Push to window via UpdateLayeredWindow
        let pt_src = POINT { x: 0, y: 0 };
        let sz = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: 0, // AC_SRC_OVER
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };

        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        // Cleanup
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// Paint all widget content onto a DC with a given background color.
fn paint_content(
    hdc: HDC,
    width: i32,
    height: i32,
    is_dark: bool,
    bg: &Color,
    text_color: &Color,
    accent: &Color,
    track: &Color,
    strings: Strings,
    session_pct: f64,
    session_text: &str,
    weekly_pct: f64,
    weekly_text: &str,
    codex_session_pct: f64,
    codex_session_text: &str,
    codex_weekly_pct: f64,
    codex_weekly_text: &str,
    antigravity_session_pct: f64,
    antigravity_session_text: &str,
    antigravity_weekly_pct: f64,
    antigravity_weekly_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    codex_accent: &Color,
    antigravity_accent: &Color,
) {
    unsafe {
        let client_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };

        let bg_brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(hdc, &client_rect, bg_brush);
        let _ = DeleteObject(bg_brush);

        let (icon_x, icon_y, icon_w, icon_h) = icon_bounds();
        let out_of_energy = session_pct >= 100.0 || weekly_pct >= 100.0;
        if out_of_energy {
            // Out of tokens: gentle constant breathing loop, not tied to usage.
            let progress = ICON_ANIM_FRAME.load(Ordering::Relaxed) as f64 * SLEEP_ANIM_SPEED;
            let frame_index =
                (progress.floor() as i64).rem_euclid(SLEEP_FRAMES.len() as i64) as usize;
            let next_index = (frame_index + 1) % SLEEP_FRAMES.len();
            let t = progress.fract() as f32;
            draw_icon_frame_blended(
                hdc,
                icon_x,
                icon_y,
                icon_w,
                icon_h,
                &SLEEP_FRAMES[frame_index],
                &SLEEP_FRAMES[next_index],
                t,
            );
        } else {
            // More tokens left (closer to 0%) = more energy = faster walk.
            let anim_speed = WALK_MAX_SPEED
                - (WALK_MAX_SPEED - WALK_MIN_SPEED) * (session_pct.clamp(0.0, 100.0) / 100.0);
            let frame_progress = ICON_ANIM_FRAME.load(Ordering::Relaxed) as f64 * anim_speed;
            let frame_index =
                (frame_progress.floor() as i64).rem_euclid(WALK_FRAMES.len() as i64) as usize;
            let next_index = (frame_index + 1) % WALK_FRAMES.len();
            let t = frame_progress.fract() as f32;
            draw_icon_frame_blended(
                hdc,
                icon_x,
                icon_y,
                icon_w,
                icon_h,
                &WALK_FRAMES[frame_index],
                &WALK_FRAMES[next_index],
                t,
            );
        }

        let content_x = sc(ICON_LEFT_MARGIN) + icon_w + sc(ICON_RIGHT_MARGIN);
        let row_gap = sc(6);
        let rows_block_h = sc(SEGMENT_H) * 2 + row_gap;
        let row1_y = (height - rows_block_h) / 2;
        let row2_y = row1_y + sc(SEGMENT_H) + row_gap;

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));

        let font_name = native_interop::wide_str("Segoe UI");
        let font = CreateFontW(
            sc(-10),
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(font_name.as_ptr()),
        );
        let old_font = SelectObject(hdc, font);

        draw_row(
            hdc,
            content_x,
            row1_y,
            is_dark,
            text_color,
            strings.session_window,
            session_pct,
            session_text,
            codex_session_pct,
            codex_session_text,
            antigravity_session_pct,
            antigravity_session_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            accent,
            codex_accent,
            antigravity_accent,
            track,
        );
        draw_row(
            hdc,
            content_x,
            row2_y,
            is_dark,
            text_color,
            strings.weekly_window,
            weekly_pct,
            weekly_text,
            codex_weekly_pct,
            codex_weekly_text,
            antigravity_weekly_pct,
            antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            accent,
            codex_accent,
            antigravity_accent,
            track,
        );

        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
    }
}

fn do_poll(send_hwnd: SendHwnd) {
    let hwnd = send_hwnd.to_hwnd();
    let (show_claude_code, show_codex, show_antigravity) = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| (s.show_claude_code, s.show_codex, s.show_antigravity))
            .unwrap_or((true, false, false))
    };

    match poller::poll(show_claude_code, show_codex, show_antigravity) {
        Ok(data) => {
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                if let Some(claude_code) = data.claude_code.as_ref() {
                    s.session_percent = claude_code.session.percentage;
                    s.weekly_percent = claude_code.weekly.percentage;
                } else if s.show_claude_code {
                    s.session_percent = 0.0;
                    s.weekly_percent = 0.0;
                }
                if let Some(codex) = data.codex.as_ref() {
                    s.codex_session_percent = codex.session.percentage;
                    s.codex_weekly_percent = codex.weekly.percentage;
                } else if s.show_codex {
                    s.codex_session_percent = 0.0;
                    s.codex_weekly_percent = 0.0;
                }
                if let Some(antigravity) = data.antigravity.as_ref() {
                    s.antigravity_session_percent = antigravity.session.percentage;
                    s.antigravity_weekly_percent = antigravity.weekly.percentage;
                } else if s.show_antigravity {
                    s.antigravity_session_percent = 0.0;
                    s.antigravity_weekly_percent = 0.0;
                }
                // Stop fast-poll if reset data is now fresh
                if !poller::app_is_past_reset(&data) {
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                    }
                }

                s.data = Some(data);
                s.last_poll_ok = true;
                refresh_usage_texts(s);

                // Recovered from errors — restore normal poll interval
                if s.retry_count > 0 {
                    s.retry_count = 0;
                    let interval = s.poll_interval_ms;
                    unsafe {
                        SetTimer(hwnd, TIMER_POLL, interval, None);
                    }
                }
                s.force_notify_auth_error = false;
                s.auth_error_paused_polling = false;
                s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                s.auth_watch_snapshot.clear();
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
        Err(e) => {
            let auth_watch = match e {
                poller::PollError::AuthRequired | poller::PollError::TokenExpired
                    if show_antigravity && !show_claude_code && !show_codex =>
                {
                    Some((
                        poller::CredentialWatchMode::Antigravity,
                        poller::credential_watch_snapshot(poller::CredentialWatchMode::Antigravity),
                    ))
                }
                poller::PollError::AuthRequired | poller::PollError::TokenExpired => Some((
                    poller::CredentialWatchMode::ActiveSource,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::ActiveSource),
                )),
                poller::PollError::NoCredentials => Some((
                    poller::CredentialWatchMode::AllSources,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::AllSources),
                )),
                poller::PollError::RequestFailed => None,
            };
            // Distinguish auth-required errors from transient errors.
            let notify_auth_error = {
                let mut state = lock_state();
                let mut should_notify = false;
                if let Some(s) = state.as_mut() {
                    s.last_poll_ok = false;
                    match auth_watch {
                        Some((watch_mode, watch_snapshot)) => {
                            // Only show the balloon on the first failure so it doesn't spam.
                            if s.retry_count == 0 || s.force_notify_auth_error {
                                should_notify = true;
                            }
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = true;
                            s.auth_watch_mode = watch_mode;
                            s.auth_watch_snapshot = watch_snapshot;
                            s.session_text = "!".to_string();
                            s.weekly_text = "!".to_string();
                            s.codex_session_text = "!".to_string();
                            s.codex_weekly_text = "!".to_string();
                            s.antigravity_session_text = "!".to_string();
                            s.antigravity_weekly_text = "!".to_string();
                            s.retry_count = s.retry_count.saturating_add(1);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_POLL);
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
                                SetTimer(hwnd, TIMER_POLL, s.poll_interval_ms, None);
                            }
                        }
                        _ => {
                            // Transient network / credential-missing errors: exponential backoff.
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = false;
                            s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                            s.auth_watch_snapshot.clear();
                            s.session_text = "...".to_string();
                            s.weekly_text = "...".to_string();
                            s.codex_session_text = "...".to_string();
                            s.codex_weekly_text = "...".to_string();
                            s.antigravity_session_text = "...".to_string();
                            s.antigravity_weekly_text = "...".to_string();
                            s.retry_count = s.retry_count.saturating_add(1);
                            let backoff = RETRY_BASE_MS.saturating_mul(
                                1u32.checked_shl(s.retry_count - 1).unwrap_or(u32::MAX),
                            );
                            let retry_ms = backoff.min(s.poll_interval_ms);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                SetTimer(hwnd, TIMER_POLL, retry_ms, None);
                            }
                        }
                    }
                }
                should_notify
            };

            if notify_auth_error {
                let notification = {
                    let state = lock_state();
                    state.as_ref().map(|s| {
                        let strings = s.language.strings();
                        // NoCredentials means this source has never been logged
                        // into on this machine — show a step-by-step setup
                        // guide instead of the terser "re-login" balloon.
                        let first_setup =
                            matches!(s.auth_watch_mode, poller::CredentialWatchMode::AllSources);
                        // The exact command a first-time Claude Code setup
                        // needs to paste into PowerShell — copied to the
                        // clipboard so the user doesn't have to retype it
                        // out of a dialog they can't select text from.
                        const CLAUDE_INSTALL_CMD: &str = "irm https://claude.ai/install.ps1 | iex";
                        let (kind, title, body, clipboard_cmd) = if s.show_claude_code {
                            if first_setup {
                                (
                                    tray_icon::TrayIconKind::Claude,
                                    strings.no_credentials_title,
                                    strings.no_credentials_body,
                                    Some(CLAUDE_INSTALL_CMD),
                                )
                            } else {
                                (
                                    tray_icon::TrayIconKind::Claude,
                                    strings.token_expired_title,
                                    strings.token_expired_body,
                                    None,
                                )
                            }
                        } else if s.show_codex {
                            if first_setup {
                                (
                                    tray_icon::TrayIconKind::Codex,
                                    strings.codex_no_credentials_title,
                                    strings.codex_no_credentials_body,
                                    None,
                                )
                            } else {
                                (
                                    tray_icon::TrayIconKind::Codex,
                                    strings.codex_token_expired_title,
                                    strings.codex_token_expired_body,
                                    None,
                                )
                            }
                        } else if first_setup {
                            (
                                tray_icon::TrayIconKind::Antigravity,
                                strings.antigravity_no_credentials_title,
                                strings.antigravity_no_credentials_body,
                                None,
                            )
                        } else {
                            (
                                tray_icon::TrayIconKind::Antigravity,
                                strings.antigravity_token_expired_title,
                                strings.antigravity_token_expired_body,
                                None,
                            )
                        };
                        (first_setup, kind, title, body, clipboard_cmd)
                    })
                };
                if let Some((_first_setup, kind, title, body, clipboard_cmd)) = notification {
                    if let Some(cmd) = clipboard_cmd {
                        native_interop::copy_text_to_clipboard(cmd);
                    }
                    tray_icon::notify_balloon(hwnd, kind, title, body);
                }
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn schedule_countdown_timer() {
    let state = lock_state();
    let s = match state.as_ref() {
        Some(s) => s,
        None => return,
    };

    let hwnd = s.hwnd.to_hwnd();
    if !s.last_poll_ok {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
            let _ = KillTimer(hwnd, TIMER_RESET_POLL);
        }
        return;
    }

    let data = match &s.data {
        Some(d) => d,
        None => return,
    };

    // If a reset time has passed, poll every 5s to pick up fresh data
    if poller::app_is_past_reset(data) {
        unsafe {
            SetTimer(hwnd, TIMER_RESET_POLL, 5_000, None);
        }
    }

    let delays = [
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
    ];
    let min_delay = delays.into_iter().flatten().min();

    let ms = min_delay
        .unwrap_or(Duration::from_secs(60))
        .as_millis()
        .max(1000) as u32;

    unsafe {
        SetTimer(hwnd, TIMER_COUNTDOWN, ms, None);
    }
}

fn check_theme_change() {
    let taskbar_hwnd = {
        let state = lock_state();
        state.as_ref().and_then(|s| s.taskbar_hwnd)
    };
    let new_dark = taskbar_hwnd
        .and_then(|hwnd| {
            native_interop::get_taskbar_rect(hwnd)
                .and_then(|rect| theme::is_taskbar_dark(hwnd, rect))
        })
        .unwrap_or_else(theme::is_dark_mode);
    let changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.is_dark != new_dark {
                s.is_dark = new_dark;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        render_layered();
    }
}

fn check_language_change() {
    if update_language_change() {
        render_layered();
    }
}

fn update_display() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    // Don't overwrite error text with stale cached data
    if !s.last_poll_ok {
        return;
    }

    refresh_usage_texts(s);
}

fn suppress_tray_reposition_for(duration: Duration) {
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *until = Some(Instant::now() + duration);
}

fn tray_reposition_is_suppressed() -> bool {
    let now = Instant::now();
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    match *until {
        Some(deadline) if now < deadline => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

fn position_at_taskbar() {
    refresh_dpi();
    // Drop the app-state lock before any Win32 call that may synchronously
    // re-enter our window procedure.
    let (hwnd, embedded, tray_offset, taskbar_hwnd) = {
        let state = lock_state();
        let s = match state.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Don't fight the user's drag
        if s.dragging {
            return;
        }

        let taskbar_hwnd = match s.taskbar_hwnd {
            Some(h) => h,
            None => {
                diagnose::log("position_at_taskbar skipped: no taskbar handle");
                return;
            }
        };

        (s.hwnd.to_hwnd(), s.embedded, s.tray_offset, taskbar_hwnd)
    };

    let taskbar_rect = match native_interop::get_taskbar_rect(taskbar_hwnd) {
        Some(r) => r,
        None => {
            diagnose::log("position_at_taskbar skipped: unable to query taskbar rect");
            return;
        }
    };

    let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
    let mut tray_left = taskbar_rect.right;
    let anchor_top = taskbar_rect.top;
    let anchor_height = taskbar_height;

    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }

    let widget_width = total_widget_width();
    let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
    let tray_offset = tray_offset.clamp(0, max_offset);
    let offset_changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.tray_offset != tray_offset {
                s.tray_offset = tray_offset;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if offset_changed {
        save_state_settings();
    }

    let widget_height = sc(WIDGET_HEIGHT);
    let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
    if embedded {
        // Child window: coordinates relative to parent (taskbar)
        let x = tray_left - taskbar_rect.left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y - taskbar_rect.top, widget_width, widget_height);
        diagnose::log(format!(
            "positioned embedded widget at x={x} y={} w={widget_width} h={widget_height}",
            y - taskbar_rect.top
        ));
    } else {
        // Topmost popup: screen coordinates
        let x = tray_left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y, widget_width, widget_height);
        diagnose::log(format!(
            "positioned fallback widget at x={x} y={y} w={widget_width} h={widget_height}"
        ));
    }
}

fn compute_anchor_y(anchor_top: i32, anchor_height: i32, widget_height: i32) -> i32 {
    let anchor_bottom = anchor_top + anchor_height;
    (anchor_bottom - widget_height).max(anchor_top)
}

/// WinEvent callback for tray icon location changes
unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    static LAST_REPOSITION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    let is_tray = {
        let state = lock_state();
        state
            .as_ref()
            .and_then(|s| s.tray_notify_hwnd)
            .map(|h| h == hwnd)
            .unwrap_or(false)
    };

    if is_tray {
        if tray_reposition_is_suppressed() {
            return;
        }

        let should_reposition = {
            let mut last = LAST_REPOSITION.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            if last
                .map(|t| now.duration_since(t).as_millis() > 500)
                .unwrap_or(true)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        };
        if should_reposition {
            position_at_taskbar();
            render_layered();
        }
    }
}

/// Main window procedure
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // For non-embedded fallback, paint normally
            let embedded = {
                let state = lock_state();
                state.as_ref().map(|s| s.embedded).unwrap_or(false)
            };
            if embedded {
                // Layered windows don't use WM_PAINT; just validate the region
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            } else {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc, hwnd);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DISPLAYCHANGE | WM_DPICHANGED_MSG | WM_SETTINGCHANGE => {
            if msg == WM_DPICHANGED_MSG {
                let new_dpi = (wparam.0 & 0xFFFF) as u32;
                CURRENT_DPI.store(new_dpi, Ordering::Relaxed);
            }
            if msg == WM_SETTINGCHANGE {
                check_theme_change();
                check_language_change();
            }
            refresh_dpi();
            position_at_taskbar();
            render_layered();
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            match timer_id {
                TIMER_POLL => {
                    let auth_watch = {
                        let state = lock_state();
                        state.as_ref().map(|s| {
                            (
                                s.auth_error_paused_polling,
                                s.auth_watch_mode,
                                s.auth_watch_snapshot.clone(),
                            )
                        })
                    };
                    match auth_watch {
                        Some((true, watch_mode, previous_snapshot)) => {
                            let current_snapshot = poller::credential_watch_snapshot(watch_mode);
                            if current_snapshot != previous_snapshot {
                                let mut state = lock_state();
                                if let Some(s) = state.as_mut() {
                                    if s.auth_error_paused_polling
                                        && s.auth_watch_mode == watch_mode
                                    {
                                        s.auth_watch_snapshot = current_snapshot;
                                    }
                                }
                                drop(state);
                                let sh = SendHwnd::from_hwnd(hwnd);
                                std::thread::spawn(move || {
                                    do_poll(sh);
                                });
                            }
                        }
                        Some((false, _, _)) => {
                            let sh = SendHwnd::from_hwnd(hwnd);
                            std::thread::spawn(move || {
                                do_poll(sh);
                            });
                        }
                        None => {}
                    }
                }
                TIMER_COUNTDOWN => {
                    update_display();
                    render_layered();
                    schedule_countdown_timer();
                }
                TIMER_RESET_POLL => {
                    let should_poll = {
                        let state = lock_state();
                        state
                            .as_ref()
                            .map(|s| !s.auth_error_paused_polling)
                            .unwrap_or(false)
                    };
                    if should_poll {
                        let sh = SendHwnd::from_hwnd(hwnd);
                        std::thread::spawn(move || {
                            do_poll(sh);
                        });
                    }
                }
                TIMER_ICON_ANIM => {
                    ICON_ANIM_FRAME.fetch_add(1, Ordering::Relaxed);
                    render_layered();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            check_theme_change();
            check_language_change();
            render_layered();
            schedule_countdown_timer();
            suppress_tray_reposition_for(Duration::from_millis(
                TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS,
            ));
            sync_tray_icons(hwnd);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            if cursor_is_on_drag_handle(hwnd) {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
            let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if !is_drag_handle_point(client_x, client_y) {
                return LRESULT(0);
            }

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                s.dragging = true;
                s.drag_start_mouse_x = pt.x;
                s.drag_start_client_x = client_x;
                s.drag_start_offset = s.tray_offset;
            }
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let move_target = {
                    let mut state = lock_state();
                    let s = match state.as_mut() {
                        Some(s) => s,
                        None => return LRESULT(0),
                    };

                    // Moving mouse left = positive delta = larger offset (further left)
                    let delta = s.drag_start_mouse_x - pt.x;
                    let mut new_offset = s.drag_start_offset + delta;

                    // Clamp: offset >= 0 (can't go right of default)
                    if new_offset < 0 {
                        new_offset = 0;
                    }

                    let taskbar_hwnd = s.taskbar_hwnd;
                    let embedded = s.embedded;
                    let hwnd_val = s.hwnd.to_hwnd();

                    // Clamp: don't go past left edge of taskbar
                    if let Some(taskbar_hwnd) = taskbar_hwnd {
                        if let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd) {
                            let mut tray_left = taskbar_rect.right;
                            if let Some(tray_hwnd) =
                                native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd")
                            {
                                if let Some(tray_rect) =
                                    native_interop::get_window_rect_safe(tray_hwnd)
                                {
                                    tray_left = tray_rect.left;
                                }
                            }
                            let widget_width = total_widget_width_for_state(s);
                            let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
                            if new_offset > max_offset {
                                new_offset = max_offset;
                            }

                            s.tray_offset = new_offset;

                            let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
                            let anchor_top = taskbar_rect.top;
                            let anchor_height = taskbar_height;
                            let widget_height = sc(WIDGET_HEIGHT);
                            let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
                            let x = if embedded {
                                tray_left - taskbar_rect.left - widget_width - new_offset
                            } else {
                                tray_left - widget_width - new_offset
                            };
                            Some((
                                hwnd_val,
                                embedded,
                                x,
                                y,
                                taskbar_rect.top,
                                widget_width,
                                widget_height,
                            ))
                        } else {
                            s.tray_offset = new_offset;
                            None
                        }
                    } else {
                        s.tray_offset = new_offset;
                        None
                    }
                };

                if let Some((hwnd_val, embedded, x, y, taskbar_top, widget_width, widget_height)) =
                    move_target
                {
                    if embedded {
                        native_interop::move_window(
                            hwnd_val,
                            x,
                            y - taskbar_top,
                            widget_width,
                            widget_height,
                        );
                    } else {
                        native_interop::move_window(hwnd_val, x, y, widget_width, widget_height);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let drag_result = {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    if s.dragging {
                        s.dragging = false;
                        Some((s.taskbar_index, s.drag_start_client_x))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((current_taskbar_index, drag_start_client_x)) = drag_result {
                let _ = ReleaseCapture();
                if let Some((target_index, target_taskbar)) = taskbar_at_point(pt) {
                    if target_index != current_taskbar_index {
                        let new_offset = offset_for_drop_point(
                            target_taskbar.hwnd,
                            target_taskbar.rect,
                            pt,
                            drag_start_client_x,
                        );
                        {
                            let mut state = lock_state();
                            if let Some(s) = state.as_mut() {
                                s.tray_offset = new_offset;
                            }
                        }
                        if attach_to_taskbar(hwnd, target_index) {
                            position_at_taskbar();
                            render_layered();
                        }
                    }
                }
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 as u16;
            match id {
                1 => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.session_text = "...".to_string();
                            s.weekly_text = "...".to_string();
                            s.codex_session_text = "...".to_string();
                            s.codex_weekly_text = "...".to_string();
                            s.force_notify_auth_error = true;
                        }
                    }
                    render_layered();
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                2 => {
                    let hook = {
                        let state = lock_state();
                        state.as_ref().and_then(|s| s.win_event_hook)
                    };
                    if let Some(h) = hook {
                        native_interop::unhook_win_event(h);
                    }
                    PostQuitMessage(0);
                }
                IDM_RESET_POSITION => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.tray_offset = 0;
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                }
                IDM_START_WITH_WINDOWS => {
                    set_startup_enabled(!is_startup_enabled());
                }
                IDM_ABOUT => {
                    let strings = {
                        let state = lock_state();
                        state
                            .as_ref()
                            .map(|s| s.language.strings())
                            .unwrap_or(LanguageId::English.strings())
                    };
                    let title_wide = native_interop::wide_str(strings.about);
                    let message_wide = native_interop::wide_str(strings.about_message);
                    let _ = MessageBoxW(
                        hwnd,
                        PCWSTR::from_raw(message_wide.as_ptr()),
                        PCWSTR::from_raw(title_wide.as_ptr()),
                        MB_OK | MB_ICONINFORMATION,
                    );
                }
                id if id >= IDM_MONITOR_BASE && id < IDM_MONITOR_BASE + IDM_MONITOR_MAX_COUNT => {
                    let target_index = (id - IDM_MONITOR_BASE) as usize;
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.tray_offset = 0;
                        }
                    }
                    if attach_to_taskbar(hwnd, target_index) {
                        save_state_settings();
                        position_at_taskbar();
                        render_layered();
                    }
                }
                IDM_FREQ_1MIN | IDM_FREQ_5MIN | IDM_FREQ_15MIN | IDM_FREQ_1HOUR => {
                    let new_interval = match id {
                        IDM_FREQ_1MIN => POLL_1_MIN,
                        IDM_FREQ_5MIN => POLL_5_MIN,
                        IDM_FREQ_15MIN => POLL_15_MIN,
                        IDM_FREQ_1HOUR => POLL_1_HOUR,
                        _ => POLL_15_MIN,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.poll_interval_ms = new_interval;
                        }
                    }
                    save_state_settings();
                    // Reset the poll timer with the new interval
                    SetTimer(hwnd, TIMER_POLL, new_interval, None);
                }
                IDM_MODEL_CLAUDE_CODE | IDM_MODEL_CODEX | IDM_MODEL_ANTIGRAVITY => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            match id {
                                IDM_MODEL_CLAUDE_CODE => {
                                    if s.show_codex || s.show_antigravity || !s.show_claude_code {
                                        s.show_claude_code = !s.show_claude_code;
                                    }
                                }
                                IDM_MODEL_CODEX => {
                                    if s.show_claude_code || s.show_antigravity || !s.show_codex {
                                        s.show_codex = !s.show_codex;
                                    }
                                }
                                IDM_MODEL_ANTIGRAVITY => {
                                    if s.show_claude_code || s.show_codex || !s.show_antigravity {
                                        s.show_antigravity = !s.show_antigravity;
                                    }
                                }
                                _ => {}
                            }
                            s.session_text = "...".to_string();
                            s.weekly_text = "...".to_string();
                            s.codex_session_text = "...".to_string();
                            s.codex_weekly_text = "...".to_string();
                            s.antigravity_session_text = "...".to_string();
                            s.antigravity_weekly_text = "...".to_string();
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                    render_layered();
                    sync_tray_icons(hwnd);
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                IDM_LANG_SYSTEM
                | IDM_LANG_ENGLISH
                | IDM_LANG_DUTCH
                | IDM_LANG_SPANISH
                | IDM_LANG_FRENCH
                | IDM_LANG_GERMAN
                | IDM_LANG_JAPANESE
                | IDM_LANG_KOREAN
                | IDM_LANG_TRADITIONAL_CHINESE
                | IDM_LANG_RUSSIAN
                | IDM_LANG_PORTUGUESE_BRAZIL => {
                    let language_override = match id {
                        IDM_LANG_SYSTEM => None,
                        IDM_LANG_ENGLISH => Some(LanguageId::English),
                        IDM_LANG_DUTCH => Some(LanguageId::Dutch),
                        IDM_LANG_SPANISH => Some(LanguageId::Spanish),
                        IDM_LANG_FRENCH => Some(LanguageId::French),
                        IDM_LANG_GERMAN => Some(LanguageId::German),
                        IDM_LANG_JAPANESE => Some(LanguageId::Japanese),
                        IDM_LANG_KOREAN => Some(LanguageId::Korean),
                        IDM_LANG_TRADITIONAL_CHINESE => Some(LanguageId::TraditionalChinese),
                        IDM_LANG_RUSSIAN => Some(LanguageId::Russian),
                        IDM_LANG_PORTUGUESE_BRAZIL => Some(LanguageId::PortugueseBrazil),
                        _ => None,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            apply_language_to_state(s, language_override);
                        }
                    }
                    save_state_settings();
                    render_layered();
                }
                id if id == tray_icon::IDM_TOGGLE_WIDGET => {
                    toggle_widget_visibility(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => {
                    toggle_widget_visibility(hwnd);
                }
                tray_icon::TrayAction::ShowContextMenu => {
                    show_context_menu(hwnd);
                }
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let hook = {
                let state = lock_state();
                state.as_ref().and_then(|s| s.win_event_hook)
            };
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            tray_icon::remove_all(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let (
            current_interval,
            strings,
            language_override,
            widget_visible,
            show_claude_code,
            show_codex,
            show_antigravity,
            current_taskbar_index,
            session_percent,
            weekly_percent,
            session_resets_at,
            weekly_resets_at,
        ) = {
            let state = lock_state();
            match state.as_ref() {
                Some(s) => {
                    let claude_code = s.data.as_ref().and_then(|d| d.claude_code.as_ref());
                    (
                        s.poll_interval_ms,
                        s.language.strings(),
                        s.language_override,
                        s.widget_visible,
                        s.show_claude_code,
                        s.show_codex,
                        s.show_antigravity,
                        s.taskbar_index,
                        s.session_percent,
                        s.weekly_percent,
                        claude_code.and_then(|c| c.session.resets_at),
                        claude_code.and_then(|c| c.weekly.resets_at),
                    )
                }
                None => (
                    POLL_15_MIN,
                    LanguageId::English.strings(),
                    None,
                    true,
                    true,
                    false,
                    false,
                    0,
                    0.0,
                    0.0,
                    None,
                    None,
                ),
            }
        };

        let menu = CreatePopupMenu().unwrap();

        // Two grayed-out, non-clickable summary rows at the top of the menu.
        let session_line_str = native_interop::wide_str(
            &strings
                .session_limit_line
                .replace("{pct}", &format!("{session_percent:02.0}"))
                .replace("{time}", &format_hm_remaining(session_resets_at)),
        );
        let _ = AppendMenuW(
            menu,
            MF_GRAYED,
            0,
            PCWSTR::from_raw(session_line_str.as_ptr()),
        );

        let weekly_line_str = native_interop::wide_str(
            &strings
                .weekly_limit_line
                .replace("{pct}", &format!("{weekly_percent:02.0}"))
                .replace("{time}", &format_days_remaining(weekly_resets_at, strings)),
        );
        let _ = AppendMenuW(
            menu,
            MF_GRAYED,
            0,
            PCWSTR::from_raw(weekly_line_str.as_ptr()),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let refresh_str = native_interop::wide_str(strings.refresh);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            1,
            PCWSTR::from_raw(refresh_str.as_ptr()),
        );

        // Update Frequency submenu
        let freq_menu = CreatePopupMenu().unwrap();
        let freq_items: [(u16, u32, &str); 4] = [
            (IDM_FREQ_1MIN, POLL_1_MIN, strings.one_minute),
            (IDM_FREQ_5MIN, POLL_5_MIN, strings.five_minutes),
            (IDM_FREQ_15MIN, POLL_15_MIN, strings.fifteen_minutes),
            (IDM_FREQ_1HOUR, POLL_1_HOUR, strings.one_hour),
        ];
        for (id, interval, label) in freq_items {
            let label_str = native_interop::wide_str(label);
            let flags = if interval == current_interval {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                freq_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let freq_label = native_interop::wide_str(strings.update_frequency);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            freq_menu.0 as usize,
            PCWSTR::from_raw(freq_label.as_ptr()),
        );

        // Models submenu
        let models_menu = CreatePopupMenu().unwrap();
        let claude_model = native_interop::wide_str(strings.claude_code_model);
        let claude_flags = if show_claude_code {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            models_menu,
            claude_flags,
            IDM_MODEL_CLAUDE_CODE as usize,
            PCWSTR::from_raw(claude_model.as_ptr()),
        );

        let codex_model = native_interop::wide_str(strings.codex_model);
        let codex_flags = if show_codex {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            models_menu,
            codex_flags,
            IDM_MODEL_CODEX as usize,
            PCWSTR::from_raw(codex_model.as_ptr()),
        );

        let antigravity_model = native_interop::wide_str(strings.antigravity_model);
        let antigravity_flags = if show_antigravity {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            models_menu,
            antigravity_flags,
            IDM_MODEL_ANTIGRAVITY as usize,
            PCWSTR::from_raw(antigravity_model.as_ptr()),
        );

        let models_label = native_interop::wide_str(strings.models);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            models_menu.0 as usize,
            PCWSTR::from_raw(models_label.as_ptr()),
        );

        // Settings submenu
        let settings_menu = CreatePopupMenu().unwrap();

        let startup_str = native_interop::wide_str(strings.start_with_windows);
        let startup_flags = if is_startup_enabled() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            startup_flags,
            IDM_START_WITH_WINDOWS as usize,
            PCWSTR::from_raw(startup_str.as_ptr()),
        );

        let reset_pos_str = native_interop::wide_str(strings.reset_position);
        let _ = AppendMenuW(
            settings_menu,
            MENU_ITEM_FLAGS(0),
            IDM_RESET_POSITION as usize,
            PCWSTR::from_raw(reset_pos_str.as_ptr()),
        );

        let taskbars = native_interop::find_taskbars();
        if taskbars.len() > 1 {
            let monitor_menu = CreatePopupMenu().unwrap();
            let monitor_labels: Vec<Vec<u16>> = (0..taskbars.len())
                .map(|i| native_interop::wide_str(&format!("Monitor {}", i + 1)))
                .collect();
            for (i, label) in monitor_labels.iter().enumerate() {
                if i as u16 >= IDM_MONITOR_MAX_COUNT {
                    break;
                }
                let flags = if i == current_taskbar_index {
                    MF_CHECKED
                } else {
                    MENU_ITEM_FLAGS(0)
                };
                let _ = AppendMenuW(
                    monitor_menu,
                    flags,
                    (IDM_MONITOR_BASE + i as u16) as usize,
                    PCWSTR::from_raw(label.as_ptr()),
                );
            }

            let monitor_label = native_interop::wide_str(strings.monitor);
            let _ = AppendMenuW(
                settings_menu,
                MF_POPUP,
                monitor_menu.0 as usize,
                PCWSTR::from_raw(monitor_label.as_ptr()),
            );
        }

        let language_menu = CreatePopupMenu().unwrap();
        let system_label = native_interop::wide_str(strings.system_default);
        let system_flags = if language_override.is_none() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            language_menu,
            system_flags,
            IDM_LANG_SYSTEM as usize,
            PCWSTR::from_raw(system_label.as_ptr()),
        );

        for language in LanguageId::ALL {
            let id = match language {
                LanguageId::English => IDM_LANG_ENGLISH,
                LanguageId::Dutch => IDM_LANG_DUTCH,
                LanguageId::Spanish => IDM_LANG_SPANISH,
                LanguageId::French => IDM_LANG_FRENCH,
                LanguageId::German => IDM_LANG_GERMAN,
                LanguageId::Japanese => IDM_LANG_JAPANESE,
                LanguageId::Korean => IDM_LANG_KOREAN,
                LanguageId::TraditionalChinese => IDM_LANG_TRADITIONAL_CHINESE,
                LanguageId::Russian => IDM_LANG_RUSSIAN,
                LanguageId::PortugueseBrazil => IDM_LANG_PORTUGUESE_BRAZIL,
            };
            let label_str = native_interop::wide_str(language.native_name());
            let flags = if language_override == Some(language) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                language_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let language_label = native_interop::wide_str(strings.language);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            language_menu.0 as usize,
            PCWSTR::from_raw(language_label.as_ptr()),
        );

        let settings_label = native_interop::wide_str(strings.settings);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            settings_menu.0 as usize,
            PCWSTR::from_raw(settings_label.as_ptr()),
        );

        let widget_label = native_interop::wide_str(strings.show_widget);
        let widget_flags = if widget_visible {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            menu,
            widget_flags,
            tray_icon::IDM_TOGGLE_WIDGET as usize,
            PCWSTR::from_raw(widget_label.as_ptr()),
        );

        let about_str = native_interop::wide_str(strings.about);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            IDM_ABOUT as usize,
            PCWSTR::from_raw(about_str.as_ptr()),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let exit_str = native_interop::wide_str(strings.exit);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            2,
            PCWSTR::from_raw(exit_str.as_ptr()),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

/// Paint for non-embedded fallback (normal WM_PAINT path)
fn paint(hdc: HDC, hwnd: HWND) {
    let (
        is_dark,
        strings,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (
                s.is_dark,
                s.language.strings(),
                s.session_percent,
                s.session_text.clone(),
                s.weekly_percent,
                s.weekly_text.clone(),
                s.codex_session_percent,
                s.codex_session_text.clone(),
                s.codex_weekly_percent,
                s.codex_weekly_text.clone(),
                s.antigravity_session_percent,
                s.antigravity_session_text.clone(),
                s.antigravity_weekly_percent,
                s.antigravity_weekly_text.clone(),
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
            ),
            None => return,
        }
    };

    let accent = claude_accent_color(is_dark);
    let codex_accent = codex_accent_color(is_dark);
    let antigravity_accent = antigravity_accent_color();
    let track = if is_dark {
        Color::from_hex("#444444")
    } else {
        Color::from_hex("#D6D6D6")
    };
    let text_color = if is_dark {
        Color::from_hex("#888888")
    } else {
        Color::from_hex("#404040")
    };
    let bg_color = if is_dark {
        Color::from_hex("#1C1C1C")
    } else {
        Color::from_hex("#F3F3F3")
    };

    unsafe {
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;

        if width <= 0 || height <= 0 {
            return;
        }

        let mem_dc = CreateCompatibleDC(hdc);
        let mem_bmp = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, mem_bmp);

        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            &bg_color,
            &text_color,
            &accent,
            &track,
            strings,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            &codex_accent,
            &antigravity_accent,
        );

        let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(mem_bmp);
        let _ = DeleteDC(mem_dc);
    }
}

fn draw_row(
    hdc: HDC,
    x: i32,
    y: i32,
    is_dark: bool,
    text_color: &Color,
    label: &str,
    claude_percent: f64,
    claude_text: &str,
    codex_percent: f64,
    codex_text: &str,
    antigravity_percent: f64,
    antigravity_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    claude_accent: &Color,
    codex_accent: &Color,
    antigravity_accent: &Color,
    track: &Color,
) {
    let seg_h = sc(SEGMENT_H);
    let active_models = active_model_count(show_claude_code, show_codex, show_antigravity);
    let segment_count = row_bar_segment_count(active_models);
    let use_model_text_colors = active_models > 1;
    let claude_value_color = if use_model_text_colors {
        claude_usage_text_color(is_dark)
    } else {
        *text_color
    };
    let codex_value_color = if use_model_text_colors {
        codex_usage_text_color(is_dark)
    } else {
        *text_color
    };
    let antigravity_value_color = if use_model_text_colors {
        antigravity_usage_text_color(is_dark)
    } else {
        *text_color
    };

    {
        let label_fg = claude_usage_text_color(is_dark);
        let label_width = draw_chip(hdc, x, y, seg_h, label, track, &label_fg, true, false);

        let mut model_x = x + label_width + sc(LABEL_RIGHT_MARGIN);
        if show_claude_code {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                claude_percent,
                is_dark,
                claude_text,
                claude_accent,
                track,
                &claude_value_color,
            );
            model_x += model_usage_width(segment_count) + sc(MODEL_RIGHT_MARGIN);
        }
        if show_codex {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                codex_percent,
                is_dark,
                codex_text,
                codex_accent,
                track,
                &codex_value_color,
            );
            model_x += model_usage_width(segment_count) + sc(MODEL_RIGHT_MARGIN);
        }
        if show_antigravity {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                antigravity_percent,
                is_dark,
                antigravity_text,
                antigravity_accent,
                track,
                &antigravity_value_color,
            );
        }
    }
}

fn model_usage_width(segment_count: i32) -> i32 {
    (sc(SEGMENT_W) + sc(SEGMENT_GAP)) * segment_count - sc(SEGMENT_GAP)
        + sc(BAR_RIGHT_MARGIN)
        + sc(TEXT_WIDTH)
}

fn lerp_color(a: &Color, b: &Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::new(lerp(a.r, b.r), lerp(a.g, b.g), lerp(a.b, b.b))
}

fn draw_usage_bar(
    hdc: HDC,
    bar_x: i32,
    y: i32,
    segment_count: i32,
    percent: f64,
    is_dark: bool,
    text: &str,
    accent: &Color,
    track: &Color,
    text_color: &Color,
) {
    let seg_w = sc(SEGMENT_W);
    let seg_h = sc(SEGMENT_H);
    let seg_gap = sc(SEGMENT_GAP);
    let corner_r = sc(CORNER_RADIUS);

    {
        let percent_clamped = percent.clamp(0.0, 100.0);
        let segment_percent = 100.0 / segment_count as f64;

        for i in 0..segment_count {
            let seg_x = bar_x + i * (seg_w + seg_gap);
            let seg_start = (i as f64) * segment_percent;
            let seg_end = seg_start + segment_percent;

            let seg_rect = RECT {
                left: seg_x,
                top: y,
                right: seg_x + seg_w,
                bottom: y + seg_h,
            };

            if percent_clamped >= seg_end {
                draw_rounded_rect(hdc, &seg_rect, accent, corner_r);
            } else if percent_clamped <= seg_start {
                draw_rounded_rect(hdc, &seg_rect, track, corner_r);
            } else {
                let fraction = (percent_clamped - seg_start) / segment_percent;
                let step = (fraction * 10.0).floor() / 10.0;
                let blended = lerp_color(track, accent, step);
                draw_rounded_rect(hdc, &seg_rect, &blended, corner_r);
            }
        }

        let text_x = bar_x + segment_count * (seg_w + seg_gap) - seg_gap + sc(BAR_RIGHT_MARGIN);

        let (value_part, countdown_part) = match text.split_once('\u{b7}') {
            Some((v, cd)) => (v.trim().to_string(), Some(cd.trim().to_string())),
            None => (text.to_string(), None),
        };

        // Uses the ambient font already selected on hdc (the same one the
        // row label chip uses) so both chips' text match in size.
        // Squared on the left (flush against the bar), rounded on the right —
        // the mirror of the row label chip, which sits flush on its right.
        let value_width = draw_chip(hdc, text_x, y, seg_h, &value_part, track, text_color, false, true);

        if let Some(countdown) = countdown_part {
            let countdown_gap = sc(5);
            // Nudges the countdown color away from the track: lighter in
            // dark mode, darker in light mode — always toward the bg end
            // of the contrast scale, never toward it.
            let countdown_color = if is_dark {
                lerp_color(track, &Color::from_hex("#FFFFFF"), 0.2)
            } else {
                lerp_color(track, &Color::from_hex("#000000"), 0.2)
            };
            let countdown_x = text_x + value_width + countdown_gap;
            draw_countdown_text(hdc, countdown_x, y, seg_h, &countdown, &countdown_color);
        }
    }
}

/// Draws the countdown ("1h 19m", "1d") as plain text — no chip pill —
/// right after the percentage chip.
fn draw_countdown_text(hdc: HDC, x: i32, y: i32, height: i32, text: &str, fg: &Color) {
    unsafe {
        let mut text_wide: Vec<u16> = text.encode_utf16().collect();
        let mut text_size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &text_wide, &mut text_size);
        let mut text_rect = RECT {
            left: x,
            top: y,
            right: x + text_size.cx,
            bottom: y + height,
        };
        let _ = SetTextColor(hdc, COLORREF(fg.to_colorref()));
        let _ = DrawTextW(
            hdc,
            &mut text_wide,
            &mut text_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// Draws a pill with centered text (used for the countdown and the row
/// label). `round_left`/`round_right` control whether each side's corners
/// are rounded — a chip flush against the bar has its adjoining side
/// squared off so it reads as one continuous shape. Returns the chip's width.
fn draw_chip(
    hdc: HDC,
    x: i32,
    y: i32,
    chip_height: i32,
    text: &str,
    bg: &Color,
    fg: &Color,
    round_left: bool,
    round_right: bool,
) -> i32 {
    unsafe {
        let mut text_wide: Vec<u16> = text.encode_utf16().collect();
        let mut text_size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &text_wide, &mut text_size);

        let chip_pad_x = sc(5);
        let chip_rect = RECT {
            left: x,
            top: y,
            right: x + text_size.cx + chip_pad_x * 2,
            bottom: y + chip_height,
        };

        draw_chip_shape(hdc, &chip_rect, bg, chip_height / 2, round_left, round_right);

        let mut text_rect = chip_rect;
        text_rect.bottom -= sc(1);
        let _ = SetTextColor(hdc, COLORREF(fg.to_colorref()));
        let _ = DrawTextW(
            hdc,
            &mut text_wide,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );

        chip_rect.right - chip_rect.left
    }
}

fn draw_chip_shape(
    hdc: HDC,
    rect: &RECT,
    color: &Color,
    radius: i32,
    round_left: bool,
    round_right: bool,
) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let rgn = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right + 1,
            rect.bottom + 1,
            radius * 2,
            radius * 2,
        );
        if !round_right {
            let square_half =
                CreateRectRgn(rect.left + radius, rect.top, rect.right + 1, rect.bottom + 1);
            let _ = CombineRgn(rgn, rgn, square_half, RGN_OR);
            let _ = DeleteObject(square_half);
        }
        if !round_left {
            let square_half =
                CreateRectRgn(rect.left, rect.top, rect.right + 1 - radius, rect.bottom + 1);
            let _ = CombineRgn(rgn, rgn, square_half, RGN_OR);
            let _ = DeleteObject(square_half);
        }
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(rgn);
        let _ = DeleteObject(brush);
    }
}

/// One axis-aligned block of the walking-crab pixel-art sprite, in the
/// original 58x30 viewBox coordinate space: (x0, y0, x1, y1, is_black).
struct WalkRect(f32, f32, f32, f32, bool);

const WALK_VIEWBOX_W: f32 = 58.0;
const WALK_VIEWBOX_H: f32 = 30.0;

// Frames per TIMER_ICON_ANIM tick. Full of energy (0% used) walks fastest;
// energy drains as usage climbs toward 100%.
const WALK_MIN_SPEED: f64 = 0.08;
const WALK_MAX_SPEED: f64 = 1.3;
const SLEEP_ANIM_SPEED: f64 = 0.12;

/// 14-frame walk cycle, transcribed from the provided sprite frames.
const WALK_FRAMES: [[WalkRect; 9]; 14] = [
    [
        WalkRect(6.0, 0.0, 42.0, 24.0, false),
        WalkRect(9.0, 21.0, 12.0, 30.0, false),
        WalkRect(15.0, 19.0, 18.0, 28.0, false),
        WalkRect(30.0, 19.0, 33.0, 28.0, false),
        WalkRect(36.0, 21.0, 39.0, 30.0, false),
        WalkRect(0.0, 15.0, 7.5, 21.0, false),
        WalkRect(40.5, 15.0, 48.0, 21.0, false),
        WalkRect(21.0, 6.0, 24.0, 12.0, true),
        WalkRect(33.0, 6.0, 36.0, 12.0, true),
    ],
    [
        WalkRect(8.0, 0.0, 44.0, 24.0, false),
        WalkRect(11.0, 21.0, 14.0, 30.0, false),
        WalkRect(17.0, 20.0, 20.0, 29.0, false),
        WalkRect(32.0, 20.0, 35.0, 29.0, false),
        WalkRect(38.0, 21.0, 41.0, 30.0, false),
        WalkRect(2.0, 15.0, 9.5, 21.0, false),
        WalkRect(42.5, 15.0, 50.0, 21.0, false),
        WalkRect(23.0, 6.0, 26.0, 12.0, true),
        WalkRect(35.0, 6.0, 38.0, 12.0, true),
    ],
    [
        WalkRect(10.0, 0.0, 46.0, 24.0, false),
        WalkRect(13.0, 20.0, 16.0, 29.0, false),
        WalkRect(19.0, 21.0, 22.0, 30.0, false),
        WalkRect(34.0, 21.0, 37.0, 30.0, false),
        WalkRect(40.0, 20.0, 43.0, 29.0, false),
        WalkRect(4.0, 15.0, 11.5, 21.0, false),
        WalkRect(44.5, 15.0, 52.0, 21.0, false),
        WalkRect(25.0, 6.0, 28.0, 12.0, true),
        WalkRect(37.0, 6.0, 40.0, 12.0, true),
    ],
    [
        WalkRect(12.0, 0.0, 48.0, 24.0, false),
        WalkRect(15.0, 19.0, 18.0, 28.0, false),
        WalkRect(21.0, 21.0, 24.0, 30.0, false),
        WalkRect(36.0, 21.0, 39.0, 30.0, false),
        WalkRect(42.0, 19.0, 45.0, 28.0, false),
        WalkRect(6.0, 15.0, 13.5, 21.0, false),
        WalkRect(46.5, 15.0, 54.0, 21.0, false),
        WalkRect(27.0, 6.0, 30.0, 12.0, true),
        WalkRect(39.0, 6.0, 42.0, 12.0, true),
    ],
    [
        WalkRect(14.0, 0.0, 50.0, 24.0, false),
        WalkRect(17.0, 20.0, 20.0, 29.0, false),
        WalkRect(23.0, 21.0, 26.0, 30.0, false),
        WalkRect(38.0, 21.0, 41.0, 30.0, false),
        WalkRect(44.0, 20.0, 47.0, 29.0, false),
        WalkRect(8.0, 15.0, 15.5, 21.0, false),
        WalkRect(48.5, 15.0, 56.0, 21.0, false),
        WalkRect(29.0, 6.0, 32.0, 12.0, true),
        WalkRect(41.0, 6.0, 44.0, 12.0, true),
    ],
    [
        WalkRect(16.0, 0.0, 52.0, 24.0, false),
        WalkRect(19.0, 21.0, 22.0, 30.0, false),
        WalkRect(25.0, 20.0, 28.0, 29.0, false),
        WalkRect(40.0, 20.0, 43.0, 29.0, false),
        WalkRect(46.0, 21.0, 49.0, 30.0, false),
        WalkRect(10.0, 15.0, 17.5, 21.0, false),
        WalkRect(50.5, 15.0, 58.0, 21.0, false),
        WalkRect(31.0, 6.0, 34.0, 12.0, true),
        WalkRect(43.0, 6.0, 46.0, 12.0, true),
    ],
    [
        WalkRect(16.0, 0.0, 52.0, 24.0, false),
        WalkRect(19.0, 21.0, 22.0, 30.0, false),
        WalkRect(25.0, 19.0, 28.0, 28.0, false),
        WalkRect(40.0, 19.0, 43.0, 28.0, false),
        WalkRect(46.0, 21.0, 49.0, 30.0, false),
        WalkRect(10.0, 15.0, 17.5, 21.0, false),
        WalkRect(50.5, 15.0, 58.0, 21.0, false),
        WalkRect(26.0, 6.0, 29.0, 12.0, true),
        WalkRect(38.0, 6.0, 41.0, 12.0, true),
    ],
    [
        WalkRect(16.0, 0.0, 52.0, 24.0, false),
        WalkRect(19.0, 21.0, 22.0, 30.0, false),
        WalkRect(25.0, 20.0, 28.0, 29.0, false),
        WalkRect(40.0, 20.0, 43.0, 29.0, false),
        WalkRect(46.0, 21.0, 49.0, 30.0, false),
        WalkRect(10.0, 15.0, 17.5, 21.0, false),
        WalkRect(50.5, 15.0, 58.0, 21.0, false),
        WalkRect(22.0, 6.0, 25.0, 12.0, true),
        WalkRect(34.0, 6.0, 37.0, 12.0, true),
    ],
    [
        WalkRect(14.0, 0.0, 50.0, 24.0, false),
        WalkRect(17.0, 20.0, 20.0, 29.0, false),
        WalkRect(23.0, 21.0, 26.0, 30.0, false),
        WalkRect(38.0, 21.0, 41.0, 30.0, false),
        WalkRect(44.0, 20.0, 47.0, 29.0, false),
        WalkRect(8.0, 15.0, 15.5, 21.0, false),
        WalkRect(48.5, 15.0, 56.0, 21.0, false),
        WalkRect(20.0, 6.0, 23.0, 12.0, true),
        WalkRect(32.0, 6.0, 35.0, 12.0, true),
    ],
    [
        WalkRect(12.0, 0.0, 48.0, 24.0, false),
        WalkRect(15.0, 19.0, 18.0, 28.0, false),
        WalkRect(21.0, 21.0, 24.0, 30.0, false),
        WalkRect(36.0, 21.0, 39.0, 30.0, false),
        WalkRect(42.0, 19.0, 45.0, 28.0, false),
        WalkRect(6.0, 15.0, 13.5, 21.0, false),
        WalkRect(46.5, 15.0, 54.0, 21.0, false),
        WalkRect(18.0, 6.0, 21.0, 12.0, true),
        WalkRect(30.0, 6.0, 33.0, 12.0, true),
    ],
    [
        WalkRect(10.0, 0.0, 46.0, 24.0, false),
        WalkRect(13.0, 20.0, 16.0, 29.0, false),
        WalkRect(19.0, 21.0, 22.0, 30.0, false),
        WalkRect(34.0, 21.0, 37.0, 30.0, false),
        WalkRect(40.0, 20.0, 43.0, 29.0, false),
        WalkRect(4.0, 15.0, 11.5, 21.0, false),
        WalkRect(44.5, 15.0, 52.0, 21.0, false),
        WalkRect(16.0, 6.0, 19.0, 12.0, true),
        WalkRect(28.0, 6.0, 31.0, 12.0, true),
    ],
    [
        WalkRect(8.0, 0.0, 44.0, 24.0, false),
        WalkRect(11.0, 21.0, 14.0, 30.0, false),
        WalkRect(17.0, 20.0, 20.0, 29.0, false),
        WalkRect(32.0, 20.0, 35.0, 29.0, false),
        WalkRect(38.0, 21.0, 41.0, 30.0, false),
        WalkRect(2.0, 15.0, 9.5, 21.0, false),
        WalkRect(42.5, 15.0, 50.0, 21.0, false),
        WalkRect(14.0, 6.0, 17.0, 12.0, true),
        WalkRect(26.0, 6.0, 29.0, 12.0, true),
    ],
    [
        WalkRect(6.0, 0.0, 42.0, 24.0, false),
        WalkRect(9.0, 21.0, 12.0, 30.0, false),
        WalkRect(15.0, 19.0, 18.0, 28.0, false),
        WalkRect(30.0, 19.0, 33.0, 28.0, false),
        WalkRect(36.0, 21.0, 39.0, 30.0, false),
        WalkRect(0.0, 15.0, 7.5, 21.0, false),
        WalkRect(40.5, 15.0, 48.0, 21.0, false),
        WalkRect(12.0, 6.0, 15.0, 12.0, true),
        WalkRect(24.0, 6.0, 27.0, 12.0, true),
    ],
    [
        WalkRect(6.0, 0.0, 42.0, 24.0, false),
        WalkRect(9.0, 21.0, 12.0, 30.0, false),
        WalkRect(15.0, 20.0, 18.0, 29.0, false),
        WalkRect(30.0, 20.0, 33.0, 29.0, false),
        WalkRect(36.0, 21.0, 39.0, 30.0, false),
        WalkRect(0.0, 15.0, 7.5, 21.0, false),
        WalkRect(40.5, 15.0, 48.0, 21.0, false),
        WalkRect(16.0, 6.0, 19.0, 12.0, true),
        WalkRect(28.0, 6.0, 31.0, 12.0, true),
    ],
];

/// 4-frame idle breathing loop shown once a usage window hits 100% — the
/// crab is out of energy (out of tokens) and curls up to rest.
const SLEEP_FRAMES: [[WalkRect; 5]; 4] = [
    [
        WalkRect(11.0, 6.0, 47.0, 30.0, false),
        WalkRect(5.0, 24.0, 12.5, 30.0, false),
        WalkRect(45.5, 24.0, 53.0, 30.0, false),
        WalkRect(19.5, 18.0, 25.5, 21.0, true),
        WalkRect(31.5, 18.0, 37.5, 21.0, true),
    ],
    [
        WalkRect(11.0, 7.0, 47.0, 30.0, false),
        WalkRect(5.0, 24.0, 12.5, 30.0, false),
        WalkRect(45.5, 24.0, 53.0, 30.0, false),
        WalkRect(19.5, 18.0, 25.5, 21.0, true),
        WalkRect(31.5, 18.0, 37.5, 21.0, true),
    ],
    [
        WalkRect(11.0, 8.0, 47.0, 30.0, false),
        WalkRect(5.0, 24.0, 12.5, 30.0, false),
        WalkRect(45.5, 24.0, 53.0, 30.0, false),
        WalkRect(19.5, 19.0, 25.5, 22.0, true),
        WalkRect(31.5, 19.0, 37.5, 22.0, true),
    ],
    [
        WalkRect(11.0, 7.0, 47.0, 30.0, false),
        WalkRect(5.0, 24.0, 12.5, 30.0, false),
        WalkRect(45.5, 24.0, 53.0, 30.0, false),
        WalkRect(19.5, 18.0, 25.5, 21.0, true),
        WalkRect(31.5, 18.0, 37.5, 21.0, true),
    ],
];

fn draw_icon_frame(hdc: HDC, x: i32, y: i32, w: i32, h: i32, rects: &[WalkRect]) {
    if w <= 0 || h <= 0 {
        return;
    }
    let orange = Color::from_hex("#D77757");
    let black = Color::from_hex("#000000");
    let scale_x = w as f32 / WALK_VIEWBOX_W;
    let scale_y = h as f32 / WALK_VIEWBOX_H;

    unsafe {
        for rect in rects {
            let r = RECT {
                left: x + (rect.0 * scale_x).round() as i32,
                top: y + (rect.1 * scale_y).round() as i32,
                right: x + (rect.2 * scale_x).round() as i32,
                bottom: y + (rect.3 * scale_y).round() as i32,
            };
            let color = if rect.4 { &black } else { &orange };
            let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
            FillRect(hdc, &r, brush);
            let _ = DeleteObject(brush);
        }
    }
}

/// Linearly interpolates two same-shaped sprite frames rect-for-rect, so a
/// pose can be drawn at a fractional position between two authored frames
/// instead of jumping straight from one to the next.
fn lerp_walk_rect(a: &WalkRect, b: &WalkRect, t: f32) -> WalkRect {
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    WalkRect(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2), lerp(a.3, b.3), a.4)
}

/// Draws a pose blended between frames `a` and `b` (0.0 = `a`, 1.0 = `b`),
/// smoothing the walk/sleep cycles without needing more authored frames.
fn draw_icon_frame_blended(
    hdc: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    a: &[WalkRect],
    b: &[WalkRect],
    t: f32,
) {
    let blended: Vec<WalkRect> = a
        .iter()
        .zip(b.iter())
        .map(|(ra, rb)| lerp_walk_rect(ra, rb, t))
        .collect();
    draw_icon_frame(hdc, x, y, w, h, &blended);
}

fn draw_rounded_rect(hdc: HDC, rect: &RECT, color: &Color, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let rgn = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right + 1,
            rect.bottom + 1,
            radius * 2,
            radius * 2,
        );
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(rgn);
        let _ = DeleteObject(brush);
    }
}

