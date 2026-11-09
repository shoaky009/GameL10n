use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Command,
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    Globalization::{CP_ACP, GetACP, MultiByteToWideChar},
    Graphics::Gdi::{
        AddFontResourceExW, CreateCompatibleDC, DeleteDC, ENUMLOGFONTEXA, EnumFontFamiliesExA, HDC,
        LOGFONTA, RemoveFontResourceExW, SHIFTJIS_CHARSET, TEXTMETRICA,
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Registry::{
            HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
            RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
        },
        Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    },
    UI::{
        Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
            OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::{MB_ICONERROR, MessageBoxW, SW_SHOWNORMAL},
    },
};

use crate::{
    Result,
    config::{self, LaunchConfig, ProfileKind},
    font,
};

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn show_error(message: &str) {
    let message = wide(message);
    let title = wide("汉化启动失败");
    // SAFETY: Both strings are terminated and live until the synchronous dialog returns.
    unsafe {
        MessageBoxW(null_mut(), message.as_ptr(), title.as_ptr(), MB_ICONERROR);
    }
}

pub fn launch(args: &[OsString]) -> Result<i32> {
    let _apartment = ComApartment::initialize()?;
    let explicit_le = match args {
        [] => None,
        [flag, path] if flag == "--locale-emulator" => Some(std::path::absolute(path)?),
        _ => return Err("用法：启动汉化.exe [--locale-emulator LEProc.exe]".into()),
    };
    let executable = std::env::current_exe()?;
    let root = executable.parent().ok_or("无法确定启动器目录。")?;
    let config = config::launch_config(&config::read_xml(&root.join("qlie-l10n/launch.xml"))?)?;
    let game = config::under_root(root, &config.executable)?;
    if !game.is_file() {
        return Err("未找到原游戏程序，请先将汉化补丁安装到完整游戏目录。".into());
    }
    let directory = game.parent().ok_or("原游戏程序路径无效。")?;
    let mut profile_path = game.as_os_str().to_os_string();
    profile_path.push(".le.config");
    let profile_path = PathBuf::from(profile_path);
    let mut program = game.clone();
    let mut arguments = Vec::new();
    let mut elevated = false;
    // SAFETY: GetACP has no pointer arguments or initialization requirements.
    if profile_path.is_file() || unsafe { GetACP() } != 932 {
        program = match explicit_le {
            Some(path) => path,
            None => find_locale_emulator()?,
        };
        if !program.is_file() {
            return Err("未找到 Locale Emulator 的 LEProc.exe。".into());
        }
        let kind = if profile_path.is_file() {
            ProfileKind::Game
        } else {
            ProfileKind::Global
        };
        let path = if matches!(kind, ProfileKind::Game) {
            profile_path
        } else {
            program
                .parent()
                .ok_or("Locale Emulator 路径无效。")?
                .join("LEConfig.xml")
        };
        let profile = config::locale_profile(&config::read_xml(&path)?, kind)?;
        elevated = profile.elevated;
        if let Some(guid) = profile.guid {
            arguments.extend([OsString::from("-runas"), OsString::from(guid)]);
        } else {
            arguments.push(OsString::from("-run"));
        }
        arguments.push(game.as_os_str().to_owned());
    }
    let _fonts = RegisteredFonts::load(root, &config)?;
    start_and_wait(&program, &arguments, directory, elevated)
}

struct RegisteredFonts(Vec<Vec<u16>>);

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self> {
        // SAFETY: The launch thread has no COM objects yet; this matches the shell's STA requirements.
        let status = unsafe {
            CoInitializeEx(
                null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        if status < 0 {
            return Err(format!("无法初始化 Windows 启动组件：{:08X}", status as u32).into());
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: Drop runs on the thread with one successful matching initialization.
        unsafe {
            CoUninitialize();
        }
    }
}

impl RegisteredFonts {
    fn load(root: &Path, config: &LaunchConfig) -> Result<Self> {
        let mut loaded = Self(Vec::new());
        for entry in &config.fonts {
            let path = config::under_root(root, &entry.file)?;
            font::verify_font_metadata(&mut File::open(&path)?, &entry.face)?;
            let path = wide(&path);
            // Flags 0 make the font visible to the game and Locale Emulator in this session.
            // SAFETY: The path is terminated and GDI copies its contents before returning.
            if unsafe { AddFontResourceExW(path.as_ptr(), 0, null()) } == 0 {
                return Err(format!("无法加载汉化字体：{}", entry.file).into());
            }
            loaded.0.push(path);
        }
        verify_font_names(config)?;
        Ok(loaded)
    }
}

impl Drop for RegisteredFonts {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            // SAFETY: Each entry records one successful matching registration and remains terminated.
            unsafe {
                RemoveFontResourceExW(path.as_ptr(), 0, null());
            }
        }
    }
}

