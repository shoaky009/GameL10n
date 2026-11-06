[CmdletBinding()]
param([string]$ArchivePath)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
& (Join-Path $PSScriptRoot 'prepare-runtime.ps1')
$version = '3.14.10'
$expectedHash = '90905e64a840557d8c1bc4f5b74db3d7ee85e3ffb217611cac1c7813acea4159'
$output = Join-Path $repo 'target/runtime/python'
$manifestPath = Join-Path $output 'runtime.json'
$hasInterpreter = Test-Path -LiteralPath (Join-Path $output 'python.exe') -PathType Leaf
if ($hasInterpreter -and (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    & (Join-Path $output 'python.exe') -I --version
    if ($LASTEXITCODE -ne 0) { throw 'Existing portable Python could not start.' }
    return
}
if (-not $hasInterpreter -and (Test-Path -LiteralPath $output) -and @(Get-ChildItem -LiteralPath $output -Force).Count -gt 0) {
    throw 'target/runtime/python is incomplete. Move or remove it before restoring the pinned interpreter.'
}
if (-not $ArchivePath) {
    $cache = Join-Path $repo 'target/downloads'
    $null = [IO.Directory]::CreateDirectory($cache)
    $ArchivePath = Join-Path $cache "python-$version-embed-amd64.zip"
    if (-not (Test-Path -LiteralPath $ArchivePath)) {
        Invoke-WebRequest -Uri "https://www.python.org/ftp/python/$version/python-$version-embed-amd64.zip" -OutFile $ArchivePath
    }
}
$ArchivePath = (Get-Item -LiteralPath $ArchivePath).FullName
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
    throw 'Portable Python archive SHA-256 does not match the pinned distribution.'
}
if (-not $hasInterpreter) {
    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $output
}
# Derive the runtime manifest from the verified archive, including when an existing
# interpreter has no manifest. Never label modified local files as the pinned runtime.
$files = [ordered]@{}
$archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
try {
    foreach ($entry in $archive.Entries) {
        if (-not $entry.Name) { continue }
        $stream = $entry.Open()
        try {
            $hash = (Get-FileHash -InputStream $stream -Algorithm SHA256).Hash.ToLowerInvariant()
        } finally { $stream.Dispose() }
        $path = Join-Path $output $entry.FullName
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
            throw "Portable Python differs from the pinned archive: $($entry.FullName)"
        }
        $files[$entry.FullName] = $hash
    }
} finally { $archive.Dispose() }
& (Join-Path $output 'python.exe') -I --version
if ($LASTEXITCODE -ne 0) { throw 'Restored portable Python could not start.' }
$manifest = [ordered]@{ archiveSha256 = $expectedHash; files = $files }
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 3) + [Environment]::NewLine)
