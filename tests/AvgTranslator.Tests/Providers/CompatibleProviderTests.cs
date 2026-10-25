using System.Net;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core;
using AvgTranslator.Core.Providers;

namespace AvgTranslator.Tests.Providers;

public sealed class CompatibleProviderTests
{
    private static readonly TranslationRequest Request = new([new("one", "script", "line", "hello {name}", "context")], "简体中文", new Dictionary<string, string>(), "natural");

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task PlainTextSendsOnlySourceAndRestoresGameFormatting(ProviderApiProtocol protocol)
    {
        var source = Request.Units[0] with { SourceText = "　Alice: Hello {name}!\nWelcome." };
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var root = body.RootElement;
            var data = protocol == ProviderApiProtocol.Responses ? root.GetProperty("input").GetString()
                : root.GetProperty("messages")[1].GetProperty("content").GetString();
            var system = protocol == ProviderApiProtocol.Responses ? root.GetProperty("instructions").GetString()
                : root.GetProperty("messages")[0].GetProperty("content").GetString();
            Assert.Equal("　Alice: Hello {v0_0}!\nWelcome.", data);
            Assert.Contains("简体中文", system);
            Assert.Contains("natural", system);
            Assert.Contains("Alice → 爱丽丝", system);
            Assert.DoesNotContain("Unrelated", system);
            Assert.DoesNotContain(source.Context, system);
            if (protocol == ProviderApiProtocol.ChatCompletions)
            {
                Assert.Equal(4096, root.GetProperty("max_tokens").GetInt32());
                Assert.False(root.TryGetProperty("max_completion_tokens", out _));
                Assert.Equal(2, root.GetProperty("messages").GetArrayLength());
            }
            return ForProtocol("　爱丽丝：你好 {v0_0}！\n欢迎。", protocol);
        });
        using var client = new HttpClient(handler);
        var provider = new OpenAiCompatibleProvider(client, new ProviderSettings("https://example.invalid/v1", "", "local",
            protocol, translationFormat: TranslationInputFormat.PlainText));
        Assert.Equal(1, new ScheduledProvider(provider, Guid.NewGuid().ToString()).MaximumBatchSize);
        var result = await provider.TranslateAsync(Request with
        {
            Units = [source], Terminology = new Dictionary<string, string> { ["Alice"] = "爱丽丝", ["Unrelated"] = "无关" }
        });
        Assert.Equal("　爱丽丝：你好 {name}！\n欢迎。", result.Translations["one"]);
        Assert.Empty(AvgTranslator.Core.Translation.TextValidation.Validate(source, result.Translations["one"]));
        Assert.False(result.Usage.Estimated);
    }

    [Fact]
    public async Task PlainTextDoesNotGuessMissingPlaceholdersOrDiscardRefusalLikeDialogue()
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response("我不能帮你。"))));
        var provider = new OpenAiCompatibleProvider(client, new ProviderSettings("https://example.invalid/v1", "", "local",
            translationFormat: TranslationInputFormat.PlainText));
        var result = await provider.TranslateAsync(Request);
        Assert.Equal("我不能帮你。", result.Translations["one"]);
        Assert.Contains(AvgTranslator.Core.Translation.TextValidation.Validate(Request.Units[0], result.Translations["one"]),
            issue => issue.Code == "placeholder-mismatch");
    }

    [Theory]
    [InlineData("length", "output-token-limit")]
    [InlineData("content_filter", "model-refusal")]
    public async Task PlainTextRejectsTruncationAndExplicitContentFiltering(string finish, string code)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response("译文", finishReason: finish))));
        var provider = new OpenAiCompatibleProvider(client, new ProviderSettings("https://example.invalid/v1", "", "local",
            translationFormat: TranslationInputFormat.PlainText));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => provider.TranslateAsync(Request));
        Assert.Equal(code, error.Diagnostic?.Code);
        Assert.NotNull(error.Usage);
    }

    [Fact]
    public async Task PlainTextRejectsMultipleUnitsBeforeCallingApiAndHasSeparateRequestIdentity()
    {
        var handler = new Handler(_ => throw new InvalidOperationException("Unexpected call"));
        using var client = new HttpClient(handler);
        var settings = new ProviderSettings("https://example.invalid/v1", "", "local");
        var provider = new OpenAiCompatibleProvider(client, settings with { TranslationFormat = TranslationInputFormat.PlainText });
        Assert.NotEqual(new OpenAiCompatibleProvider(client, settings).Identity, provider.Identity);
        await Assert.ThrowsAsync<ArgumentException>(() => provider.TranslateAsync(Request with
        {
            Units = [Request.Units[0], Request.Units[0] with { Id = "two" }]
        }));
        Assert.Empty((await provider.TranslateAsync(Request with { Units = [] })).Translations);
        Assert.Equal(0, handler.Calls);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task HidesGameSyntaxAndLongIdsAndRestoresIndependentFragments(ProviderApiProtocol protocol)
    {
        var first = Request.Units[0] with { Id = new string('a', 90), SourceText = "Gain (+x0%) Stats from Dexterity and" };
        var second = first with { Id = new string('b', 90), SourceText = " Intelligence." };
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var data = protocol == ProviderApiProtocol.Responses ? body.RootElement.GetProperty("input").GetString()
                : body.RootElement.GetProperty("messages")[1].GetProperty("content").GetString();
            using var document = JsonDocument.Parse(data!);
            var sent = document.RootElement.GetProperty("units");
            Assert.Equal("0", sent[0].GetProperty("id").GetString());
            Assert.Equal("Gain {v0_0} Stats from Dexterity and", sent[0].GetProperty("sourceText").GetString());
            Assert.Equal(" Intelligence.", sent[1].GetProperty("sourceText").GetString());
            Assert.DoesNotContain("(+x0%)", data);
            Assert.DoesNotContain(first.Id, data);
            return ForProtocol("""{"0":"获得{v0_0}的属性加成，来源为敏捷和","1":" 智力。"}""", protocol);
        });
        using var client = new HttpClient(handler);
        var result = await Provider(client, protocol).TranslateAsync(Request with { Units = [first, second] });
        Assert.Equal("获得(+x0%)的属性加成，来源为敏捷和", result.Translations[first.Id]);
        Assert.Equal(" 智力。", result.Translations[second.Id]);
    }

    [Fact]
    public async Task TemporaryTokensCannotCollideWithLiteralSourceTokens()
    {
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            using var data = JsonDocument.Parse(body.RootElement.GetProperty("messages")[1].GetProperty("content").GetString()!);
            Assert.Equal("Value {vv0_0} and {vv0_1}", data.RootElement.GetProperty("units")[0].GetProperty("sourceText").GetString());
            return Response("""{"one":"数值{vv0_0}与{vv0_1}"}""");
        });
        using var client = new HttpClient(handler);
        var result = await Provider(client).TranslateAsync(Request with { Units = [Request.Units[0] with { SourceText = "Value {v0_0} and (x0)" }] });
        Assert.Equal("数值{v0_0}与(x0)", result.Translations["one"]);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task SendsProtectedMarkersWithoutFreezingBracketedProse(ProviderApiProtocol protocol)
    {
        const string source = "[name] <color=grey>[Will reroll visitors.]</color> [[+25% increased damage]]";
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var data = protocol == ProviderApiProtocol.Responses ? body.RootElement.GetProperty("input").GetString()
                : body.RootElement.GetProperty("messages")[1].GetProperty("content").GetString();
            using var document = JsonDocument.Parse(data!);
            var unit = Assert.Single(document.RootElement.GetProperty("units").EnumerateArray());
            Assert.Equal("{v0_0} {v0_1}{v0_2}Will reroll visitors.{v0_3}{v0_4} {v0_5}+25% increased damage{v0_6}", unit.GetProperty("sourceText").GetString());
            Assert.Equal(new[] { "{v0_0}", "{v0_1}", "{v0_2}", "{v0_3}", "{v0_4}", "{v0_5}", "{v0_6}" },
                unit.GetProperty("protectedMarkers").EnumerateArray().Select(item => item.GetProperty("text").GetString()));
            Assert.All(unit.GetProperty("protectedMarkers").EnumerateArray(), item => Assert.Equal(1, item.GetProperty("count").GetInt32()));
            Assert.False(unit.TryGetProperty("translatableBracketGroups", out _));
            return ForProtocol("{\"one\":\"{v0_0} {v0_1}{v0_2}将重新随机生成访客。{v0_3}{v0_4} {v0_5}伤害提高25%{v0_6}\"}", protocol);
        });
        using var client = new HttpClient(handler);
        var result = await Provider(client, protocol).TranslateAsync(Request with { Units = [Request.Units[0] with { SourceText = source }] });
        Assert.Contains("[将重新随机生成访客。]", result.Translations["one"]);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task SendsMarkerCountsAndPreviousValidationFeedback(ProviderApiProtocol protocol)
    {
        const string source = "When Stamina<Stamina> is drained, instead of ((emptying)) Stamina Recovery<Stamina> progress, the Stamina Recovery<Stamina> progress is [[halved]].";
        const string feedback = "缺少或减少了“<Stamina>”（原文 3 次，译文 2 次）。";
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var data = protocol == ProviderApiProtocol.Responses ? body.RootElement.GetProperty("input").GetString()
                : body.RootElement.GetProperty("messages")[1].GetProperty("content").GetString();
            using var document = JsonDocument.Parse(data!);
            var unit = Assert.Single(document.RootElement.GetProperty("units").EnumerateArray());
            var marker = unit.GetProperty("protectedMarkers")[0];
            Assert.Equal("{v0_0}", marker.GetProperty("text").GetString());
            Assert.Equal(3, marker.GetProperty("count").GetInt32());
            Assert.Equal(feedback.Replace("<Stamina>", "{v0_0}"), Assert.Single(unit.GetProperty("validationFeedback").EnumerateArray()).GetString());
            return ForProtocol(JsonSerializer.Serialize(new Dictionary<string, string> { ["one"] = "耐力{v0_0}耗尽时，不会((清空))耐力恢复{v0_0}进度，而是将耐力恢复{v0_0}进度{v0_1}减半{v0_2}。" }), protocol);
        });
        using var client = new HttpClient(handler);
        var request = Request with
        {
            Units = [Request.Units[0] with { SourceText = source }],
            ValidationFeedback = new Dictionary<string, IReadOnlyList<string>> { ["one"] = [feedback] }
        };
        var result = await Provider(client, protocol).TranslateAsync(request);
        Assert.Empty(AvgTranslator.Core.Translation.TextValidation.Validate(request.Units[0], result.Translations["one"]));
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task SendsStandaloneEmphasisAsTranslatableInsteadOfPartialProtectedTokens(ProviderApiProtocol protocol)
    {
        const string source = "[name] has [[Infinite]] Gold<sprite name=\"Gold\"> and [[also]] gains [[+1]] heart.";
        var handler = new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var data = protocol == ProviderApiProtocol.Responses ? body.RootElement.GetProperty("input").GetString()
                : body.RootElement.GetProperty("messages")[1].GetProperty("content").GetString();
            using var document = JsonDocument.Parse(data!);
            var unit = Assert.Single(document.RootElement.GetProperty("units").EnumerateArray());
            Assert.Equal(new[] { "{v0_0}", "{v0_1}", "{v0_2}", "{v0_3}", "{v0_4}" },
                unit.GetProperty("protectedMarkers").EnumerateArray().Select(item => item.GetProperty("text").GetString()));
            Assert.Equal("{v0_0} has {v0_1}Infinite{v0_2} Gold{v0_3} and {v0_1}also{v0_2} gains {v0_4} heart.", unit.GetProperty("sourceText").GetString());
            return ForProtocol(JsonSerializer.Serialize(new Dictionary<string, string> { ["one"] = "{v0_0}拥有{v0_1}无限{v0_2}金币{v0_3}，{v0_1}也{v0_2}获得{v0_4}颗心。" }), protocol);
        });
        using var client = new HttpClient(handler);
        var request = Request with { Units = [Request.Units[0] with { SourceText = source }] };
        var result = await Provider(client, protocol).TranslateAsync(request);
        Assert.Empty(AvgTranslator.Core.Translation.TextValidation.Validate(request.Units[0], result.Translations["one"]));
    }

    [Fact]
    public async Task SendsConfiguredEndpointAndAuthAndReadsActualUsage()
    {
        var handler = new Handler(async request =>
        {
            Assert.Equal("https://example.invalid/v1/chat/completions", request.RequestUri!.AbsoluteUri);
            Assert.Equal("Bearer private-key", request.Headers.Authorization!.ToString());
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            Assert.Equal("chosen-model", body.RootElement.GetProperty("model").GetString());
            return Response("{\"one\":\"你好 {name}\"}");
        });
        using var client = new HttpClient(handler);
        var result = await Provider(client).TranslateAsync(Request);
        Assert.Equal("你好 {name}", result.Translations["one"]);
        Assert.Equal(new TokenUsage(10, 5), result.Usage);
        Assert.Null(client.DefaultRequestHeaders.Authorization);
    }

    [Fact]
    public async Task RateLimitAndUnauthorizedLeaveRetriesToPipelineAndDoNotLeakSecret()
    {
        var handler = new Handler(_ =>
        {
            var response = new HttpResponseMessage(HttpStatusCode.TooManyRequests);
            response.Headers.RetryAfter = new RetryConditionHeaderValue(TimeSpan.Zero);
            return Task.FromResult(response);
        });
        using var client = new HttpClient(handler);
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client).TranslateAsync(Request));
        Assert.Equal(1, handler.Calls);
        Assert.False(error.OutcomeUnknown);
        Assert.Contains("限流或额度限制", error.FailureSummary);
        var rejected = new Handler(_ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.Unauthorized) { Content = new StringContent("private-key") }));
        using var rejectedClient = new HttpClient(rejected);
        error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(rejectedClient).TranslateAsync(Request));
        Assert.Equal(1, rejected.Calls);
        Assert.DoesNotContain("private-key", error.ToString());
        Assert.Contains("身份验证失败", error.FailureSummary);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task AmbiguousTransportFailureIsNotRetried(ProviderApiProtocol protocol)
    {
        var handler = new Handler(_ => throw new HttpRequestException("secret private-key"));
        using var client = new HttpClient(handler);
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).TranslateAsync(Request));
        Assert.True(error.OutcomeUnknown);
        Assert.Equal(1, handler.Calls);
        Assert.DoesNotContain("private-key", error.ToString());
        Assert.Equal("网络连接异常", error.FailureSummary);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task DistinguishesTimeoutFromUserCancellation(ProviderApiProtocol protocol)
    {
        using var client = new HttpClient(new Handler(_ => throw new TaskCanceledException("private-key")));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).TranslateAsync(Request));
        Assert.Equal("请求超时", error.FailureSummary);
        Assert.Equal("request-timeout", error.Diagnostic!.Code);
        Assert.DoesNotContain("private-key", JsonSerializer.Serialize(error.Diagnostic));

        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => Provider(client, protocol).TranslateAsync(Request, cancellation.Token));
    }

    [Theory]
    [InlineData(HttpStatusCode.Forbidden, "权限")]
    [InlineData(HttpStatusCode.BadRequest, "请求被拒绝")]
    [InlineData(HttpStatusCode.ServiceUnavailable, "服务商异常")]
    [InlineData(HttpStatusCode.GatewayTimeout, "服务商响应超时")]
    public async Task DescribesHttpFailureWithoutExposingResponseBody(HttpStatusCode status, string summary)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(new HttpResponseMessage(status)
            { Content = new StringContent("private-key detailed provider error") })));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client).TranslateAsync(Request));
        Assert.Contains(summary, error.FailureSummary);
        Assert.Contains(((int)status).ToString(), error.FailureSummary);
        Assert.DoesNotContain("private-key", error.FailureSummary);
    }

    [Theory]
    [InlineData("{\"wrong\":\"你好\"}", "translation-unknown-id")]
    [InlineData("{\"one\":\"你好\",\"one\":\"另一条\"}", "translation-duplicate-id")]
    [InlineData("{}", "translation-missing-ids")]
    [InlineData("{\"one\":null}", "translation-value-type")]
    [InlineData("[]", "translation-root")]
    [InlineData("{\"one\":\"你好", "translation-json-syntax")]
    [InlineData("Here is the result: {\"one\":\"你好\"}", "translation-json-syntax")]
    [InlineData("```json\n{\"one\":\"你好\"}\n```\n```json\n{}\n```", "translation-json-syntax")]
    [InlineData("```json\n{\"wrong\":\"你好\"}\n```", "translation-unknown-id")]
    public async Task RejectsInvalidTranslationsWithSpecificDiagnosticAndUsage(string content, string code)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(content))));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client).TranslateAsync(Request));
        Assert.Equal(code, error.Diagnostic!.Code);
        Assert.Equal(content, error.Diagnostic.ResponsePreview);
        Assert.Equal(new TokenUsage(10, 5), error.Usage);
        Assert.Equal("request-id", error.Diagnostic.RequestId);
        Assert.StartsWith("返回格式不正确", error.FailureSummary);
        if (code == "translation-duplicate-id") Assert.Contains("文本 ID 重复", error.FailureSummary);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task AcceptsWholeResponseJsonFencesWithoutChangingTranslatedText(ProviderApiProtocol protocol)
    {
        foreach (string content in new[]
        {
            "```json\n{\"one\":\"你好 {name}\\n第二行\"}\n```",
            " \r\n```JSON\r\n{\"one\":\"你好 {name}\\n第二行\"}\r\n``` \r\n",
            "```\n{\"one\":\"你好 {name}\\n第二行\"}\n```",
            "\uFEFF{\"one\":\"你好 {name}\\n第二行\"}"
        })
        {
            using var client = new HttpClient(new Handler(_ => Task.FromResult(ForProtocol(content, protocol))));
            var response = await Provider(client, protocol).TranslateAsync(Request);
            Assert.Equal("你好 {name}\n第二行", response.Translations["one"]);
        }
    }

    [Theory]
    [InlineData("length", "output-token-limit")]
    [InlineData("content_filter", "model-refusal")]
    public async Task RejectsIncompleteChatOutputEvenIfJsonIsValid(string finishReason, string code)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response("{\"one\":\"你好\"}", finishReason: finishReason))));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client).TranslateAsync(Request));
        Assert.Equal(code, error.Diagnostic!.Code);
        Assert.Equal(new TokenUsage(10, 5), error.Usage);
        if (code == "output-token-limit") Assert.Contains("被截断", error.FailureSummary);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task RecognizesPlainTextAndErrorEnvelopeRefusalsWithoutTreatingDialogueAsRefusal(ProviderApiProtocol protocol)
    {
        const string refusal = "抱歉，我无法翻译涉及未成年或疑似未成年角色的露骨性内容。可以协助翻译删除露骨性描写后的版本。";
        foreach (string content in new[]
        {
            refusal,
            JsonSerializer.Serialize(new { error = refusal }),
            JsonSerializer.Serialize(new { refusal }),
            JsonSerializer.Serialize(new { error = new { message = refusal } }),
            "```json\n" + JsonSerializer.Serialize(new { error = refusal }) + "\n```",
            "I'm sorry, I can't assist with that request."
        })
        {
            var handler = new Handler(_ => Task.FromResult(ForProtocol(content, protocol)));
            using var client = new HttpClient(handler);
            var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).TranslateAsync(Request));
            Assert.True(error.IsModelRefusal);
            Assert.Equal(content, error.Diagnostic!.ResponsePreview);
            Assert.NotNull(error.Usage);
            Assert.Equal(1, handler.Calls);
        }
        const string dialogue = "抱歉，我不能帮助你。";
        using var dialogueClient = new HttpClient(new Handler(_ => Task.FromResult(ForProtocol(JsonSerializer.Serialize(new { one = dialogue }), protocol))));
        Assert.Equal(dialogue, (await Provider(dialogueClient, protocol).TranslateAsync(Request)).Translations["one"]);

        var errorIdRequest = Request with { Units = [Request.Units[0] with { Id = "error" }] };
        using var errorIdClient = new HttpClient(new Handler(_ => Task.FromResult(ForProtocol(JsonSerializer.Serialize(new { error = dialogue }), protocol))));
        Assert.Equal(dialogue, (await Provider(errorIdClient, protocol).TranslateAsync(errorIdRequest)).Translations["error"]);
    }

    [Theory]
    [InlineData("{\"error\":\"Invalid data format\"}")]
    [InlineData("这段对白提到我不能帮助他，但这不是模型拒绝说明。")]
    public async Task DoesNotTreatOtherMalformedOutputAsContentRefusal(string content)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(content))));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client).TranslateAsync(Request));
        Assert.False(error.IsModelRefusal);
        Assert.StartsWith("返回格式不正确", error.FailureSummary);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task RecognizesProtocolRefusalAndPreservesItsReason(ProviderApiProtocol protocol)
    {
        const string reason = "I cannot assist with this request.";
        object body = protocol == ProviderApiProtocol.ChatCompletions
            ? new { choices = new[] { new { finish_reason = "stop", message = new { content = (string?)null, refusal = reason } } } }
            : new { status = "completed", output = new[] { new { type = "message", role = "assistant", status = "completed",
                content = new[] { new { type = "refusal", refusal = reason } } } } };
        using var client = new HttpClient(new Handler(_ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            { Content = new StringContent(JsonSerializer.Serialize(body), Encoding.UTF8, "application/json") })));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).TranslateAsync(Request));
        Assert.True(error.IsModelRefusal);
        Assert.Equal(reason, error.Diagnostic!.ResponsePreview);
    }

    [Fact]
    public async Task WritesBoundedCredentialRedactedDiagnosticToRequestedLogsDirectory()
    {
        string directory = Path.Combine(Path.GetTempPath(), "avg-provider-logs-" + Guid.NewGuid().ToString("N"));
        try
        {
            string content = "private-key " + new string('x', 20000) + " private-key";
            using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(content))));
            var provider = new OpenAiCompatibleProvider(client, new ProviderSettings("https://example.invalid/v1", "private-key", "model"), directory);
            var error = await Assert.ThrowsAsync<TranslationProviderException>(() => provider.TranslateAsync(Request));
            string log = await File.ReadAllTextAsync(Assert.Single(Directory.GetFiles(directory, "api-*.json")));
            Assert.DoesNotContain("private-key", log);
            using var document = JsonDocument.Parse(log);
            var diagnostic = document.RootElement.GetProperty("diagnostic");
            Assert.Equal("translation-json-syntax", diagnostic.GetProperty("code").GetString());
            Assert.True(diagnostic.GetProperty("responseTruncated").GetBoolean());
            Assert.Contains("[redacted]", diagnostic.GetProperty("responsePreview").GetString());
            Assert.True(diagnostic.GetProperty("responsePreview").GetString()!.Length < 17000);
            Assert.Equal(10, document.RootElement.GetProperty("usage").GetProperty("inputTokens").GetInt32());
            Assert.DoesNotContain(content, error.Message);
        }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    }

    [Fact]
    public async Task RedactsJsonEscapedCredentialsFromDiagnostics()
    {
        const string key = "secret\"key";
        string content = JsonSerializer.Serialize(new { wrong = key });
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(content))));
        var provider = new OpenAiCompatibleProvider(client, new ProviderSettings("https://example.invalid/v1", key, "model"));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => provider.TranslateAsync(Request));
        Assert.DoesNotContain("secret", error.Diagnostic!.ResponsePreview);
        Assert.Contains("[redacted]", error.Diagnostic.ResponsePreview);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task AbsentUsageIsMarkedEstimated(ProviderApiProtocol protocol)
    {
        using var client = new HttpClient(new Handler(_ => Task.FromResult(ForProtocol("{\"one\":\"你好 {name}\"}", protocol, includeUsage: false))));
        var response = await Provider(client, protocol).TranslateAsync(Request);
        Assert.True(response.Usage.Estimated);
        Assert.True(response.Usage.TotalTokens > 0);
    }

    [Theory]
    [InlineData("https://example.invalid/v1")]
    [InlineData("https://example.invalid/v1/responses/")]
    public async Task ResponsesUsesItsOwnRequestSchemaAndCollectsTextAfterReasoning(string endpoint)
    {
        var handler = new Handler(async request =>
        {
            Assert.Equal("https://example.invalid/v1/responses", request.RequestUri!.AbsoluteUri);
            Assert.Equal("Bearer private-key", request.Headers.Authorization!.ToString());
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var root = body.RootElement;
            Assert.Equal("chosen-model", root.GetProperty("model").GetString());
            Assert.Contains("Return exactly one JSON object", root.GetProperty("instructions").GetString());
            using var input = JsonDocument.Parse(root.GetProperty("input").GetString()!);
            Assert.Equal("one", input.RootElement.GetProperty("units")[0].GetProperty("id").GetString());
            Assert.False(root.GetProperty("store").GetBoolean());
            Assert.False(root.GetProperty("stream").GetBoolean());
            Assert.True(root.GetProperty("max_output_tokens").GetInt32() > 0);
            Assert.False(root.TryGetProperty("messages", out _));
            return ResponsesResponse("{\"one\":\"你好 {name}\"}");
        });
        using var client = new HttpClient(handler);
        var provider = new OpenAiCompatibleProvider(client, new ProviderSettings(endpoint, "private-key", "chosen-model", ProviderApiProtocol.Responses));
        var result = await provider.TranslateAsync(Request);
        Assert.Equal("你好 {name}", result.Translations["one"]);
        Assert.Equal(new TokenUsage(17, 9), result.Usage);
        Assert.Equal("response-id", result.ProviderRequestId);
        Assert.NotEqual(Provider(client).Identity, provider.Identity);
    }

    [Theory]
    [InlineData("incomplete", "message", "output_text", false)]
    [InlineData("in_progress", "message", "output_text", true)]
    [InlineData("completed", "message", "refusal", false)]
    [InlineData("completed", "function_call", "output_text", false)]
    public async Task ResponsesRejectsPartialRefusedOrToolOutputAndRetainsUsage(string status, string itemType, string contentType, bool unknown)
    {
        var handler = new Handler(_ => Task.FromResult(ResponsesResponse("{\"one\":\"你好 {name}\"}", status: status, itemType: itemType, contentType: contentType)));
        using var client = new HttpClient(handler);
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, ProviderApiProtocol.Responses).TranslateAsync(Request));
        Assert.Equal(new TokenUsage(17, 9), error.Usage);
        Assert.Equal(unknown, error.OutcomeUnknown);
        Assert.Equal(1, handler.Calls);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task AnalysisEffortDoesNotChangeTranslationRequests(ProviderApiProtocol protocol)
    {
        using var client = new HttpClient(new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            Assert.False(body.RootElement.TryGetProperty("reasoning", out _));
            Assert.False(body.RootElement.TryGetProperty("reasoning_effort", out _));
            return ForProtocol("{\"one\":\"你好 {name}\"}", protocol);
        }));
        var provider = new OpenAiCompatibleProvider(client, new("https://example.invalid/v1", "private-key", "chosen-model", protocol, "high"));
        Assert.Equal(Provider(client, protocol).Identity, provider.Identity);
        Assert.Equal("你好 {name}", (await provider.TranslateAsync(Request)).Translations["one"]);
    }

    private static OpenAiCompatibleProvider Provider(HttpClient client, ProviderApiProtocol protocol = ProviderApiProtocol.ChatCompletions) =>
        new(client, new ProviderSettings("https://example.invalid/v1", "private-key", "chosen-model", protocol));
    private static HttpResponseMessage ForProtocol(string content, ProviderApiProtocol protocol, bool includeUsage = true) =>
        protocol == ProviderApiProtocol.Responses ? ResponsesResponse(content, includeUsage) : Response(content, includeUsage);
    private static HttpResponseMessage ResponsesResponse(string content, bool includeUsage = true,
        string status = "completed", string itemType = "message", string contentType = "output_text") => new(HttpStatusCode.OK)
    {
        Content = new StringContent(JsonSerializer.Serialize(new
        {
            id = "response-id", status,
            output = new object[]
            {
                new { type = "reasoning", summary = new[] { new { type = "summary_text", text = "Not translation JSON" } } },
                new { type = itemType, role = "assistant", status = "completed", content = new[]
                {
                    new { type = contentType, text = content[..(content.Length / 2)] },
                    new { type = contentType, text = content[(content.Length / 2)..] }
                } }
            },
            usage = includeUsage ? new { input_tokens = 17, output_tokens = 9, output_tokens_details = new { reasoning_tokens = 4 } } : null
        }), Encoding.UTF8, "application/json")
    };
    private static HttpResponseMessage Response(string content, bool includeUsage = true, string finishReason = "stop") => new(HttpStatusCode.OK)
    {
        Content = new StringContent(JsonSerializer.Serialize(new
        {
            id = "request-id", choices = new[] { new { finish_reason = finishReason, message = new { content } } },
            usage = includeUsage ? new { prompt_tokens = 10, completion_tokens = 5 } : null
        }), Encoding.UTF8, "application/json")
    };
    private sealed class Handler(Func<HttpRequestMessage, Task<HttpResponseMessage>> handle) : HttpMessageHandler
    {
        public int Calls { get; private set; }
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) { Calls++; return handle(request); }
    }
}
