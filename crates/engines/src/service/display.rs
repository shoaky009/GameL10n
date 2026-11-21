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
    ) -> Result<(), EngineError> {
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
        if evidence.aliases.is_empty() {
            return Ok(());
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
        Ok(())
    }
}
