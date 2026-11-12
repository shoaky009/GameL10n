use crate::{
    capture, emit,
    protocol::{
        CheckEvent, CheckMode, CheckRequest, CheckStatus, ErrorWindow, Observation, SceneStep,
        normalize_text,
    },
};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        System::{
            Diagnostics::Debug::{
                SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, SetErrorMode,
            },
            Diagnostics::ToolHelp::*,
            JobObjects::*,
            Threading::*,
            WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Input::KeyboardAndMouse::*,
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCWSTR, PWSTR},
};

pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

struct Session {
    job: Handle,
    process: Handle,
}
impl Session {
    fn launch(request: &CheckRequest) -> Result<Self, Observation> {
        if !request.executable.is_file() || !request.working_directory.is_dir() {
            return Err(Observation::new(
                CheckStatus::Failed,
                "启动程序或工作目录不存在。",
            ));
        }
        let mut file = std::fs::File::open(&request.executable).map_err(|e| {
            Observation::new(CheckStatus::Inconclusive, format!("无法读取启动程序：{e}"))
        })?;
        let invalid = || {
            Observation::new(
                CheckStatus::Failed,
                "启动程序不是有效的 Windows EXE，无法启动。",
            )
        };
        let mut header = [0; 64];
        file.read_exact(&mut header).map_err(|_| invalid())?;
        if &header[..2] != b"MZ" {
            return Err(invalid());
        }
        let offset = u32::from_le_bytes(header[60..64].try_into().map_err(|_| invalid())?);
        if offset > 32 * 1024 * 1024 {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(u64::from(offset)))
            .map_err(|_| invalid())?;
        let mut signature = [0; 4];
        file.read_exact(&mut signature).map_err(|_| invalid())?;
        if signature != *b"PE\0\0" {
            return Err(invalid());
        }
        let executable = wide(request.executable.as_os_str());
        let directory = wide(request.working_directory.as_os_str());
        let mut command = wide(OsStr::new(&format!(
            "\"{}\" {}",
            request.executable.display(),
            request.arguments
        )));
        unsafe {
            let job = Handle(CreateJobObjectW(None, PCWSTR::null()).map_err(|e| {
                Observation::new(CheckStatus::Inconclusive, format!("无法跟踪游戏进程：{e}"))
            })?);
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            let mut information = PROCESS_INFORMATION::default();
            CreateProcessW(
                PCWSTR(executable.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED,
                None,
                PCWSTR(directory.as_ptr()),
                &startup,
                &mut information,
            )
            .map_err(|e| Observation::new(CheckStatus::Failed, format!("游戏无法启动：{e}")))?;
            let process = Handle(information.hProcess);
            let primary_thread = Handle(information.hThread);
            if let Err(error) = AssignProcessToJobObject(job.0, process.0) {
                let _ = TerminateProcess(process.0, 1);
                return Err(Observation::new(
                    CheckStatus::Inconclusive,
                    format!("无法跟踪启动器及游戏子进程：{error}"),
                ));
            }
            if ResumeThread(primary_thread.0) == u32::MAX {
                let _ = TerminateProcess(process.0, 1);
                return Err(Observation::new(
                    CheckStatus::Inconclusive,
                    "无法继续启动游戏。",
                ));
            }
            // No kill-on-close limit: finishing a check leaves the game open for inspection.
            Ok(Self { job, process })
        }
    }

    fn processes(&self) -> Result<BTreeSet<u32>, String> {
        // Two u32 counts precede the pointer-sized PID array; usize storage keeps it aligned.
        let mut buffer = [0usize; 258];
        unsafe {
            QueryInformationJobObject(
                Some(self.job.0),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                None,
            )
            .map_err(|e| format!("无法查询游戏子进程：{e}"))?;
            let list = &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST);
            let count = list.NumberOfProcessIdsInList as usize;
            if count > 256 {
                return Err("游戏启动的进程数量超出检查范围。".into());
            }
            let ids = std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count);
            Ok(ids.iter().map(|id| *id as u32).collect())
        }
    }

    fn exit_code(&self) -> u32 {
        let mut code = 0;
        unsafe {
            let _ = GetExitCodeProcess(self.process.0, &mut code);
        }
        code
    }
}

fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut buffer = vec![0; 32768];
        let mut count = buffer.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut count,
        )
        .ok()?;
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            &buffer[..count as usize],
        )))
    }
}

fn belongs_to_game(path: &Path, request: &CheckRequest) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let root = request
        .game_directory
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_lowercase()
        + "\\";
    path.starts_with(&root) || path == request.executable.to_string_lossy().to_lowercase()
}

