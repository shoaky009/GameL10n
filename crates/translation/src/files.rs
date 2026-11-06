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

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, FileError> {
    let input = File::open(path).map_err(|error| io_error(path, error))?;
    serde_json::from_reader(BufReader::new(input)).map_err(|source| FileError::Json {
        path: path.display().to_string(),
        source,
    })
}

/// Flushes a temporary file in the target directory, then atomically replaces the target.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), FileError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let mut temp = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, value).map_err(|source| FileError::Json {
            path: path.display().to_string(),
            source,
        })?;
        writer.flush().map_err(|error| io_error(path, error))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|error| io_error(path, error))?;
    temp.persist(path)
        .map_err(|error| io_error(path, error.error))?;
    Ok(())
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
    let mut buffer = [0_u8; 64 * 1024];
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
