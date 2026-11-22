//! Windows' animation and transparency settings as GPUI's [`AccessibilityDisplaySettings`]. The decision lives here,
//! on plain values, so it is tested on any host (Parallax's `scripts/patch-test.sh` builds this file on its own);
//! `platform.rs` reads the OS and follows `WM_SETTINGCHANGE`.

use gpui::AccessibilityDisplaySettings;

/// `client_area_animation`: `SPI_GETCLIENTAREAANIMATION` (Settings > Accessibility > Visual effects > Animation
/// effects), `None` when the call failed. `enable_transparency`: `EnableTransparency` under
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` (Settings > Personalization > Colors >
/// Transparency effects), `None` when the value is missing, as before the user first changes it: Windows then draws
/// transparency.
pub(crate) fn settings(
    client_area_animation: Option<bool>,
    enable_transparency: Option<u32>,
) -> AccessibilityDisplaySettings {
    AccessibilityDisplaySettings {
        reduce_motion: client_area_animation == Some(false),
        reduce_transparency: enable_transparency == Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_windows_values() {
        // (name, SPI_GETCLIENTAREAANIMATION, EnableTransparency, reduce_motion, reduce_transparency). Extend with a
        // row per new value Windows reports.
        let rows = [
            ("defaults", Some(true), Some(1), false, false),
            ("animation effects off", Some(false), Some(1), true, false),
            ("transparency effects off", Some(true), Some(0), false, true),
            ("both off", Some(false), Some(0), true, true),
            ("animation unreadable", None, Some(1), false, false),
            ("transparency never set", Some(true), None, false, false),
        ];
        for (name, animation, transparency, reduce_motion, reduce_transparency) in rows {
            assert_eq!(
                settings(animation, transparency),
                AccessibilityDisplaySettings {
                    reduce_motion,
                    reduce_transparency
                },
                "{name}"
            );
        }
    }
}
