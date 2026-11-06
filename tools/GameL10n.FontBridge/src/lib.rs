// SPDX-License-Identifier: MIT
//! Windows x86 GDI+ font bridge for a configured private font collection.

#![deny(unsafe_op_in_unsafe_fn)]

mod ffi;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use ffi::*;

static MODULE_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static CONFIGURATION: OnceLock<Result<Configuration, GpStatus>> = OnceLock::new();
static FAMILY_LOCK: Mutex<()> = Mutex::new(());
static PUBLISHED_PRIVATE_FAMILY: AtomicPtr<GpFontFamily> = AtomicPtr::new(null_mut());

// Neither this static nor its native objects are dropped. In particular, DLL
// teardown runs under the loader lock and must not destroy GDI+ resources.
struct Configuration {
    face: Vec<u16>,
    match_face: Vec<u16>,
    replace_matching: bool,
    font_status: GpStatus,
    startup_token: usize,
    collection: *mut GpFontCollection,
    family: *mut GpFontFamily,
}

// SAFETY: Initialization finishes before OnceLock publishes this immutable
// configuration. All subsequent native access to its family is serialized by
// FAMILY_LOCK; the collection and startup token remain alive for the process.
unsafe impl Send for Configuration {}
// SAFETY: The same publication, serialization, and lifetime rules apply when
// multiple callers borrow the configuration concurrently.
unsafe impl Sync for Configuration {}

const fn wide<const N: usize>(ascii: &[u8; N]) -> [u16; N] {
    let mut result = [0; N];
    let mut index = 0;
    while index < N {
        result[index] = ascii[index] as u16;
        index += 1;
    }
    result
}

fn zeroed_vec<T: Copy + Default>(length: usize) -> Result<Vec<T>, GpStatus> {
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|_| OutOfMemory)?;
    values.resize(length, T::default());
    Result::Ok(values)
}

fn joined_path(directory: &[u16], filename: &[u16]) -> Result<Vec<u16>, GpStatus> {
    let length = directory
        .len()
        .checked_add(filename.len())
        .and_then(|length| length.checked_add(1))
        .ok_or(OutOfMemory)?;
    let mut path = Vec::new();
    path.try_reserve_exact(length).map_err(|_| OutOfMemory)?;
    path.extend_from_slice(directory);
    path.extend_from_slice(filename);
    path.push(0);
    Result::Ok(path)
}

fn equal_face(left: &[u16], right: *const u16) -> bool {
    // SAFETY: Internal strings are NUL-terminated. The caller supplies a
    // checked LOGFONT face or a GDI+ family-name buffer as the right string.
    unsafe { CompareStringOrdinal(left.as_ptr(), -1, right, -1, 1) == CSTR_EQUAL }
}

fn trim_setting(buffer: &mut Vec<u16>, length: usize) {
    let whitespace = |value: &u16| matches!(*value, 32 | 9 | 13 | 10);
    let first = buffer[..length]
        .iter()
        .position(|value| !whitespace(value))
        .unwrap_or(length);
    let last = buffer[..length]
        .iter()
        .rposition(|value| !whitespace(value))
        .map_or(first, |position| position + 1);
    buffer.copy_within(first..last, 0);
    buffer.truncate(last - first);
    // GetPrivateProfileStringW leaves at least two spare elements on success.
    buffer.push(0);
}

fn read_setting(ini: &[u16], key: &[u16], default: &[u16]) -> Result<Vec<u16>, GpStatus> {
    let mut buffer = zeroed_vec::<u16>(32768)?;
    // SAFETY: All inputs include their terminator, and the writable output has
    // exactly the capacity passed to the Windows profile API.
    let length = unsafe {
        GetPrivateProfileStringW(
            wide(b"Font\0").as_ptr(),
            key.as_ptr(),
            default.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            ini.as_ptr(),
        )
    } as usize;
    if length >= buffer.len() - 1 {
        return Err(InvalidParameter);
    }
    trim_setting(&mut buffer, length);
    Result::Ok(buffer)
}

