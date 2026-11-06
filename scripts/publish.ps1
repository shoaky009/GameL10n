[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repo $(if ($Configuration -eq 'Release') { 'artifacts/GameL10n' } else { 'artifacts/GameL10n-debug' })
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if ($OutputDirectory -eq [IO.Path]::GetPathRoot($OutputDirectory) -or $OutputDirectory -eq $repo -or
    $repo.StartsWith($OutputDirectory.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
    (Test-Path -LiteralPath (Join-Path $OutputDirectory 'games')) -or
    (Test-Path -LiteralPath (Join-Path $OutputDirectory 'extensions'))) {
    throw 'Output directory must not be the repository or an application data directory.'
}
foreach ($inputDirectory in @('adapters', 'tools', 'crates', 'scripts', 'third_party', 'target/runtime', '.git')) {
    $inputPath = Join-Path $repo $inputDirectory
    if ($OutputDirectory -eq $inputPath -or $OutputDirectory.StartsWith($inputPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Output directory must not be inside a source or tool directory.'
    }
}
for ($ancestor = $OutputDirectory; $ancestor; $ancestor = [IO.Path]::GetDirectoryName($ancestor)) {
    if ((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Output directory must not use a link or junction: $ancestor"
    }
}
& (Join-Path $PSScriptRoot 'prepare-runtime.ps1')
$runtime = Join-Path $repo 'target/runtime'
$native = Join-Path $runtime 'vntextpatch/GameL10n.VnTextPatch.exe'
$python = Join-Path $runtime 'python/python.exe'
if (-not (Test-Path -LiteralPath $native -PathType Leaf)) { throw 'Build the native parser with scripts/build-vntextpatch.ps1 first.' }
if (-not (Test-Path -LiteralPath $python -PathType Leaf)) { throw 'Run scripts/prepare-python.ps1 first.' }
$runtimeFiles = @(
    foreach ($name in @('GameL10n.VnTextPatch.exe', 'capstone.dll', 'sources.zip', 'Capstone.LICENSE.TXT', 'VNTextPatch.LICENSE', 'VNTextPatch.NOTICE.md')) {
        Get-Item -LiteralPath (Join-Path $runtime "vntextpatch/$name")
    }
    $pythonManifest = Get-Content -LiteralPath (Join-Path $runtime 'python/runtime.json') -Raw | ConvertFrom-Json
    foreach ($name in @('runtime.json') + @($pythonManifest.files.PSObject.Properties.Name)) {
        Get-Item -LiteralPath (Join-Path $runtime "python/$name")
    }
    Get-Item -LiteralPath (Join-Path $runtime 'host/credentials.py')
    Get-Item -LiteralPath (Join-Path $runtime 'adapter-components/QlieLauncher.exe')
    Get-Item -LiteralPath (Join-Path $runtime 'adapter-components/GameL10n.FontBridge.dll')
    Get-Item -LiteralPath (Join-Path $runtime 'adapter-components/GameL10n.FontBridge.LICENSE')
    $references = Join-Path $repo 'third_party/engine-tools'
    foreach ($file in Get-ChildItem -LiteralPath $references -Recurse -File) {
        $relative = [IO.Path]::GetRelativePath($references, $file.FullName)
        Get-Item -LiteralPath (Join-Path $runtime (Join-Path 'engine-tools' $relative))
    }
    Get-ChildItem -LiteralPath (Join-Path $repo 'adapters/components') -File -Filter '*.py'
    Get-Item -LiteralPath (Join-Path $repo 'adapters/components/VNTextProxy-LICENSE.txt')
    Get-ChildItem -LiteralPath (Join-Path $repo 'adapters/siglus') -File |
        Where-Object { $_.Extension -in @('.py', '.json') }
)
# Only these two generated trees are synchronized; project data and repository sources are separate.
$previousFiles = @()
foreach ($directory in @('tools', 'adapters')) {
    $managedDirectory = Join-Path $OutputDirectory $directory
    if (Test-Path -LiteralPath $managedDirectory) {
        $entries = @(Get-Item -LiteralPath $managedDirectory -Force) + @(Get-ChildItem -LiteralPath $managedDirectory -Recurse -Force)
        if (@($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count -gt 0) {
            throw "Packaged tool and adapter directories must not contain links or junctions: $managedDirectory"
        }
        $previousFiles += $entries | Where-Object { -not $_.PSIsContainer }
    }
}
Push-Location $repo
try {
    $buildArguments = @('build', '--package', 'gamel10n-desktop', '--locked')
    if ($Configuration -eq 'Debug') { $buildArguments += @('--package', 'gamel10n-core') }
    if ($Configuration -eq 'Release') { $buildArguments += '--release' }
    & cargo @buildArguments
    if ($LASTEXITCODE -ne 0) { throw "Rust build failed ($LASTEXITCODE)." }
} finally { Pop-Location }
$null = [IO.Directory]::CreateDirectory($OutputDirectory)
$binaryDirectory = Join-Path $repo ('target/' + $Configuration.ToLowerInvariant())
[IO.File]::Copy((Join-Path $binaryDirectory 'gamel10n-desktop.exe'), (Join-Path $OutputDirectory 'GameL10n.exe'), $true)
$cliOutput = Join-Path $OutputDirectory 'GameL10n.Cli.exe'
if ($Configuration -eq 'Debug') {
    [IO.File]::Copy((Join-Path $binaryDirectory 'gamel10n.exe'), $cliOutput, $true)
}
$expectedFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($file in $runtimeFiles) {
    $relative = if ($file.FullName.StartsWith($runtime + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        Join-Path 'tools' ([IO.Path]::GetRelativePath($runtime, $file.FullName))
    } else {
        [IO.Path]::GetRelativePath($repo, $file.FullName)
    }
    $destination = Join-Path $OutputDirectory $relative
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
    [IO.File]::Copy($file.FullName, $destination, $true)
    $null = $expectedFiles.Add($destination)
}
foreach ($notice in @('LICENSE', 'THIRD-PARTY-NOTICES.md')) {
    [IO.File]::Copy((Join-Path $repo $notice), (Join-Path $OutputDirectory $notice), $true)
}
if ($Configuration -eq 'Release' -and (Test-Path -LiteralPath $cliOutput)) {
    Remove-Item -LiteralPath $cliOutput -Force
}
foreach ($file in $previousFiles) {
    if (-not $expectedFiles.Contains($file.FullName)) {
        Remove-Item -LiteralPath $file.FullName -Force
    }
}
foreach ($directory in @('tools', 'adapters')) {
    Get-ChildItem -LiteralPath (Join-Path $OutputDirectory $directory) -Directory -Recurse -Force |
        Sort-Object { $_.FullName.Length } -Descending |
        ForEach-Object {
            if (@(Get-ChildItem -LiteralPath $_.FullName -Force).Count -eq 0) {
                Remove-Item -LiteralPath $_.FullName -Force
            }
        }
}
Get-Item -LiteralPath (Join-Path $OutputDirectory 'GameL10n.exe') | Select-Object FullName, Length
