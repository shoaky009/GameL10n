param(
    [string]$OutputDirectory = 'artifacts/vntextpatch-assets',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $OutputDirectory))
$project = Join-Path $repoRoot 'tools/AvgTranslator.VnTextPatch/AvgTranslator.VnTextPatch.csproj'
$buildOutput = Join-Path $repoRoot "tools/AvgTranslator.VnTextPatch/obj/reuse-assets/$Configuration"
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'dotnet.ps1') build $project -c $Configuration -o $buildOutput --no-self-contained '-p:UseAppHost=false' '-p:PublishSingleFile=false' '-p:DebugType=None' '-p:DebugSymbols=false'
    if ($LASTEXITCODE -ne 0) { throw 'VNTextPatch reusable assembly build failed.' }
}
$assembly = Join-Path $buildOutput 'AvgTranslator.VnTextPatch.dll'
if (-not (Test-Path -LiteralPath $assembly)) { throw 'The reusable parser assembly is missing; run without -SkipBuild.' }
New-Item -ItemType Directory -Path $output -Force | Out-Null
Copy-Item -LiteralPath $assembly -Destination (Join-Path $output 'AvgTranslator.VnTextPatch.dll') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'third_party/VNTextPatch/LICENSE') -Destination (Join-Path $output 'VNTextPatch.LICENSE') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'third_party/VNTextPatch/NOTICE.md') -Destination (Join-Path $output 'VNTextPatch.NOTICE.md') -Force
$zipPath = Join-Path $output 'sources.zip'
$stream = [IO.File]::Open($zipPath, [IO.FileMode]::Create, [IO.FileAccess]::Write)
$zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create)
try {
    $vendor = Join-Path $repoRoot 'third_party/VNTextPatch'
    foreach ($file in Get-ChildItem -LiteralPath $vendor -Recurse -File | Sort-Object FullName) {
        if ($file.Extension -ne '.cs' -and $file.Name -notin @('LICENSE', 'NOTICE.md')) { continue }
        $relative = [IO.Path]::GetRelativePath($vendor, $file.FullName).Replace('\', '/')
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $file.FullName, "vendor/$relative", [IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
    foreach ($name in @('Worker.cs', 'Protocol.cs')) {
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $repoRoot "tools/AvgTranslator.VnTextPatch/$name"), "worker/$name", [IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
}
finally {
    $zip.Dispose()
    $stream.Dispose()
}
Write-Output "VNTextPatch reuse assets: $output"