fn valid_relative_path(path: &mut [u16]) -> bool {
    if path.is_empty() || matches!(path[0], 47 | 92) {
        return false;
    }
    for value in path.iter_mut() {
        if matches!(*value, 58 | 60 | 62 | 34 | 124 | 63 | 42) {
            return false;
        }
        if *value == 47 {
            *value = 92;
        }
    }
    path.split(|value| *value == 92).all(|segment| {
        !segment.is_empty()
            && segment != [46]
            && segment != [46, 46]
            && !matches!(segment.last(), Some(46 | 32))
    })
}

fn file_error() -> GpStatus {
    // SAFETY: GetLastError has no pointer arguments or preconditions.
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => FileNotFound,
        _ => AccessDenied,
    }
}

fn initialize() -> Result<Configuration, GpStatus> {
    let mut module_path = zeroed_vec::<u16>(32768)?;
    // SAFETY: DllMain records the owning module before any export is called;
    // the output buffer matches the size supplied to Windows.
    let length = unsafe {
        GetModuleFileNameW(
            MODULE_HANDLE.load(Ordering::Relaxed),
            module_path.as_mut_ptr(),
            module_path.len() as u32,
        )
    } as usize;
    if length == 0 || length >= module_path.len() {
        return Err(GenericError);
    }
    module_path.truncate(length);
    let Some(slash) = module_path
        .iter()
        .rposition(|value| matches!(*value, 47 | 92))
    else {
        return Err(InvalidParameter);
    };
    module_path.truncate(slash + 1);
    let ini_name = wide(b"GameL10n.FontBridge.ini");
    let ini = joined_path(&module_path, &ini_name)?;
    // SAFETY: joined_path returns a NUL-terminated UTF-16 path.
    if unsafe { GetFileAttributesW(ini.as_ptr()) } == INVALID_FILE_ATTRIBUTES {
        return Err(file_error());
    }

    let mut filename = read_setting(&ini, &wide(b"File\0"), &wide(b"\0"))?;
    let face = read_setting(&ini, &wide(b"Face\0"), &wide(b"\0"))?;
    let match_face = read_setting(&ini, &wide(b"MatchFace\0"), &wide(b"\0"))?;
    let mode = read_setting(&ini, &wide(b"Mode\0"), &wide(b"fallback\0"))?;
    let filename_length = filename.len() - 1;
    if !valid_relative_path(&mut filename[..filename_length])
        || face.len() <= 1
        || face.len() > LF_FACESIZE
        || match_face.len() > LF_FACESIZE
    {
        return Err(InvalidParameter);
    }
    let replace_matching = if equal_face(&mode, wide(b"replace-matching\0").as_ptr()) {
        true
    } else if equal_face(&mode, wide(b"fallback\0").as_ptr()) {
        false
    } else {
        return Err(InvalidParameter);
    };
    let mut configuration = Configuration {
        face,
        match_face,
        replace_matching,
        font_status: InvalidParameter,
        startup_token: 0,
        collection: null_mut(),
        family: null_mut(),
    };
    let font_path = joined_path(&module_path, &filename[..filename_length])?;
    configuration.font_status = load_private_font(&mut configuration, &font_path);
    if configuration.font_status == Ok {
        PUBLISHED_PRIVATE_FAMILY.store(configuration.family, Ordering::Release);
    }
    Result::Ok(configuration)
}

