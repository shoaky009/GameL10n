using System.Text;

namespace GameL10n.Core.Adapters;

/// <summary>LiLiM SFA16 scripts in AOS v2 archives, with separately verified executable build profiles.</summary>
public sealed class Sfa16AosV2Adapter : IGameAdapter
{
    public static AdapterFamily Family { get; } = new("engine", "sfa16.aos-v2", "LiLiM SFA16 / AOS v2 / SCR");
    public string Id => Family.AdapterId;
    public string Version => "2.0.4";
    public AdapterCatalogEntry CatalogEntry => new(Family, Version, Sfa16ExecutableProfiles.Known.Select(profile =>
        new GameCompatibility(profile.GameId, profile.GameTitle, profile.Variant, Version,
            "resource-roundtrip-passed")).ToArray());
    private const string ArchiveName = "scr.aos";
    private static readonly string[] Warnings =
    [
        "建议先选择少量文本试译，进入游戏确认中文显示和换行正常后，再开始全量翻译。",
        "支持翻译剧情对白、旁白、角色名和选项；图片、视频中的文字及游戏菜单不在翻译范围内。"
    ];

    public IReadOnlyList<ValidationIssue> ValidateTranslation(TranslationUnit unit, string text)
    {
        try
        {
            Sfa16Script.ValidateText(unit, text);
            _ = Sfa16Script.Chinese.GetByteCount(Sfa16Script.NormalizeChineseDisplayText(text));
            return [];
        }
        catch (EncoderFallbackException error)
        {
            int codePoint = error.IsUnknownSurrogate()
                ? char.ConvertToUtf32(error.CharUnknownHigh, error.CharUnknownLow) : error.CharUnknown;
            string character = Rune.IsValid(codePoint) ? char.ConvertFromUtf32(codePoint) : "无效字符";
            return [new ValidationIssue("unsupported-character", $"游戏的 CP936 编码无法显示“{character}”（U+{codePoint:X4}）。", unit.Id)];
        }
        catch (InvalidDataException error)
        {
            return [new ValidationIssue("script-text-invalid", error.Message, unit.Id)];
        }
    }

