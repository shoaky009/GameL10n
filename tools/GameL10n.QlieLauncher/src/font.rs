use std::io::{Read, Seek, SeekFrom};

use crate::{Result, config::validate_face};

pub fn verify_font_metadata(reader: &mut (impl Read + Seek), expected: &str) -> Result<()> {
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

fn verify_names(names: &[u8], expected: &str) -> Result<()> {
    let records = usize::from(u16_at(names, 2)?);
    let storage = usize::from(u16_at(names, 4)?);
    if 6 + records * 12 > names.len() || storage < 6 + records * 12 || storage > names.len() {
        return Err("汉化字体名称表不完整。".into());
    }
    let mut english = false;
    for i in 0..records {
        let at = 6 + i * 12;
        if u16_at(names, at)? != 3 || u16_at(names, at + 2)? != 1 || u16_at(names, at + 6)? != 4 {
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
        if !size.is_multiple_of(2)
            || !value
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .eq(expected.encode_utf16())
        {
            return Err(
                "汉化字体内部名称与游戏配置不一致，转区运行会回退到原字体。请重新生成补丁。".into(),
            );
        }
        english |= language == 0x409;
    }
    if !english {
        return Err("汉化字体缺少可用于转区的完整名称，请重新生成补丁。".into());
    }
    Ok(())
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
    use crate::self_test::fixture_font;
    use std::io::Cursor;

    #[test]
    fn accepts_matching_english_and_japanese_full_names() {
        verify_font_metadata(&mut Cursor::new(fixture_font("Patch Font")), "Patch Font").unwrap();
    }

    #[test]
    fn rejects_mismatched_names_in_either_language() {
        let font = fixture_font("Patch Font");
        for index in [58, 58 + "Patch Font".len() * 2] {
            let mut altered = font.clone();
            altered[index + 1] = b'X';
            assert!(verify_font_metadata(&mut Cursor::new(altered), "Patch Font").is_err());
        }
    }

    #[test]
    fn rejects_missing_english_full_name() {
        let mut font = fixture_font("Patch Font");
        font[38..40].copy_from_slice(&0x411u16.to_be_bytes());
        assert!(verify_font_metadata(&mut Cursor::new(font), "Patch Font").is_err());
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
