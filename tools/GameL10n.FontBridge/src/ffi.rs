// SPDX-License-Identifier: MIT
//! Windows x86 ABI declarations shared by the bridge and its native checks.

// The names match the Windows SDK; each consumer uses a different API subset.
#![allow(
    non_snake_case,
    non_upper_case_globals,
    clippy::upper_case_acronyms,
    dead_code
)]

use std::ffi::{c_char, c_void};

pub type BOOL = i32;
pub type DWORD = u32;
pub type HDC = *mut c_void;
pub type HMODULE = *mut c_void;
pub type HFONT = *mut c_void;
pub type HGDIOBJ = *mut c_void;
pub type GpStatus = i32;
pub type REAL = f32;

#[repr(C)]
pub struct GpFont {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct GpFontFamily {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct GpFontCollection {
    _opaque: [u8; 0],
}

pub const Ok: GpStatus = 0;
pub const GenericError: GpStatus = 1;
pub const InvalidParameter: GpStatus = 2;
pub const OutOfMemory: GpStatus = 3;
pub const NotImplemented: GpStatus = 6;
pub const FileNotFound: GpStatus = 10;
pub const AccessDenied: GpStatus = 12;
pub const FontFamilyNotFound: GpStatus = 14;
pub const FontStyleNotFound: GpStatus = 15;
pub const NotTrueTypeFont: GpStatus = 16;
pub const LF_FACESIZE: usize = 32;
pub const FontStyleRegular: i32 = 0;
pub const FontStyleBold: i32 = 1;
pub const FontStyleItalic: i32 = 2;
pub const FontStyleUnderline: i32 = 4;
pub const FontStyleStrikeout: i32 = 8;
pub const UnitWorld: i32 = 0;
pub const UnitPixel: i32 = 2;
pub const FW_NORMAL: i32 = 400;
pub const FW_BOLD: i32 = 700;
pub const DEFAULT_CHARSET: u8 = 1;
pub const OEM_CHARSET: u8 = 255;
pub const OUT_RASTER_PRECIS: u8 = 6;
pub const MM_TEXT: i32 = 1;
pub const MM_ANISOTROPIC: i32 = 8;
pub const GM_COMPATIBLE: i32 = 1;
pub const GM_ADVANCED: i32 = 2;
pub const MWT_IDENTITY: DWORD = 1;
pub const LOGPIXELSY: i32 = 90;
pub const OBJ_FONT: u32 = 6;
pub const CP_ACP: u32 = 0;
pub const CSTR_EQUAL: i32 = 2;
pub const INVALID_FILE_ATTRIBUTES: DWORD = u32::MAX;
pub const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
pub const ERROR_FILE_NOT_FOUND: DWORD = 2;
pub const ERROR_PATH_NOT_FOUND: DWORD = 3;
pub const DLL_PROCESS_ATTACH: DWORD = 1;
pub const HGDI_ERROR: HGDIOBJ = -1isize as HGDIOBJ;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LOGFONTA {
    pub lfHeight: i32,
    pub lfWidth: i32,
    pub lfEscapement: i32,
    pub lfOrientation: i32,
    pub lfWeight: i32,
    pub lfItalic: u8,
    pub lfUnderline: u8,
    pub lfStrikeOut: u8,
    pub lfCharSet: u8,
    pub lfOutPrecision: u8,
    pub lfClipPrecision: u8,
    pub lfQuality: u8,
    pub lfPitchAndFamily: u8,
    pub lfFaceName: [u8; LF_FACESIZE],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LOGFONTW {
    pub lfHeight: i32,
    pub lfWidth: i32,
    pub lfEscapement: i32,
    pub lfOrientation: i32,
    pub lfWeight: i32,
    pub lfItalic: u8,
    pub lfUnderline: u8,
    pub lfStrikeOut: u8,
    pub lfCharSet: u8,
    pub lfOutPrecision: u8,
    pub lfClipPrecision: u8,
    pub lfQuality: u8,
    pub lfPitchAndFamily: u8,
    pub lfFaceName: [u16; LF_FACESIZE],
}

#[repr(C)]
#[derive(Default)]
pub struct TEXTMETRICW {
    pub tmHeight: i32,
    pub tmAscent: i32,
    pub tmDescent: i32,
    pub tmInternalLeading: i32,
    pub tmExternalLeading: i32,
    pub tmAveCharWidth: i32,
    pub tmMaxCharWidth: i32,
    pub tmWeight: i32,
    pub tmOverhang: i32,
    pub tmDigitizedAspectX: i32,
    pub tmDigitizedAspectY: i32,
    pub tmFirstChar: u16,
    pub tmLastChar: u16,
    pub tmDefaultChar: u16,
    pub tmBreakChar: u16,
    pub tmItalic: u8,
    pub tmUnderlined: u8,
    pub tmStruckOut: u8,
    pub tmPitchAndFamily: u8,
    pub tmCharSet: u8,
}

#[repr(C)]
#[derive(Default)]
pub struct SIZE {
    pub cx: i32,
    pub cy: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct XFORM {
    pub eM11: f32,
    pub eM12: f32,
    pub eM21: f32,
    pub eM22: f32,
    pub eDx: f32,
    pub eDy: f32,
}

#[repr(C)]
pub struct GdiplusStartupInput {
    pub GdiplusVersion: u32,
    pub DebugEventCallback: *mut c_void,
    pub SuppressBackgroundThread: BOOL,
    pub SuppressExternalCodecs: BOOL,
}

impl Default for GdiplusStartupInput {
    fn default() -> Self {
        Self {
            GdiplusVersion: 1,
            DebugEventCallback: std::ptr::null_mut(),
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        }
    }
}

// These assertions make accidental SDK layout changes fail at compile time.
const _: () = {
    assert!(size_of::<usize>() == 4);
    assert!(size_of::<GdiplusStartupInput>() == 16);
    assert!(size_of::<LOGFONTA>() == 60);
    assert!(size_of::<LOGFONTW>() == 92);
    assert!(std::mem::offset_of!(LOGFONTA, lfFaceName) == 28);
    assert!(std::mem::offset_of!(LOGFONTW, lfFaceName) == 28);
    assert!(size_of::<TEXTMETRICW>() == 60);
    assert!(size_of::<XFORM>() == 24);
};

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetModuleFileNameW(module: HMODULE, filename: *mut u16, size: DWORD) -> DWORD;
    pub fn GetModuleHandleW(name: *const u16) -> HMODULE;
    pub fn GetProcAddress(module: HMODULE, name: *const c_char) -> *mut c_void;
    pub fn LoadLibraryW(name: *const u16) -> HMODULE;
    pub fn GetPrivateProfileStringW(
        section: *const u16,
        key: *const u16,
        default: *const u16,
        value: *mut u16,
        size: DWORD,
        filename: *const u16,
    ) -> DWORD;
    pub fn GetFileAttributesW(filename: *const u16) -> DWORD;
    pub fn GetLastError() -> DWORD;
    pub fn CompareStringOrdinal(
        left: *const u16,
        left_count: i32,
        right: *const u16,
        right_count: i32,
        ignore_case: BOOL,
    ) -> i32;
    pub fn MultiByteToWideChar(
        code_page: u32,
        flags: DWORD,
        input: *const u8,
        input_count: i32,
        output: *mut u16,
        output_count: i32,
    ) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn GetMapMode(dc: HDC) -> i32;
    pub fn GetLayout(dc: HDC) -> DWORD;
    pub fn GetWindowExtEx(dc: HDC, size: *mut SIZE) -> BOOL;
    pub fn GetViewportExtEx(dc: HDC, size: *mut SIZE) -> BOOL;
    pub fn GetGraphicsMode(dc: HDC) -> i32;
    pub fn GetWorldTransform(dc: HDC, transform: *mut XFORM) -> BOOL;
    pub fn GetDeviceCaps(dc: HDC, index: i32) -> i32;
    pub fn CreateCompatibleDC(dc: HDC) -> HDC;
    pub fn DeleteDC(dc: HDC) -> BOOL;
    pub fn CreateFontIndirectW(font: *const LOGFONTW) -> HFONT;
    pub fn SelectObject(dc: HDC, object: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteObject(object: HGDIOBJ) -> BOOL;
    pub fn GetTextMetricsW(dc: HDC, metrics: *mut TEXTMETRICW) -> BOOL;
    pub fn GetCurrentObject(dc: HDC, object_type: u32) -> HGDIOBJ;
    pub fn GetObjectW(object: HGDIOBJ, size: i32, output: *mut c_void) -> i32;
    pub fn SetMapMode(dc: HDC, mode: i32) -> i32;
    pub fn SetGraphicsMode(dc: HDC, mode: i32) -> i32;
    pub fn SetWorldTransform(dc: HDC, transform: *const XFORM) -> BOOL;
    pub fn ModifyWorldTransform(dc: HDC, transform: *const XFORM, mode: DWORD) -> BOOL;
}

// DLL imports always call the gdiplus.dll IAT, even when this cdylib exports
// functions with the same names. Undecorated imports match the system DLL ABI.
#[link(name = "gdiplus", kind = "raw-dylib", import_name_type = "undecorated")]
unsafe extern "system" {
    pub fn GdiplusStartup(
        token: *mut usize,
        input: *const GdiplusStartupInput,
        output: *mut c_void,
    ) -> GpStatus;
    pub fn GdiplusShutdown(token: usize);
    pub fn GdipNewPrivateFontCollection(collection: *mut *mut GpFontCollection) -> GpStatus;
    pub fn GdipPrivateAddFontFile(
        collection: *mut GpFontCollection,
        filename: *const u16,
    ) -> GpStatus;
    pub fn GdipGetFontCollectionFamilyCount(
        collection: *mut GpFontCollection,
        count: *mut i32,
    ) -> GpStatus;
    pub fn GdipGetFontCollectionFamilyList(
        collection: *mut GpFontCollection,
        count: i32,
        families: *mut *mut GpFontFamily,
        found: *mut i32,
    ) -> GpStatus;
    pub fn GdipGetFamilyName(family: *mut GpFontFamily, name: *mut u16, language: u16) -> GpStatus;
    pub fn GdipCreateFontFamilyFromName(
        name: *const u16,
        collection: *mut GpFontCollection,
        family: *mut *mut GpFontFamily,
    ) -> GpStatus;
    pub fn GdipCreateFont(
        family: *mut GpFontFamily,
        size: f32,
        style: i32,
        unit: i32,
        font: *mut *mut GpFont,
    ) -> GpStatus;
    pub fn GdipCreateFontFromLogfontW(
        dc: HDC,
        request: *const LOGFONTW,
        font: *mut *mut GpFont,
    ) -> GpStatus;
    pub fn GdipCreateFontFromLogfontA(
        dc: HDC,
        request: *const LOGFONTA,
        font: *mut *mut GpFont,
    ) -> GpStatus;
    pub fn GdipCreateFontFromDC(dc: HDC, font: *mut *mut GpFont) -> GpStatus;
    pub fn GdipGetFamily(font: *mut GpFont, family: *mut *mut GpFontFamily) -> GpStatus;
    pub fn GdipCloneFontFamily(
        family: *mut GpFontFamily,
        clone: *mut *mut GpFontFamily,
    ) -> GpStatus;
    pub fn GdipDeleteFontFamily(family: *mut GpFontFamily) -> GpStatus;
    pub fn GdipDeleteFont(font: *mut GpFont) -> GpStatus;
    pub fn GdipGetFontSize(font: *mut GpFont, size: *mut f32) -> GpStatus;
    pub fn GdipGetFontStyle(font: *mut GpFont, style: *mut i32) -> GpStatus;
    pub fn GdipGetFontUnit(font: *mut GpFont, unit: *mut i32) -> GpStatus;
}