fn game_processes(request: &CheckRequest) -> Result<BTreeSet<u32>, String> {
    unsafe {
        let snapshot = Handle(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|e| format!("无法查询游戏进程：{e}"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ids = BTreeSet::new();
        let mut next = Process32FirstW(snapshot.0, &mut entry);
        while next.is_ok() {
            if process_path(entry.th32ProcessID).is_some_and(|path| belongs_to_game(&path, request))
            {
                ids.insert(entry.th32ProcessID);
            }
            next = Process32NextW(snapshot.0, &mut entry);
        }
        Ok(ids)
    }
}

struct Windows {
    pids: BTreeSet<u32>,
    found: Vec<HWND>,
    errors: Vec<ErrorWindow>,
}

fn error_dialog(window: HWND) -> bool {
    unsafe {
        let mut class = [0u16; 128];
        let length = GetClassNameW(window, &mut class);
        if String::from_utf16_lossy(&class[..length as usize]) != "#32770" {
            return false;
        }
        let mut title = [0u16; 256];
        let length = GetWindowTextW(window, &mut title);
        let title = String::from_utf16_lossy(&title[..length as usize]).to_lowercase();
        [
            "错误",
            "error",
            "runtime library",
            "debug library",
            "assertion",
            "fatal",
            "exception",
            "不支持",
            "无法启动",
            "崩溃",
            "エラー",
        ]
        .iter()
        .any(|text| title.contains(text))
    }
}

fn process_started(pid: u32) -> Option<u64> {
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
        Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

pub fn close_error_windows(windows: &[ErrorWindow]) {
    for record in windows {
        let window = HWND(record.window as usize as *mut _);
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid != record.process_id
                || process_started(pid) != Some(record.process_started)
                || process_path(pid).as_ref() != Some(&record.executable)
                || !error_dialog(window)
            {
                continue;
            }
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}
unsafe extern "system" fn collect_window(window: HWND, data: LPARAM) -> BOOL {
    // EnumWindows invokes synchronously with a valid, exclusively borrowed collection.
    let windows = unsafe { &mut *(data.0 as *mut Windows) };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
        let mut rect = RECT::default();
        if windows.pids.contains(&pid)
            && IsWindowVisible(window).as_bool()
            && error_dialog(window)
            && let Some((process_started, executable)) = process_started(pid).zip(process_path(pid))
        {
            windows.errors.push(ErrorWindow {
                window: window.0 as usize as u64,
                process_id: pid,
                process_started,
                executable,
            });
        }
        let mut class = [0u16; 128];
        let length = if windows.pids.contains(&pid) {
            GetClassNameW(window, &mut class)
        } else {
            0
        };
        if windows.pids.contains(&pid)
            && IsWindowVisible(window).as_bool()
            && IsWindowEnabled(window).as_bool()
            && String::from_utf16_lossy(&class[..length as usize]) != "#32770"
            && GetWindow(window, GW_OWNER).is_err()
            && GetClientRect(window, &mut rect).is_ok()
            && rect.right >= 240
            && rect.bottom >= 120
        {
            windows.found.push(window);
        }
    }
    BOOL(1)
}

fn windows_for(pids: BTreeSet<u32>) -> Result<Windows, String> {
    let mut windows = Windows {
        pids,
        found: vec![],
        errors: vec![],
    };
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM((&mut windows as *mut Windows) as isize),
        )
        .map_err(|e| format!("无法查询游戏窗口：{e}"))?;
    }
    Ok(windows)
}

fn actual_game_window(window: HWND, request: &CheckRequest) -> bool {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }
    process_path(pid).is_some_and(|path| {
        let value = path.to_string_lossy().to_lowercase();
        if let Some(expected) = &request.game_executable {
            value == expected.to_string_lossy().to_lowercase()
        } else {
            let root = request
                .game_directory
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_lowercase()
                + "\\";
            value.starts_with(&root)
                && !path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("launcher")
        }
    })
}

