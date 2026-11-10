//! Bounded font/corpus experiments. Cmap coverage is independent of game rendering acceptance.
use super::*;
use std::io::Read;

fn bytes(workspace: &Path, path: &str) -> Result<Vec<u8>, EngineError> {
    if !["work/", "resources/", "src/"]
        .iter()
        .any(|root| path.replace('\\', "/").starts_with(root))
    {
        return Err(EngineError::Protocol(
            "字体实验输入必须位于 work/、resources/ 或 src/。".into(),
        ));
    }
    let path = resolve_relative(workspace, path)?;
    let file = fs::File::open(path)?;
    let mut data = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut data)?;
    if data.len() > 64 * 1024 * 1024 {
        return Err(EngineError::Rejected(
            "字体实验单个输入不能超过 64 MiB。".into(),
        ));
    }
    Ok(data)
}

fn characters(corpus: &Value) -> Result<BTreeSet<char>, EngineError> {
    let texts: Vec<&Value> = match corpus {
        Value::Array(items) => items.iter().collect(),
        Value::Object(items) => items.values().collect(),
        _ => {
            return Err(EngineError::Protocol(
                "译文文件须为字符串数组或 ID 到字符串的对象。".into(),
            ));
        }
    };
    let mut characters = BTreeSet::new();
    for text in texts {
        let text = text.as_str().ok_or_else(|| {
            EngineError::Protocol("译文文件的每项必须是字符串，不能混入元数据。".into())
        })?;
        characters.extend(text.chars().filter(|ch| !ch.is_control()));
        if characters.len() > 100_000 {
            return Err(EngineError::Rejected(
                "字体实验最多检查 100,000 个不同 Unicode 字符。".into(),
            ));
        }
    }
    if characters.is_empty() {
        return Err(EngineError::Rejected("译文文件没有可检查字符。".into()));
    }
    Ok(characters)
}

