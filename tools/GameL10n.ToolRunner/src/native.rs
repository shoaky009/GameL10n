use std::{
    collections::BTreeMap,
    ffi::{OsStr, c_void},
    fs,
    io::{self, BufRead, Read, Seek, SeekFrom, Write},
    mem::{size_of, size_of_val},
    os::windows::{ffi::OsStrExt, fs::MetadataExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HLOCAL, LocalFree,
            WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
        },
        Security::{
            ACL,
            Authorization::{
                EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, REVOKE_ACCESS,
                SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_W,
            },
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, FreeSid, GetTokenInformation,
            Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
            OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, TOKEN_QUERY,
            TokenIsAppContainer,
        },
        Storage::FileSystem::{
            DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        },
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW,
                CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ReleaseMutex,
                ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

#[path = "inputs.rs"]
mod inputs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    protocol_version: u32,
    executable: PathBuf,
    arguments: Vec<String>,
    directory: Option<PathBuf>,
    read_paths: Vec<PathBuf>,
    #[serde(default)]
    input_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    input: serde_json::Value,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Allocation(*mut c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

struct AclLock(Handle);
impl AclLock {
    fn acquire() -> Result<Self> {
        let name = wide(OsStr::new("Local\\GameL10n.ToolRunner.ACL"));
        let handle = Handle(unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr()))? });
        match unsafe { WaitForSingleObject(handle.0, 30_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            _ => Err("等待目录权限更新超时。".into()),
        }
    }
}
impl Drop for AclLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0.0);
        }
    }
}

struct Container {
    name: String,
    sid: PSID,
    grants: Vec<PathBuf>,
}
impl Container {
    fn create() -> Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let name = format!("GameL10n.Run.{}.{nonce}", std::process::id());
        let profile_name = wide(OsStr::new(&name));
        let sid = unsafe {
            CreateAppContainerProfile(
                PCWSTR(profile_name.as_ptr()),
                PCWSTR(profile_name.as_ptr()),
                PCWSTR(profile_name.as_ptr()),
                None,
            )?
        };
        Ok(Self {
            name,
            sid,
            grants: Vec::new(),
        })
    }

    fn grant(&mut self, path: &Path, writable: bool) -> Result<()> {
        let path = checked_path(path)?;
        check_tree(&path)?;
        let mut permissions = (FILE_GENERIC_READ | FILE_GENERIC_EXECUTE).0;
        if writable {
            permissions |= (FILE_GENERIC_WRITE | DELETE).0;
        }
        // Also revoke a partially applied grant if Windows reports a propagation error.
        self.grants.push(path.clone());
        change_acl(&path, self.sid, permissions, false).map_err(|error| {
            format!(
                "执行前{}授权失败，脚本未启动：{}；{error}",
                if writable { "写入" } else { "读取" },
                display_path(&path)
            )
        })?;
        Ok(())
    }
}
impl Drop for Container {
    fn drop(&mut self) {
        match AclLock::acquire() {
            Ok(_lock) => {
                for path in self.grants.iter().rev() {
                    if path.exists()
                        && let Err(error) =
                            check_tree(path).and_then(|()| change_acl(path, self.sid, 0, true))
                    {
                        eprintln!("无法清理隔离目录授权 {}：{error}", display_path(path));
                    }
                }
            }
            Err(error) => eprintln!("无法清理隔离目录授权：{error}"),
        }
        let profile_name = wide(OsStr::new(&self.name));
        unsafe {
            if let Err(error) = DeleteAppContainerProfile(PCWSTR(profile_name.as_ptr())) {
                eprintln!("无法清理 AppContainer 配置：{error}");
            }
            FreeSid(self.sid);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

fn win32_result(status: WIN32_ERROR) -> io::Result<()> {
    if status.is_ok() {
        Ok(())
    } else {
        // These APIs return the error directly; COM error information may be stale.
        Err(io::Error::from_raw_os_error(status.0 as i32))
    }
}

fn checked_path(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    for ancestor in path.ancestors() {
        let metadata = ancestor
            .symlink_metadata()
            .map_err(|error| format!("无法检查隔离授权路径 {}：{error}", display_path(ancestor)))?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(format!("隔离授权路径不能使用重解析点：{}", display_path(ancestor)).into());
        }
    }
    Ok(path.canonicalize()?)
}

fn check_tree(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.path().symlink_metadata()?;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(format!(
                    "隔离授权目录不能包含重解析点：{}",
                    display_path(&entry.path())
                )
                .into());
            }
            if metadata.is_dir() {
                check_tree(&entry.path())?;
            }
        }
    }
    Ok(())
}

