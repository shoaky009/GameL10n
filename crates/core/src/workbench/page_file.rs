//! Framed workbench pages. Each section carries its own checksum so random reads need only verify one page.
use super::{RowState, WorkbenchSummary};
use crate::CoreError;
use gamel10n_translation::{TranslationUnit, ValidationIssue};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};
use tempfile::NamedTempFile;

pub(super) const SIZE: usize = 100;
pub(crate) const DATA: &str = "pages.bin";
pub(crate) const INDEX: &str = "pages.idx";
pub(crate) const TEMP_PREFIX: &str = ".workbench-build-";
const MAGIC: &[u8; 8] = b"GL10nP01";
const HEADER_LEN: usize = MAGIC.len() + gamel10n_translation::storage::SHA256_HEX_LEN;
const SECTION_HEADER_LEN: u64 = 12;
const INDEX_ENTRY_LEN: usize = 12;
const FLAG_HAS_MODEL: u8 = 1 << 0;
const FLAG_HAS_SPEAKER: u8 = 1 << 1;
const FLAG_INVALID: u8 = 1 << 2;
const FLAG_FAILED: u8 = 1 << 3;
const FNV1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_PRIME: u64 = 0x0100_0000_01b3;

fn invalid() -> CoreError {
    CoreError::DamagedWorkbench("页数据或索引校验失败。".into())
}
fn json_error(error: serde_json::Error) -> CoreError {
    CoreError::DamagedWorkbench(error.to_string())
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, CoreError> {
    serde_json::to_vec(v).map_err(json_error)
}
fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], CoreError> {
    let (head, tail) = bytes.split_at_checked(count).ok_or_else(invalid)?;
    *bytes = tail;
    Ok(head)
}
fn number(bytes: &mut &[u8]) -> Result<u32, CoreError> {
    Ok(u32::from_le_bytes(
        take(bytes, 4)?.try_into().map_err(|_| invalid())?,
    ))
}
fn put_number(out: &mut Vec<u8>, n: usize) -> Result<(), CoreError> {
    out.extend_from_slice(&u32::try_from(n).map_err(|_| invalid())?.to_le_bytes());
    Ok(())
}
fn put_str(out: &mut Vec<u8>, s: &str) -> Result<(), CoreError> {
    put_number(out, s.len())?;
    out.extend_from_slice(s.as_bytes());
    Ok(())
}
fn get_str<'a>(b: &mut &'a [u8]) -> Result<&'a str, CoreError> {
    let n = number(b)? as usize;
    std::str::from_utf8(take(b, n)?).map_err(|_| invalid())
}

