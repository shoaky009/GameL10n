//! Independent readback of display codepoints from supported game resources.
use super::*;
use crate::protocol::{BuildResult, LaunchLocation};
use serde::ser::Serializer;
use std::io::{BufWriter, Write};

#[derive(Deserialize)]
struct FontSelection {
    path: String,
    face: String,
    fixed: bool,
}

#[derive(Deserialize)]
struct DisplayEvidence {
    fonts: Vec<FontSelection>,
    aliases: BTreeMap<char, char>,
    #[serde(default)]
    warnings: Vec<String>,
}

pub(super) struct QlieDisplay {
    template: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample<'a> {
    resource_path: &'a str,
    locator: &'a str,
    expected: &'a str,
    original: &'a str,
}

fn write_samples(
    units: &[TranslationUnit],
    translations: &BTreeMap<String, String>,
    include_unchanged: bool,
) -> Result<tempfile::NamedTempFile, EngineError> {
    let samples = units.iter().filter_map(|unit| {
        let expected = translations.get(&unit.id)?;
        (include_unchanged || expected != &unit.source_text).then_some(Sample {
            resource_path: &unit.resource_path,
            locator: &unit.locator,
            expected,
            original: &unit.source_text,
        })
    });
    let file = tempfile::NamedTempFile::new()?;
    let mut writer = BufWriter::new(file.as_file());
    serde_json::Serializer::new(&mut writer).collect_seq(samples)?;
    writer.flush()?;
    drop(writer);
    Ok(file)
}

impl QlieDisplay {
    pub(super) fn detect(
        adapter: &AdapterHandle,
        root: &Path,
        files: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
    ) -> Result<Option<Self>, EngineError> {
        if !matches!(adapter, AdapterHandle::Python { .. }) {
            return Ok(None);
        }
        if matches!(
            adapter.id(),
            "template.qlie-pack3-imos" | "template.qlie-pack31-imos"
        ) {
            return Ok(Some(Self { template: true }));
        }
        for path in files.iter().filter(|path| {
            path.rsplit_once('/').is_some_and(|(dir, file)| {
                dir.eq_ignore_ascii_case("GameData")
                    && Path::new(file)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("pack"))
            })
        }) {
            let mut file = File::open(inputs.file(root, path)?)?;
            if file.metadata()?.len() >= 28 {
                file.seek(SeekFrom::End(-28))?;
                let mut footer = [0; 28];
                file.read_exact(&mut footer)?;
                if crate::adaptation_context::filepack_version(&footer).is_some() {
                    return Ok(Some(Self { template: false }));
                }
            }
        }
        Ok(None)
    }
}