fn load_private_font(configuration: &mut Configuration, filename: &[u16]) -> GpStatus {
    // SAFETY: filename is a NUL-terminated path constructed by joined_path.
    let attributes = unsafe { GetFileAttributesW(filename.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return file_error();
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return InvalidParameter;
    }

    // SAFETY: GDI+ receives correctly laid-out inputs and writable out-pointers.
    // Its token and collection intentionally remain alive even on later errors.
    unsafe {
        let status = GdiplusStartup(
            &mut configuration.startup_token,
            &GdiplusStartupInput::default(),
            null_mut(),
        );
        if status != Ok {
            return status;
        }
        let status = GdipNewPrivateFontCollection(&mut configuration.collection);
        if status != Ok {
            return status;
        }
        let status = GdipPrivateAddFontFile(configuration.collection, filename.as_ptr());
        if status != Ok {
            return status;
        }
        let mut count = 0;
        let status = GdipGetFontCollectionFamilyCount(configuration.collection, &mut count);
        if status != Ok {
            return status;
        }
        if count <= 0 {
            return FontFamilyNotFound;
        }
        let mut families = match zeroed_vec::<*mut GpFontFamily>(count as usize) {
            Result::Ok(families) => families,
            Err(status) => return status,
        };
        let mut found = 0;
        let status = GdipGetFontCollectionFamilyList(
            configuration.collection,
            count,
            families.as_mut_ptr(),
            &mut found,
        );
        if status != Ok {
            return status;
        }
        // Enumeration returns borrowed families; establish a retained reference
        // with CreateFontFamilyFromName rather than deleting enumeration entries.
        for &family in families.iter().take(found.max(0) as usize) {
            let mut name = [0; LF_FACESIZE];
            if configuration.family.is_null()
                && GdipGetFamilyName(family, name.as_mut_ptr(), 0) == Ok
                && equal_face(&configuration.face, name.as_ptr())
            {
                let status = GdipCreateFontFamilyFromName(
                    configuration.face.as_ptr(),
                    configuration.collection,
                    &mut configuration.family,
                );
                if status != Ok {
                    return status;
                }
            }
        }
    }
    if configuration.family.is_null() {
        FontFamilyNotFound
    } else {
        Ok
    }
}

fn configuration() -> Result<&'static Configuration, GpStatus> {
    CONFIGURATION
        .get_or_init(|| catch_unwind(initialize).unwrap_or(Err(GenericError)))
        .as_ref()
        .map_err(|status| *status)
}

fn validate_mapping(dc: HDC, request: &LOGFONTW) -> GpStatus {
    if dc.is_null() {
        return InvalidParameter;
    }
    if request.lfWidth != 0 || request.lfEscapement != 0 || request.lfOrientation != 0 {
        return NotImplemented;
    }
    // SAFETY: The HDC belongs to the caller and is only queried here. Windows
    // reports invalid handles through these APIs' return values.
    unsafe {
        if GetMapMode(dc) != MM_TEXT || GetLayout(dc) != 0 {
            return NotImplemented;
        }
        let mut window = SIZE::default();
        let mut viewport = SIZE::default();
        if GetWindowExtEx(dc, &mut window) == 0 || GetViewportExtEx(dc, &mut viewport) == 0 {
            return GenericError;
        }
        if window.cx != viewport.cx || window.cy != viewport.cy {
            return NotImplemented;
        }
        let mode = GetGraphicsMode(dc);
        if mode == 0 {
            return GenericError;
        }
        if mode == GM_ADVANCED {
            let mut transform = XFORM::default();
            if GetWorldTransform(dc, &mut transform) == 0 {
                return GenericError;
            }
            if transform.eM11 != 1.0
                || transform.eM12 != 0.0
                || transform.eM21 != 0.0
                || transform.eM22 != 1.0
                || transform.eDx != 0.0
                || transform.eDy != 0.0
            {
                return NotImplemented;
            }
        }
        if GetDeviceCaps(dc, LOGPIXELSY) > 0 {
            Ok
        } else {
            NotImplemented
        }
    }
}

fn em_height(dc: HDC, request: &LOGFONTW) -> Result<f32, GpStatus> {
    // SAFETY: Each created GDI object belongs to this function. The previous
    // selection is restored before the font and DC are released on every path.
    unsafe {
        let metrics_dc = CreateCompatibleDC(dc);
        if metrics_dc.is_null() {
            return Err(GenericError);
        }
        let metrics_font = CreateFontIndirectW(request);
        if metrics_font.is_null() {
            DeleteDC(metrics_dc);
            return Err(GenericError);
        }
        let previous = SelectObject(metrics_dc, metrics_font);
        let mut metrics = TEXTMETRICW::default();
        let valid_previous = !previous.is_null() && previous != HGDI_ERROR;
        let measured = valid_previous && GetTextMetricsW(metrics_dc, &mut metrics) != 0;
        if valid_previous {
            SelectObject(metrics_dc, previous);
        }
        DeleteObject(metrics_font);
        DeleteDC(metrics_dc);
        if !measured || metrics.tmHeight <= metrics.tmInternalLeading {
            return Err(GenericError);
        }
        // Positive LOGFONT heights include internal leading; negative heights
        // specify the em. GDI resolves height zero and device rounding as well.
        Result::Ok((metrics.tmHeight - metrics.tmInternalLeading) as f32)
    }
}