#[derive(Clone, Copy)]
pub(super) struct Field<'a> {
    pub raw: &'a str,
    pub lower: &'a str,
}
impl<'a> Field<'a> {
    fn write(out: &mut Vec<u8>, s: &str) -> Result<(), CoreError> {
        put_str(out, s)?;
        let lower = s.to_lowercase();
        put_str(out, if lower == s { "" } else { &lower })
    }
    fn read(b: &mut &'a [u8]) -> Result<Self, CoreError> {
        let raw = get_str(b)?;
        let lower = get_str(b)?;
        Ok(Self {
            raw,
            lower: if lower.is_empty() { raw } else { lower },
        })
    }
}
pub(super) struct Record<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub source: Field<'a>,
    pub model: Field<'a>,
    pub path: Field<'a>,
    pub locator: Field<'a>,
    pub speaker: Field<'a>,
    pub has_model: bool,
    has_speaker: bool,
    pub invalid: bool,
    pub failed: bool,
}
impl<'a> Record<'a> {
    pub fn next(b: &mut &'a [u8]) -> Result<Self, CoreError> {
        let flags = take(b, 1)?[0];
        Ok(Self {
            has_model: flags & FLAG_HAS_MODEL != 0,
            has_speaker: flags & FLAG_HAS_SPEAKER != 0,
            invalid: flags & FLAG_INVALID != 0,
            failed: flags & FLAG_FAILED != 0,
            id: get_str(b)?,
            kind: get_str(b)?,
            source: Field::read(b)?,
            model: Field::read(b)?,
            path: Field::read(b)?,
            locator: Field::read(b)?,
            speaker: Field::read(b)?,
        })
    }
    fn write(b: &mut Vec<u8>, unit: &TranslationUnit, state: &RowState) -> Result<(), CoreError> {
        let mut flags = 0;
        if state.model.is_some() {
            flags |= FLAG_HAS_MODEL;
        }
        if unit.speaker.is_some() {
            flags |= FLAG_HAS_SPEAKER;
        }
        if !state.issues.is_empty() {
            flags |= FLAG_INVALID;
        }
        if state.failure.is_some() {
            flags |= FLAG_FAILED;
        }
        b.push(flags);
        put_str(b, &unit.id)?;
        put_str(b, &unit.kind)?;
        for s in [
            unit.source_text.as_str(),
            state.model.as_deref().unwrap_or_default(),
            &unit.resource_path,
            &unit.locator,
            unit.speaker.as_deref().unwrap_or_default(),
        ] {
            Field::write(b, s)?;
        }
        Ok(())
    }
    fn detail(self, extra: Detail) -> (TranslationUnit, RowState) {
        (
            TranslationUnit {
                id: self.id.into(),
                kind: self.kind.into(),
                source_text: self.source.raw.into(),
                resource_path: self.path.raw.into(),
                locator: self.locator.raw.into(),
                speaker: self.has_speaker.then(|| self.speaker.raw.into()),
                context: extra.context,
                protected_tokens: extra.tokens,
            },
            RowState {
                model: self.has_model.then(|| self.model.raw.into()),
                failure: extra.failure,
                issues: extra.issues,
                ..Default::default()
            },
        )
    }
}

