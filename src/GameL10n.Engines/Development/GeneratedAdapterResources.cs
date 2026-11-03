using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

internal static class GeneratedAdapterResources
{
    internal static string Normalize(string path) => path.Replace('\\', '/');

    internal static void ValidatePatterns(IReadOnlyList<string> patterns)
    {
        if (patterns.Count == 0) throw new GeneratedAdapterException("At least one resource discovery pattern is required.");
        foreach (var pattern in patterns)
        {
            if (string.IsNullOrWhiteSpace(pattern)) throw new GeneratedAdapterException("A resource pattern cannot be empty.");
            // Validate the path components as usual after replacing the supported glob characters.
            _ = WorkspaceFiles.Resolve(Path.GetTempPath(), Normalize(pattern).Replace('*', 'x').Replace('?', 'x'));
            if (pattern.Length > 1024) throw new GeneratedAdapterException("A resource pattern is too long.");
        }
    }

    internal static IReadOnlyList<string> Discover(GeneratedAdapterPackage package, string directory)
    {
        if (package.ResourcePatterns is not { Count: > 0 } patterns)
        {
            foreach (var file in package.ResourceFiles) _ = WorkspaceFiles.Resolve(directory, file);
            return package.ResourceFiles.Select(Normalize).Order(StringComparer.Ordinal).ToArray();
        }
        ValidatePatterns(patterns);
        var matchers = patterns.Select(Pattern).ToArray();
        return GeneratedGameAdapter.EnumerateFiles(directory)
            .Select(path => Normalize(Path.GetRelativePath(directory, path)))
            .Where(path => matchers.Any(matcher => matcher.IsMatch(path)))
            .Order(StringComparer.Ordinal).ToArray();
    }

    internal static Regex Pattern(string pattern)
    {
        var expression = new StringBuilder("\\A");
        pattern = Normalize(pattern);
        for (var index = 0; index < pattern.Length; index++)
        {
            var current = pattern[index];
            if (current == '*' && index + 1 < pattern.Length && pattern[index + 1] == '*')
            {
                index++;
                if (index + 1 < pattern.Length && pattern[index + 1] == '/')
                { index++; expression.Append("(?:.*/)?"); }
                else expression.Append(".*");
            }
            else if (current == '*') expression.Append("[^/]*");
            else if (current == '?') expression.Append("[^/]");
            else expression.Append(Regex.Escape(current.ToString()));
        }
        expression.Append("\\z");
        return new Regex(expression.ToString(), RegexOptions.IgnoreCase | RegexOptions.CultureInvariant,
            TimeSpan.FromSeconds(1));
    }

    internal static string HashFile(string path)
    {
        WorkspaceFiles.RejectLinkAncestors(path);
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
            128 * 1024, FileOptions.SequentialScan);
        return Convert.ToHexStringLower(SHA256.HashData(stream));
    }

    internal static string HashEntries(IEnumerable<KeyValuePair<string, string>> entries) =>
        Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(string.Join('\n',
            entries.OrderBy(pair => pair.Key, StringComparer.Ordinal).Select(pair => pair.Key + "\0" + pair.Value)))));

    internal static void VerifyPackage(GeneratedAdapterPackage package)
    {
        if (package.ProtocolVersion != 1 || package.ManifestVersion is < 1 or > 3)
            throw new GeneratedAdapterException("The local adapter uses an unsupported package or CLI protocol version.");
        if (package.Family is { } family)
        {
            family.Validate();
            if (package.Id != family.AdapterId) throw new GeneratedAdapterException("The engine/template and package identities disagree.");
        }
        if (package.Recognition is { SchemaVersion: 1 } recognition) recognition.Validate();
        if (package.ResourceFiles is null) throw new GeneratedAdapterException("The resource manifest is invalid.");
        foreach (var file in package.ResourceFiles) _ = WorkspaceFiles.Resolve(package.DirectoryPath, file);
        if (package.ResourcePatterns is { } patterns) ValidatePatterns(patterns);
        if (HashFile(WorkspaceFiles.Resolve(package.DirectoryPath, "adapter.dll")) != package.AssemblySha256)
            throw new GeneratedAdapterException("The local adapter assembly failed its integrity check.");
        if (package.ManifestVersion == 1)
        {
            if (HashFile(WorkspaceFiles.Resolve(package.DirectoryPath, "Program.cs")) != package.SourceSha256)
                throw new GeneratedAdapterException("The local adapter source failed its integrity check.");
            return;
        }
        if (package.ResourcePatterns is not { Count: > 0 })
            throw new GeneratedAdapterException("The adapter package has no resource discovery rules.");
        if (package.PackageFiles is not { Count: > 0 } files || !files.ContainsKey("adapter.dll")
            || !files.ContainsKey("adapter.runtimeconfig.json"))
            throw new GeneratedAdapterException("The adapter package has an incomplete file manifest.");
        if (package.ManifestVersion >= 3)
        {
            var release = AdapterReleaseFiles.Validate(package.ReleaseFiles);
            var expected = release.Select(file => file.Path).Append("adapter.dll").Append("adapter.runtimeconfig.json");
            if (!files.Keys.ToHashSet(StringComparer.Ordinal).SetEquals(expected))
                throw new GeneratedAdapterException("正式包文件与 releaseFiles 发布清单不一致。");
        }
        var actual = GeneratedGameAdapter.EnumerateFiles(package.DirectoryPath)
            .Select(path => Normalize(Path.GetRelativePath(package.DirectoryPath, path)))
            .Where(path => path is not ("package.json" or "verification.json"))
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        if (!actual.SetEquals(files.Keys) || files.Keys.Distinct(StringComparer.OrdinalIgnoreCase).Count() != files.Count)
            throw new GeneratedAdapterException("The adapter package file list does not match its manifest.");
        foreach (var pair in files)
            if (HashFile(WorkspaceFiles.Resolve(package.DirectoryPath, pair.Key)) != pair.Value)
                throw new GeneratedAdapterException("The local adapter file failed its integrity check: " + pair.Key);
        var sources = files.Where(pair => pair.Key.StartsWith("src/", StringComparison.Ordinal)
            && pair.Key.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)).ToArray();
        if (sources.Length == 0 || HashEntries(sources) != package.SourceSha256)
            throw new GeneratedAdapterException("The adapter source manifest is invalid.");
    }

    internal static async Task CopyAsync(string source, string destination, CancellationToken cancellationToken)
    {
        WorkspaceFiles.RejectLinkAncestors(source);
        WorkspaceFiles.RejectLinkAncestors(destination);
        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
        await using var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read,
            128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
        await using var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None,
            128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
        await input.CopyToAsync(output, cancellationToken);
    }
}