    public async Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        var archivePath = Path.Combine(gameDirectory, ArchiveName);
        var executablePath = Path.Combine(gameDirectory, "sfa16.exe");
        if (!File.Exists(archivePath) || !File.Exists(executablePath)) return null;
        var executable = await File.ReadAllBytesAsync(executablePath, cancellationToken);
        var profile = Sfa16ExecutableProfiles.Match(executable);
        if (profile is null) return null;
        try
        {
            var archive = AosArchive.Read(await File.ReadAllBytesAsync(archivePath, cancellationToken));
            if (!archive.Entries.Any(x => x.Name.EndsWith(".scr", StringComparison.OrdinalIgnoreCase))) return null;
            return new GameProbe("sfa16", Id, Version,
                $"{Family.Name} · {profile.GameTitle}（{profile.Variant}）", [ArchiveName, "sfa16.exe"], Warnings, "sfa16.exe");
        }
        catch (Exception error) when (error is InvalidDataException or DecoderFallbackException or OverflowException)
        {
            return null;
        }
    }

    public async Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default)
    {
        var archive = AosArchive.Read(await File.ReadAllBytesAsync(Path.Combine(sourceDirectory, ArchiveName), cancellationToken));
        var encoding = await GetScriptEncodingAsync(sourceDirectory, cancellationToken);
        var units = new List<TranslationUnit>();
        foreach (var entry in archive.Entries.Where(x => x.Name.EndsWith(".scr", StringComparison.OrdinalIgnoreCase)))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var decoded = AosHuffman.Decode(archive.GetBytes(entry));
            units.AddRange(Sfa16Script.Extract(entry.Name, decoded, encoding).Select(x => x.Unit));
        }
        return new ExtractionResult(units, Warnings);
    }

    public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory,
        CancellationToken cancellationToken = default)
    {
        if (Path.GetFullPath(sourceDirectory).TrimEnd(Path.DirectorySeparatorChar)
            .Equals(Path.GetFullPath(patchDirectory).TrimEnd(Path.DirectorySeparatorChar), StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("A patch must be built in a separate output directory.");
        var raw = await File.ReadAllBytesAsync(Path.Combine(sourceDirectory, ArchiveName), cancellationToken);
        var archive = AosArchive.Read(raw);
        var scripts = new List<(AosArchive.Entry Entry, byte[] Decoded, IReadOnlyList<Sfa16Script.TextSpan> Spans)>();
        var known = new Dictionary<string, TranslationUnit>(StringComparer.Ordinal);
        foreach (var entry in archive.Entries.Where(x => x.Name.EndsWith(".scr", StringComparison.OrdinalIgnoreCase)))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var decoded = AosHuffman.Decode(archive.GetBytes(entry));
            var spans = Sfa16Script.Extract(entry.Name, decoded, Sfa16Script.ShiftJis);
            scripts.Add((entry, decoded, spans));
            foreach (var span in spans) known.Add(span.Unit.Id, span.Unit);
        }
        foreach (var (id, translated) in translations)
        {
            if (!known.TryGetValue(id, out var unit)) throw new InvalidDataException($"Unknown translation unit: {id}");
            Sfa16Script.ValidateText(unit, translated);
        }
        var changed = translations.Any(x => x.Value != known[x.Key].SourceText);
        var replacements = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        byte[]? executable = null;
        if (changed)
        {
            // Rendering is globally CP936 in the patched executable, so every visible span
            // is re-encoded, including captions/names that retain their source wording.
            executable = Sfa16Executable.Patch(await File.ReadAllBytesAsync(Path.Combine(sourceDirectory, "sfa16.exe"), cancellationToken));
            foreach (var (entry, decoded, spans) in scripts)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var rebuilt = Sfa16Script.Rebuild(decoded, spans, translations, chinese: true);
                if (!decoded.AsSpan().SequenceEqual(rebuilt)) replacements.Add(entry.Name, AosHuffman.Encode(rebuilt));
            }
        }
        // Finish all validation/encoding before creating any deliverable.
        var output = changed ? archive.Rebuild(replacements) : raw;
        Directory.CreateDirectory(patchDirectory);
        await File.WriteAllBytesAsync(Path.Combine(patchDirectory, ArchiveName), output, cancellationToken);
        if (executable is not null)
        {
            await File.WriteAllBytesAsync(Path.Combine(patchDirectory, Sfa16Executable.FileName), executable, cancellationToken);
            var substitutions = known.Values.Count(unit =>
            {
                string text = translations.GetValueOrDefault(unit.Id, unit.SourceText);
                return Sfa16Script.NormalizeChineseDisplayText(text) != text;
            });
            IReadOnlyList<string> warnings = substitutions == 0 ? Warnings : [.. Warnings,
                $"{substitutions} 条文本中的特殊符号已按游戏支持的形式替换：・→·，♪→～。"];
            return new AdapterBuildResult([ArchiveName, Sfa16Executable.FileName], warnings, Sfa16Executable.FileName);
        }
        return new AdapterBuildResult([ArchiveName], Warnings);
    }

    public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory,
        CancellationToken cancellationToken = default)
    {
        var issues = new List<ValidationIssue>();
        try
        {
            var originalBytes = await File.ReadAllBytesAsync(Path.Combine(sourceDirectory, ArchiveName), cancellationToken);
            var patchedBytes = await File.ReadAllBytesAsync(Path.Combine(patchDirectory, ArchiveName), cancellationToken);
            if (originalBytes.AsSpan().SequenceEqual(patchedBytes))
            {
                if (File.Exists(Path.Combine(patchDirectory, Sfa16Executable.FileName)))
                    issues.Add(new ValidationIssue("unexpected-executable", "A no-op archive must not enable the Chinese renderer."));
                return new ValidationResult(issues.Count == 0, issues);
            }
            var executablePath = Path.Combine(patchDirectory, Sfa16Executable.FileName);
            if (!File.Exists(executablePath) || !Sfa16Executable.IsPatched(await File.ReadAllBytesAsync(executablePath, cancellationToken)))
                issues.Add(new ValidationIssue("executable-version", "The Chinese executable does not match the guarded patch."));
            var original = AosArchive.Read(originalBytes);
            var patched = AosArchive.Read(patchedBytes);
            if (!original.Entries.Select(x => x.Name).SequenceEqual(patched.Entries.Select(x => x.Name)))
                throw new InvalidDataException("AOS entry names or order changed.");
            for (var i = 0; i < original.Entries.Count; i++)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var entry = original.Entries[i];
                var sourceData = original.GetBytes(entry);
                var patchData = patched.GetBytes(patched.Entries[i]);
                if (!entry.Name.EndsWith(".scr", StringComparison.OrdinalIgnoreCase))
                {
                    if (!sourceData.AsSpan().SequenceEqual(patchData))
                        issues.Add(new ValidationIssue("non-script-changed", $"Non-script entry changed: {entry.Name}"));
                    continue;
                }
                var sourceScript = AosHuffman.Decode(sourceData);
                var patchScript = AosHuffman.Decode(patchData);
                var sourceSpans = Sfa16Script.Extract(entry.Name, sourceScript, Sfa16Script.ShiftJis);
                var patchSpans = Sfa16Script.Extract(entry.Name, patchScript, Sfa16Script.Chinese);
                if (!sourceSpans.Select(x => (x.Unit.Locator, x.Unit.Kind)).SequenceEqual(patchSpans.Select(x => (x.Unit.Locator, x.Unit.Kind))))
                    throw new InvalidDataException($"Script text locations changed: {entry.Name}");
                for (var j = 0; j < sourceSpans.Count; j++)
                    Sfa16Script.ValidateText(sourceSpans[j].Unit, patchSpans[j].Unit.SourceText);
                if (!Skeleton(sourceScript, sourceSpans).AsSpan().SequenceEqual(Skeleton(patchScript, patchSpans)))
                    issues.Add(new ValidationIssue("script-instructions-changed", $"Non-text script bytes changed: {entry.Name}"));
            }
        }
        catch (Exception error) when (error is IOException or InvalidDataException or DecoderFallbackException or OverflowException)
        {
            issues.Add(new ValidationIssue("invalid-patch", error.Message));
        }
        return new ValidationResult(issues.Count == 0, issues);
    }

    private static byte[] Skeleton(byte[] script, IReadOnlyList<Sfa16Script.TextSpan> spans)
    {
        using var output = new MemoryStream();
        var cursor = 0;
        foreach (var span in spans)
        {
            output.Write(script.AsSpan(cursor, span.Offset - cursor));
            cursor = span.Offset + span.Length;
        }
        output.Write(script.AsSpan(cursor));
        return output.ToArray();
    }

    private static async Task<Encoding> GetScriptEncodingAsync(string directory, CancellationToken cancellationToken)
    {
        var path = Path.Combine(directory, Sfa16Executable.FileName);
        return File.Exists(path) && Sfa16Executable.IsPatched(await File.ReadAllBytesAsync(path, cancellationToken))
            ? Sfa16Script.Chinese : Sfa16Script.ShiftJis;
    }
}
