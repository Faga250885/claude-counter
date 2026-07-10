use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID};
use windows::Win32::System::Registry::*;

use crate::native_interop::wide_str;

const REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const REGISTRY_KEY: &str = "SystemUsesLightTheme";

/// Check if the system is in dark mode by reading the registry
pub fn is_dark_mode() -> bool {
    !is_light_theme()
}

/// Detects whether a taskbar is visually light or dark by sampling its own
/// rendered background color, rather than the system-wide light/dark theme
/// setting — the two can diverge (e.g. a custom accent color applied to the
/// taskbar, or a shell theme that doesn't follow the app light/dark toggle).
///
/// Reads from the *screen* DC at the taskbar's absolute coordinates (not the
/// taskbar window's own DC): modern Explorer taskbars are DWM/DirectComposition
/// composited, so a window-owned DC doesn't reflect what's actually on
/// screen and GetPixel against it returns a stale/default color. Sampling
/// the screen mirrors what a screenshot would capture.
pub fn is_taskbar_dark(_taskbar_hwnd: HWND, taskbar_rect: RECT) -> Option<bool> {
    unsafe {
        let hdc = GetDC(HWND::default());
        if hdc.is_invalid() {
            return None;
        }

        let width = taskbar_rect.right - taskbar_rect.left;
        let height = taskbar_rect.bottom - taskbar_rect.top;
        if width <= 4 || height <= 0 {
            let _ = ReleaseDC(HWND::default(), hdc);
            return None;
        }

        // Sample right at the taskbar's far edge (nearest the screen edge),
        // vertically centered — Windows always keeps this corner clear of
        // icons/text regardless of taskbar alignment (left, centered, RTL).
        let sample_x = taskbar_rect.right - 2;
        let sample_y = taskbar_rect.top + height / 2;
        let pixel = GetPixel(hdc, sample_x, sample_y);
        let _ = ReleaseDC(HWND::default(), hdc);

        if pixel.0 == CLR_INVALID {
            return None;
        }

        let bgr = pixel.0;
        let r = (bgr & 0xFF) as f64;
        let g = ((bgr >> 8) & 0xFF) as f64;
        let b = ((bgr >> 16) & 0xFF) as f64;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        Some(luminance < 128.0)
    }
}

fn is_light_theme() -> bool {
    unsafe {
        let path = wide_str(REGISTRY_PATH);
        let key_name = wide_str(REGISTRY_KEY);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );

        if result.is_err() {
            return false; // Default to dark mode
        }

        let mut data: u32 = 0;
        let mut data_size: u32 = std::mem::size_of::<u32>() as u32;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut data_size),
        );

        let _ = RegCloseKey(hkey);

        if result.is_err() {
            return false; // Default to dark mode
        }

        data == 1
    }
}
