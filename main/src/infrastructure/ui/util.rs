use swell_ui::{DialogUnits, Dimensions};

/// The optimal size of the main panel in dialog units.
pub const MAIN_PANEL_DIMENSIONS: Dimensions<DialogUnits> =
    Dimensions::new(DialogUnits(470), DialogUnits(423));

pub mod symbols {
    pub fn arrow_up_symbol() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            if arrows_are_supported() { "🡹" } else { "Up" }
        }
        #[cfg(target_os = "macos")]
        {
            "⬆"
        }
        #[cfg(target_os = "linux")]
        {
            "Up"
        }
    }

    pub fn arrow_down_symbol() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            if arrows_are_supported() {
                "🡻"
            } else {
                "Down"
            }
        }
        #[cfg(target_os = "macos")]
        {
            "⬇"
        }
        #[cfg(target_os = "linux")]
        {
            "Down"
        }
    }

    pub fn arrow_left_symbol() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            if arrows_are_supported() { "🡸" } else { "<=" }
        }
        #[cfg(target_os = "macos")]
        {
            "⬅"
        }
        #[cfg(target_os = "linux")]
        {
            "<="
        }
    }

    pub fn arrow_right_symbol() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            if arrows_are_supported() { "🡺" } else { "=>" }
        }
        #[cfg(target_os = "macos")]
        {
            "⮕"
        }
        #[cfg(target_os = "linux")]
        {
            "=>"
        }
    }

    #[cfg(target_os = "windows")]
    fn arrows_are_supported() -> bool {
        use once_cell::sync::Lazy;
        static SOMETHING_LIKE_WINDOWS_10: Lazy<bool> = Lazy::new(|| {
            let win_version = if let Ok(v) = sys_info::os_release() {
                v
            } else {
                return true;
            };
            win_version.as_str() >= "6.2"
        });
        *SOMETHING_LIKE_WINDOWS_10
    }
}

pub mod view {
    use crate::infrastructure::ui::util::SHADED_WHITE;
    use once_cell::sync::Lazy;
    use reaper_low::{raw, Swell};

    pub fn control_color_static_default(hdc: raw::HDC, brush: raw::HBRUSH) -> raw::HBRUSH {
        unsafe {
            Swell::get().SetBkMode(hdc, raw::TRANSPARENT as _);
        }
        brush
    }

    pub fn control_color_dialog_default(_hdc: raw::HDC, brush: raw::HBRUSH) -> raw::HBRUSH {
        brush
    }

    pub fn shaded_white_brush() -> raw::HBRUSH {
        static BRUSH: Lazy<isize> = Lazy::new(|| create_brush(SHADED_WHITE));
        *BRUSH as _
    }

    /// Use with care! Should be freed after use.
    fn create_brush(color: (u8, u8, u8)) -> isize {
        Swell::get().CreateSolidBrush(rgb(color)) as _
    }

    fn rgb((r, g, b): (u8, u8, u8)) -> std::os::raw::c_int {
        Swell::RGB(r, g, b) as _
    }
}

const SHADED_WHITE: (u8, u8, u8) = (248, 248, 248);
