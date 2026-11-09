use std::{io::Cursor, path::Path};

use crate::{Result, config, font};

pub fn run() -> Result<()> {
    let launch = config::launch_config(
        "<launch executable='Game.exe'><font file='font.ttf' face='Fixture Font'/></launch>",
    )?;
    if launch.executable != "Game.exe"
        || config::under_root(Path::new("root"), "../outside.exe").is_ok()
        || config::launch_config("<!DOCTYPE launch><launch/>").is_ok()
    {
        return Err("启动器配置自检失败。".into());
    }
    config::locale_profile(
        "<LEConfig><Profiles><Profile><Location>ja-JP</Location></Profile></Profiles></LEConfig>",
        config::ProfileKind::Game,
    )?;
    config::locale_profile(
        "<LEConfig><Profiles><Profile Guid='01234567-89ab-cdef-0123-456789abcdef'><Location>ja-JP</Location></Profile></Profiles></LEConfig>",
        config::ProfileKind::Global,
    )?;
    let bytes = fixture_font("Fixture Font");
    font::verify_font_metadata(&mut Cursor::new(&bytes), "Fixture Font")?;
    if font::verify_font_metadata(&mut Cursor::new(&bytes), "Other Font").is_ok() {
        return Err("启动器字体自检失败。".into());
    }
    Ok(())
}

pub fn fixture_font(face: &str) -> Vec<u8> {
    let text: Vec<_> = face.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut font = vec![0; 58 + text.len() * 2];
    font[..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    font[4..6].copy_from_slice(&1u16.to_be_bytes());
    font[12..16].copy_from_slice(b"name");
    font[20..24].copy_from_slice(&28u32.to_be_bytes());
    font[24..28].copy_from_slice(&((30 + text.len() * 2) as u32).to_be_bytes());
    font[30..32].copy_from_slice(&2u16.to_be_bytes());
    font[32..34].copy_from_slice(&30u16.to_be_bytes());
    for (i, language) in [0x409u16, 0x411].into_iter().enumerate() {
        let at = 34 + i * 12;
        for (j, value) in [
            3,
            1,
            language,
            4,
            text.len() as u16,
            (i * text.len()) as u16,
        ]
        .into_iter()
        .enumerate()
        {
            font[at + j * 2..at + j * 2 + 2].copy_from_slice(&value.to_be_bytes());
        }
        font[58 + i * text.len()..58 + (i + 1) * text.len()].copy_from_slice(&text);
    }
    font
}