unsafe fn create_private_font(
    configuration: &Configuration,
    dc: HDC,
    request: &LOGFONTW,
    font: *mut *mut GpFont,
) -> GpStatus {
    if font.is_null() {
        return InvalidParameter;
    }
    // SAFETY: The export contract requires a writable out-pointer when nonnull.
    unsafe { *font = null_mut() };
    if configuration.font_status != Ok {
        return configuration.font_status;
    }
    let status = validate_mapping(dc, request);
    if status != Ok {
        return status;
    }
    let size = match em_height(dc, request) {
        Result::Ok(size) => size,
        Err(status) => return status,
    };
    let mut style = FontStyleRegular;
    if request.lfWeight >= FW_BOLD {
        style |= FontStyleBold;
    }
    if request.lfItalic != 0 {
        style |= FontStyleItalic;
    }
    if request.lfUnderline != 0 {
        style |= FontStyleUnderline;
    }
    if request.lfStrikeOut != 0 {
        style |= FontStyleStrikeout;
    }
    let _guard = FAMILY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // SAFETY: The retained family is valid for the process lifetime and all
    // shared-family access is serialized. The caller owns the returned font.
    unsafe { GdipCreateFont(configuration.family, size, style, UnitPixel, font) }
}

unsafe fn dispatch(
    dc: HDC,
    request: &LOGFONTW,
    font: *mut *mut GpFont,
    original: impl FnOnce() -> GpStatus,
) -> GpStatus {
    let configuration = match configuration() {
        Result::Ok(configuration) => configuration,
        Err(status) => return status,
    };
    if !request.lfFaceName.contains(&0) {
        return InvalidParameter;
    }
    let matching = equal_face(&configuration.face, request.lfFaceName.as_ptr())
        || (configuration.match_face.len() > 1
            && equal_face(&configuration.match_face, request.lfFaceName.as_ptr()));
    if !matching {
        return original();
    }
    if !configuration.replace_matching {
        let status = original();
        if status != FontFamilyNotFound && status != NotTrueTypeFont {
            return status;
        }
    }
    // SAFETY: dispatch preserves the export's out-pointer contract.
    unsafe { create_private_font(configuration, dc, request, font) }
}

fn ffi_status(action: impl FnOnce() -> GpStatus) -> GpStatus {
    // Rust unwinding must never cross a Windows ABI boundary.
    catch_unwind(AssertUnwindSafe(action)).unwrap_or(GenericError)
}

/// Creates a font from a Unicode LOGFONT, applying the configured matching rule.
///
/// # Safety
/// Pointers and handles must satisfy GdipCreateFontFromLogfontW's ABI contract.
#[unsafe(export_name = "BridgeCreateFontFromLogfontW")]
unsafe extern "system" fn bridge_create_font_from_logfont_w(
    dc: HDC,
    request: *const LOGFONTW,
    font: *mut *mut GpFont,
) -> GpStatus {
    ffi_status(|| {
        // SAFETY: The caller supplies the native API's pointer contract; a null
        // request is forwarded to GDI+ without dereferencing it.
        unsafe {
            if request.is_null() {
                return GdipCreateFontFromLogfontW(dc, request, font);
            }
            dispatch(dc, &*request, font, || {
                GdipCreateFontFromLogfontW(dc, request, font)
            })
        }
    })
}