fn verify_font(
    root: &Path,
    font: &FontSelection,
    aliases: &BTreeMap<char, char>,
) -> Result<(), EngineError> {
    let path = resolve_relative(root, &font.path)?;
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take((64 << 20) + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 << 20 {
        return Err(EngineError::Rejected(format!(
            "字体超过 64 MiB：{}",
            font.path
        )));
    }
    let face = ttf_parser::Face::parse(&bytes, 0)
        .map_err(|error| EngineError::Rejected(format!("无法读取字体 {}：{error:?}", font.path)))?;
    if font.fixed && !face.is_monospaced() {
        return Err(EngineError::Rejected(format!(
            "QLIE 的 system_fix 字体槽需要等宽字体：{}",
            font.path
        )));
    }
    let mut names = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == ttf_parser::name_id::FULL_NAME && name.is_unicode())
        .filter_map(|name| name.to_string())
        .peekable();
    if names.peek().is_none() || names.any(|name| name != font.face) {
        return Err(EngineError::Rejected(format!(
            "字体 {} 的完整名称与游戏选择的「{}」不一致",
            font.path, font.face
        )));
    }
    let families: Vec<_> = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == ttf_parser::name_id::FAMILY && name.is_unicode())
        .filter_map(|name| name.to_string())
        .collect();
    if families.is_empty()
        || families.iter().any(|name| name.trim().is_empty())
        || families.windows(2).any(|pair| pair[0] != pair[1])
    {
        return Err(EngineError::Rejected(format!(
            "字体 {} 缺少一致的字体族名",
            font.path
        )));
    }
    if face.is_variable() {
        let non_regular_default_weight = face.variation_axes().into_iter().any(|axis| {
            axis.tag == ttf_parser::Tag::from_bytes(b"wght")
                && (axis.def_value - 400.0).abs() > f32::EPSILON
        });
        let collides_with_regular = non_regular_default_weight
            && font
                .face
                .to_ascii_lowercase()
                .ends_with(" regular")
            && face.names().into_iter().any(|name| {
                matches!(
                    name.name_id,
                    ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY | ttf_parser::name_id::WWS_SUBFAMILY
                ) && name.is_unicode()
                    && name
                        .to_string()
                        .is_some_and(|value| value.eq_ignore_ascii_case("regular"))
            });
        if collides_with_regular {
            return Err(EngineError::Rejected(format!(
                "可变字体 {} 将非 Regular 默认字重的排版子族改写为 Regular，转区枚举时会与 Regular 实例冲突并隐藏字体",
                font.path
            )));
        }
    }
    for (&slot, &ch) in aliases {
        let glyph = face.glyph_index(ch);
        if glyph.is_none_or(|glyph| glyph.0 == 0) || face.glyph_index(slot) != glyph {
            return Err(EngineError::Rejected(format!(
                "试译字符「{ch}」写为 U+{:04X}，字体 {} 的该码位未映射到目标字形",
                u32::from(slot),
                font.path
            )));
        }
    }
    Ok(())
}

impl EngineService {
    /// Resolve additional physical copies for the user's patch selection against original archives.
    pub async fn patch_translation_copies(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        files: &[String],
        units: &[TranslationUnit],
        translations: &BTreeMap<String, String>,
        cancellation: &CancellationToken,
    ) -> Result<BTreeMap<String, String>, EngineError> {
        if !matches!(
            adapter.id(),
            "template.qlie-pack3-imos" | "template.qlie-pack31-imos"
        ) || translations.len() == units.len()
            || files
                .iter()
                .filter(|p| p.to_ascii_lowercase().ends_with(".pack"))
                .take(2)
                .count()
                < 2
        {
            return Ok(BTreeMap::new());
        }
        let samples = write_samples(units, translations, true)?;
        let output = tempfile::NamedTempFile::new()?;
        self.helper(&json!({
            "operation":"qlie_patch_copies", "components":self.asset_root.join("adapters/components"),
            "library":self.reference_library(&self.current_reference()?)?, "source":source,
            "resources":files, "samplesFile":samples.path(), "output":output.path(),
        }), cancellation).await?;
        Ok(serde_json::from_reader(std::io::BufReader::new(
            output.as_file(),
        ))?)
    }

    pub(super) fn display_cache(&self, key: &str) -> PathBuf {
        self.shared_directory
            .join("verified-display")
            .join(format!("{key}.json"))
    }
}

