using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

/// <summary>A single explicit discovery pass. Never persisted or reused for extraction/build snapshots.</summary>
public sealed class GameDiscoverySession
{
    private sealed record Entry(string Path, long Length);
    private readonly Dictionary<string, Entry> files = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string[]> extensions = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string[]> names = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string[]> matches = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<(string, long, int), byte[]?> ranges = new();
    private const int MaximumRangeReads = 512, MaximumRuleFiles = 128;
    public string DirectoryPath { get; }
    public bool Complete { get; private set; } = true;
    public int FileCount => files.Count;
    public int RangeReads { get; private set; }
    public long BytesRead { get; private set; }
    public List<string> Diagnostics { get; } = [];

    private GameDiscoverySession(string directory) => DirectoryPath = Path.GetFullPath(directory);

    public static GameDiscoverySession Scan(string directory, CancellationToken ct = default)
    {
        var session = new GameDiscoverySession(directory);
        WorkspaceFiles.RejectLinkAncestors(session.DirectoryPath);
        if (!Directory.Exists(session.DirectoryPath)) throw new DirectoryNotFoundException("游戏目录不存在。");
        var pending = new Stack<string>();
        pending.Push(session.DirectoryPath);
        while (pending.TryPop(out var current))
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                foreach (var path in Directory.EnumerateFileSystemEntries(current))
                {
                    ct.ThrowIfCancellationRequested();
                    try
                    {
                        var attributes = File.GetAttributes(path);
                        if ((attributes & FileAttributes.ReparsePoint) != 0) { session.Complete = false; continue; }
                        if ((attributes & FileAttributes.Directory) != 0) pending.Push(path);
                        else
                        {
                            var relative = GeneratedAdapterResources.Normalize(Path.GetRelativePath(session.DirectoryPath, path));
                            if (!session.files.TryAdd(relative, new(relative, new FileInfo(path).Length))) session.Complete = false;
                        }
                    }
                    catch (Exception error) when (IsReadError(error)) { session.NoteReadError(path, error); }
                }
            }
            catch (Exception error) when (IsReadError(error)) { session.NoteReadError(current, error); }
        }
        foreach (var group in session.files.Keys.GroupBy(Path.GetExtension, StringComparer.OrdinalIgnoreCase))
            session.extensions[group.Key!] = group.ToArray();
        foreach (var group in session.files.Keys.GroupBy(Path.GetFileName, StringComparer.OrdinalIgnoreCase))
            session.names[group.Key!] = group.ToArray();
        return session;
    }

    private void NoteReadError(string path, Exception error)
    {
        Complete = false;
        if (Diagnostics.Count < 10) Diagnostics.Add("目录扫描未完整读取：" + Path.GetRelativePath(DirectoryPath, path) + "；" + error.Message);
    }
    private static bool IsReadError(Exception error) => error is IOException or UnauthorizedAccessException;

    public IReadOnlyList<string> Select(IReadOnlyList<string> patterns, CancellationToken ct = default)
    {
        var selected = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var pattern in patterns)
        {
            ct.ThrowIfCancellationRequested();
            var normalized = GeneratedAdapterResources.Normalize(pattern);
            if (!matches.TryGetValue(normalized, out var found))
            {
                if (!normalized.Contains('*') && !normalized.Contains('?'))
                    found = files.TryGetValue(normalized, out var entry) ? [entry.Path] : [];
                else
                {
                    IEnumerable<string> pool = files.Keys;
                    var leaf = normalized[(normalized.LastIndexOf('/') + 1)..];
                    if (!leaf.Contains('*') && !leaf.Contains('?')) pool = names.GetValueOrDefault(leaf) ?? [];
                    else
                    {
                        var extension = Path.GetExtension(leaf);
                        if (extension.Length > 0 && !extension.Contains('*') && !extension.Contains('?'))
                            pool = extensions.GetValueOrDefault(extension) ?? [];
                    }
                    var matcher = GeneratedAdapterResources.Pattern(normalized);
                    var result = new List<string>();
                    foreach (var path in pool)
                    {
                        ct.ThrowIfCancellationRequested();
                        if (matcher.IsMatch(path)) result.Add(path);
                    }
                    found = result.ToArray();
                }
                matches[normalized] = found;
            }
            selected.UnionWith(found);
        }
        return selected.Order(StringComparer.Ordinal).ToArray();
    }

    public IReadOnlyList<string> Resources(AdapterDiscoveryDescriptor descriptor, CancellationToken ct = default) =>
        descriptor.ResourcePatterns is { Count: > 0 } patterns ? Select(patterns, ct) : descriptor.ResourceFiles.Select(GeneratedAdapterResources.Normalize).Order(StringComparer.Ordinal).ToArray();

    public async Task<RecognitionResult> EvaluateAsync(AdapterDiscoveryDescriptor? descriptor, bool content, CancellationToken ct = default)
    {
        if (descriptor?.ResourceFiles is null) return new(RecognitionStatus.Unknown, 0);
        IReadOnlyList<string> resources;
        try { resources = Resources(descriptor, ct); }
        catch (Exception error) when (error is InvalidDataException or ArgumentException or GeneratedAdapterException
            or System.Text.RegularExpressions.RegexMatchTimeoutException)
        { return new(RecognitionStatus.Unknown, 0); }
        if (Complete && (resources.Count == 0 || descriptor.ResourcePatterns is not { Count: > 0 }
            && resources.Any(path => !files.ContainsKey(GeneratedAdapterResources.Normalize(path)))))
            return new(RecognitionStatus.NoMatch, 0);
        // Existing resource declarations are useful for ordering, never inferred as a conjunction of required files.
        int resourceEvidence = (descriptor.ResourcePatterns ?? []).Select(pattern =>
        {
            var extension = Path.GetExtension(pattern).ToLowerInvariant();
            return extension.Length > 1 && !extension.Contains('*') && !extension.Contains('?')
                && extension is not (".exe" or ".dll" or ".ini" or ".json" or ".xml" or ".txt" or ".png" or ".jpg" or ".ogg" or ".wav")
                && Select([pattern], ct).Count > 0 ? 1 : 0;
        }).Sum();
        resourceEvidence = Math.Min(resourceEvidence, 3);
        var recognition = descriptor.Recognition;
        if (recognition is null || recognition.SchemaVersion != 1) return new(RecognitionStatus.Unknown, resourceEvidence);
        try { recognition.Validate(); }
        catch (InvalidDataException) { return new(RecognitionStatus.Unknown, 0); }
        int evidence = resourceEvidence;
        bool unknown = false;
        foreach (var rule in recognition.Required)
        {
            var result = await MatchRuleAsync(rule, content, ct);
            if (result == RecognitionStatus.NoMatch) return new(result, evidence);
            if (result == RecognitionStatus.Unknown) unknown = true;
            else evidence += rule.Signatures is { Count: > 0 } ? 10 : 2;
        }
        foreach (var rule in recognition.Hints)
            if (await MatchRuleAsync(rule, content, ct) == RecognitionStatus.Match)
                evidence += rule.Signatures is { Count: > 0 } ? 10 : 1;
        return new(unknown ? RecognitionStatus.Unknown : RecognitionStatus.Match, evidence);
    }

    private async Task<RecognitionStatus> MatchRuleAsync(RecognitionFileRule rule, bool content, CancellationToken ct)
    {
        var selected = Select(rule.Patterns, ct);
        bool unknown = !Complete || selected.Count > MaximumRuleFiles;
        foreach (var path in selected.Take(MaximumRuleFiles))
        {
            var file = files[path];
            if (rule.MinimumSize is { } min && file.Length < min || rule.MaximumSize is { } max && file.Length > max) continue;
            if (rule.Signatures is not { Count: > 0 }) return RecognitionStatus.Match;
            if (!content) { unknown = true; continue; }
            bool mismatch = false, missing = false;
            foreach (var signature in rule.Signatures)
            {
                var expected = Convert.FromHexString(signature.Hex);
                long offset = signature.Origin == "start" ? signature.Offset : file.Length - signature.Offset - expected.Length;
                if (offset < 0 || offset > file.Length - expected.Length) { mismatch = true; break; }
                var bytes = await ReadRangeAsync(path, offset, expected.Length, ct);
                if (bytes is null) missing = true;
                else if (!bytes.AsSpan().SequenceEqual(expected)) { mismatch = true; break; }
            }
            if (!mismatch && !missing) return RecognitionStatus.Match;
            if (!mismatch) unknown = true;
        }
        return unknown ? RecognitionStatus.Unknown : RecognitionStatus.NoMatch;
    }

    public async Task<byte[]?> ReadRangeAsync(string relative, long offset, int length, CancellationToken ct)
    {
        var key = (relative.ToUpperInvariant(), offset, length);
        if (ranges.TryGetValue(key, out var cached)) return cached;
        if (RangeReads >= MaximumRangeReads || BytesRead + length > 8L * 1024 * 1024 || length is < 1 or > 65536) return null;
        byte[]? bytes = null;
        try
        {
            var path = WorkspaceFiles.Resolve(DirectoryPath, relative);
            WorkspaceFiles.RejectLinkAncestors(path);
            await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, FileOptions.Asynchronous);
            RangeReads++;
            if (!files.TryGetValue(relative, out var entry) || stream.Length != entry.Length)
            { ranges[key] = null; return null; }
            stream.Position = offset;
            bytes = new byte[length];
            await stream.ReadExactlyAsync(bytes, ct);
            BytesRead += length;
        }
        catch (Exception error) when (IsReadError(error)) { bytes = null; }
        ranges[key] = bytes;
        return bytes;
    }
}
