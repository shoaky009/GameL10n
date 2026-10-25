using System.Text.Json;

namespace AvgTranslator.Core.Development;

public sealed record AgentToolDefinition(string Name, string Description, JsonElement Parameters);
public sealed record AgentToolCall(string Id, string Name, string Arguments);

/// <summary>ProviderData retains the complete assistant output for lossless reasoning and tool-call continuation.</summary>
public sealed record AgentMessage(string Role, string Content = "", IReadOnlyList<AgentToolCall>? ToolCalls = null,
    string? ToolCallId = null, JsonElement? ProviderData = null);

public sealed record AgentRequest(string Instructions, IReadOnlyList<AgentMessage> Messages,
    IReadOnlyList<AgentToolDefinition> Tools, int MaximumOutputTokens);
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
