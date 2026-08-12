//! 平台钩子：macOS 在窗口隐藏时切 accessory（隐藏 Dock 图标）。

#[cfg(target_os = "macos")]
pub fn set_accessory_mode(accessory: bool) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("set_accessory_mode called off main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if accessory {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    };
    app.setActivationPolicy(policy);
}

#[cfg(not(target_os = "macos"))]
pub fn set_accessory_mode(_accessory: bool) {}
