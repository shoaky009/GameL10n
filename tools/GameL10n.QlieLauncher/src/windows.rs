use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    thread::sleep,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    Globalization::{CP_ACP, GetACP, MultiByteToWideChar},
    Graphics::Gdi::{
        AddFontResourceExW, CreateCompatibleDC, DeleteDC, ENUMLOGFONTEXA, EnumFontFamiliesExA, HDC,
        LOGFONTA, RemoveFontResourceExW, SHIFTJIS_CHARSET, TEXTMETRICA,
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
            JobObjectAssociateCompletionPortInformation, SetInformationJobObject,
        },
        Registry::{
            HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
            RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
        },
        Threading::{
            CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess, INFINITE, OpenProcess,
            PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            QueryFullProcessImageNameW, ResumeThread, STARTUPINFOW, WaitForSingleObject,
        },
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
    start_and_wait(&program, &arguments, directory, &game, elevated)
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

const JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO: u32 = 4;
const JOB_OBJECT_MSG_NEW_PROCESS: u32 = 6;
const TARGET_APPEAR_TIMEOUT: Duration = Duration::from_secs(15);

struct KernelHandle(HANDLE);

impl Drop for KernelHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: This wrapper owns one live kernel handle closed exactly once on drop.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn start_and_wait(
    program: &Path,
    arguments: &[OsString],
    directory: &Path,
    target_game: &Path,
    elevated: bool,
) -> Result<i32> {
    let target_key = normalized_image_key(target_game);
    let indirect = normalized_image_key(program) != target_key;
    let pre_existing = if indirect {
        matching_pids(&target_key)
    } else {
        BTreeSet::new()
    };
    if !elevated {
        return start_and_wait_in_job(
            program,
            arguments,
            directory,
            indirect,
            &target_key,
            &pre_existing,
        );
    }
    let program_w = wide(program);
    let directory_w = wide(directory);
    let verb = wide("runas");
    let parameters = command_line(arguments);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: program_w.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory_w.as_ptr(),
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
    let process = KernelHandle(info.hProcess);
    let mut code = 0;
    // SAFETY: process owns a valid waitable handle until both calls finish.
    unsafe {
        if WaitForSingleObject(process.0, INFINITE) != WAIT_OBJECT_0
            || GetExitCodeProcess(process.0, &mut code) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    // Locale Emulator's elevated LEProc helper exits as soon as it spawns the game.
    // Keep session fonts registered until the launched game process itself exits.
    if code == 0 && indirect {
        wait_for_target_process(&target_key, &pre_existing);
    }
    Ok(code as i32)
}

fn start_and_wait_in_job(
    program: &Path,
    arguments: &[OsString],
    directory: &Path,
    indirect: bool,
    target_key: &str,
    pre_existing: &BTreeSet<u32>,
) -> Result<i32> {
    let mut cmdline = process_command_line(program, arguments);
    let program_w = wide(program);
    let directory_w = wide(directory);
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut proc_info = PROCESS_INFORMATION::default();
    // SAFETY: All strings and structs remain live across CreateProcessW; cmdline is mutable as required.
    let created = unsafe {
        CreateProcessW(
            program_w.as_ptr(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_SUSPENDED,
            null(),
            directory_w.as_ptr(),
            &startup,
            &mut proc_info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let process = KernelHandle(proc_info.hProcess);
    let thread = KernelHandle(proc_info.hThread);
    // Attach a Job Object before resuming so indirect launchers (e.g. LEProc -> LEProc -> game)
    // keep the job non-empty until the game itself exits, preventing premature font unloading.
    let tracked = attach_job_completion_port(process.0);
    // SAFETY: thread owns the suspended primary thread returned by CreateProcessW.
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        return Err(io::Error::last_os_error().into());
    }
    drop(thread);

    let mut saw_target = false;
    if let Some((_job, iocp)) = tracked {
        loop {
            let mut message = 0u32;
            let mut key = 0usize;
            let mut overlapped: *mut OVERLAPPED = null_mut();
            // SAFETY: iocp is a valid completion port owned by this loop; pointers are writable.
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    iocp.0,
                    &mut message,
                    &mut key,
                    &mut overlapped,
                    INFINITE,
                )
            };
            if ok == 0 {
                break;
            }
            if message == JOB_OBJECT_MSG_NEW_PROCESS && indirect {
                let pid = overlapped as usize as u32;
                if pid_image_key(pid).as_deref() == Some(target_key) {
                    saw_target = true;
                }
            } else if message == JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO {
                break;
            }
        }
    }
    let mut code = 0u32;
    // SAFETY: process owns a valid handle from CreateProcessW.
    unsafe {
        if WaitForSingleObject(process.0, INFINITE) != WAIT_OBJECT_0
            || GetExitCodeProcess(process.0, &mut code) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    if code == 0 && indirect && !saw_target {
        wait_for_target_process(target_key, pre_existing);
    }
    Ok(code as i32)
}

fn attach_job_completion_port(process: HANDLE) -> Option<(KernelHandle, KernelHandle)> {
    // SAFETY: Null attributes/name create an anonymous job object owned by KernelHandle.
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return None;
    }
    let job = KernelHandle(job);
    // SAFETY: INVALID_HANDLE_VALUE creates an unbound I/O completion port owned by KernelHandle.
    let iocp = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) };
    if iocp.is_null() {
        return None;
    }
    let iocp = KernelHandle(iocp);
    let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: job.0,
        CompletionPort: iocp.0,
    };
    // SAFETY: Both handles are valid and association matches the requested information class size.
    let linked = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectAssociateCompletionPortInformation,
            (&raw const association).cast(),
            size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
        )
    };
    if linked == 0 {
        return None;
    }
    // SAFETY: job and process are live handles; process is suspended before assignment.
    if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
        return None;
    }
    Some((job, iocp))
}

