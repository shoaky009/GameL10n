using AvgTranslator.Core;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Tests.Translation;

public sealed class TranslationEngineTests : IDisposable
{
    private readonly string directory = Path.Combine(Path.GetTempPath(), "avg-translation-" + Guid.NewGuid().ToString("N"));
    private readonly TranslationPolicy policy = new("简体中文", "natural", new Dictionary<string, string>());

    [Fact]
    public async Task RunsWithoutGameResourcesAndResumesCommittedBatchesAcrossInstances()
    {
        var assembly = typeof(TranslationEngine).Assembly;
        Assert.Equal("AvgTranslator.Translation", assembly.GetName().Name);
        Assert.DoesNotContain(assembly.GetReferencedAssemblies(), reference => reference.Name!.StartsWith("AvgTranslator.", StringComparison.Ordinal));
        TranslationUnit[] units = [
            new("first", "chapter.txt", "1", "Hello {name}", "scene"),
            new("repeat", "another.txt", "2", "Hello {name}", "scene"),
            new("last", "chapter.txt", "3", "Goodbye {name}", "scene")
        ];
        var committed = new List<string>();
        var interrupted = new Provider((request, _) =>
        {
            var unit = Assert.Single(request.Units);
            committed.Add(unit.Id);
            if (unit.Id == "last") throw new OperationCanceledException();
            return Task.FromResult(new TranslationResponse(new Dictionary<string, string> { [unit.Id] = "你好 {name}" }, new(3, 2)));
        });
        await Assert.ThrowsAsync<OperationCanceledException>(() => new TranslationEngine(directory)
            .RunAsync(units, [interrupted], policy, new(BatchSize: 1)));
        Assert.Equal(["first", "last"], committed);

        var resumed = new Provider((request, _) =>
        {
            Assert.Equal("last", Assert.Single(request.Units).Id);
            return Task.FromResult(new TranslationResponse(new Dictionary<string, string> { ["last"] = "再见 {name}" }, new(3, 2)));
        });
        var result = await new TranslationEngine(directory).RunAsync(units, [resumed], policy);
        Assert.Equal(2, result.CacheHits);
        Assert.Equal("你好 {name}", result.Translations["first"]);
        Assert.Equal("你好 {name}", result.Translations["repeat"]);
        Assert.Equal("再见 {name}", result.Translations["last"]);
        Assert.Equal(5, result.Usage.TotalTokens);
        Assert.False(Directory.Exists(Path.Combine(directory, "sources")));

        var unused = new Provider((_, _) => throw new InvalidOperationException("Committed translations must not call the model again."));
        var cached = await new TranslationEngine(directory).RunAsync(units, [unused], policy);
        Assert.Equal(3, cached.CacheHits);
        Assert.Equal(0, cached.Usage.TotalTokens);
    }

    [Fact]
    public async Task HostValidationPreservesValidResultsAndRetriesOnlyRejectedTexts()
    {
        TranslationUnit[] units = [
            new("first", "chapter.txt", "1", "Hello", "scene"),
            new("last", "chapter.txt", "2", "Goodbye", "scene")
        ];
        IReadOnlyList<ValidationIssue> Validate(TranslationUnit unit, string translated) =>
            translated == "bad" ? [new("unsupported-character", "Resource encoding rejected this translation.", unit.Id)] : [];
        var initial = new Provider((_, _) => Task.FromResult(new TranslationResponse(
            new Dictionary<string, string> { ["first"] = "你好", ["last"] = "bad" }, new(3, 2))));
        await Assert.ThrowsAsync<InvalidDataException>(() => new TranslationEngine(directory)
            .RunAsync(units, [initial], policy, new() { RequireAllTranslations = true }, additionalValidation: Validate));
        var retry = new Provider((request, _) =>
        {
            Assert.Equal("last", Assert.Single(request.Units).Id);
            Assert.Contains("Resource encoding", Assert.Single(request.ValidationFeedback!["last"]));
            return Task.FromResult(new TranslationResponse(new Dictionary<string, string> { ["last"] = "再见" }, new(3, 2)));
        });
        var result = await new TranslationEngine(directory).RunAsync(units, [retry], policy, additionalValidation: Validate);
        Assert.Equal(1, result.CacheHits);
        Assert.Equal("你好", result.Translations["first"]);
        Assert.Equal("再见", result.Translations["last"]);
    }

    public void Dispose()
    {
        if (Directory.Exists(directory)) Directory.Delete(directory, true);
    }

    private sealed class Provider(Func<TranslationRequest, CancellationToken, Task<TranslationResponse>> translate) : ITranslationProvider
    {
        public string Identity => "standalone-test-provider";
        public Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default) =>
            translate(request, cancellationToken);
    }
}
