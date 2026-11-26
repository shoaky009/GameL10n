use std::io::{Read, Seek, SeekFrom};

use crate::{Result, config::validate_face};

pub fn verify_font_metadata(reader: &mut (impl Read + Seek), expected: &str) -> Result<String> {
    validate_face(expected)?;
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.rewind()?;
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    for _ in 0..u16_at(&header, 4)? {
        let mut record = [0; 16];
        reader.read_exact(&mut record)?;
        if &record[..4] != b"name" {
            continue;
        }
        let offset = u64::from(u32_at(&record, 8)?);
        let length = u32_at(&record, 12)? as usize;
        if length > 1024 * 1024 || offset + length as u64 > file_size {
            return Err("汉化字体名称表越界。".into());
        }
        reader.seek(SeekFrom::Start(offset))?;
        let mut names = vec![0; length];
        reader.read_exact(&mut names)?;
        return verify_names(&names, expected);
    }
    Err("汉化字体缺少可用于转区的完整名称，请重新生成补丁。".into())
}

fn decode_utf16_be(value: &[u8]) -> Result<String> {
    let (pairs, trailing) = value.as_chunks::<2>();
    if !trailing.is_empty() {
        return Err("汉化字体内部名称编码不完整，请重新生成补丁。".into());
    }
    let units: Vec<u16> = pairs.iter().copied().map(u16::from_be_bytes).collect();
    String::from_utf16(&units).map_err(|_| "汉化字体内部名称编码无效，请重新生成补丁。".into())
}

fn verify_names(names: &[u8], expected: &str) -> Result<String> {
    let records = usize::from(u16_at(names, 2)?);
    let storage = usize::from(u16_at(names, 4)?);
    if 6 + records * 12 > names.len() || storage < 6 + records * 12 || storage > names.len() {
        return Err("汉化字体名称表不完整。".into());
    }
    let mut english_full = false;
    let mut english_family: Option<String> = None;
    let mut japanese_family: Option<String> = None;
    for i in 0..records {
        let at = 6 + i * 12;
        if u16_at(names, at)? != 3 || u16_at(names, at + 2)? != 1 {
            continue;
        }
        let name_id = u16_at(names, at + 6)?;
        if name_id != 1 && name_id != 4 {
            continue;
        }
        let language = u16_at(names, at + 4)?;
        if language != 0x409 && language != 0x411 {
            continue;
        }
        let size = usize::from(u16_at(names, at + 8)?);
        let start = storage + usize::from(u16_at(names, at + 10)?);
        let value = names
            .get(start..start + size)
            .ok_or("汉化字体名称数据越界。")?;
        let text = decode_utf16_be(value)?;
        if name_id == 4 {
            if text != expected {
                return Err(
                    "汉化字体内部名称与游戏配置不一致，转区运行会回退到原字体。请重新生成补丁。"
                        .into(),
                );
            }
            english_full |= language == 0x409;
        } else {
            validate_face(&text)?;
            let slot = if language == 0x409 {
                &mut english_family
            } else {
                &mut japanese_family
            };
            if slot.as_ref().is_some_and(|existing| existing != &text) {
                return Err(
                    "汉化字体内部族名不一致，转区运行会回退到原字体。请重新生成补丁。".into(),
                );
            }
            *slot = Some(text);
        }
    }
    let Some(family) = english_family else {
        return Err("汉化字体缺少可用于转区的字体族名，请重新生成补丁。".into());
    };
    if japanese_family.as_ref().is_some_and(|jp| jp != &family) {
        return Err("汉化字体内部族名与日语语言项不一致，转区运行会回退到原字体。请重新生成补丁。".into());
    }
    if !english_full {
        return Err("汉化字体缺少可用于转区的完整名称，请重新生成补丁。".into());
    }
    Ok(family)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let pair = bytes
        .get(offset..offset + 2)
        .ok_or("汉化字体数据不完整。")?;
    Ok(u16::from_be_bytes([pair[0], pair[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok((u32::from(u16_at(bytes, offset)?) << 16) | u32::from(u16_at(bytes, offset + 2)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_test::{fixture_font, fixture_font_with_family};
    use std::io::Cursor;

    #[test]
    fn accepts_matching_english_and_japanese_full_and_family_names() {
        let family = verify_font_metadata(
            &mut Cursor::new(fixture_font_with_family("PatchFont", "PatchFont Regular")),
            "PatchFont Regular",
        )
        .unwrap();
        assert_eq!(family, "PatchFont");
    }

    #[test]
    fn rejects_mismatched_names_in_either_language() {
        let font = fixture_font_with_family("PatchFam", "PatchFam Regular");
        let fam_len = "PatchFam".len() * 2;
        let full_len = "PatchFam Regular".len() * 2;
        // String storage starts at 28 + (6 + 4 * 12) = 82: [en-fam, en-full, ja-fam, ja-full]
        for index in [
            82,
            82 + fam_len,
            82 + fam_len + full_len,
            82 + fam_len * 2 + full_len,
        ] {
            let mut altered = font.clone();
            altered[index + 1] = b'X';
            assert!(
                verify_font_metadata(&mut Cursor::new(altered), "PatchFam Regular").is_err(),
                "index {index}"
            );
        }
    }

    #[test]
    fn rejects_missing_english_full_or_family_name() {
        // Flip en-US full name record language (record 1, offset 34 + 12 + 4 = 50) to ja-JP.
        let mut missing_full = fixture_font("Patch Font");
        missing_full[50..52].copy_from_slice(&0x411u16.to_be_bytes());
        assert!(verify_font_metadata(&mut Cursor::new(missing_full), "Patch Font").is_err());

        // Flip en-US family name record language (record 0, offset 34 + 4 = 38) to ja-JP.
        let mut missing_family = fixture_font("Patch Font");
        missing_family[38..40].copy_from_slice(&0x411u16.to_be_bytes());
        assert!(verify_font_metadata(&mut Cursor::new(missing_family), "Patch Font").is_err());
    }

    #[test]
    fn rejects_truncation_at_any_byte() {
        let font = fixture_font("Patch Font");
        for length in 0..font.len() {
            assert!(
                verify_font_metadata(&mut Cursor::new(&font[..length]), "Patch Font").is_err(),
                "length {length}"
            );
        }
    }

    #[test]
    fn rejects_out_of_bounds_or_odd_name_records() {
        for (at, value) in [
            (20, u32::MAX),
            (24, 1024 * 1024 + 1),
            (32, 0xffff0000),
            (42, 0x00010000),
        ] {
            let mut font = fixture_font("Patch Font");
            font[at..at + 4].copy_from_slice(&value.to_be_bytes());
            assert!(
                verify_font_metadata(&mut Cursor::new(font), "Patch Font").is_err(),
                "offset {at}"
            );
        }
    }
}
