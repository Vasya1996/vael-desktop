use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

/// Fraction (0..1) of pixels with any non-zero RGB — a quick "not a black frame" check.
pub fn non_black_fraction(rgba: &[u8]) -> f64 {
    if rgba.len() < 4 {
        return 0.0;
    }
    let px = rgba.len() / 4;
    let mut nz = 0usize;
    for c in rgba.chunks_exact(4) {
        if c[0] | c[1] | c[2] != 0 {
            nz += 1;
        }
    }
    nz as f64 / px as f64
}

type Shared = Arc<Mutex<Option<Result<image::RgbaImage, String>>>>;

#[derive(Clone)]
struct Flags {
    result: Shared,
}

struct Cap {
    flags: Flags,
}

impl GraphicsCaptureApiHandler for Cap {
    type Flags = Flags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        ctl: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let r = (|| -> Result<image::RgbaImage, String> {
            let fb = frame.buffer().map_err(|e| format!("buffer: {e:?}"))?;
            let (w, h) = (fb.width(), fb.height());
            let mut scratch = Vec::new();
            let px = fb.as_nopadding_buffer(&mut scratch); // tightly packed RGBA
            image::RgbaImage::from_raw(w, h, px.to_vec()).ok_or_else(|| "rgba/size mismatch".into())
        })();
        *self.flags.result.lock().unwrap() = Some(r);
        ctl.stop();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Capture one frame of the window (HWND given as isize) into memory as an RGBA image.
/// Blocks the CALLING thread (WGC runs a message loop) until the first frame —
/// callers must run this on a dedicated thread.
pub fn capture_window_rgba(hwnd: isize) -> Result<image::RgbaImage, String> {
    let result: Shared = Arc::new(Mutex::new(None));
    let window = Window::from_raw_hwnd(hwnd as *mut c_void);
    let settings = Settings::new(
        window,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder, // hide the Win11 yellow capture border
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        Flags {
            result: result.clone(),
        },
    );
    Cap::start(settings).map_err(|e| format!("WGC start: {e:?}"))?;
    let r = result.lock().unwrap().take();
    r.unwrap_or_else(|| Err("no frame captured".into()))
}

/// Capture one frame and save it as PNG (dev/QA). Returns (w, h, non-black fraction).
pub fn capture_window_to_png(hwnd: isize, out: &Path) -> Result<(u32, u32, f64), String> {
    let img = capture_window_rgba(hwnd)?;
    let (w, h) = (img.width(), img.height());
    let nz = non_black_fraction(img.as_raw());
    img.save(out).map_err(|e| format!("save: {e}"))?;
    Ok((w, h, nz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_black_fraction_extremes() {
        assert_eq!(non_black_fraction(&[0u8; 4 * 32]), 0.0);
        assert_eq!(non_black_fraction(&[255u8; 4 * 32]), 1.0);
    }
}
