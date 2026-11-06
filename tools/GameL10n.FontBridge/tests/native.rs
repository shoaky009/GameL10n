// SPDX-License-Identifier: MIT
//! Independent Windows loader and rendering checks for the font bridge.

use std::ffi::{CStr, c_void};
use std::ptr::{null, null_mut};

#[path = "../src/ffi.rs"]
mod ffi;
use ffi::*;

mod drawing {
    #![allow(non_snake_case)]

    use super::*;

    pub const PIXEL_FORMAT_ARGB: i32 = 0x0026_200a;

    #[repr(C)]
    pub struct RectF {
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
    }

    #[repr(C)]
    pub struct Rect {
        pub x: i32,
        pub y: i32,
        pub width: i32,
        pub height: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct BitmapData {
        pub width: u32,
        pub height: u32,
        pub stride: i32,
        pub pixel_format: i32,
        pub scan0: *mut u8,
        pub reserved: usize,
    }

    const _: () = assert!(size_of::<BitmapData>() == 24);

    #[link(name = "gdiplus", kind = "raw-dylib", import_name_type = "undecorated")]
    unsafe extern "system" {
        pub fn GdipCreateBitmapFromScan0(
            width: i32,
            height: i32,
            stride: i32,
            format: i32,
            scan0: *mut u8,
            bitmap: *mut *mut c_void,
        ) -> GpStatus;
        pub fn GdipGetImageGraphicsContext(
            image: *mut c_void,
            graphics: *mut *mut c_void,
        ) -> GpStatus;
        pub fn GdipGraphicsClear(graphics: *mut c_void, color: u32) -> GpStatus;
        pub fn GdipCreateSolidFill(color: u32, brush: *mut *mut c_void) -> GpStatus;
        pub fn GdipMeasureString(
            graphics: *mut c_void,
            text: *const u16,
            length: i32,
            font: *mut GpFont,
            layout: *const RectF,
            format: *const c_void,
            bounds: *mut RectF,
            fitted: *mut i32,
            lines: *mut i32,
        ) -> GpStatus;
        pub fn GdipDrawString(
            graphics: *mut c_void,
            text: *const u16,
            length: i32,
            font: *mut GpFont,
            layout: *const RectF,
            format: *const c_void,
            brush: *mut c_void,
        ) -> GpStatus;
        pub fn GdipDeleteBrush(brush: *mut c_void) -> GpStatus;
        pub fn GdipDeleteGraphics(graphics: *mut c_void) -> GpStatus;
        pub fn GdipBitmapLockBits(
            bitmap: *mut c_void,
            area: *const Rect,
            flags: u32,
            format: i32,
            data: *mut BitmapData,
        ) -> GpStatus;
        pub fn GdipBitmapUnlockBits(bitmap: *mut c_void, data: *mut BitmapData) -> GpStatus;
        pub fn GdipDisposeImage(image: *mut c_void) -> GpStatus;
    }
}
use drawing::*;

type FromDC = unsafe extern "system" fn(HDC, *mut *mut GpFont) -> GpStatus;
type FromA = unsafe extern "system" fn(HDC, *const LOGFONTA, *mut *mut GpFont) -> GpStatus;
type FromW = unsafe extern "system" fn(HDC, *const LOGFONTW, *mut *mut GpFont) -> GpStatus;
type GetFamily = unsafe extern "system" fn(*mut GpFont, *mut *mut GpFontFamily) -> GpStatus;

#[derive(Clone, Copy)]
struct Api {
    dc: FromDC,
    a: FromA,
    w: FromW,
    family: GetFamily,
}

impl Api {
    fn module(module: HMODULE) -> Self {
        assert!(!module.is_null(), "DLL must load");
        let address = |name: &CStr| {
            let pointer = unsafe { GetProcAddress(module, name.as_ptr()) };
            assert!(!pointer.is_null(), "missing export {name:?}");
            pointer
        };
        // Both DLLs expose the same four Windows SDK signatures and stdcall ABI.
        unsafe {
            Self {
                dc: std::mem::transmute::<*mut c_void, FromDC>(address(c"GdipCreateFontFromDC")),
                a: std::mem::transmute::<*mut c_void, FromA>(address(
                    c"GdipCreateFontFromLogfontA",
                )),
                w: std::mem::transmute::<*mut c_void, FromW>(address(
                    c"GdipCreateFontFromLogfontW",
                )),
                family: std::mem::transmute::<*mut c_void, GetFamily>(address(c"GdipGetFamily")),
            }
        }
    }