fn change_acl(path: &Path, sid: PSID, permissions: u32, revoke: bool) -> Result<()> {
    let name = wide(path.as_os_str());
    let mut old_acl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_acl),
            None,
            &mut descriptor,
        )
    };
    win32_result(status).map_err(|error| format!("读取访问权限失败：{error}"))?;
    let _descriptor = Allocation(descriptor.0);
    // A null DACL grants unrestricted access and is unsuitable for isolation.
    if old_acl.is_null() {
        return Err("该路径没有可用于隔离的访问控制列表。".into());
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: permissions,
        grfAccessMode: if revoke { REVOKE_ACCESS } else { GRANT_ACCESS },
        grfInheritance: if path.is_dir() {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            Default::default()
        },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    };
    let mut new_acl = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(Some(&[entry]), Some(old_acl), &mut new_acl) };
    win32_result(status).map_err(|error| format!("生成隔离访问权限失败：{error}"))?;
    let _new_acl = Allocation(new_acl.cast());
    let status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        )
    };
    win32_result(status).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            format!("当前用户无权更改访问权限：{error}；请授予该路径的“更改权限”，或改用当前用户拥有的目录。")
        } else {
            format!("更新访问权限失败：{error}")
        }
    })?;
    Ok(())
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> Result<Self> {
        let mut bytes = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 2, None, &mut bytes);
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes)?;
        }
        Ok(Self {
            _storage: storage,
            list,
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

fn inherit(handle: HANDLE) -> Result<Handle> {
    let mut duplicate = HANDLE::default();
    unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            handle,
            process,
            &mut duplicate,
            0,
            true,
            DUPLICATE_SAME_ACCESS,
        )?;
    }
    Ok(Handle(duplicate))
}

fn quote(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if character == '"' {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

fn environment(executable: &Path, temporary: &Path, inputs: &str) -> Result<Vec<u16>> {
    let system = std::env::var_os("SystemRoot").ok_or("Windows 缺少 SystemRoot。")?;
    let system = PathBuf::from(system);
    let mut values = BTreeMap::new();
    values.insert("SystemRoot", system.as_os_str().to_owned());
    values.insert("WINDIR", system.as_os_str().to_owned());
    values.insert(
        "PATH",
        std::env::join_paths([
            executable.parent().ok_or("执行程序缺少目录。")?,
            &system.join("System32"),
            &system.join("System32/WindowsPowerShell/v1.0"),
        ])?,
    );
    values.insert("COMSPEC", system.join("System32/cmd.exe").into_os_string());
    for key in ["TEMP", "TMP", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        values.insert(key, temporary.as_os_str().to_owned());
    }
    values.insert("GAMEL10N_INPUT_HANDLES", inputs.into());
    values.insert("PYTHONUTF8", "1".into());
    values.insert("PYTHONDONTWRITEBYTECODE", "1".into());
    let mut block = Vec::new();
    for (key, value) in values {
        block.extend(OsStr::new(key).encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn execute(
    request: &Request,
    container: &Container,
    temporary: &Path,
    cancelled: &AtomicBool,
) -> Result<u32> {
    let mut input = tempfile::tempfile_in(temporary)?;
    serde_json::to_writer(&mut input, &request.input)?;
    input.flush()?;
    input.seek(SeekFrom::Start(0))?;
    let stdin = inherit(HANDLE(input.as_raw_handle()))?;
    let stdout = inherit(unsafe { GetStdHandle(STD_OUTPUT_HANDLE)? })?;
    let stderr = inherit(unsafe { GetStdHandle(STD_ERROR_HANDLE)? })?;
    let job = Handle(unsafe { CreateJobObjectW(None, PCWSTR::null())? });
    let mut inputs = inputs::Inputs::new(&request.input_paths, temporary, job.0)?;
    let mut handles = vec![stdin.0, stdout.0, stderr.0];
    handles.extend(inputs.handles.iter().map(|handle| handle.0));
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: container.sid,
        ..Default::default()
    };
    let attributes = Attributes::new()?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )?;
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some((&capabilities as *const SECURITY_CAPABILITIES).cast()),
            size_of_val(&capabilities),
            None,
            None,
        )?;
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(handles.as_ptr().cast()),
            std::mem::size_of_val(handles.as_slice()),
            None,
            None,
        )?;
    }
    let startup = STARTUPINFOEXW {
        StartupInfo: windows::Win32::System::Threading::STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: stdin.0,
            hStdOutput: stdout.0,
            hStdError: stderr.0,
            ..Default::default()
        },
        lpAttributeList: attributes.list,
    };
    let executable = wide(request.executable.as_os_str());
    let mut command = wide(OsStr::new(
        &std::iter::once(request.executable.to_string_lossy().into_owned())
            .chain(request.arguments.clone())
            .map(|argument| quote(&argument))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    let directory = wide(
        request
            .directory
            .as_deref()
            .unwrap_or(temporary)
            .as_os_str(),
    );
    let environment = environment(&request.executable, temporary, &inputs.environment)?;
    let mut information = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(directory.as_ptr()),
            &startup.StartupInfo,
            &mut information,
        )
        .map_err(|error| {
            format!(
                "无法在 AppContainer 中启动 {}：{error}",
                display_path(&request.executable)
            )
        })?;
    }
    inputs.handles.clear();
    let process = Handle(information.hProcess);
    let thread = Handle(information.hThread);
    let start = (|| -> Result<()> {
        unsafe {
            AssignProcessToJobObject(job.0, process.0)?;
            let mut token = HANDLE::default();
            OpenProcessToken(process.0, TOKEN_QUERY, &mut token)?;
            let token = Handle(token);
            let mut isolated = 0u32;
            let mut length = 0;
            GetTokenInformation(
                token.0,
                TokenIsAppContainer,
                Some((&mut isolated as *mut u32).cast()),
                size_of::<u32>() as u32,
                &mut length,
            )?;
            if isolated == 0 {
                return Err("子进程未进入 AppContainer。".into());
            }
            if ResumeThread(thread.0) == u32::MAX {
                return Err(windows::core::Error::from_thread().into());
            }
        }
        Ok(())
    })();
    if let Err(error) = start {
        unsafe {
            let _ = TerminateProcess(process.0, 1);
        }
        return Err(error);
    }
    loop {
        match unsafe { WaitForSingleObject(process.0, 100) } {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT if cancelled.load(Ordering::Acquire) => {
                unsafe { TerminateJobObject(job.0, 1)? };
                return Ok(1);
            }
            WAIT_TIMEOUT => continue,
            _ => return Err(windows::core::Error::from_thread().into()),
        }
    }
    let mut code = 1;
    unsafe {
        GetExitCodeProcess(process.0, &mut code)?;
        // Descendants must not survive a successful parent or retain the output pipes.
        TerminateJobObject(job.0, code)?;
    }
    if code >= 0x8000_0000 {
        // Preserve the child status in diagnostics without reporting a runner failure.
        eprintln!("工具进程异常结束，Windows 退出码为 0x{code:08X}。");
        return Ok(1);
    }
    Ok(code)
}

