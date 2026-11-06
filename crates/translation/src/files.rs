//! Atomic JSON replacement without persistent backup files.

use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
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
