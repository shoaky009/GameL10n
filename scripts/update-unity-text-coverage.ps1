param(
    [Parameter(Mandatory = $true)][string]$ProjectDirectory,
    [switch]$Apply
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
if ($Apply -and (Get-Process -Name GameL10n.Desktop, AvgTranslator.Desktop -ErrorAction SilentlyContinue)) {
    throw '请先关闭 GameL10n，避免覆盖应用正在保存的项目状态。'
}
& (Join-Path $PSScriptRoot 'dotnet.ps1') build (Join-Path $repoRoot 'tools/GameL10n.Diagnostics/GameL10n.Diagnostics.csproj') --no-restore
if ($LASTEXITCODE -ne 0) { throw '更新工具编译失败。' }
$updateArguments = @(
    (Join-Path $repoRoot 'tools/GameL10n.Diagnostics/bin/Debug/net10.0/GameL10n.Diagnostics.dll'),
    'update-adapter-coverage', $ProjectDirectory,
    (Join-Path $repoRoot 'target/unity-coverage-update'),
    (Join-Path $repoRoot 'adapters/unity-serialized-text/coverage-update'),
    (Join-Path $repoRoot '.tools/dotnet')
)
if ($Apply) { $updateArguments += '--apply' }
& (Join-Path $PSScriptRoot 'dotnet.ps1') @updateArguments
if ($LASTEXITCODE -ne 0) { throw '文字覆盖更新失败，请查看上方具体原因。已提交译文会保留。' }
