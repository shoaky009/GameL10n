using System.Text.Json;
using System.Text.RegularExpressions;

namespace AvgTranslator.Core.Providers;

/// <summary>Recognizes response-level refusals without inspecting legitimate translated dialogue.</summary>
internal static partial class ModelRefusal
{
    public static bool IsPlainText(string text) => text.Length <= 4096 && RefusalPrefix().IsMatch(text.Trim());

    public static bool IsEnvelope(JsonElement root, IEnumerable<string> expectedIds)
    {
        if (root.ValueKind != JsonValueKind.Object) return false;
        var fields = root.EnumerateObject().ToArray();
        if (fields.Length != 1) return false;
        var field = fields[0];
        if (field.Name is not ("error" or "refusal") || expectedIds.Contains(field.Name, StringComparer.Ordinal)) return false;
        var value = field.Value;
        if (value.ValueKind == JsonValueKind.Object && value.TryGetProperty("message", out var message)) value = message;
        return value.ValueKind == JsonValueKind.String && IsPlainText(value.GetString()!);
    }

    public static bool IsRecordedRefusal(ProviderResponseDiagnostic? diagnostic, IEnumerable<string> expectedIds)
    {
        if (diagnostic?.Code == "model-refusal") return true;
        if (diagnostic is not { ResponsePreview: { } content, ResponseTruncated: false } ||
            diagnostic.Code is not ("translation-json-syntax" or "translation-unknown-id")) return false;
        if (IsPlainText(content)) return true;
        try
        {
            using var document = JsonDocument.Parse(content, new JsonDocumentOptions { MaxDepth = 16 });
            return IsEnvelope(document.RootElement, expectedIds);
        }
        catch (JsonException) { return false; }
    }

    // Deliberately narrow: a refusal must begin as a first-person denial of the requested operation.
    // Words such as "cannot" inside a valid ID-to-translation object are never inspected.
    [GeneratedRegex("^(?:(?:抱歉|对不起)[，,。:：!！\\s]*)?我(?:无法|不能|不可以)(?:(?:帮助|协助)(?:你|您)?)?(?:翻译|处理|提供|生成|协助|帮助|满足|完成)|^(?:(?:(?:I'm|I am)\\s+sorry|Sorry)[,.:!\\s]*)?(?:I\\s+(?:cannot|can't|won't|am\\s+unable\\s+to)|I'm\\s+unable\\s+to)\\s+(?:help|assist|translate|provide|fulfill|comply|process|complete)\\b",
        RegexOptions.IgnoreCase | RegexOptions.CultureInvariant)]
    private static partial Regex RefusalPrefix();
}