pub fn run() -> Result<u32> {
    let mut bytes = Vec::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut stdin = io::stdin().lock().take(64 * 1024 * 1024 + 1);
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--control-stdin")) {
        stdin.read_until(b'\n', &mut bytes)?;
        if bytes.pop() != Some(b'\n') {
            return Err("隔离执行请求不完整或超过大小限制。".into());
        }
        drop(stdin);
        let signal = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            let _ = io::stdin().read(&mut [0]);
            signal.store(true, Ordering::Release);
        });
    } else {
        stdin.read_to_end(&mut bytes)?;
    }
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("隔离执行请求超过大小限制。".into());
    }
    let mut request: Request = serde_json::from_slice(&bytes)?;
    if request.protocol_version != 1 {
        return Err("不支持的隔离执行协议。".into());
    }
    request.executable = checked_path(&request.executable)?;
    if !request.executable.is_file() {
        return Err("执行程序不存在。".into());
    }
    if let Some(directory) = &request.directory {
        request.directory = Some(checked_path(directory)?);
    }
    let temporary = tempfile::Builder::new()
        .prefix("gamel10n-isolated-")
        .tempdir()?;
    let mut container = Container::create()?;
    // Windows redirects TEMP beneath the supplied LOCALAPPDATA for this profile.
    fs::create_dir_all(
        temporary
            .path()
            .join("Packages")
            .join(&container.name)
            .join("AC/Temp"),
    )?;
    {
        let _lock = AclLock::acquire()?;
        let mut paths = BTreeMap::new();
        paths.insert(
            request
                .executable
                .parent()
                .ok_or("执行程序缺少目录。")?
                .to_path_buf(),
            false,
        );
        for path in &request.read_paths {
            paths.entry(checked_path(path)?).or_insert(false);
        }
        for path in &request.write_paths {
            paths.insert(checked_path(path)?, true);
        }
        paths.insert(temporary.path().to_path_buf(), true);
        let mut granted: Vec<(PathBuf, bool)> = Vec::new();
        for (path, writable) in paths {
            if cancelled.load(Ordering::Acquire) {
                return Ok(1);
            }
            if granted.iter().any(|(root, root_writable)| {
                path.starts_with(root) && (*root_writable || !writable)
            }) {
                continue;
            }
            container.grant(&path, writable)?;
            granted.push((path, writable));
        }
    }
    execute(&request, &container, temporary.path(), &cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_paths_use_drive_and_unc_notation() {
        for (input, expected) in [
            (r"\\?\I:\ゲーム\script.pac", r"I:\ゲーム\script.pac"),
            (r"\\?\UNC\server\share\ゲーム", r"\\server\share\ゲーム"),
            (r"I:\ゲーム\script.pac", r"I:\ゲーム\script.pac"),
        ] {
            assert_eq!(display_path(Path::new(input)), expected);
        }
    }

    #[test]
    fn acl_errors_use_returned_status_despite_stale_com_error_information() {
        use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND};

        let stale =
            windows::core::Error::new(ERROR_FILE_NOT_FOUND.to_hresult(), "unrelated missing file");
        let _: windows::core::HRESULT = stale.into();
        let error = win32_result(ERROR_ACCESS_DENIED).unwrap_err();
        let captured = ERROR_ACCESS_DENIED.ok().unwrap_err();
        assert_eq!(captured.message(), "unrelated missing file");
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            io::Error::from_raw_os_error(5).to_string()
        );
    }

    #[test]
    fn quoting_preserves_empty_arguments_quotes_and_trailing_slashes() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("中文 path\\"), "\"中文 path\\\\\"");
        assert_eq!(quote("a\\\"b"), "\"a\\\\\\\"b\"");
    }
}
