[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'artifacts/GameL10n' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if ($OutputDirectory -eq $repo -or (Test-Path -LiteralPath (Join-Path $OutputDirectory 'games'))) {
    throw 'Output directory must not be the repository or an application data directory.'
}
$native = Join-Path $repo 'tools/vntextpatch/GameL10n.VnTextPatch.exe'
$python = Join-Path $repo 'tools/python/python.exe'
if (-not (Test-Path -LiteralPath $native -PathType Leaf)) { throw 'Build tools/vntextpatch with scripts/build-vntextpatch.ps1 first.' }
if (-not (Test-Path -LiteralPath $python -PathType Leaf)) { throw 'Place the complete portable CPython 3.14 x64 distribution in tools/python first.' }
Push-Location $repo
try {
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw "Rust build failed ($LASTEXITCODE)." }
} finally { Pop-Location }
$null = [IO.Directory]::CreateDirectory($OutputDirectory)
[IO.File]::Copy((Join-Path $repo 'target/release/gamel10n-desktop.exe'), (Join-Path $OutputDirectory 'GameL10n.exe'), $true)
[IO.File]::Copy((Join-Path $repo 'target/release/gamel10n.exe'), (Join-Path $OutputDirectory 'GameL10n.Cli.exe'), $true)
$toolOutput = Join-Path $OutputDirectory 'tools'
$null = [IO.Directory]::CreateDirectory($toolOutput)
foreach ($tool in @('vntextpatch', 'python')) {
    $sourceDirectory = Join-Path $repo ('tools/' + $tool)
    $targetDirectory = Join-Path $toolOutput $tool
    $null = [IO.Directory]::CreateDirectory($targetDirectory)
    foreach ($file in Get-ChildItem -LiteralPath $sourceDirectory -Recurse -File | Where-Object { $_.Extension -notin @('.pdb', '.lib', '.exp') }) {
        $relative = [IO.Path]::GetRelativePath($sourceDirectory, $file.FullName)
        $destination = Join-Path $targetDirectory $relative
        $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
        [IO.File]::Copy($file.FullName, $destination, $true)
    }
}
[IO.File]::Copy((Join-Path $repo 'THIRD-PARTY-NOTICES.md'), (Join-Path $OutputDirectory 'THIRD-PARTY-NOTICES.md'), $true)
Get-Item -LiteralPath (Join-Path $OutputDirectory 'GameL10n.exe') | Select-Object FullName, Length
