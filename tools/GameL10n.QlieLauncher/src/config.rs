#[cfg(windows)]
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::Result;

#[derive(Debug, PartialEq, Eq)]
pub struct Font {
    pub file: String,
    pub face: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LaunchConfig {
    pub executable: String,
    pub fonts: Vec<Font>,
}

#[derive(Clone, Copy)]
pub enum ProfileKind {
    Game,
    Global,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LocaleProfile {
    pub guid: Option<String>,
    pub elevated: bool,
}

#[cfg(windows)]
pub fn read_xml(path: &Path) -> Result<String> {
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 1024 * 1024 {
        return Err("启动配置超过 1 MiB。".into());
    }
    decode_xml(&data)
}

#[cfg(any(windows, test))]
fn decode_xml(data: &[u8]) -> Result<String> {
    let big_endian = data.starts_with(&[0xfe, 0xff]) || data.starts_with(&[0, b'<']);
    if big_endian || data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[b'<', 0]) {
        let data = if data.starts_with(&[0xfe, 0xff]) || data.starts_with(&[0xff, 0xfe]) {
            &data[2..]
        } else {
            data
        };
        if !data.len().is_multiple_of(2) {
            return Err("启动配置的 UTF-16 编码不完整。".into());
        }
        let units: Vec<_> = data
            .chunks_exact(2)
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        return Ok(String::from_utf16(&units)?);
    }
    Ok(std::str::from_utf8(data.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(data))?.to_owned())
}

fn document(xml: &str) -> Result<roxmltree::Document<'_>> {
    Ok(roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 65536,
            ..Default::default()
        },
    )?)
}

pub fn launch_config(xml: &str) -> Result<LaunchConfig> {
    let doc = document(xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("launch") {
        return Err("汉化启动配置缺少 launch 元素。".into());
    }
    let executable = root.attribute("executable").unwrap_or_default().to_owned();
    let mut fonts = Vec::new();
    for node in root.children().filter(|node| node.has_tag_name("font")) {
        let face = node.attribute("face").unwrap_or_default();
        validate_face(face)?;
        fonts.push(Font {
            file: node.attribute("file").unwrap_or_default().to_owned(),
            face: face.to_owned(),
        });
    }
    if fonts.is_empty() {
        return Err("汉化启动配置没有声明字体。".into());
    }
    Ok(LaunchConfig { executable, fonts })
}

pub fn validate_face(face: &str) -> Result<()> {
    if face.trim().is_empty() || face.encode_utf16().count() > 31 || face.contains('\0') {
        return Err("汉化启动配置缺少有效的完整字体名称，请重新生成补丁。".into());
    }
    Ok(())
}

pub fn locale_profile(xml: &str, kind: ProfileKind) -> Result<LocaleProfile> {
    let doc = document(xml)?;
    let root = doc.root_element();
    let profile = root
        .has_tag_name("LEConfig")
        .then(|| root.children().find(|node| node.has_tag_name("Profiles")))
        .flatten()
        .and_then(|profiles| {
            profiles.children().find(|node| {
                node.has_tag_name("Profile")
                    && (matches!(kind, ProfileKind::Game)
                        || child_text(*node, "Location") == "ja-JP")
            })
        })
        .ok_or("请在 Locale Emulator 中为原游戏程序设置日语运行配置，然后重新启动汉化程序。")?;
    if child_text(profile, "Location") != "ja-JP" {
        return Err(
            "原游戏的 Locale Emulator 配置不是日语。请将原 EXE 的运行区域设置为日语后再试。".into(),
        );
    }
    if child_text(profile, "RunWithSuspend") == "true" {
        return Err("请关闭原游戏 Locale Emulator 配置中的暂停进程选项后再启动。".into());
    }
    let guid = match kind {
        ProfileKind::Game => None,
        ProfileKind::Global => Some(normalize_guid(
            profile.attribute("Guid").unwrap_or_default(),
        )?),
    };
    Ok(LocaleProfile {
        guid,
        elevated: child_text(profile, "RunAsAdmin") == "true",
    })
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> &'a str {
    node.children()
        .find(|node| node.has_tag_name(name))
        .and_then(|node| node.text())
        .unwrap_or_default()
        .trim()
}

fn normalize_guid(value: &str) -> Result<String> {
    let value = value.trim();
    let value = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .or_else(|| {
            value
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
        })
        .unwrap_or(value);
    let digits: String =
        if value.len() == 36 && [8, 13, 18, 23].iter().all(|&i| value.as_bytes()[i] == b'-') {
            value.chars().filter(|&ch| ch != '-').collect()
        } else {
            value.to_owned()
        };
    if digits.len() != 32 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Locale Emulator 配置标识无效。".into());
    }
    let digits = digits.to_ascii_lowercase();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &digits[..8],
        &digits[8..12],
        &digits[12..16],
        &digits[16..20],
        &digits[20..]
    ))
}