fn normalized_image_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy();
    let stripped = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
    stripped.replace('/', "\\").to_lowercase()
}

fn pid_image_key(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: OpenProcess returns null on failure; valid handles are wrapped in KernelHandle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let handle = KernelHandle(handle);
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer holds `length` writable u16 units and handle has query rights.
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
    Some(normalized_image_key(&path))
}

fn matching_pids(target_key: &str) -> BTreeSet<u32> {
    let mut pids = BTreeSet::new();
    // SAFETY: TH32CS_SNAPPROCESS requires no process ID; returned handle is owned by KernelHandle.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return pids;
    }
    let snapshot = KernelHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: entry.dwSize matches PROCESSENTRY32W; snapshot is a valid toolhelp snapshot.
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0 {
        loop {
            if pid_image_key(entry.th32ProcessID).as_deref() == Some(target_key) {
                pids.insert(entry.th32ProcessID);
            }
            // SAFETY: entry and snapshot remain valid for the next iteration.
            if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
                break;
            }
        }
    }
    pids
}

fn wait_for_target_process(target_key: &str, pre_existing: &BTreeSet<u32>) {
    let deadline = Instant::now() + TARGET_APPEAR_TIMEOUT;
    let mut watched = BTreeSet::new();
    while Instant::now() < deadline {
        watched = matching_pids(target_key)
            .difference(pre_existing)
            .copied()
            .collect();
        if !watched.is_empty() {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    if watched.is_empty() {
        return;
    }
    let mut unwaitable = BTreeSet::new();
    for &pid in &watched {
        // SAFETY: OpenProcess returns null if access is denied (e.g. across elevation); otherwise owned.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            unwaitable.insert(pid);
        } else {
            let handle = KernelHandle(handle);
            // SAFETY: handle has PROCESS_SYNCHRONIZE and stays alive across the wait.
            unsafe {
                WaitForSingleObject(handle.0, INFINITE);
            }
        }
    }
    while !unwaitable.is_empty() {
        let alive = matching_pids(target_key);
        unwaitable.retain(|pid| alive.contains(pid));
        if !unwaitable.is_empty() {
            sleep(Duration::from_millis(500));
        }
    }
}

fn process_command_line(program: &Path, arguments: &[OsString]) -> Vec<u16> {
    let mut output = Vec::new();
    push_quoted_arg(&mut output, program.as_os_str(), true);
    for argument in arguments {
        output.push(b' ' as u16);
        let needs_quotes = argument.is_empty()
            || argument
                .encode_wide()
                .any(|unit| matches!(unit, 0x20 | 0x09 | 0x22));
        push_quoted_arg(&mut output, argument, needs_quotes);
    }
    output.push(0);
    output
}

fn push_quoted_arg(output: &mut Vec<u16>, argument: &OsStr, quote: bool) {
    if !quote {
        output.extend(argument.encode_wide());
        return;
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

fn command_line(arguments: &[OsString]) -> Vec<u16> {
    let mut output = Vec::new();
    for (i, argument) in arguments.iter().enumerate() {
        if i != 0 {
            output.push(b' ' as u16);
        }
        push_quoted_arg(&mut output, argument, true);
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
        let path = Path::new(&command);
        assert_eq!(
            start_and_wait(
                path,
                &["/d".into(), "/c".into(), "exit 37".into()],
                Path::new("."),
                path,
                false
            )
            .unwrap(),
            37
        );
    }

    #[test]
    fn job_object_waits_for_detached_grandchild_after_helper_exits() {
        let command = std::env::var_os("COMSPEC").expect("Windows command processor");
        let path = Path::new(&command);
        let started = Instant::now();
        // cmd.exe spawns a background ping.exe and exits immediately (just like LEProc.exe);
        // the Job Object must keep waiting until ping.exe completes so fonts are not unloaded early.
        let code = start_and_wait(
            path,
            &[
                "/d".into(),
                "/c".into(),
                "start /b ping -n 2 127.0.0.1 >nul".into(),
            ],
            Path::new("."),
            path,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(
            started.elapsed() >= Duration::from_millis(800),
            "expected job wait to hold until grandchild exited, got {:?}",
            started.elapsed()
        );
    }
}
