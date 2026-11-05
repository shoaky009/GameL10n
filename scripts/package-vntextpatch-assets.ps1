param(
    [string]$OutputDirectory = 'artifacts/vntextpatch-assets',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $OutputDirectory))
$project = Join-Path $repoRoot 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj'
$buildOutput = Join-Path $repoRoot "tools/GameL10n.VnTextPatch/obj/reuse-assets/$Configuration"
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish $project -c $Configuration -o $buildOutput --no-self-contained '-p:UseAppHost=false' '-p:PublishSingleFile=false' '-p:DebugType=None' '-p:DebugSymbols=false'
    if ($LASTEXITCODE -ne 0) { throw 'VNTextPatch reusable tool publish failed.' }
}
$assembly = Join-Path $buildOutput 'GameL10n.VnTextPatch.dll'
if (-not (Test-Path -LiteralPath $assembly)) { throw 'The reusable parser assembly is missing; run without -SkipBuild.' }
New-Item -ItemType Directory -Path $output -Force | Out-Null
Copy-Item -LiteralPath $assembly -Destination (Join-Path $output 'GameL10n.VnTextPatch.dll') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'third_party/VNTextPatch/LICENSE') -Destination (Join-Path $output 'VNTextPatch.LICENSE') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'third_party/VNTextPatch/NOTICE.md') -Destination (Join-Path $output 'VNTextPatch.NOTICE.md') -Force
$toolFiles = [ordered]@{}
foreach ($name in @('GameL10n.VnTextPatch.dll', 'GameL10n.VnTextPatch.deps.json', 'GameL10n.VnTextPatch.runtimeconfig.json', 'VNTextPatch.LICENSE', 'VNTextPatch.NOTICE.md')) {
    if ($name -like 'GameL10n.VnTextPatch.*') {
        $source = Join-Path $buildOutput $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "The reusable parser tool is incomplete: $name" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $output $name) -Force
    }
    $toolFiles[$name] = (Get-FileHash -LiteralPath (Join-Path $output $name) -Algorithm SHA256).Hash.ToLowerInvariant()
}
@{ schemaVersion = 2; files = $toolFiles; entryPoint = 'GameL10n.VnTextPatch.dll'; runtime = 'dotnet' } |
    ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output 'parser-tool.json') -Encoding utf8
$zipPath = Join-Path $output 'sources.zip'
$pendingZipPath = Join-Path $output ('.sources-' + [Guid]::NewGuid().ToString('N') + '.tmp')
try {
    $expectedEntries = @{}
    $stream = [IO.File]::Open($pendingZipPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            $vendor = Join-Path $repoRoot 'third_party/VNTextPatch'
            foreach ($file in Get-ChildItem -LiteralPath $vendor -Recurse -File | Sort-Object FullName) {
                if ($file.Extension -ne '.cs' -and $file.Name -notin @('LICENSE', 'NOTICE.md')) { continue }
                $relative = [IO.Path]::GetRelativePath($vendor, $file.FullName).Replace('\', '/')
                $entryPath = "vendor/$relative"
                $expectedEntries.Add($entryPath, $file.Length)
                [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $file.FullName, $entryPath, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
            }
            foreach ($name in @('Worker.cs', 'Protocol.cs', 'Serialization/VnTextPatchJson.cs')) {
                $file = Get-Item -LiteralPath (Join-Path $repoRoot "tools/GameL10n.VnTextPatch/$name")
                $expectedEntries.Add("worker/$name", $file.Length)
                [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $file.FullName, "worker/$name", [IO.Compression.CompressionLevel]::Optimal) | Out-Null
            }
        }
        finally { $zip.Dispose() }
        $stream.Flush($true)
    }
    finally { $stream.Dispose() }

    $validation = [IO.Compression.ZipFile]::OpenRead($pendingZipPath)
    try {
        if ($validation.Entries.Count -ne $expectedEntries.Count) { throw 'The parser source archive is incomplete.' }
        foreach ($entry in $validation.Entries) {
            if (-not $expectedEntries.ContainsKey($entry.FullName) -or $entry.Length -ne $expectedEntries[$entry.FullName]) {
                throw "The parser source archive entry is incomplete: $($entry.FullName)"
            }
            $input = $entry.Open()
            try { $input.CopyTo([IO.Stream]::Null) }
            finally { $input.Dispose() }
        }
    }
    finally { $validation.Dispose() }

    # Replacing a completed file keeps readers from observing a truncated archive.
    if ([IO.File]::Exists($zipPath)) { [IO.File]::Replace($pendingZipPath, $zipPath, [NullString]::Value) }
    else { [IO.File]::Move($pendingZipPath, $zipPath) }
}
finally {
    if ([IO.File]::Exists($pendingZipPath)) { Remove-Item -LiteralPath $pendingZipPath -Force }
}
Write-Output "VNTextPatch reuse assets: $output"
