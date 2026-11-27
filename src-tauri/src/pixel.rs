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

/// Pixels of a screen area as RGB, plus the pixel width and height.
#[cfg(target_os = "windows")]
pub fn read_area(x: i32, y: i32, w: i32, h: i32) -> Result<(Vec<Rgb>, usize, usize), String> {
    use windows_sys::Win32::Graphics::Gdi::*;
    if w <= 0 || h <= 0 {
        return Err("Empty area".into());
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("Could not read the screen".into());
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT);
        let mut info: BITMAPINFO = Default::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // top-down rows
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = if ok != 0 {
            GetDIBits(mem, bmp, 0, h as u32, buf.as_mut_ptr() as *mut _, &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        if lines == 0 {
            return Err("Could not read that area of the screen".into());
        }
        let px = buf.chunks_exact(4).map(|c| [c[2], c[1], c[0]]).collect();
        Ok((px, w as usize, h as usize))
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
        pub fn CGImageGetWidth(image: *const c_void) -> usize;
        pub fn CGImageGetHeight(image: *const c_void) -> usize;
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
pub fn read_area(x: i32, y: i32, w: i32, h: i32) -> Result<(Vec<Rgb>, usize, usize), String> {
    use mac::*;
    if w <= 0 || h <= 0 {
        return Err("Empty area".into());
    }
    unsafe {
        let rect = CGRect {
            origin: CGPoint { x: x as f64, y: y as f64 },
            size: CGSize { width: w as f64, height: h as f64 },
        };
        let img = CGWindowListCreateImage(rect, 1, 0, 0);
        if img.is_null() {
            return Err("Allow Tapline under Privacy & Security, Screen Recording".into());
        }
        let result = (|| {
            if CGImageGetBitsPerPixel(img) != 32 {
                return Err("Unsupported screen format".to_string());
            }
            let (pw, ph) = (CGImageGetWidth(img), CGImageGetHeight(img));
            let row = CGImageGetBytesPerRow(img);
            let provider = CGImageGetDataProvider(img);
            if provider.is_null() || pw == 0 || ph == 0 {
                return Err("Could not read the screen".to_string());
            }
            let data = CGDataProviderCopyData(provider);
            if data.is_null() {
                return Err("Could not read the screen".to_string());
            }
            let len = CFDataGetLength(data) as usize;
            let ptr = CFDataGetBytePtr(data);
            if ptr.is_null() || len < row * (ph - 1) + pw * 4 {
                CFRelease(data);
                return Err("Could not read the screen".to_string());
            }
            let bytes = std::slice::from_raw_parts(ptr, len);
            let little = (CGImageGetBitmapInfo(img) & 0x7000) == 0x2000;
            let alpha_first = matches!(CGImageGetAlphaInfo(img), 2 | 4 | 6);
            let mut out = Vec::with_capacity(pw * ph);
            for yy in 0..ph {
                for xx in 0..pw {
                    let i = yy * row + xx * 4;
                    let p = &bytes[i..i + 4];
                    out.push(match (little, alpha_first) {
                        (true, true) => [p[2], p[1], p[0]],
                        (true, false) => [p[3], p[2], p[1]],
                        (false, true) => [p[1], p[2], p[3]],
                        (false, false) => [p[0], p[1], p[2]],
                    });
                }
            }
            CFRelease(data);
            Ok((out, pw, ph))
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
pub fn read_area(_x: i32, _y: i32, _w: i32, _h: i32) -> Result<(Vec<Rgb>, usize, usize), String> {
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

/// Shrink an area to a small grid of average colors (at most `max` cells per side).
pub fn fingerprint(px: &[Rgb], pw: usize, ph: usize, max: usize) -> (Vec<Rgb>, u32, u32) {
    if pw == 0 || ph == 0 || px.len() < pw * ph {
        return (vec![], 0, 0);
    }
    let scale = (pw.max(ph) as f64 / max as f64).max(1.0);
    let gw = ((pw as f64 / scale).round() as usize).max(1);
    let gh = ((ph as f64 / scale).round() as usize).max(1);
    let mut out = Vec::with_capacity(gw * gh);
    for gy in 0..gh {
        let (y0, y1) = (gy * ph / gh, ((gy + 1) * ph / gh).max(gy * ph / gh + 1));
        for gx in 0..gw {
            let (x0, x1) = (gx * pw / gw, ((gx + 1) * pw / gw).max(gx * pw / gw + 1));
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1.min(ph) {
                for x in x0..x1.min(pw) {
                    let c = px[y * pw + x];
                    r += c[0] as u64;
                    g += c[1] as u64;
                    b += c[2] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.push([(r / n) as u8, (g / n) as u8, (b / n) as u8]);
        }
    }
    (out, gw as u32, gh as u32)
}

/// Share of grid cells (0.0 - 1.0) that look clearly different.
pub fn diff(a: &[Rgb], b: &[Rgb]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 1.0;
    }
    let changed = a.iter().zip(b).filter(|(x, y)| distance(**x, **y) > 0.08).count();
    changed as f64 / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: usize, h: usize, c: Rgb) -> Vec<Rgb> {
        vec![c; w * h]
    }

    #[test]
    fn same_area_has_no_difference() {
        let a = area(200, 40, [30, 40, 50]);
        let (g1, w1, h1) = fingerprint(&a, 200, 40, 40);
        let (g2, _, _) = fingerprint(&a, 200, 40, 40);
        assert_eq!((w1, h1), (40, 8));
        assert_eq!(diff(&g1, &g2), 0.0);
    }

    #[test]
    fn small_badge_is_detected() {
        let base = area(200, 40, [240, 240, 240]);
        let mut badge = base.clone();
        for y in 10..30 {
            for x in 170..190 {
                badge[y * 200 + x] = [200, 40, 40]; // red badge, 5% of the area
            }
        }
        let (g1, ..) = fingerprint(&base, 200, 40, 40);
        let (g2, ..) = fingerprint(&badge, 200, 40, 40);
        let d = diff(&g1, &g2);
        assert!(d > 0.03 && d < 0.10, "badge diff was {d}");
    }

    #[test]
    fn tiny_noise_is_ignored() {
        let a = area(100, 100, [100, 100, 100]);
        let b = area(100, 100, [104, 98, 101]);
        let (g1, ..) = fingerprint(&a, 100, 100, 40);
        let (g2, ..) = fingerprint(&b, 100, 100, 40);
        assert_eq!(diff(&g1, &g2), 0.0);
    }

    #[test]
    fn single_pixel_area_works() {
        let (g, w, h) = fingerprint(&[[1, 2, 3]], 1, 1, 40);
        assert_eq!((g, w, h), (vec![[1, 2, 3]], 1, 1));
    }

    #[test]
    fn retina_capture_gives_same_grid_shape() {
        // a 2x pixel capture of the same 200x40 area should shrink to the same grid
        let (_, w1, h1) = fingerprint(&area(200, 40, [0, 0, 0]), 200, 40, 40);
        let (_, w2, h2) = fingerprint(&area(400, 80, [0, 0, 0]), 400, 80, 40);
        assert_eq!((w1, h1), (w2, h2));
    }

    #[test]
    fn mismatched_grids_count_as_fully_different() {
        assert_eq!(diff(&[[0, 0, 0]], &[[0, 0, 0], [0, 0, 0]]), 1.0);
        assert_eq!(diff(&[], &[]), 1.0);
    }
}
