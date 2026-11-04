param([switch]$SkipBuild, [switch]$WithoutToolchain, [string]$OutputDirectory = 'artifacts/GameL10n', [string]$EngineToolsCacheDirectory = '.tools/engine-tools')
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $OutputDirectory))
$runtimeVersion = '10.0.13'
$sdkVersion = '10.0.402'
$toolchainSource = Join-Path $repoRoot '.tools/dotnet'
$toolchainTarget = Join-Path $output 'toolchain'
$toolchainItems = @('dotnet.exe', "host/fxr/$runtimeVersion", "shared/Microsoft.NETCore.App/$runtimeVersion", 'LICENSE.txt', 'ThirdPartyNotices.txt')
if (-not $WithoutToolchain) { $toolchainItems += "sdk/$sdkVersion/Roslyn/bincore" }
foreach ($relative in $toolchainItems) {
    if (-not (Test-Path -LiteralPath (Join-Path $toolchainSource $relative))) { throw "Pinned toolchain component is missing: $relative" }
}
$referenceDirectory = "packs/Microsoft.NETCore.App.Ref/$runtimeVersion/ref/net10.0"
if (-not $WithoutToolchain -and -not (Test-Path -LiteralPath (Join-Path $toolchainSource $referenceDirectory))) {
    throw "Pinned framework references are missing: $referenceDirectory"
}
$desktopExecutable = Join-Path $output 'GameL10n.Desktop.exe'
if (Get-Process -Name GameL10n.Desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $desktopExecutable }) {
    throw 'The desktop app is using this output directory. Close it before publishing, or select another -OutputDirectory.'
}
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'src/GameL10n.Desktop/GameL10n.Desktop.csproj') -c Release -o $output --self-contained true
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed.' }
    # Remove old self-contained tool outputs before switching to the shared runtime.
    foreach ($relative in @('tools/data', 'tools/vntextpatch')) {
        $directory = Join-Path $output $relative
        if (Test-Path -LiteralPath $directory) { Remove-Item -LiteralPath $directory -Recurse -Force }
    }
    $sharedRuntimeArguments = @('--self-contained', 'false', '-p:AppHostDotNetSearch=AppRelative',
        '-p:AppHostRelativeDotNet=../../toolchain', "-p:RuntimeFrameworkVersion=$runtimeVersion", '-p:RollForward=Disable')
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.DataTool/GameL10n.DataTool.csproj') -c Release -r win-x64 @sharedRuntimeArguments -o (Join-Path $output 'tools/data')
    if ($LASTEXITCODE -ne 0) { throw 'Data tool publish failed.' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj') -c Release -r win-x64 @sharedRuntimeArguments '-p:PublishSingleFile=true' '-p:IncludeNativeLibrariesForSelfExtract=false' '-p:DebugType=None' '-p:DebugSymbols=false' -o (Join-Path $output 'tools/vntextpatch')
    if ($LASTEXITCODE -ne 0) { throw 'Built-in script tool publish failed.' }
}
foreach ($relative in @('tools/data', 'tools/vntextpatch')) {
    if (Test-Path -LiteralPath (Join-Path $output "$relative/coreclr.dll")) {
        throw 'Self-contained helper files remain in the output. Run publish.ps1 without -SkipBuild to rebuild the helpers.'
    }
}
& (Join-Path $PSScriptRoot 'package-vntextpatch-assets.ps1') -OutputDirectory (Join-Path $output 'tools/vntextpatch') -Configuration Release -SkipBuild:$SkipBuild
& (Join-Path $PSScriptRoot 'package-adapter-components.ps1') -OutputDirectory (Join-Path $output 'tools/adapter-components')
$engineToolsCache = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $EngineToolsCacheDirectory))
& (Join-Path $PSScriptRoot 'dotnet.ps1') run --project (Join-Path $repoRoot 'tools/GameL10n.Diagnostics/GameL10n.Diagnostics.csproj') -c Release -- prepare-engine-tools $engineToolsCache (Join-Path $output 'tools/engine-tools')
if ($LASTEXITCODE -ne 0) { throw 'Shared engine tools are missing; publishing stopped instead of deferring downloads to game analysis.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'GameL10n.Desktop.exe'))) { throw 'The desktop executable is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'tools/data/GameL10n.DataTool.exe'))) { throw 'The data tool is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'tools/vntextpatch/GameL10n.VnTextPatch.exe'))) { throw 'The built-in script tool is missing.' }
if (Test-Path -LiteralPath $toolchainTarget) { Remove-Item -LiteralPath $toolchainTarget -Recurse -Force }
foreach ($relative in $toolchainItems) {
    $source = Join-Path $toolchainSource $relative
    $target = Join-Path $toolchainTarget $relative
    if (Test-Path -LiteralPath $source -PathType Container) {
        New-Item -ItemType Directory -Path $target -Force | Out-Null
        Get-ChildItem -LiteralPath $source -Force | Copy-Item -Destination $target -Recurse -Force
    } else {
        New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
        Copy-Item -LiteralPath $source -Destination $target -Force
    }
}
if (-not $WithoutToolchain) {
    # csc consumes only reference DLLs; XML documentation, analyzers and SDK metadata are not used.
    $referenceTarget = Join-Path $toolchainTarget $referenceDirectory
    New-Item -ItemType Directory -Path $referenceTarget -Force | Out-Null
    Get-ChildItem -LiteralPath (Join-Path $toolchainSource $referenceDirectory) -Filter '*.dll' -File |
        Copy-Item -Destination $referenceTarget -Force
}
Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md'), (Join-Path $repoRoot 'THIRD-PARTY-NOTICES.md'), (Join-Path $repoRoot 'logo.png') -Destination $output -Force
New-Item -ItemType Directory -Path (Join-Path $output 'docs') -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs') -Force | Copy-Item -Destination (Join-Path $output 'docs') -Recurse -Force
New-Item -ItemType Directory -Path (Join-Path $output 'contracts') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'contracts/adapter-v1.md') -Destination (Join-Path $output 'contracts') -Force
New-Item -ItemType Directory -Path (Join-Path $output 'scripts') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts/analyze-adaptation-log.ps1') -Destination (Join-Path $output 'scripts') -Force
Write-Output "Desktop package: $output"
