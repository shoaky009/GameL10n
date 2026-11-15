[CmdletBinding()]
param(
    [string]$Dotnet,
    [string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'target/runtime/vntextpatch' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$candidates = if ($Dotnet) {
    @($Dotnet)
} else {
    @(
        if ($env:DOTNET_ROOT_X64) { Join-Path $env:DOTNET_ROOT_X64 'dotnet.exe' }
        if ($env:DOTNET_ROOT) { Join-Path $env:DOTNET_ROOT 'dotnet.exe' }
        (Get-Command dotnet -CommandType Application -All -ErrorAction SilentlyContinue).Source
        Join-Path ([Environment]::GetFolderPath('UserProfile')) '.dotnet/dotnet.exe'
        if ($env:ProgramFiles) { Join-Path $env:ProgramFiles 'dotnet/dotnet.exe' }
    )
}
$sdk = $null
Push-Location $repo
try {
    foreach ($candidate in ($candidates | Where-Object { $_ } | Select-Object -Unique)) {
        $command = Get-Command $candidate -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $command) { continue }
        $version = & $command.Source --version 2>$null
        if ($LASTEXITCODE -eq 0 -and "$version" -match '^(\d+)\.' -and [int]$Matches[1] -ge 10) {
            $sdk = $command.Source
            break
        }
    }
    if (-not $sdk) {
        throw 'A .NET 10 or newer SDK is required. Install the SDK or specify its dotnet.exe with -Dotnet.'
    }
    Write-Host "Building VNTextPatch with $sdk (SDK $version)."
    $python = Join-Path $repo 'target/runtime/python/python.exe'
    if (-not (Test-Path -LiteralPath $python -PathType Leaf)) {
        & (Join-Path $PSScriptRoot 'prepare-python.ps1')
    }
    $project = Join-Path $repo 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj'
    & $sdk publish $project -c Release -r win-x64 --self-contained true `
        -p:PublishAot=true -p:StripSymbols=true -p:OptimizationPreference=Size `
        -o $OutputDirectory
    if ($LASTEXITCODE -ne 0) { throw "VNTextPatch NativeAOT build failed ($LASTEXITCODE)." }
    & $python (Join-Path $repo 'scripts/build_vntextpatch.py') --output $OutputDirectory --sources-only
    if ($LASTEXITCODE -ne 0) { throw "VNTextPatch source bundle failed ($LASTEXITCODE)." }
} finally { Pop-Location }
Get-Item -LiteralPath (Join-Path $OutputDirectory 'GameL10n.VnTextPatch.exe') |
    Select-Object FullName, Length