#[derive(Serialize, Deserialize)]
struct Detail {
    context: String,
    tokens: Option<Vec<String>>,
    issues: Vec<ValidationIssue>,
    failure: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Block {
    search: u64,
    detail: u64,
    end: u64,
}
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Directory {
    pub summary: WorkbenchSummary,
    pub resources: BTreeSet<String>,
    pub kinds: BTreeSet<String>,
    pub blocks: Vec<Block>,
}
pub(crate) struct Pages {
    file: File,
    pub(super) directory: Directory,
    ids: Vec<(u64, u32)>,
}

fn id_hash(id: &str) -> u64 {
    // Collisions are resolved against the actual ID in the page.
    id.bytes().fold(FNV1A_OFFSET_BASIS, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(FNV1A_PRIME)
    })
}
fn write_section(file: &mut File, bytes: &[u8]) -> Result<(), CoreError> {
    file.write_all(&(bytes.len() as u64).to_le_bytes())?;
    file.write_all(&crc32fast::hash(bytes).to_le_bytes())?;
    file.write_all(bytes)?;
    Ok(())
}
fn read_section(file: &mut File, offset: u64, end: u64) -> Result<Vec<u8>, CoreError> {
    file.seek(SeekFrom::Start(offset))?;
    let mut n = [0; 8];
    read_exact(file, &mut n)?;
    let len = u64::from_le_bytes(n);
    if len
        > end
            .checked_sub(offset)
            .and_then(|s| s.checked_sub(SECTION_HEADER_LEN))
            .ok_or_else(invalid)?
    {
        return Err(invalid());
    }
    let mut digest = [0; 4];
    read_exact(file, &mut digest)?;
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| invalid())?];
    read_exact(file, &mut bytes)?;
    if crc32fast::hash(&bytes) != u32::from_le_bytes(digest) {
        return Err(invalid());
    }
    Ok(bytes)
}
fn read_exact(file: &mut File, bytes: &mut [u8]) -> Result<(), CoreError> {
    file.read_exact(bytes).map_err(|error| match error.kind() {
        io::ErrorKind::UnexpectedEof => invalid(),
        _ => error.into(),
    })
}
fn header(file: &mut File, key: &str) -> Result<(), CoreError> {
    let mut head = [0; HEADER_LEN];
    read_exact(file, &mut head)?;
    if &head[..MAGIC.len()] != MAGIC || head[MAGIC.len()..] != *key.as_bytes() {
        return Err(invalid());
    }
    Ok(())
}
fn read_markers(file: &mut File, game: &Path) -> Result<(), CoreError> {
    let end = file.metadata()?.len();
    let offset = file.stream_position()?;
    let markers: BTreeMap<String, bool> =
        serde_json::from_slice(&read_section(file, offset, end)?).map_err(json_error)?;
    if markers.iter().any(|(name, expected)| {
        gamel10n_engines::discovery::resolve_relative(game, name).is_ok_and(|p| p.is_file())
            != *expected
    }) {
        return Err(invalid());
    }
    Ok(())
}
fn publish(temp: NamedTempFile, path: &Path) -> Result<(), CoreError> {
    temp.as_file().sync_all()?;
    gamel10n_translation::files::persist_temp(temp, path)?;
    Ok(())
}
impl Pages {
    pub(crate) fn build(
        root: &Path,
        key: &str,
        game: &Path,
        markers: &BTreeMap<String, bool>,
        units: &[TranslationUnit],
        workbench: &super::Workbench,
    ) -> Result<Self, CoreError> {
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(root)?;
        let file = temp.as_file_mut();
        file.write_all(MAGIC)?;
        file.write_all(key.as_bytes())?;
        file.write_all(&(units.len() as u64).to_le_bytes())?;
        write_section(file, &encode(markers)?)?;
        for (page, chunk) in units.chunks(SIZE).enumerate() {
            let mut search = Vec::new();
            let mut details = Vec::with_capacity(chunk.len());
            for (i, unit) in chunk.iter().enumerate() {
                let state = workbench.state(page * SIZE + i);
                // Edits and candidates use the existing durable edit store, including after an undo.
                let base = RowState {
                    model: state.model.clone(),
                    failure: state.failure.clone(),
                    issues: if state.edit.is_some() {
                        state
                            .model
                            .as_ref()
                            .map(|s| gamel10n_translation::text::validate(unit, s))
                            .unwrap_or_default()
                    } else {
                        state.issues.clone()
                    },
                    ..Default::default()
                };
                Record::write(&mut search, unit, &base)?;
                details.push(Detail {
                    context: unit.context.clone(),
                    tokens: unit.protected_tokens.clone(),
                    issues: base.issues,
                    failure: base.failure,
                });
            }
            write_section(file, &search)?;
            write_section(file, &encode(&details)?)?;
        }
        publish(temp, &root.join(DATA))?;
        Self::reindex(root, key, game)
    }
    pub(crate) fn reindex(root: &Path, key: &str, game: &Path) -> Result<Self, CoreError> {
        let mut file = File::open(root.join(DATA))?;
        header(&mut file, key)?;
        let mut expected = [0; 8];
        read_exact(&mut file, &mut expected)?;
        read_markers(&mut file, game)?;
        let len = file.metadata()?.len();
        let mut directory = Directory::default();
        let mut ids = Vec::new();
        let mut pos = file.stream_position()?;
        while pos < len {
            let search = read_section(&mut file, pos, len)?;
            let detail = file.stream_position()?;
            let details: Vec<Detail> =
                serde_json::from_slice(&read_section(&mut file, detail, len)?)
                    .map_err(json_error)?;
            let mut bytes = search.as_slice();
            for _ in &details {
                let record = Record::next(&mut bytes)?;
                ids.push((
                    id_hash(record.id),
                    u32::try_from(ids.len()).map_err(|_| invalid())?,
                ));
                let summary = &mut directory.summary;
                summary.total += 1;
                summary.translated += usize::from(record.has_model);
                summary.invalid += usize::from(record.invalid);
                summary.failed += usize::from(record.failed);
                if !directory.resources.contains(record.path.raw) {
                    directory.resources.insert(record.path.raw.into());
                }
                if !directory.kinds.contains(record.kind) {
                    directory.kinds.insert(record.kind.into());
                }
            }
            let end = file.stream_position()?;
            if !bytes.is_empty()
                || details.is_empty()
                || details.len() > SIZE
                || (end < len && details.len() != SIZE)
            {
                return Err(invalid());
            }
            directory.blocks.push(Block {
                search: pos,
                detail,
                end,
            });
            pos = end;
        }
        if ids.len() as u64 != u64::from_le_bytes(expected) {
            return Err(invalid());
        }
        ids.sort_unstable();
        let mut index = Vec::new();
        let meta = encode(&directory)?;
        put_number(&mut index, meta.len())?;
        index.extend(meta);
        for (hash, ordinal) in &ids {
            index.extend(hash.to_le_bytes());
            index.extend(ordinal.to_le_bytes());
        }
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(root)?;
        temp.write_all(MAGIC)?;
        temp.write_all(key.as_bytes())?;
        write_section(temp.as_file_mut(), &index)?;
        publish(temp, &root.join(INDEX))?;
        Ok(Self {
            file,
            directory,
            ids,
        })
    }
    pub(crate) fn open(root: &Path, key: &str, game: &Path) -> Result<Self, CoreError> {
        let mut file = File::open(root.join(DATA))?;
        header(&mut file, key)?;
        let mut expected = [0; 8];
        read_exact(&mut file, &mut expected)?;
        read_markers(&mut file, game)?;
        let mut idx = File::open(root.join(INDEX))?;
        header(&mut idx, key)?;
        let len = idx.metadata()?.len();
        let data = read_section(&mut idx, HEADER_LEN as u64, len)?;
        if idx.stream_position()? != len {
            return Err(invalid());
        }
        let mut bytes = data.as_slice();
        let size = number(&mut bytes)? as usize;
        let directory: Directory =
            serde_json::from_slice(take(&mut bytes, size)?).map_err(json_error)?;
        if directory.summary.total as u64 != u64::from_le_bytes(expected)
            || bytes.len() / INDEX_ENTRY_LEN != directory.summary.total
            || !bytes.len().is_multiple_of(INDEX_ENTRY_LEN)
            || directory.blocks.len() != directory.summary.total.div_ceil(SIZE)
            || directory
                .blocks
                .last()
                .map_or(file.stream_position()?, |b| b.end)
                != file.metadata()?.len()
        {
            return Err(invalid());
        }
        let mut ids = Vec::with_capacity(directory.summary.total);
        while !bytes.is_empty() {
            let hash = u64::from_le_bytes(take(&mut bytes, 8)?.try_into().map_err(|_| invalid())?);
            ids.push((hash, number(&mut bytes)?));
        }
        Ok(Self {
            file,
            directory,
            ids,
        })
    }
    pub(super) fn search_block(&mut self, p: usize) -> Result<Vec<u8>, CoreError> {
        let b = self.directory.blocks.get(p).ok_or_else(invalid)?;
        read_section(&mut self.file, b.search, b.detail)
    }
    pub(super) fn rows(&mut self, p: usize) -> Result<Vec<(TranslationUnit, RowState)>, CoreError> {
        let search = self.search_block(p)?;
        let b = &self.directory.blocks[p];
        let details: Vec<Detail> =
            serde_json::from_slice(&read_section(&mut self.file, b.detail, b.end)?)
                .map_err(json_error)?;
        let mut bytes = search.as_slice();
        details
            .into_iter()
            .map(|extra| Ok(Record::next(&mut bytes)?.detail(extra)))
            .collect()
    }
    pub(super) fn find(&mut self, id: &str) -> Result<Option<usize>, CoreError> {
        let hash = id_hash(id);
        let start = self.ids.partition_point(|v| v.0 < hash);
        for i in start..self.ids.len() {
            let (other, n) = self.ids[i];
            if other != hash {
                break;
            }
            let n = n as usize;
            let block = self.search_block(n / SIZE)?;
            let mut bytes = block.as_slice();
            for j in 0..=n % SIZE {
                let r = Record::next(&mut bytes)?;
                if j == n % SIZE && r.id == id {
                    return Ok(Some(n));
                }
            }
        }
        Ok(None)
    }
}
