[CmdletBinding()]
param(
    [string]$Dotnet = 'dotnet',
    [string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'tools/vntextpatch' }
$project = Join-Path $repo 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj'
& $Dotnet publish $project -c Release -r win-x64 --self-contained true `
    -p:PublishAot=true -p:StripSymbols=true -p:OptimizationPreference=Size `
    -o $OutputDirectory
if ($LASTEXITCODE -ne 0) { throw "VNTextPatch NativeAOT build failed ($LASTEXITCODE)." }
Get-Item -LiteralPath (Join-Path $OutputDirectory 'GameL10n.VnTextPatch.exe') |
    Select-Object FullName, Length