fn codepoint(ch: char) -> String {
    format!("U+{:04X}", u32::from(ch))
}
fn sequence_dependent(ch: char) -> bool {
    matches!(u32::from(ch), 0x200c..=0x200d | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
}

fn inspect(
    data: &[u8],
    index: u32,
    characters: &BTreeSet<char>,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let face = match ttf_parser::Face::parse(data, index) {
        Ok(face) => face,
        Err(error) => {
            return Ok(json!({"status":"unreadable","error":format!("{error:?}"),"coverage":null}));
        }
    };
    if !face
        .tables()
        .cmap
        .is_some_and(|table| table.subtables.into_iter().any(|table| table.is_unicode()))
    {
        return Ok(json!({"status":"no-unicode-cmap","coverage":null}));
    }
    let mut missing = Vec::new();
    let mut contextual = Vec::new();
    for (position, &ch) in characters.iter().enumerate() {
        if position % 1024 == 0 && cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if sequence_dependent(ch) {
            contextual.push(codepoint(ch));
        } else if face.glyph_index(ch).is_none_or(|glyph| glyph.0 == 0) {
            missing.push(codepoint(ch));
        }
    }
    let tested = characters.len() - contextual.len();
    Ok(
        json!({"status":"inspected","tested":tested,"covered":tested-missing.len(),"missingCount":missing.len(),"missing":missing,"sequenceDependent":contextual,"glyphCount":face.number_of_glyphs(),"unitsPerEm":face.units_per_em(),"variable":face.is_variable()}),
    )
}

pub(super) fn run(
    workspace: &Path,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let corpus_path = text(args, "translationsFile")?;
    let report_path = text(args, "reportFile")?;
    if !report_path.replace('\\', "/").starts_with("work/") || !report_path.ends_with(".json") {
        return Err(EngineError::Protocol(
            "字体报告必须是 work/ 下的 JSON 文件。".into(),
        ));
    }
    let report = resolve_relative(workspace, report_path)?;
    let fonts = args["fonts"]
        .as_array()
        .filter(|fonts| (1..=4).contains(&fonts.len()))
        .ok_or_else(|| EngineError::Protocol("字体实验需要 1～4 项 {path, faceIndex?}。".into()))?;
    let normalized = |path: &str| path.replace('\\', "/").to_lowercase();
    if fonts
        .iter()
        .filter_map(|font| font["path"].as_str())
        .any(|path| {
            normalized(path) == normalized(corpus_path)
                || normalized(path) == normalized(report_path)
        })
        || normalized(corpus_path) == normalized(report_path)
    {
        return Err(EngineError::Rejected(
            "实验报告不能覆盖译文或字体输入。".into(),
        ));
    }
    let corpus = bytes(workspace, corpus_path)?;
    let characters = characters(&serde_json::from_slice::<Value>(&corpus)?)?;
    let mut results = Vec::new();
    for font in fonts {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let path = text(font, "path")?;
        let index = font.get("faceIndex").map_or(Ok(0), |index| {
            index
                .as_u64()
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(|| EngineError::Protocol("faceIndex 必须是非负 32 位整数。".into()))
        })?;
        let data = bytes(workspace, path)?;
        let mut result = inspect(&data, index, &characters, cancellation)?;
        result["path"] = json!(path);
        result["faceIndex"] = json!(index);
        result["sha256"] = json!(gamel10n_translation::files::hash_bytes(&data));
        results.push(result);
    }
    let full = json!({"schemaVersion":1,"source":"host-font-inspection","validation":false,"runtimeTested":false,"translationsFile":corpus_path,"translationsSha256":gamel10n_translation::files::hash_bytes(&corpus),"uniqueCodepoints":characters.len(),"fonts":results,"remainingChecks":["actual resource selection and font mapping","engine fallback, shaping and atlas behavior","representative in-game rendering"]});
    if let Some(parent) = report.parent() {
        fs::create_dir_all(parent)?;
    }
    resolve_relative(workspace, report_path)?;
    write_json(&report, &full).map_err(file_error)?;
    let mut summary = full;
    if let Some(fonts) = summary["fonts"].as_array_mut() {
        for font in fonts {
            if let Some(missing) = font["missing"].as_array_mut() {
                missing.truncate(8);
            }
        }
    }
    summary["reportFile"] = json!(report_path);
    summary["reportSha256"] = json!(hash_file(&report).map_err(file_error)?);
    summary["success"] = json!(true);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal SFNT with a Unicode format 12 cmap: one BMP and one supplementary glyph.
    fn font() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes());
        let mut maxp = vec![0, 0, 0x50, 0, 0, 3];
        let mut cmap = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12, 0, 12, 0, 0];
        cmap.extend(40u32.to_be_bytes());
        cmap.extend(0u32.to_be_bytes());
        cmap.extend(2u32.to_be_bytes());
        for (ch, glyph) in [(0x4e2du32, 1u32), (0x1f600, 2)] {
            cmap.extend(ch.to_be_bytes());
            cmap.extend(ch.to_be_bytes());
            cmap.extend(glyph.to_be_bytes());
        }
        let tables = [
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"maxp", std::mem::take(&mut maxp)),
            (*b"cmap", cmap),
        ];
        let mut data = vec![0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0];
        let mut offset = 12 + 16 * tables.len();
        for (tag, table) in &tables {
            data.extend(tag);
            data.extend(0u32.to_be_bytes());
            data.extend((offset as u32).to_be_bytes());
            data.extend((table.len() as u32).to_be_bytes());
            offset += table.len();
        }
        for (_, table) in tables {
            data.extend(table);
        }
        data
    }

    #[test]
    fn full_corpus_checks_bmp_and_supplementary_characters_and_retains_report() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("work")).unwrap();
        fs::write(
            root.path().join("work/text.json"),
            "{\"a\":\"中😀缺\",\"b\":\"中\\n\"}",
        )
        .unwrap();
        fs::write(root.path().join("work/font.ttf"), font()).unwrap();
        let args = json!({"translationsFile":"work/text.json","fonts":[{"path":"work/font.ttf"}],"reportFile":"work/report.json"});
        let result = run(root.path(), &args, &CancellationToken::new()).unwrap();
        assert_eq!(result["fonts"][0]["status"], "inspected");
        assert_eq!(result["fonts"][0]["tested"], 3);
        assert_eq!(result["fonts"][0]["covered"], 2);
        assert_eq!(result["fonts"][0]["missing"], json!(["U+7F3A"]));
        assert_eq!(result["validation"], false);
        assert!(root.path().join("work/report.json").is_file());
        fs::write(root.path().join("work/font.ttf"), b"unknown font").unwrap();
        assert_eq!(
            run(root.path(), &args, &CancellationToken::new()).unwrap()["fonts"][0]["status"],
            "unreadable"
        );
    }

    #[test]
    fn malformed_corpus_and_output_overwrite_are_rejected() {
        assert!(characters(&json!({"metadata":{"name":"not translations"}})).is_err());
        let root = tempfile::tempdir().unwrap();
        let args = json!({"translationsFile":"work/text.json","fonts":[{"path":"work/font.ttf"}],"reportFile":"work/text.json"});
        assert!(
            run(root.path(), &args, &CancellationToken::new())
                .unwrap_err()
                .to_string()
                .contains("不能覆盖")
        );
        assert!(
            inspect(&font(), 0, &BTreeSet::from(['中']), &{
                let token = CancellationToken::new();
                token.cancel();
                token
            })
            .is_err()
        );
    }
}