fn observe_launch(request: &CheckRequest) -> Result<Option<(Session, HWND)>, String> {
    if !game_processes(request)?.is_empty() {
        emit(CheckEvent::Launch(Observation::new(
            CheckStatus::Inconclusive,
            "游戏已在运行，请关闭后再检查启动；当前画面仍可检查译文。",
        )))?;
        return Ok(None);
    }
    let session = match Session::launch(request) {
        Ok(session) => session,
        Err(error) => {
            emit(CheckEvent::Launch(error))?;
            return Ok(None);
        }
    };
    let deadline =
        Instant::now() + Duration::from_secs(u64::from(request.settings.timeout_seconds));
    let mut observed = None;
    let mut stable_since = Instant::now();
    loop {
        let pids = session.processes()?;
        if pids.is_empty() {
            emit(CheckEvent::Launch(Observation::new(
                if session.exit_code() != 0
                    || observed.is_some()
                    || request.game_executable.as_ref() == Some(&request.executable)
                {
                    CheckStatus::Failed
                } else {
                    CheckStatus::Inconclusive
                },
                format!(
                    "游戏启动后退出（启动进程退出码：{}），请在游戏中检查。",
                    session.exit_code()
                ),
            )))?;
            return Ok(None);
        }
        let windows = windows_for(pids)?;
        if !windows.errors.is_empty() {
            for window in windows.errors {
                emit(CheckEvent::ErrorWindow(window))?;
            }
            emit(CheckEvent::Launch(Observation::new(
                CheckStatus::Failed,
                "检测到游戏错误窗口，请查看游戏提示；关闭运行检查时会清理该错误窗口。",
            )))?;
            return Ok(None);
        }
        let actual: Vec<_> = windows
            .found
            .into_iter()
            .filter(|window| actual_game_window(*window, request))
            .collect();
        if let [window] = actual.as_slice() {
            if observed != Some(*window) {
                observed = Some(*window);
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= Duration::from_secs(3)
                && !unsafe { IsHungAppWindow(*window) }.as_bool()
            {
                emit(CheckEvent::Launch(Observation::new(
                    CheckStatus::Passed,
                    "游戏窗口已出现，观察期间未退出。",
                )))?;
                return Ok(Some((session, *window)));
            }
        } else {
            observed = None;
        }
        if Instant::now() >= deadline {
            emit(CheckEvent::Launch(Observation::new(
                CheckStatus::Inconclusive,
                "未能确认唯一且稳定的游戏窗口，请在游戏中检查启动情况。",
            )))?;
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn focus(window: HWND) -> Result<(), String> {
    unsafe {
        if !IsWindow(Some(window)).as_bool() || IsIconic(window).as_bool() {
            return Err("游戏窗口已关闭或最小化，请手动打开后检查。".into());
        }
        if GetForegroundWindow() != window {
            return Err("游戏未处于前台，已停止操作，请切到游戏后重试。".into());
        }
        if [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| GetAsyncKeyState(i32::from(key.0)) < 0)
        {
            return Err("检测到用户按住修饰键，已停止操作，请松开后重试。".into());
        }
    }
    Ok(())
}

fn send_key(window: HWND, key: &str) -> Result<(), String> {
    focus(window)?;
    let key = match key {
        "Enter" => VK_RETURN,
        "Space" => VK_SPACE,
        "Escape" => VK_ESCAPE,
        "Up" => VK_UP,
        "Down" => VK_DOWN,
        "Left" => VK_LEFT,
        "Right" => VK_RIGHT,
        "Tab" => VK_TAB,
        _ => return Err("不支持的按键。".into()),
    };
    if unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0 {
        return Err("待发送的按键正被按住，已停止操作。".into());
    }
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                ..Default::default()
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: KEYEVENTF_KEYUP,
                ..Default::default()
            },
        },
    };
    unsafe {
        if SendInput(&[down, up], size_of::<INPUT>() as i32) != 2 {
            return Err("无法向游戏发送按键，请检查窗口权限。".into());
        }
    }
    Ok(())
}

fn click(window: HWND, x: u16, y: u16) -> Result<(), String> {
    focus(window)?;
    unsafe {
        if GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 {
            return Err("鼠标左键正被按住，已停止操作。".into());
        }
        let mut area = RECT::default();
        GetClientRect(window, &mut area).map_err(|e| e.to_string())?;
        let mut point = POINT {
            x: (area.right * i32::from(x) / 100).min(area.right - 1),
            y: (area.bottom * i32::from(y) / 100).min(area.bottom - 1),
        };
        ClientToScreen(window, &mut point)
            .ok()
            .map_err(|e| e.to_string())?;
        if WindowFromPoint(point) != window
            && GetAncestor(WindowFromPoint(point), GA_ROOT) != window
        {
            return Err("点击位置被其他窗口遮挡，已停止操作。".into());
        }
        SetCursorPos(point.x, point.y).map_err(|e| e.to_string())?;
        if GetForegroundWindow() != window {
            return Err("游戏失去焦点，已停止操作。".into());
        }
        let down = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: MOUSEEVENTF_LEFTDOWN,
                    ..Default::default()
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: MOUSEEVENTF_LEFTUP,
                    ..Default::default()
                },
            },
        };
        if SendInput(&[down, up], size_of::<INPUT>() as i32) != 2 {
            return Err("无法向游戏发送点击，请检查窗口权限。".into());
        }
    }
    Ok(())
}

