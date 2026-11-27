//! Read the color of one screen pixel. Windows: GDI. macOS: CoreGraphics.
//! Coordinates are the same ones enigo uses for the mouse.

pub type Rgb = [u8; 3];

#[cfg(target_os = "windows")]
pub fn read(x: i32, y: i32) -> Result<Rgb, String> {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return Err("Could not read the screen".into());
        }
        let c = GetPixel(hdc, x, y);
        ReleaseDC(std::ptr::null_mut(), hdc);
        if c == 0xFFFF_FFFF {
            return Err("That spot is off screen".into());
        }
        Ok([(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8])
    }
}

#[cfg(target_os = "windows")]
pub fn has_permission(_ask: bool) -> bool {
    true
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGWindowListCreateImage(rect: CGRect, list: u32, window: u32, image: u32) -> *const c_void;
        pub fn CGImageGetDataProvider(image: *const c_void) -> *const c_void;
        pub fn CGDataProviderCopyData(provider: *const c_void) -> *const c_void;
        pub fn CGImageGetBytesPerRow(image: *const c_void) -> usize;
        pub fn CGImageGetBitsPerPixel(image: *const c_void) -> usize;
        pub fn CGImageGetBitmapInfo(image: *const c_void) -> u32;
        pub fn CGImageGetAlphaInfo(image: *const c_void) -> u32;
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        pub fn CFDataGetLength(data: *const c_void) -> isize;
        pub fn CFRelease(obj: *const c_void);
    }
}

#[cfg(target_os = "macos")]
pub fn read(x: i32, y: i32) -> Result<Rgb, String> {
    use mac::*;
    unsafe {
        let rect = CGRect {
            origin: CGPoint { x: x as f64, y: y as f64 },
            size: CGSize { width: 1.0, height: 1.0 },
        };
        // kCGWindowListOptionOnScreenOnly = 1, kCGNullWindowID = 0, kCGWindowImageDefault = 0
        let img = CGWindowListCreateImage(rect, 1, 0, 0);
        if img.is_null() {
            return Err("Allow Tapline under Privacy & Security, Screen Recording".into());
        }
        let result = (|| {
            if CGImageGetBitsPerPixel(img) != 32 {
                return Err("Unsupported screen format".to_string());
            }
            let provider = CGImageGetDataProvider(img);
            if provider.is_null() {
                return Err("Could not read the screen".to_string());
            }
            let data = CGDataProviderCopyData(provider);
            if data.is_null() {
                return Err("Could not read the screen".to_string());
            }
            let len = CFDataGetLength(data);
            let ptr = CFDataGetBytePtr(data);
            let _ = CGImageGetBytesPerRow(img);
            if ptr.is_null() || len < 4 {
                CFRelease(data);
                return Err("Could not read the screen".to_string());
            }
            let p = std::slice::from_raw_parts(ptr, 4);
            let little = (CGImageGetBitmapInfo(img) & 0x7000) == 0x2000;
            let alpha = CGImageGetAlphaInfo(img);
            let alpha_first = matches!(alpha, 2 | 4 | 6); // PremultipliedFirst, First, NoneSkipFirst
            let rgb = match (little, alpha_first) {
                (true, true) => [p[2], p[1], p[0]],  // BGRA in memory
                (true, false) => [p[3], p[2], p[1]], // ABGR in memory
                (false, true) => [p[1], p[2], p[3]], // ARGB
                (false, false) => [p[0], p[1], p[2]], // RGBA
            };
            CFRelease(data);
            Ok(rgb)
        })();
        CFRelease(img);
        result
    }
}

#[cfg(target_os = "macos")]
pub fn has_permission(ask: bool) -> bool {
    unsafe {
        if mac::CGPreflightScreenCaptureAccess() {
            return true;
        }
        if ask {
            return mac::CGRequestScreenCaptureAccess();
        }
        false
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn read(_x: i32, _y: i32) -> Result<Rgb, String> {
    Err("Watch mode works on Windows and Mac".into())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn has_permission(_ask: bool) -> bool {
    true
}

/// 0.0 = identical, 1.0 = black vs white
pub fn distance(a: Rgb, b: Rgb) -> f64 {
    let d = |i: usize| (a[i] as f64 - b[i] as f64).powi(2);
    ((d(0) + d(1) + d(2)).sqrt()) / 441.673
}
