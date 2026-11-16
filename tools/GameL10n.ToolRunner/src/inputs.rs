//! Read-only input handles for processes in one AppContainer job.
use super::{Handle, Result, checked_path, display_path, inherit};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::windows::{
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};
use windows::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    Storage::FileSystem::{FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW},
    System::{
        JobObjects::{IsProcessInJob, TerminateJobObject},
        Pipes::CreatePipe,
        Threading::{
            GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

#[derive(Deserialize)]
struct Query {
    pid: u32,
    op: String,
    path: PathBuf,
}

fn pipe() -> Result<(File, File)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe {
        CreatePipe(&mut read, &mut write, None, 0)?;
        Ok((
            File::from_raw_handle(read.0),
            File::from_raw_handle(write.0),
        ))
    }
}

fn within(path: &Path, root: &Path) -> bool {
    Path::new(&display_path(path).to_lowercase()).starts_with(display_path(root).to_lowercase())
}

fn metadata(path: &Path) -> io::Result<Value> {
    let m = fs::symlink_metadata(path)?;
    if m.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Input links are not allowed",
        ));
    }
    Ok(
        json!({"size":m.len(), "directory":m.is_dir(), "attributes":m.file_attributes(),
        "created":m.creation_time(), "accessed":m.last_access_time(), "modified":m.last_write_time()}),
    )
}

fn answer(query: Query, roots: &[PathBuf], job: HANDLE) -> Result<Value> {
    let requested = std::path::absolute(&query.path)?;
    if !roots.iter().any(|root| within(&requested, root)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Input path is outside the authorized roots",
        )
        .into());
    }
    // Preserve missing-file errors for callers that probe optional companion files.
    fs::symlink_metadata(&query.path)?;
    let path = checked_path(&query.path)?;
    if !roots.iter().any(|root| within(&path, root)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Input path is outside the authorized roots",
        )
        .into());
    }
    match query.op.as_str() {
        "stat" => Ok(metadata(&path)?),
        "list" => {
            let mut entries = Vec::new();
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                // Directory entries do not grant access to link targets.
                if entry.file_type()?.is_symlink()
                    || entry.metadata()?.file_attributes() & 0x400 != 0
                {
                    continue;
                }
                entries.push(json!({"name":entry.file_name().to_string_lossy(), "stat":metadata(&entry.path())?}));
            }
            Ok(json!(entries))
        }
        "open" => {
            // Each open owns a new file position. The child can never upgrade this handle to write access.
            let file = File::open(&path)?;
            if !file.metadata()?.is_file() {
                return Err("Input is not a regular file".into());
            }
            let mut final_path = vec![0u16; 32768];
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    HANDLE(file.as_raw_handle()),
                    &mut final_path,
                    FILE_NAME_NORMALIZED,
                )
            } as usize;
            if length == 0 || length >= final_path.len() {
                return Err(io::Error::last_os_error().into());
            }
            let final_path = PathBuf::from(String::from_utf16(&final_path[..length])?);
            if !roots.iter().any(|root| within(&final_path, root)) {
                return Err("Input handle is outside the authorized roots".into());
            }
            let process = Handle(unsafe {
                OpenProcess(
                    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    query.pid,
                )?
            });
            let mut member = windows::core::BOOL::default();
            unsafe { IsProcessInJob(process.0, Some(job), &mut member)? };
            if !member.as_bool() {
                return Err("Input handle recipient is outside the execution job".into());
            }
            let mut handle = HANDLE::default();
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    HANDLE(file.as_raw_handle()),
                    process.0,
                    &mut handle,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )?
            };
            Ok(json!({"handle":handle.0 as usize}))
        }
        _ => Err("Unsupported input operation".into()),
    }
}

pub(super) struct Inputs {
    pub handles: Vec<Handle>,
    pub environment: String,
    job: usize,
    worker: Option<JoinHandle<()>>,
}

impl Inputs {
    pub fn new(paths: &[PathBuf], temporary: &Path, job: HANDLE) -> Result<Self> {
        let mut paths = paths.iter().collect::<Vec<_>>();
        paths.sort_by_key(|path| path.components().count());
        let mut roots: Vec<PathBuf> = Vec::new();
        for path in paths {
            let absolute = std::path::absolute(path)?;
            if !roots.iter().any(|root| within(&absolute, root)) {
                roots.push(checked_path(path)?);
            }
        }
        let (requests, write) = pipe()?;
        let (read, mut responses) = pipe()?;
        let write = inherit(HANDLE(write.as_raw_handle()))?;
        let read = inherit(HANDLE(read.as_raw_handle()))?;
        let lock = temporary.join("input.lock");
        fs::write(&lock, [0])?;
        let settings = json!({"request":write.0.0 as usize, "response":read.0.0 as usize,
            "lock":lock, "roots":roots.iter().map(|p|display_path(p)).collect::<Vec<_>>()})
        .to_string();
        let configuration = temporary.join("input-handles.json");
        fs::write(&configuration, settings)?;
        let environment = display_path(&configuration);
        let job = job.0 as usize;
        let worker = thread::spawn(move || {
            let mut requests = BufReader::new(requests);
            loop {
                let mut line = String::new();
                match (&mut requests).take(65537).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    _ if line.len() > 65536 => break,
                    _ => {}
                }
                let result = serde_json::from_str::<Query>(&line)
                    .map_err(Into::into)
                    .and_then(|query| answer(query, &roots, HANDLE(job as _)));
                let response = match result {
                    Ok(value) => json!({"result":value}),
                    Err(error) => {
                        json!({"error":error.to_string(), "code":error.downcast_ref::<io::Error>().and_then(io::Error::raw_os_error).unwrap_or(5)})
                    }
                };
                let bytes = response.to_string().into_bytes();
                if responses
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .is_err()
                    || responses.write_all(&bytes).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            handles: vec![write, read],
            environment,
            job,
            worker: Some(worker),
        })
    }
}

impl Drop for Inputs {
    fn drop(&mut self) {
        // Closing every client pipe also releases a server blocked by a failed or cancelled child.
        unsafe {
            let _ = TerminateJobObject(HANDLE(self.job as _), 1);
        }
        self.handles.clear();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::JobObjects::CreateJobObjectW;
    use windows::core::PCWSTR;

    #[test]
    fn input_requests_cannot_escape_roots_upgrade_access_or_target_another_job() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("input.bin");
        fs::write(&file, b"original").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];
        let job = Handle(unsafe { CreateJobObjectW(None, PCWSTR::null()).unwrap() });
        for (op, path) in [
            ("open", file.clone()),
            ("write", file.clone()),
            ("stat", root.path().join("../outside")),
        ] {
            assert!(
                answer(
                    Query {
                        pid: std::process::id(),
                        op: op.into(),
                        path
                    },
                    &roots,
                    job.0
                )
                .is_err()
            );
        }
        let error = answer(
            Query {
                pid: std::process::id(),
                op: "stat".into(),
                path: root.path().join("missing"),
            },
            &roots,
            job.0,
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read(file).unwrap(), b"original");
    }
}