fn runtime_error(session: Option<&Session>) -> Result<bool, String> {
    let Some(session) = session else {
        return Ok(false);
    };
    let windows = windows_for(session.processes()?)?;
    if windows.errors.is_empty() {
        return Ok(false);
    }
    for window in windows.errors {
        emit(CheckEvent::ErrorWindow(window))?;
    }
    emit(CheckEvent::Launch(Observation::new(
        CheckStatus::Failed,
        "游戏出现错误窗口，请查看游戏提示；关闭运行检查时会清理该错误窗口。",
    )))?;
    Ok(true)
}

fn scene(request: &CheckRequest, window: HWND, session: Option<&Session>) -> Result<(), String> {
    let mut target_pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut target_pid));
    }
    let started = process_started(target_pid);
    if request
        .settings
        .steps
        .iter()
        .any(|s| !matches!(s, SceneStep::Wait { .. }))
    {
        unsafe {
            let _ = SetForegroundWindow(window);
        }
        thread::sleep(Duration::from_millis(150));
        focus(window)?;
    }
    for step in &request.settings.steps {
        if runtime_error(session)? {
            return Err("游戏出现错误窗口，已停止场景操作。".into());
        }
        let mut current_pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut current_pid));
        }
        if current_pid != target_pid || started.is_none() || process_started(current_pid) != started
        {
            return Err("游戏窗口身份已变化，已停止操作。".into());
        }
        match step {
            SceneStep::Wait { milliseconds } => {
                thread::sleep(Duration::from_millis(u64::from(*milliseconds)))
            }
            SceneStep::Key { key } => send_key(window, key)?,
            SceneStep::Click { x, y } => click(window, *x, *y)?,
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn check_text(
    request: &CheckRequest,
    window: HWND,
    session: Option<&Session>,
) -> Result<Observation, String> {
    if request.expected.is_empty() {
        return Err("请先选择需要检查的译文。".into());
    }
    let expected: Vec<_> = request.expected.iter().map(|s| normalize_text(s)).collect();
    let mut previous = String::new();
    let mut previous_match = None;
    let mut readable = false;
    let mut stable = false;
    for _ in 0..4 {
        if runtime_error(session)? {
            return Err("游戏出现错误窗口，译文检查未完成。".into());
        }
        if unsafe { !IsWindow(Some(window)).as_bool() || IsIconic(window).as_bool() } {
            return Err("游戏窗口已关闭或最小化，无法检查译文。".into());
        }
        let captured = capture::capture(window, &request.settings.region)?;
        let recognized = capture::recognize(captured, &request.settings.language)?;
        let text = normalize_text(&recognized);
        readable |= !text.is_empty();
        stable |= !text.is_empty() && text == previous;
        let matched = expected
            .iter()
            .position(|expected| !expected.is_empty() && text.contains(expected));
        if matched.is_some() && matched == previous_match {
            return Ok(Observation::new(
                CheckStatus::Passed,
                "已在游戏画面中检测到所选译文。",
            ));
        }
        previous_match = matched;
        previous = text;
        thread::sleep(Duration::from_millis(400));
    }
    Ok(if !readable {
        Observation::new(
            CheckStatus::Inconclusive,
            "当前画面未识别到可用文本，请进入对白或调整文本区域后重试。",
        )
    } else if !stable {
        Observation::new(
            CheckStatus::Inconclusive,
            "画面文字尚不稳定，请等待对白显示完整后重试。",
        )
    } else {
        Observation::new(
            CheckStatus::Failed,
            "未检测到所选译文，请在游戏中查看当前画面。",
        )
    })
}

pub fn run(request: &CheckRequest) -> Result<(), String> {
    unsafe {
        let _ =
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let launch = if request.mode.launches() {
        observe_launch(request)?
    } else {
        None
    };
    if request.mode.launches() && launch.is_none() {
        return Ok(());
    }
    if !request.mode.reads_text() {
        return Ok(());
    }
    let window = if let Some((_, window)) = &launch {
        *window
    } else {
        let windows = windows_for(game_processes(request)?)?;
        let actual: Vec<_> = windows
            .found
            .into_iter()
            .filter(|window| actual_game_window(*window, request))
            .collect();
        match actual.as_slice() {
            [window] => *window,
            _ => {
                emit(CheckEvent::Text(Observation::new(
                    CheckStatus::Inconclusive,
                    "未找到唯一的游戏窗口，请打开游戏并进入待检查画面。",
                )))?;
                return Ok(());
            }
        }
    };
    let result = (|| {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)
                .map_err(|e| format!("无法使用系统识别组件：{e}"))?;
        }
        if request.mode == CheckMode::LaunchAndText {
            scene(request, window, launch.as_ref().map(|(session, _)| session))?;
        }
        check_text(request, window, launch.as_ref().map(|(session, _)| session))
    })();
    emit(CheckEvent::Text(result.unwrap_or_else(|error| {
        Observation::new(CheckStatus::Inconclusive, error)
    })))
}
