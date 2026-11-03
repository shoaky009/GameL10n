using System.Net;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using GameL10n.Core.Translation;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Providers;

/// <summary>Shared HTTP transport, usage accounting and diagnostics for compatible API clients.</summary>
public abstract class OpenAiCompatibleClient
{
    private const int MaximumRequestBytes = 256 * 1024;
    private const int MaximumResponseBytes = 2 * 1024 * 1024;
    protected const int MaximumContentBytes = 512 * 1024;
    private const int MaximumAttempts = 3;
    private readonly HttpClient _httpClient;
    protected readonly ProviderSettings _settings;
    private readonly Uri _requestEndpoint;
    private readonly string? _diagnosticDirectory;
    protected static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        MaxDepth = 32
    };

    protected OpenAiCompatibleClient(HttpClient httpClient, ProviderSettings settings, string? diagnosticDirectory = null)
    {
        ArgumentNullException.ThrowIfNull(httpClient);
        ArgumentNullException.ThrowIfNull(settings);
        if (settings.Endpoint is null || !settings.Endpoint.IsAbsoluteUri ||
            settings.Endpoint.Scheme is not ("http" or "https") ||
            settings.Endpoint.UserInfo.Length != 0 || settings.Endpoint.Query.Length != 0 || settings.Endpoint.Fragment.Length != 0)
            throw new ArgumentException("The provider endpoint must be an absolute HTTP(S) URL without credentials, a query, or a fragment.", nameof(settings));
        if (string.IsNullOrWhiteSpace(settings.Model) || settings.Model.Length > 256 || settings.Model.Any(char.IsControl))
            throw new ArgumentException("A nonempty model identifier is required.", nameof(settings));
        if (settings.ApiKey is null || settings.ApiKey.Any(c => char.IsControl(c) || char.IsWhiteSpace(c)))
            throw new ArgumentException("The API key contains invalid header characters.", nameof(settings));
        if (!Enum.IsDefined(settings.Protocol)) throw new ArgumentException("The API protocol is unsupported.", nameof(settings));
        if (!Enum.IsDefined(settings.TranslationFormat)) throw new ArgumentException("The translation format is unsupported.", nameof(settings));
        if (settings.AnalysisReasoningEffort is not (null or "low" or "medium" or "high" or "xhigh"))
            throw new ArgumentException("分析思考等级必须为接口默认、low、medium、high 或 xhigh。", nameof(settings));
        _httpClient = httpClient;
        _settings = settings;
        DisplayName = settings.Model;
        _diagnosticDirectory = diagnosticDirectory;
        string baseUrl = settings.Endpoint.AbsoluteUri.TrimEnd('/');
        string route = settings.Protocol == ProviderApiProtocol.Responses ? "/responses" : "/chat/completions";
        string otherRoute = settings.Protocol == ProviderApiProtocol.Responses ? "/chat/completions" : "/responses";
        if (baseUrl.EndsWith(otherRoute, StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("API 地址与所选协议不一致，请使用 /v1 基础地址或所选协议的完整接口地址。", nameof(settings));
        _requestEndpoint = new Uri(baseUrl.EndsWith(route, StringComparison.OrdinalIgnoreCase) ? baseUrl : baseUrl + route);
        // Stable cache identity contains no credentials or provider-supplied response data.
        string identityData = _requestEndpoint.AbsoluteUri + '\n' + settings.Model + '\n' + settings.Protocol;
        if (settings.TranslationFormat != TranslationInputFormat.JsonBatch) identityData += "\nplain-text-v1";
        Identity = "openai-compatible:" + Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(identityData)));
    }

    public string Identity { get; }
    public string DisplayName { get; init; }

    protected async Task<T> SendAsync<T>(byte[] payload, CancellationToken cancellationToken,
        Func<JsonElement, TokenUsage, string?, T> readResponse, bool retryRateLimits = true, bool requiresTools = false)
    {
        if (payload.Length > (requiresTools ? 4 * 1024 * 1024 : MaximumRequestBytes))
            throw new TranslationProviderException("The provider request exceeds the input size limit.",
                diagnostic: new("request-size-limit"));

        for (int attempt = 0; attempt < MaximumAttempts; attempt++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            using var request = new HttpRequestMessage(HttpMethod.Post, _requestEndpoint);
            if (_settings.ApiKey.Length != 0)
                request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _settings.ApiKey);
            request.Content = new ByteArrayContent(payload);
            request.Content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
            HttpResponseMessage response;
            try
            {
                response = await _httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { throw; }
            catch (Exception exception) when (exception is HttpRequestException or IOException or OperationCanceledException)
            {
                // No inner exception: transport diagnostics can contain credentials or endpoint data.
                throw new TranslationProviderException("The provider request outcome is unknown; check the saved batch before resending.", outcomeUnknown: true,
                    diagnostic: new(exception is OperationCanceledException ? "request-timeout" : "network-error", exception.GetType().Name));
            }
            using (response)
            {
                if (retryRateLimits && response.StatusCode == HttpStatusCode.TooManyRequests && attempt + 1 < MaximumAttempts)
                {
                    TimeSpan delay = response.Headers.RetryAfter?.Delta ??
                        (response.Headers.RetryAfter?.Date is DateTimeOffset retryAt ? retryAt - DateTimeOffset.UtcNow : TimeSpan.FromSeconds(attempt + 1));
                    delay = TimeSpan.FromMilliseconds(Math.Clamp(delay.TotalMilliseconds, 0, 30_000));
                    await Task.Delay(delay, cancellationToken);
                    continue;
                }
                if (!response.IsSuccessStatusCode)
                    throw new TranslationProviderException(requiresTools && response.StatusCode is
                        HttpStatusCode.BadRequest or HttpStatusCode.NotFound or HttpStatusCode.MethodNotAllowed or HttpStatusCode.UnprocessableEntity
                            ? $"The provider rejected the tool-calling request (HTTP {(int)response.StatusCode}). Check that the selected model and API protocol support function tools."
                            : $"The provider rejected the request (HTTP {(int)response.StatusCode}).",
                        outcomeUnknown: (int)response.StatusCode >= 500, statusCode: response.StatusCode);
                byte[] bytes;
                try { bytes = await ReadBoundedResponseAsync(response.Content, cancellationToken); }
                catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { throw; }
                catch (Exception exception) when (exception is HttpRequestException or IOException or OperationCanceledException)
                {
                    throw new TranslationProviderException("The provider response was interrupted; the request outcome is unknown.", outcomeUnknown: true,
                        diagnostic: new(exception is OperationCanceledException ? "request-timeout" : "network-interrupted", exception.GetType().Name));
                }
                TokenUsage? usage = null;
                try
                {
                    using JsonDocument document = JsonDocument.Parse(bytes, new JsonDocumentOptions { MaxDepth = 32 });
                    JsonElement root = document.RootElement;
                    usage = ReadUsage(root, payload.Length, bytes.Length);
                    if (root.ValueKind != JsonValueKind.Object)
                        throw ResponseFailure("The API response root is not an object.", "api-envelope", usage);
                    string? requestId = root.TryGetProperty("id", out JsonElement id) && id.ValueKind == JsonValueKind.String ? id.GetString() : null;
                    if (requestId is not null && (requestId.Length > 256 || requestId.Any(char.IsControl) ||
                        (_settings.ApiKey.Length != 0 && requestId.Contains(_settings.ApiKey, StringComparison.Ordinal))))
                        requestId = null;
                    return readResponse(root, usage, requestId);
                }
                catch (JsonException error)
                {
                    // Do not persist the raw HTTP envelope: it can include reasoning or echoed credentials.
                    throw ResponseFailure("The API response is not valid JSON.", "api-json-syntax", usage,
                        detail: $"Invalid JSON at line {error.LineNumber}, byte {error.BytePositionInLine} (zero-based).");
                }
                catch (InvalidOperationException)
                {
                    throw ResponseFailure("The API response contains fields with unexpected types.", "api-envelope", usage);
                }
            }
        }
        throw new InvalidOperationException("The bounded retry loop ended unexpectedly.");
    }

    private static async Task<byte[]> ReadBoundedResponseAsync(HttpContent content, CancellationToken cancellationToken)
    {
        if (content.Headers.ContentLength > MaximumResponseBytes)
            throw new TranslationProviderException("The provider response exceeds the output size limit.", diagnostic: new("response-size-limit"));
        await using Stream input = await content.ReadAsStreamAsync(cancellationToken);
        using var output = new MemoryStream();
        byte[] buffer = new byte[8192];
        int count;
        while ((count = await input.ReadAsync(buffer, cancellationToken)) != 0)
        {
            if (output.Length + count > MaximumResponseBytes)
                throw new TranslationProviderException("The provider response exceeds the output size limit.", diagnostic: new("response-size-limit"));
            output.Write(buffer, 0, count);
        }
        return output.ToArray();
    }

    private TokenUsage ReadUsage(JsonElement root, int inputBytes, int outputBytes)
    {
        long? input = null;
        long? output = null;
        if (root.ValueKind == JsonValueKind.Object && root.TryGetProperty("usage", out JsonElement usage) && usage.ValueKind == JsonValueKind.Object)
        {
            input = TokenCount(usage, _settings.Protocol == ProviderApiProtocol.Responses ? "input_tokens" : "prompt_tokens");
            output = TokenCount(usage, _settings.Protocol == ProviderApiProtocol.Responses ? "output_tokens" : "completion_tokens");
        }
        return new TokenUsage(input ?? Math.Max(1, (inputBytes + 2L) / 3), output ?? Math.Max(1, (outputBytes + 2L) / 3), input is null || output is null);
    }

    protected static long? TokenCount(JsonElement usage, string property) =>
        usage.TryGetProperty(property, out JsonElement value) && value.ValueKind == JsonValueKind.Number &&
        value.TryGetInt64(out long count) && count is >= 0 and <= 1_000_000_000 ? count : null;

    protected TranslationProviderException ResponseFailure(string message, string code, TokenUsage? usage,
        string? content = null, string? requestId = null, string? detail = null, bool outcomeUnknown = false)
    {
        const int previewLength = 16 * 1024;
        string? preview = content is null ? null : RedactCredential(content);
        bool truncated = preview?.Length > previewLength;
        if (truncated) preview = preview![..(previewLength / 2)] + "\n[... truncated ...]\n" + preview[^(previewLength / 2)..];
        return new TranslationProviderException(message, outcomeUnknown, usage: usage,
            diagnostic: new(code, detail is null ? null : RedactCredential(detail), preview, truncated, requestId));
    }

    protected string RedactCredential(string text)
    {
        if (_settings.ApiKey.Length == 0) return text;
        // Model content may echo a key as plain text or as a JSON-escaped string.
        foreach (string secret in new[] { _settings.ApiKey, JsonSerializer.Serialize(_settings.ApiKey)[1..^1],
                     JsonSerializer.Serialize(_settings.ApiKey, Json)[1..^1] }.Distinct().OrderByDescending(value => value.Length))
            text = text.Replace(secret, "[redacted]", StringComparison.Ordinal);
        return text;
    }

    protected async Task WriteFailureLogAsync(string operation, TranslationProviderException error, object? requestDetails = null)
    {
        if (_diagnosticDirectory is null) return;
        var record = new
        {
            timestamp = DateTimeOffset.UtcNow, operation, protocol = _settings.Protocol, translationFormat = _settings.TranslationFormat,
            model = RedactCredential(_settings.Model), error.Message, error.FailureSummary, error.StatusCode, error.OutcomeUnknown,
            error.Usage, error.Diagnostic, requestDetails, error.StackTrace
        };
        string name = $"api-{DateTime.UtcNow:yyyyMMdd-HHmmssfff}-{Guid.NewGuid():N}.json";
        string fallback = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "GameL10n", "logs");
        foreach (string directory in new[] { _diagnosticDirectory, fallback }.Distinct(StringComparer.OrdinalIgnoreCase))
        {
            try
            {
                await LocalFiles.WriteJsonAsync(Path.Combine(directory, name), record, CancellationToken.None);
                return;
            }
            catch (Exception logError) when (logError is IOException or UnauthorizedAccessException)
            {
                // Diagnostics must not change the retry policy or discard committed translations.
                System.Diagnostics.Debug.WriteLine("Could not write API diagnostic log: " + logError.GetType().Name);
            }
        }
    }

    protected static string Limit(string? value, int length) => value is null ? "" : value[..Math.Min(value.Length, length)];
}
