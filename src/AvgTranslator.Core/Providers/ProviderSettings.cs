using System.Net;
using System.Text.Json.Serialization;

namespace AvgTranslator.Core.Providers;

[JsonConverter(typeof(JsonStringEnumConverter<ProviderApiProtocol>))]
public enum ProviderApiProtocol { ChatCompletions, Responses }

public sealed record ProviderSettings(Uri Endpoint, string ApiKey, string Model,
    ProviderApiProtocol Protocol = ProviderApiProtocol.ChatCompletions)
{
    public ProviderSettings(string endpoint, string apiKey, string model, ProviderApiProtocol protocol = ProviderApiProtocol.ChatCompletions)
        : this(new Uri(endpoint, UriKind.Absolute), apiKey, model, protocol) { }

    public override string ToString() => $"ProviderSettings {{ Endpoint = {Endpoint.GetLeftPart(UriPartial.Path)}, ApiKey = [redacted], Model = {Model}, Protocol = {Protocol} }}";
}

/// <summary>Records provider failures, known usage, and whether the remote request outcome is unknown.</summary>
public sealed class TranslationProviderException : Exception
{
    public TranslationProviderException(string message, bool outcomeUnknown = false,
        HttpStatusCode? statusCode = null, TokenUsage? usage = null,
        ProviderResponseDiagnostic? diagnostic = null) : base(message)
    {
        OutcomeUnknown = outcomeUnknown;
        StatusCode = statusCode;
        Usage = usage;
        Diagnostic = diagnostic;
    }

    public bool OutcomeUnknown { get; }
    public HttpStatusCode? StatusCode { get; }
    public TokenUsage? Usage { get; }
    public ProviderResponseDiagnostic? Diagnostic { get; }
    public bool IsModelRefusal => Diagnostic?.Code == "model-refusal";

    /// <summary>A short UI explanation derived only from known status codes and diagnostic categories.</summary>
    public string FailureSummary => StatusCode switch
    {
        HttpStatusCode.TooManyRequests => "限流或额度限制（HTTP 429）",
        HttpStatusCode.Unauthorized => "身份验证失败，请检查 API Key（HTTP 401）",
        HttpStatusCode.Forbidden => "没有访问此接口或模型的权限（HTTP 403）",
        HttpStatusCode.RequestTimeout or HttpStatusCode.GatewayTimeout => $"服务商响应超时（HTTP {(int)StatusCode.Value}）",
        >= HttpStatusCode.InternalServerError => $"服务商异常（HTTP {(int)StatusCode.Value}）",
        { } status => $"请求被拒绝（HTTP {(int)status}）",
        null => Diagnostic?.Code switch
        {
            "network-error" => "网络连接异常",
            "network-interrupted" => "接收响应时网络中断",
            "request-timeout" => "请求超时",
            "output-token-limit" => "模型输出达到长度上限，结果被截断，可减少每批文本数",
            "translation-duplicate-id" => "返回格式不正确：文本 ID 重复",
            "translation-missing-ids" => "返回格式不正确：遗漏了部分文本",
            "translation-unknown-id" => "返回格式不正确：文本 ID 与请求不符",
            "translation-value-type" => "返回格式不正确：译文不是文本",
            "translation-root" or "translation-json-syntax" or "api-json-syntax" or "api-envelope" or
                "chat-choices" or "responses-status" or "responses-output" or "response-format" => "返回格式不正确",
            "empty-content" => "模型返回了空内容",
            "model-refusal" => "模型拒绝处理这批内容（服务商内容限制）",
            "chat-incomplete" or "responses-incomplete" => "模型未返回完整结果",
            "request-size-limit" => "请求内容过大，可减少每批文本数",
            "response-size-limit" => "返回内容超过大小限制",
            _ => "原因暂未识别，请查看程序 logs 日志"
        }
    };
}

/// <summary>Bounded, credential-redacted response details for local developer logs, never UI messages.</summary>
public sealed record ProviderResponseDiagnostic(string Code, string? Detail = null,
    string? ResponsePreview = null, bool ResponseTruncated = false, string? RequestId = null);
