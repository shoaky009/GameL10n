using System.Text.Json;

namespace AvgTranslator.Core.Development;

public sealed record AgentToolDefinition(string Name, string Description, JsonElement Parameters);
public sealed record AgentToolCall(string Id, string Name, string Arguments);

/// <summary>ProviderData retains the complete assistant output for lossless reasoning and tool-call continuation.</summary>
public sealed record AgentMessage(string Role, string Content = "", IReadOnlyList<AgentToolCall>? ToolCalls = null,
    string? ToolCallId = null, JsonElement? ProviderData = null);

/// <param name="KnownToolNames">Optional host registry for accepting known calls whose schemas are omitted from Tools.
/// These names are local validation data and are not sent as tool definitions.</param>
public sealed record AgentRequest(string Instructions, IReadOnlyList<AgentMessage> Messages,
    IReadOnlyList<AgentToolDefinition> Tools, int MaximumOutputTokens, IReadOnlySet<string>? KnownToolNames = null);
public sealed record AgentTurn(AgentMessage Message, TokenUsage Usage, double QueueMilliseconds = 0);

public interface IAdaptationAgentModel
{
    /// <summary>Identifies endpoint, model, and protocol; callers must reset conversation history when it changes.</summary>
    string Identity { get; }
    string? DisplayName => null;
    string? AnalysisReasoningEffort => null;
    string? AnalysisModelName => null;
    Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken);
}
