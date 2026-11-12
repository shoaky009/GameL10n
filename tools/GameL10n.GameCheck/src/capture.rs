use crate::protocol::TextRegion;
use std::{error::Error, mem::size_of, sync::mpsc, time::Duration};
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::ClientToScreen,
        },
        UI::WindowsAndMessaging::GetClientRect,
    },
};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

pub struct Pixels {
    pub width: i32,
    pub height: i32,
    pub bytes: Vec<u8>,
}
type CaptureError = Box<dyn Error + Send + Sync>;

struct Snapshot {
    sender: mpsc::SyncSender<Result<Pixels, String>>,
    window: usize,
    region: TextRegion,
}
impl GraphicsCaptureApiHandler for Snapshot {
    type Flags = (mpsc::SyncSender<Result<Pixels, String>>, usize, TextRegion);
    type Error = CaptureError;
    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            sender: ctx.flags.0,
            window: ctx.flags.1,
            region: ctx.flags.2,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let result = self.pixels(frame);
        let _ = self.sender.try_send(result);
        control.stop();
        Ok(())
    }
}

impl Snapshot {
    fn pixels(&self, frame: &mut Frame) -> Result<Pixels, String> {
        let window = HWND(self.window as *mut _);
        let mut bounds = RECT::default();
        let mut client = RECT::default();
        let mut origin = POINT::default();
        unsafe {
            DwmGetWindowAttribute(
                window,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut bounds as *mut RECT).cast(),
                size_of::<RECT>() as u32,
            )
            .map_err(|e| e.to_string())?;
            GetClientRect(window, &mut client).map_err(|e| e.to_string())?;
            ClientToScreen(window, &mut origin)
                .ok()
                .map_err(|e| e.to_string())?;
        }
        let left = origin.x - bounds.left + client.right * i32::from(self.region.left) / 100;
        let top = origin.y - bounds.top + client.bottom * i32::from(self.region.top) / 100;
        let width = client.right * i32::from(self.region.width) / 100;
        let height = client.bottom * i32::from(self.region.height) / 100;
        if left < 0
            || top < 0
            || width < 8
            || height < 8
            || left + width > frame.width() as i32
            || top + height > frame.height() as i32
            || width > 4096
            || height > 4096
        {
            return Err("文本区域过小、过大或窗口尺寸已变化，请调整后重试。".into());
        }
        let buffer = frame
            .buffer_crop(
                left as u32,
                top as u32,
                (left + width) as u32,
                (top + height) as u32,
            )
            .map_err(|e| format!("无法读取游戏画面：{e}"))?;
        let mut padding = Vec::new();
        let bytes = buffer.as_nopadding_buffer(&mut padding).to_vec();
        Ok(Pixels {
            width,
            height,
            bytes,
        })
    }
}

pub fn capture(window: HWND, region: &TextRegion) -> Result<Pixels, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let settings = Settings::new(
        Window::from_raw_hwnd(window.0),
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        (sender, window.0 as usize, region.clone()),
    );
    let capture =
        Snapshot::start_free_threaded(settings).map_err(|e| format!("无法截取游戏窗口：{e}"))?;
    let pixels = receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "未能获取游戏画面，请保持窗口打开并关闭独占全屏。".to_string());
    capture
        .stop()
        .map_err(|e| format!("无法结束画面采集：{e}"))?;
    pixels?
}

pub fn recognize(pixels: Pixels, language: &str) -> Result<String, String> {
    (|| -> windows::core::Result<String> {
        let languages = OcrEngine::AvailableRecognizerLanguages()?;
        let selected = languages.into_iter().find(|candidate| {
            candidate
                .LanguageTag()
                .is_ok_and(|tag| tag.to_string().starts_with(language))
        });
        let language = selected.ok_or_else(|| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                "未安装所选语言的 Windows OCR 语言包，请在系统语言设置中安装。",
            )
        })?;
        let language = Language::CreateLanguage(&language.LanguageTag()?)?;
        let engine = OcrEngine::TryCreateFromLanguage(&language)?;
        let bitmap = SoftwareBitmap::CreateWithAlpha(
            BitmapPixelFormat::Bgra8,
            pixels.width,
            pixels.height,
            BitmapAlphaMode::Ignore,
        )?;
        let writer = DataWriter::new()?;
        writer.WriteBytes(&pixels.bytes)?;
        bitmap.CopyFromBuffer(&writer.DetachBuffer()?)?;
        let result = engine.RecognizeAsync(&bitmap)?.join()?;
        Ok(result.Text()?.to_string())
    })()
    .map_err(|e| format!("无法识别游戏文字：{e}"))
}