    fn imports() -> Self {
        // Keep ordinary system imports so Python can redirect the actual PE IAT.
        Self {
            dc: GdipCreateFontFromDC,
            a: GdipCreateFontFromLogfontA,
            w: GdipCreateFontFromLogfontW,
            family: GdipGetFamily,
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn request(face: &str, height: i32) -> LOGFONTW {
    let mut font = LOGFONTW {
        lfHeight: height,
        lfWeight: FW_NORMAL,
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    let name = wide(face);
    assert!(name.len() <= LF_FACESIZE);
    font.lfFaceName[..name.len()].copy_from_slice(&name);
    font
}

fn ansi(request: &LOGFONTW, face: &str) -> LOGFONTA {
    assert!(face.is_ascii() && face.len() < LF_FACESIZE);
    let mut font = LOGFONTA::default();
    // LOGFONTA/W share the 28-byte numeric prefix, checked in ffi.rs.
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(request).cast::<u8>(),
            std::ptr::from_mut(&mut font).cast::<u8>(),
            std::mem::offset_of!(LOGFONTA, lfFaceName),
        );
    }
    font.lfFaceName[..face.len()].copy_from_slice(face.as_bytes());
    font
}

struct SelectedFont {
    dc: HDC,
    font: HFONT,
    previous: HGDIOBJ,
}

impl SelectedFont {
    fn new(dc: HDC, request: &LOGFONTW) -> Self {
        unsafe {
            let font = CreateFontIndirectW(request);
            assert!(!font.is_null());
            let previous = SelectObject(dc, font);
            assert!(!previous.is_null() && previous != HGDI_ERROR);
            Self { dc, font, previous }
        }
    }
}

impl Drop for SelectedFont {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.font);
        }
    }
}

fn em_height(dc: HDC, request: &LOGFONTW) -> f32 {
    let _selected = SelectedFont::new(dc, request);
    let mut metrics = TEXTMETRICW::default();
    assert_ne!(unsafe { GetTextMetricsW(dc, &mut metrics) }, 0);
    (metrics.tmHeight - metrics.tmInternalLeading) as f32
}

fn family(api: Api, font: *mut GpFont) -> String {
    unsafe {
        let mut family = null_mut();
        assert_eq!((api.family)(font, &mut family), Ok);
        let mut name = [0; LF_FACESIZE];
        assert_eq!(GdipGetFamilyName(family, name.as_mut_ptr(), 0), Ok);
        assert_eq!(GdipDeleteFontFamily(family), Ok);
        let length = name
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(name.len());
        String::from_utf16_lossy(&name[..length])
    }
}

fn check_private(api: Api, font: *mut GpFont, face: &str, expected_size: f32, expected_style: i32) {
    assert!(!font.is_null());
    unsafe {
        assert_eq!(
            CompareStringOrdinal(
                wide(&family(api, font)).as_ptr(),
                -1,
                wide(face).as_ptr(),
                -1,
                1
            ),
            CSTR_EQUAL
        );
        // Disposing repeated caller-owned families must preserve this and later fonts.
        assert_eq!(family(api, font), family(api, font));
        let mut size = 0.0;
        let mut style = 0;
        let mut unit = UnitWorld;
        assert_eq!(GdipGetFontSize(font, &mut size), Ok);
        assert!(
            (size - expected_size).abs() < 0.01,
            "size {size}, expected {expected_size}"
        );
        assert_eq!(GdipGetFontStyle(font, &mut style), Ok);
        assert_eq!(style, expected_style);
        assert_eq!(GdipGetFontUnit(font, &mut unit), Ok);
        assert_eq!(unit, UnitPixel);
    }
}

