//! What macOS says about appearance. Light or dark comes from winit
//! (`Window::theme`, `WindowEvent::ThemeChanged`); "Increase contrast"
//! (System Settings, Accessibility, Display) from `NSWorkspace`.

/// `NSWorkspace.accessibilityDisplayShouldIncreaseContrast`. There is no
/// notification wired up for it; the app asks again now and then.
pub fn increase_contrast() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
}
