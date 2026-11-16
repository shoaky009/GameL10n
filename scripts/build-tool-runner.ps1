[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [string]$OutputDirectory = ''
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'target/runtime/tool-runner' }
$manifest = Join-Path $repo 'tools/GameL10n.ToolRunner/Cargo.toml'
$cargoTarget = Join-Path $repo 'target/tool-runner'
$arguments = @('rustc', '--manifest-path', $manifest, '--target-dir', $cargoTarget, '--locked', '--bin', 'gamel10n-tool-runner')
if ($Configuration -eq 'Release') { $arguments += '--release' }
& cargo @arguments -- -C target-feature=+crt-static
if ($LASTEXITCODE -ne 0) { throw "Isolated tool runner build failed ($LASTEXITCODE)." }
$null = [IO.Directory]::CreateDirectory($OutputDirectory)
$binary = Join-Path $cargoTarget ($Configuration.ToLowerInvariant() + '/gamel10n-tool-runner.exe')
[IO.File]::Copy($binary, (Join-Path $OutputDirectory 'GameL10n.ToolRunner.exe'), $true)
