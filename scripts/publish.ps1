param([switch]$SkipBuild, [switch]$WithoutToolchain, [string]$OutputDirectory = 'artifacts/AvgTranslator')
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputDirectory))
$desktopExecutable = Join-Path $output 'AvgTranslator.Desktop.exe'
if (Get-Process -Name AvgTranslator.Desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $desktopExecutable }) {
    throw 'The desktop app is using this output directory. Close it before publishing, or select another -OutputDirectory.'
}
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'src/AvgTranslator.Desktop/AvgTranslator.Desktop.csproj') -c Release -o $output --self-contained true
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed.' }
}
if (-not (Test-Path -LiteralPath (Join-Path $output 'AvgTranslator.Desktop.exe'))) { throw 'The desktop executable is missing.' }
if (-not $WithoutToolchain) {
    $toolchainSource = Join-Path $repoRoot '.tools/dotnet'
    $toolchainTarget = Join-Path $output 'toolchain'
    $items = @('dotnet.exe', 'host/fxr/10.0.13', 'shared/Microsoft.NETCore.App/10.0.13', 'sdk/10.0.402/Roslyn/bincore', 'packs/Microsoft.NETCore.App.Ref/10.0.13', 'LICENSE.txt', 'ThirdPartyNotices.txt')
    foreach ($relative in $items) {
        $source = Join-Path $toolchainSource $relative
        if (-not (Test-Path -LiteralPath $source)) { throw "Pinned toolchain component is missing: $relative" }
        $target = Join-Path $toolchainTarget $relative
        if (Test-Path -LiteralPath $source -PathType Container) {
            New-Item -ItemType Directory -Path $target -Force | Out-Null
            Get-ChildItem -LiteralPath $source -Force | Copy-Item -Destination $target -Recurse -Force
        } else {
            New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
            Copy-Item -LiteralPath $source -Destination $target -Force
        }
    }
}
Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md'), (Join-Path $repoRoot 'THIRD-PARTY-NOTICES.md') -Destination $output -Force
New-Item -ItemType Directory -Path (Join-Path $output 'docs') -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs') -Force | Copy-Item -Destination (Join-Path $output 'docs') -Recurse -Force
New-Item -ItemType Directory -Path (Join-Path $output 'contracts') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'contracts/adapter-v1.md') -Destination (Join-Path $output 'contracts') -Force
Write-Output "Desktop package: $output"
