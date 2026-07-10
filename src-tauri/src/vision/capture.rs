use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

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
