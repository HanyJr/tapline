//! Reads which mouse buttons and keys are held right now, for Record mode.
//! Windows: GetAsyncKeyState. macOS: CGEventSource state (needs Input Monitoring).

/// (name used in steps, Windows virtual key, macOS key code)
#[allow(dead_code)]
pub const KEYS: &[(&str, i32, u16)] = &[
    ("Enter", 0x0D, 0x24), ("Tab", 0x09, 0x30), ("Space", 0x20, 0x31), ("Backspace", 0x08, 0x33),
    ("Escape", 0x1B, 0x35), ("Delete", 0x2E, 0x75), ("Home", 0x24, 0x73), ("End", 0x23, 0x77),
    ("PageUp", 0x21, 0x74), ("PageDown", 0x22, 0x79), ("Left", 0x25, 0x7B), ("Right", 0x27, 0x7C),
    ("Down", 0x28, 0x7D), ("Up", 0x26, 0x7E),
    ("F1", 0x70, 0x7A), ("F2", 0x71, 0x78), ("F3", 0x72, 0x63), ("F4", 0x73, 0x76), ("F5", 0x74, 0x60),
    ("F6", 0x75, 0x61), ("F7", 0x76, 0x62), ("F8", 0x77, 0x64), ("F9", 0x78, 0x65), ("F10", 0x79, 0x6D),
    ("F11", 0x7A, 0x67), ("F12", 0x7B, 0x6F),
    ("A", 0x41, 0x00), ("B", 0x42, 0x0B), ("C", 0x43, 0x08), ("D", 0x44, 0x02), ("E", 0x45, 0x0E),
    ("F", 0x46, 0x03), ("G", 0x47, 0x05), ("H", 0x48, 0x04), ("I", 0x49, 0x22), ("J", 0x4A, 0x26),
    ("K", 0x4B, 0x28), ("L", 0x4C, 0x25), ("M", 0x4D, 0x2E), ("N", 0x4E, 0x2D), ("O", 0x4F, 0x1F),
    ("P", 0x50, 0x23), ("Q", 0x51, 0x0C), ("R", 0x52, 0x0F), ("S", 0x53, 0x01), ("T", 0x54, 0x11),
    ("U", 0x55, 0x20), ("V", 0x56, 0x09), ("W", 0x57, 0x0D), ("X", 0x58, 0x07), ("Y", 0x59, 0x10),
    ("Z", 0x5A, 0x06),
    ("0", 0x30, 0x1D), ("1", 0x31, 0x12), ("2", 0x32, 0x13), ("3", 0x33, 0x14), ("4", 0x34, 0x15),
    ("5", 0x35, 0x17), ("6", 0x36, 0x16), ("7", 0x37, 0x1A), ("8", 0x38, 0x1C), ("9", 0x39, 0x19),
];

/// (modifier name, Windows virtual keys, macOS key codes)
#[allow(dead_code)]
const MODS: &[(&str, &[i32], &[u16])] = &[
    ("Ctrl", &[0x11], &[0x3B, 0x3E]),
    ("Shift", &[0x10], &[0x38, 0x3C]),
    ("Alt", &[0x12], &[0x3A, 0x3D]),
    ("Cmd", &[0x5B, 0x5C], &[0x37, 0x36]),
];

#[derive(Default, Clone, PartialEq)]
pub struct State {
    pub left: bool,
    pub right: bool,
    pub keys: Vec<&'static str>,
    pub mods: Vec<&'static str>,
}

#[cfg(target_os = "windows")]
fn down_vk(vk: i32) -> bool {
    unsafe { (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

#[cfg(target_os = "windows")]
pub fn read() -> State {
    State {
        left: down_vk(0x01),
        right: down_vk(0x02),
        keys: KEYS.iter().filter(|k| down_vk(k.1)).map(|k| k.0).collect(),
        mods: MODS.iter().filter(|m| m.1.iter().any(|v| down_vk(*v))).map(|m| m.0).collect(),
    }
}

#[cfg(target_os = "windows")]
pub fn has_permission(_ask: bool) -> bool {
    true
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
        pub fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
        pub fn CGPreflightListenEventAccess() -> bool;
        pub fn CGRequestListenEventAccess() -> bool;
    }
    // kCGEventSourceStateHIDSystemState
    pub const HID: i32 = 1;
}

#[cfg(target_os = "macos")]
pub fn read() -> State {
    use mac::*;
    unsafe {
        State {
            left: CGEventSourceButtonState(HID, 0),
            right: CGEventSourceButtonState(HID, 1),
            keys: KEYS.iter().filter(|k| CGEventSourceKeyState(HID, k.2)).map(|k| k.0).collect(),
            mods: MODS.iter().filter(|m| m.2.iter().any(|c| CGEventSourceKeyState(HID, *c))).map(|m| m.0).collect(),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn has_permission(ask: bool) -> bool {
    unsafe {
        if mac::CGPreflightListenEventAccess() {
            return true;
        }
        if ask {
            return mac::CGRequestListenEventAccess();
        }
        false
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn read() -> State {
    State::default()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn has_permission(_ask: bool) -> bool {
    true
}
