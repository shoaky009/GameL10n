namespace AvgTranslator.Core.Development;

public sealed partial class LocalAdapterPackageStore
{
    /// <summary>Reads only derived metadata. Payload integrity is checked by LoadVersion immediately before execution.</summary>
    public Task<IReadOnlyList<AdapterDiscoveryCandidate>> ReadDiscoveryCandidatesAsync(
        ICollection<string>? diagnostics = null, CancellationToken ct = default) => WithLibraryIndexAsync(async () =>
    {
        var index = await ReadLibraryIndexAsync(ct);
        bool changed = false;
        if (!index.DiscoveryInitialized)
        {
            // Import pre-registry pins once, not once per game or per discovery pass.
            var state = ReadLibraryState();
            var activations = ReadCandidateActivations(state, diagnostics);
            foreach (var activation in activations)
            {
                if (!index.Families.TryGetValue(activation.Id, out var family)) continue;
                var old = family.Versions.GetValueOrDefault(activation.Version);
                family.Versions[activation.Version] = old is null
                    ? new(activation.Version, EngineLibrarySource.External, true, activation.ActivatedAt, true)
                    : old with { IsCandidate = true };
            }
            index.DiscoveryInitialized = true;
            changed = true;
        }
        var candidates = new List<AdapterDiscoveryCandidate>();
        foreach (var (id, family) in index.Families)
        {
            if (!family.Enabled) continue;
            foreach (var (version, entry) in family.Versions.ToArray())
            {
                ct.ThrowIfCancellationRequested();
                if (!entry.IsCandidate) continue;
                var descriptor = entry.Discovery;
                if (!entry.DiscoveryLoaded)
                {
                    try
                    {
                        var package = ReadPackage(PackagePath(id, version)) ?? throw new InvalidDataException("适配器清单为空。");
                        if (package.Id != id || package.Version != version) throw new InvalidDataException("适配器清单与索引标识不一致。");
                        descriptor = Descriptor(package);
                        family.Versions[version] = entry with { Discovery = descriptor, DiscoveryLoaded = true };
                        changed = true;
                    }
                    catch (Exception error) when (IsLibraryReadError(error))
                    {
                        // Keep missing/bad metadata as unknown. A later restore can be used without rebuilding the entire index.
                        diagnostics?.Add($"无法读取适配器识别清单 {id}/{version}：{error.Message}");
                    }
                }
                candidates.Add(new(id, version, entry.PublishedAt, descriptor, new(RecognitionStatus.Unknown, 0), family.Family));
            }
        }
        if (changed) await WriteLibraryIndexAsync(index, ct);
        return (IReadOnlyList<AdapterDiscoveryCandidate>)candidates;
    }, ct);

    private static AdapterDiscoveryDescriptor Descriptor(GeneratedAdapterPackage package) =>
        new(package.ResourceFiles, package.ResourcePatterns, package.Recognition);
}

/// <summary>Host policy, deliberately outside the adapter declaration and CLI protocol.</summary>
public static class AdapterDiscoveryPlanner
{
    public static async Task<IReadOnlyList<AdapterDiscoveryCandidate>> SelectAsync(GameDiscoverySession discovery,
        IReadOnlyList<AdapterDiscoveryCandidate> candidates, CancellationToken ct = default)
    {
        var remaining = new List<AdapterDiscoveryCandidate>();
        // Complete cheap directory filtering before issuing any content reads.
        foreach (var candidate in candidates)
        {
            ct.ThrowIfCancellationRequested();
            var match = await discovery.EvaluateAsync(candidate.Descriptor, content: false, ct);
            if (match.Status != RecognitionStatus.NoMatch) remaining.Add(candidate with { Match = match });
        }
        var result = new List<AdapterDiscoveryCandidate>();
        foreach (var candidate in remaining.OrderByDescending(item => item.Match.Evidence).ThenByDescending(item => item.PublishedAt))
        {
            ct.ThrowIfCancellationRequested();
            var match = await discovery.EvaluateAsync(candidate.Descriptor, content: true, ct);
            if (match.Status != RecognitionStatus.NoMatch) result.Add(candidate with { Match = match });
        }
        // Keep versions of the same engine together, but do not discard any version or generic/unknown candidate.
        return result.GroupBy(item => item.Id).OrderByDescending(group => group.Max(item => item.Match.Evidence))
            .ThenByDescending(group => group.Max(item => item.PublishedAt))
            .ThenBy(group => group.Key, StringComparer.Ordinal)
            .SelectMany(group => group.OrderByDescending(item => item.Match.Evidence)
                .ThenByDescending(item => item.PublishedAt).ThenBy(item => item.Version, StringComparer.Ordinal)).ToArray();
    }
}