struct DeviceContext(HDC);

impl Drop for DeviceContext {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns a non-null compatible DC, released exactly once.
        unsafe {
            DeleteDC(self.0);
        }
    }
}

fn verify_font_names(config: &LaunchConfig) -> Result<()> {
    let mut missing: BTreeSet<_> = config
        .fonts
        .iter()
        .map(|font| font.face.to_lowercase())
        .collect();
    // SAFETY: A null source requests a memory DC owned by this function.
    let handle = unsafe { CreateCompatibleDC(null_mut()) };
    if handle.is_null() {
        return Err("无法检查汉化字体。".into());
    }
    let dc = DeviceContext(handle);
    let query = LOGFONTA {
        lfCharSet: SHIFTJIS_CHARSET,
        ..Default::default()
    };
    // SAFETY: EnumFontFamiliesExA calls the callback synchronously; missing is exclusively
    // borrowed until it returns, and the DC/query have the required GDI layouts.
    unsafe {
        EnumFontFamiliesExA(
            dc.0,
            &query,
            Some(enumerate_font),
            (&mut missing as *mut BTreeSet<String>) as isize,
            0,
        );
    }
    if !missing.is_empty() {
        return Err(format!(
            "游戏无法识别汉化字体，请重新生成补丁：{}",
            missing.into_iter().collect::<Vec<_>>().join("、")
        )
        .into());
    }
    Ok(())
}

unsafe extern "system" fn enumerate_font(
    font: *const LOGFONTA,
    _: *const TEXTMETRICA,
    _: u32,
    data: isize,
) -> i32 {
    if font.is_null() || data == 0 {
        return 1;
    }
    // SAFETY: EnumFontFamiliesExA supplies ENUMLOGFONTEXA, whose first member is LOGFONTA.
    let font = unsafe { &*font.cast::<ENUMLOGFONTEXA>() };
    if font.elfLogFont.lfCharSet != SHIFTJIS_CHARSET {
        return 1;
    }
    let length = font
        .elfFullName
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(font.elfFullName.len());
    let mut text = [0u16; 128];
    // QLIE compares the ANSI full name, not lfFaceName or a wide variable-instance name.
    // SAFETY: The source slice and fixed destination buffer cover the supplied lengths.
    let size = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            0,
            font.elfFullName.as_ptr(),
            length as i32,
            text.as_mut_ptr(),
            text.len() as i32,
        )
    };
    if size > 0 {
        // SAFETY: data is the exclusive, live set passed by verify_font_names.
        let missing = unsafe { &mut *(data as *mut BTreeSet<String>) };
        missing.remove(&String::from_utf16_lossy(&text[..size as usize]).to_lowercase());
    }
    1
}

fn find_locale_emulator() -> Result<PathBuf> {
    let key =
        wide("SOFTWARE\\Classes\\CLSID\\{C52B9871-E5E9-41FD-B84D-C5ACADBEC7AE}\\InprocServer32");
    let value = wide("CodeBase");
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY] {
            let mut data = vec![0u16; 32768];
            let mut size = (data.len() * 2) as u32;
            // SAFETY: Predefined hives are borrowed. The buffers and byte count match REG_SZ.
            let status = unsafe {
                RegGetValueW(
                    hive,
                    key.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | view,
                    null_mut(),
                    data.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            if status != 0 {
                continue;
            }
            let end = data
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(data.len());
            let Some(path) = file_uri_path(&String::from_utf16_lossy(&data[..end])) else {
                continue;
            };
            let Some(directory) = path.parent() else {
                continue;
            };
            let launcher = directory.join("LEProc.exe");
            if launcher.is_file() {
                return Ok(launcher);
            }
        }
    }
    pick_locale_emulator()
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    if !uri.get(..7)?.eq_ignore_ascii_case("file://") {
        return None;
    }
    let path = uri.get(7..)?.split(['?', '#']).next()?;
    let path = if path.starts_with('/') {
        path.strip_prefix('/')?.to_owned()
    } else {
        format!("//{path}")
    };
    let mut bytes = Vec::new();
    let mut chars = path.bytes();
    while let Some(byte) = chars.next() {
        let byte = if byte == b'%' {
            let first = char::from(chars.next()?).to_digit(16)?;
            let second = char::from(chars.next()?).to_digit(16)?;
            (first * 16 + second) as u8
        } else {
            byte
        };
        if byte == 0 {
            return None;
        }
        bytes.push(byte);
    }
    let path = PathBuf::from(String::from_utf8(bytes).ok()?);
    path.is_absolute().then_some(path)
}

