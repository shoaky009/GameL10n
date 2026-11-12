[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [string]$OutputDirectory = ''
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'target/runtime/game-check' }
$manifest = Join-Path $repo 'tools/GameL10n.GameCheck/Cargo.toml'
$cargoTarget = Join-Path $repo 'target/game-check'
$arguments = @('rustc', '--manifest-path', $manifest, '--target-dir', $cargoTarget, '--locked', '--bin', 'gamel10n-game-check')
if ($Configuration -eq 'Release') { $arguments += '--release' }
& cargo @arguments -- -C target-feature=+crt-static
if ($LASTEXITCODE -ne 0) { throw "Game check build failed ($LASTEXITCODE)." }
$null = [IO.Directory]::CreateDirectory($OutputDirectory)
$binary = Join-Path $cargoTarget ($Configuration.ToLowerInvariant() + '/gamel10n-game-check.exe')
[IO.File]::Copy($binary, (Join-Path $OutputDirectory 'GameL10n.GameCheck.exe'), $true)