pub fn under_root(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = relative.replace('\\', "/");
    if relative.trim().is_empty() || relative.starts_with('/') || relative.contains([':', '\0']) {
        return Err("汉化启动配置中的文件路径无效。".into());
    }
    let mut components = Vec::new();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                components.pop().ok_or("汉化启动配置中的文件路径越界。")?;
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err("汉化启动配置中的文件路径无效。".into());
    }
    Ok(root.join(components.iter().collect::<PathBuf>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_reads_multiple_fonts_and_xml_escaped_paths() {
        let config = launch_config("<launch executable='Game &amp; Test.exe'><font file='font.ttf' face='Patch Font'/><font file='serif.ttf' face='Patch Serif'/></launch>").unwrap();
        assert_eq!(config.executable, "Game & Test.exe");
        assert_eq!(config.fonts.len(), 2);
    }

    #[test]
    fn launch_rejects_missing_fonts_or_invalid_names() {
        for xml in [
            "<launch/>",
            "<launch><font file='f.ttf'/></launch>",
            "<launch><font face='12345678901234567890123456789012'/></launch>",
        ] {
            assert!(launch_config(xml).is_err(), "{xml}");
        }
    }

    #[test]
    fn xml_rejects_dtd_and_external_entities() {
        assert!(
            launch_config(
                "<!DOCTYPE launch [<!ENTITY x SYSTEM 'file:///not-read'>]><launch>&x;</launch>"
            )
            .is_err()
        );
    }

    #[test]
    fn xml_decodes_utf8_bom_and_both_utf16_byte_orders() {
        let xml = "<launch executable='游戏.exe'/>";
        assert_eq!(
            decode_xml(&[b"\xef\xbb\xbf".as_slice(), xml.as_bytes()].concat()).unwrap(),
            xml
        );
        for big in [false, true] {
            let mut data = if big {
                vec![0xfe, 0xff]
            } else {
                vec![0xff, 0xfe]
            };
            for unit in xml.encode_utf16() {
                data.extend(if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
            assert_eq!(decode_xml(&data).unwrap(), xml);
        }
    }

    #[test]
    fn paths_reject_windows_absolute_paths_traversal_and_streams() {
        for path in [
            "",
            " ",
            "../game.exe",
            "a/../../game.exe",
            "/game.exe",
            "C:\\game.exe",
            "C:game.exe",
            "\\\\server\\game.exe",
            "game.exe:stream",
            ".",
        ] {
            assert!(under_root(Path::new("root"), path).is_err(), "{path}");
        }
    }

    #[test]
    fn paths_normalize_nested_windows_separators() {
        assert_eq!(
            under_root(Path::new("root"), "qlie-l10n\\temp\\..\\font.ttf").unwrap(),
            Path::new("root").join("qlie-l10n/font.ttf")
        );
    }

    #[test]
    fn game_profile_rejects_non_japanese_or_suspended_launches() {
        for settings in [
            "<Location>en-US</Location>",
            "<Location>ja-JP</Location><RunWithSuspend>true</RunWithSuspend>",
        ] {
            assert!(
                locale_profile(
                    &format!(
                        "<LEConfig><Profiles><Profile>{settings}</Profile></Profiles></LEConfig>"
                    ),
                    ProfileKind::Game
                )
                .is_err()
            );
        }
    }

    #[test]
    fn global_profile_selects_japanese_and_preserves_elevation() {
        let profile = locale_profile("<LEConfig><Profiles><Profile><Location>en-US</Location></Profile><Profile Guid='{ABCDEF0123456789ABCDEF0123456789}'><Location>ja-JP</Location><RunAsAdmin>true</RunAsAdmin></Profile></Profiles></LEConfig>", ProfileKind::Global).unwrap();
        assert_eq!(
            profile,
            LocaleProfile {
                guid: Some("abcdef01-2345-6789-abcd-ef0123456789".into()),
                elevated: true
            }
        );
    }

    #[test]
    fn game_profile_does_not_require_a_guid() {
        assert_eq!(locale_profile("<LEConfig><Profiles><Profile><Location>ja-JP</Location></Profile></Profiles></LEConfig>", ProfileKind::Game).unwrap().guid, None);
    }

    #[test]
    fn global_profile_rejects_invalid_guids() {
        assert!(locale_profile("<LEConfig><Profiles><Profile Guid='invalid'><Location>ja-JP</Location></Profile></Profiles></LEConfig>", ProfileKind::Global).is_err());
    }
}