fn pick_locale_emulator() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let filter = wide("LEProc.exe\0LEProc.exe\0");
    let title = wide("选择 Locale Emulator 的 LEProc.exe");
    let mut picker = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: All strings and the writable buffer remain live during the modal dialog.
    if unsafe { GetOpenFileNameW(&mut picker) } != 0 {
        let length = buffer
            .iter()
            .position(|&unit| unit == 0)
            .ok_or("转区程序路径过长。")?;
        return Ok(PathBuf::from(OsString::from_wide(&buffer[..length])));
    }
    // SAFETY: The last common-dialog error is read on the same thread immediately after failure.
    let error = unsafe { CommDlgExtendedError() };
    if error != 0 {
        return Err(format!("无法打开 Locale Emulator 文件选择窗口：{error:X}").into());
    }
    Err("已取消选择 Locale Emulator。".into())
}

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the process handle returned by ShellExecuteExW.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn start_and_wait(
    program: &Path,
    arguments: &[OsString],
    directory: &Path,
    elevated: bool,
) -> Result<i32> {
    if !elevated {
        let status = Command::new(program)
            .args(arguments)
            .current_dir(directory)
            .status()?;
        return status.code().ok_or_else(|| "游戏进程未返回退出码。".into());
    }
    let program = wide(program);
    let directory = wide(directory);
    let verb = wide("runas");
    let parameters = command_line(arguments);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: program.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: The structure and terminated strings live throughout the synchronous shell call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if info.hProcess.is_null() {
        return Err("游戏进程未能启动。".into());
    }
    let process = ProcessHandle(info.hProcess);
    let mut code = 0;
    // SAFETY: process owns a valid waitable handle until both calls finish.
    unsafe {
        if WaitForSingleObject(process.0, INFINITE) != WAIT_OBJECT_0
            || GetExitCodeProcess(process.0, &mut code) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(code as i32)
}

fn command_line(arguments: &[OsString]) -> Vec<u16> {
    let mut output = Vec::new();
    for (i, argument) in arguments.iter().enumerate() {
        if i != 0 {
            output.push(b' ' as u16);
        }
        output.push(b'"' as u16);
        let mut slashes = 0;
        for unit in argument.encode_wide() {
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            output.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            output.push(unit);
        }
        output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        output.push(b'"' as u16);
    }
    output.push(0);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_file_uris_decode_spaces_and_unicode() {
        assert_eq!(
            file_uri_path("file:///C:/Program%20Files/%E6%97%A5/LEContextMenu.dll"),
            Some(PathBuf::from("C:/Program Files/日/LEContextMenu.dll"))
        );
    }

    #[test]
    fn registry_rejects_non_file_or_invalid_encoded_uris() {
        for uri in [
            "https://example.com/a.dll",
            "file:///C:/bad%00.dll",
            "file:///C:/bad%GG.dll",
            "file:///relative.dll",
        ] {
            assert!(file_uri_path(uri).is_none(), "{uri}");
        }
    }

    #[test]
    fn elevated_command_line_quotes_spaces_backslashes_and_quotes() {
        let arguments = [
            OsString::from("-run"),
            OsString::from("C:\\游戏 files\\game.exe"),
            OsString::from("quoted\"value\\"),
        ];
        let encoded = command_line(&arguments);
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "\"-run\" \"C:\\游戏 files\\game.exe\" \"quoted\\\"value\\\\\""
        );
    }

    #[test]
    fn direct_launch_waits_and_preserves_exit_status() {
        let command = std::env::var_os("COMSPEC").expect("Windows command processor");
        assert_eq!(
            start_and_wait(
                Path::new(&command),
                &["/d".into(), "/c".into(), "exit 37".into()],
                Path::new("."),
                false
            )
            .unwrap(),
            37
        );
    }
}