impl QlieDisplay {
    pub(super) async fn verify(
        &self,
        service: &EngineService,
        build: &BuildResult,
        request: &AdapterRequest,
        units: &[TranslationUnit],
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, EngineError> {
        let translations = request
            .translations
            .as_ref()
            .ok_or_else(|| EngineError::Protocol("缺少中文验收译文".into()))?;
        let output = request
            .output_directory
            .as_deref()
            .ok_or_else(|| EngineError::Protocol("缺少回填目录".into()))?;
        let samples = write_samples(units, translations, false)?;
        let launch = build.launch_target()?;
        let launcher = launch
            .as_ref()
            .filter(|launch| {
                launch.location == LaunchLocation::Patch
                    && launch.configuration.working_directory == "."
                    && launch.configuration.arguments.is_empty()
            })
            .map(|launch| &launch.configuration.executable);
        let evidence: DisplayEvidence = serde_json::from_value(service.helper(&json!({
            "operation":"qlie_display", "components":service.asset_root.join("adapters/components"),
            "library":service.reference_library(&service.current_reference()?)?, "output":output,
            "template":self.template, "source":request.source_directory, "resources":request.resource_files,
            "mode":build.mode, "files":build.files, "samplesFile":samples.path(), "launcher":launcher,
            "toolPaths":{"hostCommand":[service.tools.vntextpatch()]},
        }), cancellation).await?)?;
        if evidence.aliases.is_empty() && evidence.fonts.is_empty() {
            return Ok(evidence.warnings);
        }
        let launcher = launcher
            .ok_or_else(|| EngineError::Rejected("QLIE 字形替换缺少字体启动入口".into()))?;
        let shipped = service
            .tools
            .root
            .join("adapter-components/QlieLauncher.exe");
        if !shipped.is_file() {
            return Err(EngineError::ToolMissing(shipped));
        }
        let digest =
            |path: &Path| hash_file(path).map_err(|error| EngineError::Protocol(error.to_string()));
        if evidence.fonts.is_empty()
            || digest(&resolve_relative(output, launcher)?)? != digest(&shipped)?
        {
            return Err(EngineError::Rejected(
                "QLIE 字形替换缺少可核验的字体或加载入口".into(),
            ));
        }
        for font in &evidence.fonts {
            verify_font(output, font, &evidence.aliases)?;
        }
        Ok(evidence.warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_sfnt(subfamily_17: &str, default_weight: Option<f32>) -> Vec<u8> {
        let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();

        // head (54 bytes): version 1.0, unitsPerEm=1000, indexToLocFormat=0
        let mut head = vec![0u8; 54];
        head[..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        head[12..16].copy_from_slice(&0x5F0F3CF5u32.to_be_bytes());
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        tables.push((*b"head", head));

        // hhea (36 bytes): version 1.0, numberOfHMetrics=2
        let mut hhea = vec![0u8; 36];
        hhea[..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
        tables.push((*b"hhea", hhea));

        // maxp (6 bytes): version 0.5 / 1.0, numGlyphs=2
        let mut maxp = vec![0u8; 6];
        maxp[..4].copy_from_slice(&0x00005000u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
        tables.push((*b"maxp", maxp));

        // cmap format 4 mapping U+4E2D ('中') and U+E000 -> glyph 1
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID = Windows
        cmap.extend_from_slice(&1u16.to_be_bytes()); // encodingID = Unicode BMP
        cmap.extend_from_slice(&12u32.to_be_bytes()); // offset
        // Format 4 with 3 segments: 0x4E2D, 0xE000, 0xFFFF
        let mut f4 = Vec::new();
        f4.extend_from_slice(&4u16.to_be_bytes());
        f4.extend_from_slice(&38u16.to_be_bytes()); // length = 14 + 3*8
        f4.extend_from_slice(&0u16.to_be_bytes()); // lang
        f4.extend_from_slice(&6u16.to_be_bytes()); // segCountX2
        f4.extend_from_slice(&4u16.to_be_bytes()); // searchRange
        f4.extend_from_slice(&1u16.to_be_bytes()); // entrySelector
        f4.extend_from_slice(&2u16.to_be_bytes()); // rangeShift
        for end in [0x4E2Du16, 0xE000, 0xFFFF] {
            f4.extend_from_slice(&end.to_be_bytes());
        }
        f4.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        for start in [0x4E2Du16, 0xE000, 0xFFFF] {
            f4.extend_from_slice(&start.to_be_bytes());
        }
        for delta in [
            (1u16).wrapping_sub(0x4E2D),
            (1u16).wrapping_sub(0xE000),
            1u16,
        ] {
            f4.extend_from_slice(&delta.to_be_bytes());
        }
        for ro in [0u16, 0, 0] {
            f4.extend_from_slice(&ro.to_be_bytes());
        }
        cmap.extend_from_slice(&f4);
        tables.push((*b"cmap", cmap));

        // name table: ID 1, 2, 4, 16, 17
        let entries = [
            (1u16, "QlieSans1234567890"),
            (2u16, "Regular"),
            (4u16, "QlieSans1234567890 Regular"),
            (16u16, "QlieSans1234567890"),
            (17u16, subfamily_17),
        ];
        let mut recs = Vec::new();
        let mut storage = Vec::new();
        for (nid, text) in entries {
            let encoded: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            for v in [3u16, 1, 0x0409, nid, encoded.len() as u16, storage.len() as u16] {
                recs.extend_from_slice(&v.to_be_bytes());
            }
            storage.extend_from_slice(&encoded);
        }
        let mut name = Vec::new();
        name.extend_from_slice(&0u16.to_be_bytes());
        name.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        name.extend_from_slice(&((6 + recs.len()) as u16).to_be_bytes());
        name.extend_from_slice(&recs);
        name.extend_from_slice(&storage);
        tables.push((*b"name", name));

        if let Some(def_wt) = default_weight {
            // fvar v1.0: 1 wght axis, 0 named instances
            let mut fvar = Vec::new();
            for v in [1u16, 0, 16, 2, 1, 20, 0, 8] {
                fvar.extend_from_slice(&v.to_be_bytes());
            }
            fvar.extend_from_slice(b"wght");
            fvar.extend_from_slice(&((100.0 * 65536.0) as i32).to_be_bytes());
            fvar.extend_from_slice(&((def_wt * 65536.0) as i32).to_be_bytes());
            fvar.extend_from_slice(&((900.0 * 65536.0) as i32).to_be_bytes());
            fvar.extend_from_slice(&0u16.to_be_bytes());
            fvar.extend_from_slice(&256u16.to_be_bytes());
            tables.push((*b"fvar", fvar));
        }

        tables.sort_by_key(|(tag, _)| *tag);
        let n = tables.len() as u16;
        let mut out = Vec::new();
        out.extend_from_slice(&0x00010000u32.to_be_bytes());
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let mut offset = 12 + tables.len() * 16;
        for (tag, data) in &tables {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += (data.len() + 3) & !3;
        }
        for (_, data) in &tables {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn verify_font_rejects_variable_font_with_colliding_regular_subfamily() {
        let dir = tempfile::tempdir().unwrap();
        let aliases = BTreeMap::from([('\u{e000}', '中')]);
        let font_spec = FontSelection {
            path: "Sans.ttf".into(),
            face: "QlieSans1234567890 Regular".into(),
            fixed: false,
        };

        // Non-Regular default weight (100) with ID 17 = "Regular" collides in Locale Emulator
        // even when no PUA aliases are present (e.g., bundled Unicode custom font).
        fs::write(dir.path().join("Sans.ttf"), minimal_sfnt("Regular", Some(100.0))).unwrap();
        let err = verify_font(dir.path(), &font_spec, &aliases).unwrap_err();
        assert!(err.to_string().contains("排版子族"), "{err}");
        let err_no_alias = verify_font(dir.path(), &font_spec, &BTreeMap::new()).unwrap_err();
        assert!(err_no_alias.to_string().contains("排版子族"), "{err_no_alias}");

        // Preserving ID 17 = "Thin" passes cleanly both with and without PUA aliases.
        fs::write(dir.path().join("Sans.ttf"), minimal_sfnt("Thin", Some(100.0))).unwrap();
        verify_font(dir.path(), &font_spec, &aliases).unwrap();
        verify_font(dir.path(), &font_spec, &BTreeMap::new()).unwrap();

        // Static font (no fvar) with ID 17 = "Regular" passes cleanly.
        fs::write(dir.path().join("Sans.ttf"), minimal_sfnt("Regular", None)).unwrap();
        verify_font(dir.path(), &font_spec, &aliases).unwrap();
    }
}