/// Creates a font from an ANSI LOGFONT using the process ANSI code page.
///
/// # Safety
/// Pointers and handles must satisfy GdipCreateFontFromLogfontA's ABI contract.
#[unsafe(export_name = "BridgeCreateFontFromLogfontA")]
unsafe extern "system" fn bridge_create_font_from_logfont_a(
    dc: HDC,
    request: *const LOGFONTA,
    font: *mut *mut GpFont,
) -> GpStatus {
    ffi_status(|| {
        // SAFETY: The nonnull LOGFONT is readable under the export contract.
        // Only the common SDK prefix is copied, before decoding its face name.
        unsafe {
            if request.is_null() {
                return GdipCreateFontFromLogfontA(dc, request, font);
            }
            let Some(end) = (*request).lfFaceName.iter().position(|value| *value == 0) else {
                return InvalidParameter;
            };
            let mut wide = LOGFONTW::default();
            ptr::copy_nonoverlapping(
                request.cast::<u8>(),
                (&mut wide as *mut LOGFONTW).cast::<u8>(),
                std::mem::offset_of!(LOGFONTA, lfFaceName),
            );
            if MultiByteToWideChar(
                CP_ACP,
                0,
                (*request).lfFaceName.as_ptr(),
                (end + 1) as i32,
                wide.lfFaceName.as_mut_ptr(),
                LF_FACESIZE as i32,
            ) == 0
            {
                return InvalidParameter;
            }
            dispatch(dc, &wide, font, || {
                GdipCreateFontFromLogfontA(dc, request, font)
            })
        }
    })
}

/// Creates a font from the current GDI font selected in a device context.
///
/// # Safety
/// Pointers and handles must satisfy GdipCreateFontFromDC's ABI contract.
#[unsafe(export_name = "BridgeCreateFontFromDC")]
unsafe extern "system" fn bridge_create_font_from_dc(dc: HDC, font: *mut *mut GpFont) -> GpStatus {
    ffi_status(|| {
        // SAFETY: The HDC belongs to the caller; GetObjectW writes into a
        // correctly sized LOGFONTW. Invalid DCs follow the original GDI+ path.
        unsafe {
            let mut request = LOGFONTW::default();
            let selected = if dc.is_null() {
                null_mut()
            } else {
                GetCurrentObject(dc, OBJ_FONT)
            };
            if selected.is_null()
                || GetObjectW(
                    selected,
                    size_of::<LOGFONTW>() as i32,
                    (&mut request as *mut LOGFONTW).cast(),
                ) != size_of::<LOGFONTW>() as i32
            {
                return GdipCreateFontFromDC(dc, font);
            }
            dispatch(dc, &request, font, || GdipCreateFontFromDC(dc, font))
        }
    })
}

/// Returns a caller-owned font-family reference, including for private fonts.
///
/// # Safety
/// Pointers must satisfy GdipGetFamily's ABI contract.
#[unsafe(export_name = "BridgeGetFamily")]
unsafe extern "system" fn bridge_get_family(
    font: *mut GpFont,
    family: *mut *mut GpFontFamily,
) -> GpStatus {
    ffi_status(|| {
        // SAFETY: The caller supplies native pointers. Read the family only
        // after GDI+ reports success and the out-pointer has been checked.
        unsafe {
            let status = GdipGetFamily(font, family);
            if status != Ok || family.is_null() || (*family).is_null() {
                return status;
            }
            let retained = PUBLISHED_PRIVATE_FAMILY.load(Ordering::Acquire);
            if retained.is_null() || *family != retained {
                return status;
            }
            // Windows can return the collection's borrowed family. Cloning
            // gives Font::GetFamily callers a reference they may safely delete.
            let mut owned = null_mut();
            let _guard = FAMILY_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let cloned = GdipCloneFontFamily(*family, &mut owned);
            *family = if cloned == Ok { owned } else { null_mut() };
            cloned
        }
    })
}

// SAFETY: The CRT invokes this exact Windows entry-point signature. It only
// records a handle and never initializes or releases GDI+ under the loader lock.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(instance: HMODULE, reason: DWORD, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE_HANDLE.store(instance, Ordering::Relaxed);
    }
    1
}
