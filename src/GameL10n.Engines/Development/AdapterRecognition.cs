namespace GameL10n.Core.Development;

/// <summary>Declarative evidence only. Ranking, I/O budgets and execution policy belong to the host.</summary>
public sealed record AdapterRecognition(int SchemaVersion,
    IReadOnlyList<RecognitionFileRule> Required, IReadOnlyList<RecognitionFileRule> Hints)
{
    private const string FileRuleSchema = """
        {"type":"object","properties":{
        "patterns":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string"}},
        "minimumSize":{"type":"integer","minimum":0},"maximumSize":{"type":"integer","minimum":0},
        "signatures":{"type":"array","maxItems":8,"items":{"type":"object","properties":{
        "offset":{"type":"integer","minimum":0},"hex":{"type":"string","minLength":2,"maxLength":128},
        "origin":{"type":"string","enum":["start","end"]}},"required":["offset","hex"],"additionalProperties":false}}},
        "required":["patterns"],"additionalProperties":false}
        """;
    internal static string JsonSchema =>
        """{"type":"object","properties":{"schemaVersion":{"type":"integer","enum":[1]},"required":{"type":"array","maxItems":32,"items": """
        + FileRuleSchema + """},"hints":{"type":"array","maxItems":32,"items": """ + FileRuleSchema
        + """}},"required":["schemaVersion","required","hints"],"additionalProperties":false}""";

    public void Validate()
    {
        if (SchemaVersion != 1) throw new InvalidDataException("不支持的识别声明版本。");
        if (Required is null || Hints is null || Required.Count + Hints.Count > 32)
            throw new InvalidDataException("识别声明最多包含 32 条文件规则。");
        foreach (var rule in Required.Concat(Hints))
        {
            if (rule is null || rule.Patterns is not { Count: > 0 and <= 16 })
                throw new InvalidDataException("识别文件规则必须包含 1～16 个可替代的路径模式。");
            try { GeneratedAdapterResources.ValidatePatterns(rule.Patterns); }
            catch (GeneratedAdapterException error) { throw new InvalidDataException("识别路径模式无效。", error); }
            if (rule.MinimumSize < 0 || rule.MaximumSize < 0 || rule.MinimumSize > rule.MaximumSize)
                throw new InvalidDataException("识别规则的文件大小范围无效。");
            if (rule.Signatures is { Count: > 8 }) throw new InvalidDataException("每条规则最多包含 8 个字节签名。");
            foreach (var signature in rule.Signatures ?? [])
                if (signature is null || signature.Offset < 0 || signature.Origin is not ("start" or "end")
                    || signature.Hex is not { Length: >= 2 and <= 128 } || signature.Hex.Length % 2 != 0
                    || signature.Hex.Any(ch => !char.IsAsciiHexDigit(ch)))
                    throw new InvalidDataException("字节签名必须包含 1～64 字节的十六进制数据、非负偏移和 start/end 原点。");
        }
    }
}

/// <summary>Any pattern may select a file. All size/signature predicates must hold on the same file.</summary>
public sealed record RecognitionFileRule(IReadOnlyList<string> Patterns, long? MinimumSize = null,
    long? MaximumSize = null, IReadOnlyList<RecognitionSignature>? Signatures = null);

/// <summary>start: offset from byte zero; end: offset backwards from EOF to the end of the signature.</summary>
public sealed record RecognitionSignature(long Offset, string Hex, string Origin = "start");

public enum RecognitionStatus { Match, NoMatch, Unknown }
public sealed record RecognitionResult(RecognitionStatus Status, int Evidence);

/// <summary>Derived metadata; does not authorize package execution.</summary>
public sealed record AdapterDiscoveryDescriptor(IReadOnlyList<string> ResourceFiles,
    IReadOnlyList<string>? ResourcePatterns, AdapterRecognition? Recognition);
public sealed record AdapterDiscoveryCandidate(string Id, string Version, DateTimeOffset? PublishedAt,
    AdapterDiscoveryDescriptor? Descriptor, RecognitionResult Match, GameL10n.Core.Adapters.AdapterFamily? Family = null);
