//! Atomic JSON replacement without persistent backup files.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FileError {
    #[error("文件操作失败（{path}）：{source}")]
    Io { path: String, source: io::Error },
    #[error("JSON 数据无效（{path}）：{source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
}

pub fn io_error(path: &Path, source: io::Error) -> FileError {
    FileError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(windows)]
const WIN32_ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(windows)]
const WIN32_ERROR_SHARING_VIOLATION: i32 = 32;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

fn is_transient_windows_error(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(
            error.raw_os_error(),
            Some(WIN32_ERROR_ACCESS_DENIED | WIN32_ERROR_SHARING_VIOLATION)
        ) || error.kind() == io::ErrorKind::PermissionDenied
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

/// Replaces `path` atomically, retrying briefly on transient Windows sharing/access conflicts.
pub fn persist_temp(mut temp: NamedTempFile, path: &Path) -> Result<(), FileError> {
    const DELAYS_MS: [u64; 8] = [5, 10, 15, 25, 40, 60, 80, 100];
    for delay in DELAYS_MS {
        match temp.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) if is_transient_windows_error(&error.error) => {
                temp = error.file;
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            Err(error) => return Err(io_error(path, error.error)),
        }
    }
    temp.persist(path)
        .map(|_| ())
        .map_err(|error| io_error(path, error.error))
}

fn open_for_read(path: &Path) -> Result<File, FileError> {
    const DELAYS_MS: [u64; 3] = [5, 15, 30];
    for delay in DELAYS_MS {
        match File::open(path) {
            Ok(file) => return Ok(file),
            Err(error) if is_transient_windows_error(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            Err(error) => return Err(io_error(path, error)),
        }
    }
    File::open(path).map_err(|error| io_error(path, error))
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, FileError> {
    let input = open_for_read(path)?;
    serde_json::from_reader(BufReader::new(input)).map_err(|source| FileError::Json {
        path: path.display().to_string(),
        source,
    })
}

/// Flushes a temporary file in the target directory, then atomically replaces the target.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), FileError> {
    write_json_with(path, value, true)
}

/// Flushes compact JSON to a temporary file in the target directory, then atomically replaces the target.
/// Use for large data payloads (extractions, translation memory, edit snapshots) where indentation wastes space and I/O.
pub fn write_json_compact<T: Serialize>(path: &Path, value: &T) -> Result<(), FileError> {
    write_json_with(path, value, false)
}

fn write_json_with<T: Serialize>(path: &Path, value: &T, pretty: bool) -> Result<(), FileError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let mut temp = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        let result = if pretty {
            serde_json::to_writer_pretty(&mut writer, value)
        } else {
            serde_json::to_writer(&mut writer, value)
        };
        result.map_err(|source| FileError::Json {
            path: path.display().to_string(),
            source,
        })?;
        writer.flush().map_err(|error| io_error(path, error))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|error| io_error(path, error))?;
    persist_temp(temp, path)
}

/// A newline commits one record. An interrupted final record is ignored; complete bad records fail.
pub struct JsonLines<T> {
    reader: BufReader<File>,
    path: PathBuf,
    buffer: Vec<u8>,
    pub valid_bytes: u64,
    end: u64,
    marker: std::marker::PhantomData<T>,
}

impl<T: DeserializeOwned> JsonLines<T> {
    pub fn open(path: &Path) -> Result<Self, FileError> {
        Self::range(path, 0, u64::MAX)
    }
    pub fn range(path: &Path, start: u64, end: u64) -> Result<Self, FileError> {
        let mut file = File::open(path).map_err(|error| io_error(path, error))?;
        if end != u64::MAX
            && file
                .metadata()
                .map_err(|error| io_error(path, error))?
                .len()
                < end
        {
            return Err(io_error(
                path,
                io::Error::new(io::ErrorKind::InvalidData, "增量文件短于已提交位置"),
            ));
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|error| io_error(path, error))?;
        Ok(Self {
            reader: BufReader::new(file),
            path: path.into(),
            buffer: Vec::new(),
            valid_bytes: start,
            end,
            marker: std::marker::PhantomData,
        })
    }
}

impl<T: DeserializeOwned> Iterator for JsonLines<T> {
    type Item = Result<T, FileError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.valid_bytes >= self.end {
            return None;
        }
        self.buffer.clear();
        match self.reader.read_until(b'\n', &mut self.buffer) {
            Err(error) => return Some(Err(io_error(&self.path, error))),
            Ok(0) => return None,
            Ok(_) if self.buffer.last() != Some(&b'\n') => return None,
            Ok(_) => {}
        }
        let value = serde_json::from_slice(&self.buffer).map_err(|source| FileError::Json {
            path: self.path.display().to_string(),
            source,
        });
        if value.is_ok() {
            self.valid_bytes += self.buffer.len() as u64;
        }
        Some(value)
    }
}

/// Replaces any uncommitted tail, then durably appends a small batch as one JSON line.
pub fn append_json_line<T: Serialize>(
    path: &Path,
    committed_bytes: u64,
    value: &T,
) -> Result<u64, FileError> {
    let mut bytes = serde_json::to_vec(value).map_err(|source| FileError::Json {
        path: path.display().to_string(),
        source,
    })?;
    bytes.push(b'\n');
    append_bytes(path, committed_bytes, &bytes)
}

pub fn append_bytes(path: &Path, committed_bytes: u64, bytes: &[u8]) -> Result<u64, FileError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    if file
        .metadata()
        .map_err(|error| io_error(path, error))?
        .len()
        < committed_bytes
    {
        return Err(io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidData, "增量文件短于已提交位置"),
        ));
    }
    file.set_len(committed_bytes)
        .map_err(|error| io_error(path, error))?;
    file.seek(SeekFrom::Start(committed_bytes))
        .map_err(|error| io_error(path, error))?;
    file.write_all(bytes)
        .map_err(|error| io_error(path, error))?;
    file.sync_all().map_err(|error| io_error(path, error))?;
    Ok(committed_bytes + bytes.len() as u64)
}

pub fn hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

pub fn hash_file(path: &Path) -> Result<String, FileError> {
    let mut stream = File::open(path).map_err(|error| io_error(path, error))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let size = stream
            .read(&mut buffer)
            .map_err(|error| io_error(path, error))?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(hex_digest(&hash.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 15) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_json_compact_omits_formatting_whitespace_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.json");
        let payload = serde_json::json!({
            "schemaVersion": 2,
            "units": [{"id": "1", "sourceText": "hello\nworld"}]
        });
        write_json_compact(&path, &payload).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains('\n'));
        assert_eq!(read_json::<serde_json::Value>(&path).unwrap(), payload);
    }

    #[cfg(windows)]
    #[test]
    fn write_json_retries_while_target_is_briefly_open_for_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adaptation-progress.json");
        write_json(&path, &serde_json::json!({"stage": "initial"})).unwrap();

        let reader = File::open(&path).unwrap();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            drop(reader);
        });

        write_json(&path, &serde_json::json!({"stage": "updated"})).unwrap();
        handle.join().unwrap();

        let value: serde_json::Value = read_json(&path).unwrap();
        assert_eq!(value["stage"], "updated");
    }
}