fn check_draw(font: *mut GpFont) {
    unsafe {
        let (mut bitmap, mut graphics, mut fill) = (null_mut(), null_mut(), null_mut());
        assert_eq!(
            GdipCreateBitmapFromScan0(300, 100, 0, PIXEL_FORMAT_ARGB, null_mut(), &mut bitmap),
            Ok
        );
        assert_eq!(GdipGetImageGraphicsContext(bitmap, &mut graphics), Ok);
        assert_eq!(GdipGraphicsClear(graphics, 0), Ok);
        assert_eq!(GdipCreateSolidFill(u32::MAX, &mut fill), Ok);
        let text = wide("Font bridge 中文");
        let layout = RectF {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 100.0,
        };
        let mut bounds = RectF {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        assert_eq!(
            GdipMeasureString(
                graphics,
                text.as_ptr(),
                -1,
                font,
                &layout,
                null(),
                &mut bounds,
                null_mut(),
                null_mut()
            ),
            Ok
        );
        assert!(bounds.width > 20.0 && bounds.height > 10.0);
        assert_eq!(
            GdipDrawString(graphics, text.as_ptr(), -1, font, &layout, null(), fill),
            Ok
        );
        assert_eq!(GdipDeleteBrush(fill), Ok);
        assert_eq!(GdipDeleteGraphics(graphics), Ok);
        let mut pixels = BitmapData::default();
        let area = Rect {
            x: 0,
            y: 0,
            width: 300,
            height: 100,
        };
        assert_eq!(
            GdipBitmapLockBits(bitmap, &area, 1, PIXEL_FORMAT_ARGB, &mut pixels),
            Ok
        );
        let drawn = (0..pixels.height).any(|y| {
            // LockBits owns readable rows until UnlockBits; stride may be negative.
            let row = pixels
                .scan0
                .offset(y as isize * pixels.stride as isize)
                .cast::<u32>();
            (0..pixels.width).any(|x| *row.add(x as usize) != 0)
        });
        assert!(drawn, "DrawString must change bitmap pixels");
        assert_eq!(GdipBitmapUnlockBits(bitmap, &mut pixels), Ok);
        assert_eq!(GdipDisposeImage(bitmap), Ok);
    }
}

fn create_variant(
    api: Api,
    dc: HDC,
    request: &LOGFONTW,
    face: &str,
    variant: usize,
) -> (GpStatus, *mut GpFont) {
    let mut font = null_mut();
    let status = unsafe {
        match variant {
            0 => (api.w)(dc, request, &mut font),
            1 => (api.a)(dc, &ansi(request, face), &mut font),
            _ => {
                let _selected = SelectedFont::new(dc, request);
                (api.dc)(dc, &mut font)
            }
        }
    };
    (status, font)
}

fn delete_font(font: *mut GpFont) {
    if !font.is_null() {
        assert_eq!(unsafe { GdipDeleteFont(font) }, Ok);
    }
}

fn compare_original(api: Api, original: Api, dc: HDC) {
    let request = request("Arial", -24);
    for variant in 0..3 {
        let (actual_status, actual) = create_variant(api, dc, &request, "Arial", variant);
        let (expected_status, expected) = create_variant(original, dc, &request, "Arial", variant);
        assert_eq!(actual_status, expected_status);
        if actual_status == Ok {
            assert_eq!(family(api, actual), family(api, expected));
            let (mut actual_size, mut expected_size) = (0.0, 0.0);
            unsafe {
                assert_eq!(GdipGetFontSize(actual, &mut actual_size), Ok);
                assert_eq!(GdipGetFontSize(expected, &mut expected_size), Ok);
            }
            assert_eq!(actual_size, expected_size);
        }
        delete_font(actual);
        delete_font(expected);
    }
}

fn replace_tests(api: Api, dc: HDC, face: &str) {
    for height in [-24, 24, 0] {
        let request = request("bRiDgEaLiAs", height);
        let expected_size = em_height(dc, &request);
        for variant in 0..3 {
            let (status, font) = create_variant(api, dc, &request, "BridgeAlias", variant);
            println!("replace height={height} variant={variant} status={status}");
            assert_eq!(status, Ok);
            check_private(api, font, face, expected_size, FontStyleRegular);
            if variant == 0 {
                check_draw(font);
            }
            delete_font(font);
        }
    }
    let mut request = request(face, -24);
    request.lfUnderline = 1;
    request.lfStrikeOut = 1;
    let decoration = FontStyleUnderline | FontStyleStrikeout;
    let (status, font) = create_variant(api, dc, &request, "", 0);
    assert_eq!(status, Ok);
    check_private(api, font, face, em_height(dc, &request), decoration);
    delete_font(font);
    for style in [
        FontStyleBold,
        FontStyleItalic,
        FontStyleBold | FontStyleItalic,
    ] {
        request.lfWeight = if style & FontStyleBold != 0 {
            FW_BOLD
        } else {
            FW_NORMAL
        };
        request.lfItalic = u8::from(style & FontStyleItalic != 0);
        let (status, font) = create_variant(api, dc, &request, "", 0);
        assert!(status == Ok || status == FontStyleNotFound);
        if status == Ok {
            check_private(api, font, face, em_height(dc, &request), style | decoration);
        }
        delete_font(font);
    }
    let mut request = self::request("BridgeAlias", -24);
    let mut font = null_mut();
    unsafe {
        request.lfWidth = 12;
        assert_eq!((api.w)(dc, &request, &mut font), NotImplemented);
        request.lfWidth = 0;
        request.lfEscapement = 900;
        assert_eq!((api.w)(dc, &request, &mut font), NotImplemented);
        request.lfEscapement = 0;
        request.lfOrientation = 900;
        assert_eq!((api.w)(dc, &request, &mut font), NotImplemented);
        request.lfOrientation = 0;
        SetMapMode(dc, MM_ANISOTROPIC);
        assert_eq!((api.w)(dc, &request, &mut font), NotImplemented);
        SetMapMode(dc, MM_TEXT);
        assert_ne!(SetGraphicsMode(dc, GM_ADVANCED), 0);
        let transform = XFORM {
            eM11: 2.0,
            eM22: 2.0,
            ..Default::default()
        };
        assert_ne!(SetWorldTransform(dc, &transform), 0);
        assert_eq!((api.w)(dc, &request, &mut font), NotImplemented);
        assert_ne!(ModifyWorldTransform(dc, null(), MWT_IDENTITY), 0);
        SetGraphicsMode(dc, GM_COMPATIBLE);
        assert_eq!((api.w)(dc, null(), &mut font), InvalidParameter);
        assert_eq!((api.w)(dc, &request, null_mut()), InvalidParameter);
    }
}

fn fallback_tests(api: Api, original: Api, dc: HDC, face: &str) {
    let mut request = request("Terminal", -24);
    request.lfCharSet = OEM_CHARSET;
    request.lfOutPrecision = OUT_RASTER_PRECIS;
    for variant in 0..3 {
        let (expected_status, expected) =
            create_variant(original, dc, &request, "Terminal", variant);
        let (actual_status, actual) = create_variant(api, dc, &request, "Terminal", variant);
        println!("fallback variant={variant} original={expected_status} bridge={actual_status}");
        if expected_status == FontFamilyNotFound || expected_status == NotTrueTypeFont {
            assert_eq!(actual_status, Ok);
            check_private(api, actual, face, em_height(dc, &request), FontStyleRegular);
            check_draw(actual);
        } else {
            assert_eq!(actual_status, expected_status);
            if actual_status == Ok {
                assert_eq!(family(api, actual), family(api, expected));
            }
        }
        delete_font(actual);
        delete_font(expected);
    }
}

fn concurrent_tests(api: Api) {
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(move || unsafe {
                let dc = CreateCompatibleDC(null_mut());
                assert!(!dc.is_null());
                let request = request("BridgeAlias", -24);
                let expected_size = em_height(dc, &request);
                for _ in 0..32 {
                    let (status, font) = create_variant(api, dc, &request, "", 0);
                    assert_eq!(status, Ok);
                    let mut size = 0.0;
                    assert_eq!(GdipGetFontSize(font, &mut size), Ok);
                    assert_eq!(size, expected_size);
                    assert!(!family(api, font).is_empty());
                    delete_font(font);
                }
                assert_ne!(DeleteDC(dc), 0);
            });
        }
    });
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.len() != 4 || !matches!(arguments[1].as_str(), "direct" | "imports") {
        eprintln!("FontBridgeTests direct|imports scenario family");
        std::process::exit(2);
    }
    let mode = &arguments[1];
    let scenario = arguments[2].as_str();
    let face = &arguments[3];
    unsafe {
        let mut token = 0;
        assert_eq!(
            GdiplusStartup(&mut token, &GdiplusStartupInput::default(), null_mut()),
            Ok
        );
        let original = Api::module(GetModuleHandleW(wide("gdiplus.dll").as_ptr()));
        let api = if mode == "imports" {
            Api::imports()
        } else {
            Api::module(LoadLibraryW(wide("GameL10n.FontBridge.dll").as_ptr()))
        };
        let dc = CreateCompatibleDC(null_mut());
        assert!(!dc.is_null());
        match scenario {
            "concurrent" => concurrent_tests(api),
            "replace" => {
                compare_original(api, original, dc);
                replace_tests(api, dc, face);
                // The bridge must retain its own lifetime after the game's token ends.
                GdiplusShutdown(token);
                token = 0;
                let (status, font) = create_variant(api, dc, &request("BridgeAlias", -24), "", 0);
                assert_eq!(status, Ok);
                check_draw(font);
                delete_font(font);
            }
            "fallback" => {
                compare_original(api, original, dc);
                fallback_tests(api, original, dc, face);
            }
            _ => {
                let (status, font) = create_variant(api, dc, &request("BridgeAlias", -24), "", 0);
                println!("error scenario status={status}");
                match scenario {
                    "missing-font" | "missing-config" => assert_eq!(status, FileNotFound),
                    "wrong-face" => assert_eq!(status, FontFamilyNotFound),
                    "bad-config" => assert_eq!(status, InvalidParameter),
                    "invalid-font" => assert_ne!(status, Ok),
                    _ => panic!("unknown scenario {scenario}"),
                }
                delete_font(font);
                if !matches!(scenario, "bad-config" | "missing-config") {
                    compare_original(api, original, dc);
                }
            }
        }
        assert_ne!(DeleteDC(dc), 0);
        if token != 0 {
            GdiplusShutdown(token);
        }
        // The loaded bridge and its private font collection have process lifetime.
    }
    println!("PASS");
}
