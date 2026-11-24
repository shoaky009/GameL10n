[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [string]$Dotnet
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
& (Join-Path $PSScriptRoot 'prepare-python.ps1')
$installedTargets = @(& rustup target list --installed)
if ($LASTEXITCODE -ne 0) { throw "Could not list installed Rust targets ($LASTEXITCODE)." }
foreach ($target in @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc')) {
    if ($target -notin $installedTargets) {
        & rustup target add $target
        if ($LASTEXITCODE -ne 0) { throw "Could not install Rust target '$target' ($LASTEXITCODE)." }
    }
}
& (Join-Path $PSScriptRoot 'build-vntextpatch.ps1') -Dotnet $Dotnet
& (Join-Path $repo 'target/runtime/python/python.exe') -I -X utf8 (Join-Path $PSScriptRoot 'build_qlie_launcher.py')
if ($LASTEXITCODE -ne 0) { throw "QLIE launcher build failed ($LASTEXITCODE)." }
& (Join-Path $PSScriptRoot 'build-fontbridge.ps1') -Configuration $Configuration
& (Join-Path $PSScriptRoot 'build-tool-runner.ps1') -Configuration $Configuration
