param([switch]$SkipBuild, [string]$OutputDirectory = 'artifacts/GameL10n', [string]$EngineToolsCacheDirectory = '.tools/engine-tools')
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $OutputDirectory))
$runtimeVersion = '10.0.13'
$toolchainSource = Join-Path $repoRoot '.tools/dotnet'
$toolchainTarget = Join-Path $output 'toolchain'
$toolchainItems = @('dotnet.exe', "host/fxr/$runtimeVersion", "shared/Microsoft.NETCore.App/$runtimeVersion", 'LICENSE.txt', 'ThirdPartyNotices.txt')
foreach ($relative in $toolchainItems) {
    if (-not (Test-Path -LiteralPath (Join-Path $toolchainSource $relative))) { throw "Pinned toolchain component is missing: $relative" }
}
$desktopExecutable = Join-Path $output 'GameL10n.Desktop.exe'
if (Get-Process -Name GameL10n.Desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $desktopExecutable }) {
    throw 'The desktop app is using this output directory. Close it before publishing, or select another -OutputDirectory.'
}
if (-not $SkipBuild) {
    # Recreate recognized distribution output so removed dependencies cannot linger between publishes.
    if (Test-Path -LiteralPath $output) {
        $trimmedOutput = [IO.Path]::TrimEndingDirectorySeparator($output)
        if ($trimmedOutput -eq [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetPathRoot($output)) -or
            $repoRoot -eq $trimmedOutput -or $repoRoot.StartsWith($trimmedOutput + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'The output directory must not be the repository or one of its ancestors.'
        }
        for ($ancestor = [IO.DirectoryInfo]::new($output); $null -ne $ancestor; $ancestor = $ancestor.Parent) {
            if ($ancestor.Exists -and ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'The output directory must not contain links.' }
        }
        if ((Get-ChildItem -LiteralPath $output -Force | Select-Object -First 1) -and
            (-not (Test-Path -LiteralPath $desktopExecutable) -or -not (Test-Path -LiteralPath (Join-Path $output 'GameL10n.Desktop.deps.json')))) {
            throw 'The output directory is not an existing desktop package. Choose an empty output directory.'
        }
        foreach ($name in @('.app.lock', 'settings.json', 'location.json', 'games', 'extensions', 'memory')) {
            if (Test-Path -LiteralPath (Join-Path $output $name)) { throw 'The output directory contains application data. Choose another output directory.' }
        }
        $cache = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $EngineToolsCacheDirectory))
        if ($cache -eq $trimmedOutput -or $cache.StartsWith($trimmedOutput + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'The engine tools cache must be outside the output directory.'
        }
        Remove-Item -LiteralPath $output -Recurse -Force
    }
    $sharedRuntimeArguments = @('--self-contained', 'false', '-p:AppHostDotNetSearch=AppRelative', '-p:RollForward=Disable')
    # RuntimeFrameworkVersion would also override the Windows SDK framework. The pinned SDK selects its .NET patch.
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'src/GameL10n.Desktop/GameL10n.Desktop.csproj') -c Release -o $output @sharedRuntimeArguments '-p:TargetLatestRuntimePatch=true' '-p:AppHostRelativeDotNet=toolchain'
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed.' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.DataTool/GameL10n.DataTool.csproj') -c Release -r win-x64 @sharedRuntimeArguments "-p:RuntimeFrameworkVersion=$runtimeVersion" '-p:AppHostRelativeDotNet=../../toolchain' -o (Join-Path $output 'tools/data')
    if ($LASTEXITCODE -ne 0) { throw 'Data tool publish failed.' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.AdapterHost/GameL10n.AdapterHost.csproj') -c Release -r win-x64 @sharedRuntimeArguments "-p:RuntimeFrameworkVersion=$runtimeVersion" '-p:AppHostRelativeDotNet=../../toolchain' -o (Join-Path $output 'tools/adapter-host')
    if ($LASTEXITCODE -ne 0) { throw 'Adapter component host publish failed.' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.ParserCompat/GameL10n.ParserCompat.csproj') -c Release -r win-x64 @sharedRuntimeArguments "-p:RuntimeFrameworkVersion=$runtimeVersion" '-p:AppHostRelativeDotNet=../../toolchain' -o (Join-Path $output 'tools/parser-compat')
    if ($LASTEXITCODE -ne 0) { throw 'Legacy parser compatibility tool publish failed.' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') publish (Join-Path $repoRoot 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj') -c Release -r win-x64 @sharedRuntimeArguments "-p:RuntimeFrameworkVersion=$runtimeVersion" '-p:AppHostRelativeDotNet=../../toolchain' '-p:PublishSingleFile=true' '-p:IncludeNativeLibrariesForSelfExtract=false' '-p:DebugType=None' '-p:DebugSymbols=false' -o (Join-Path $output 'tools/vntextpatch')
    if ($LASTEXITCODE -ne 0) { throw 'Built-in script tool publish failed.' }
}
$desktopRuntime = Get-Content -LiteralPath (Join-Path $output 'GameL10n.Desktop.runtimeconfig.json') -Raw | ConvertFrom-Json
if ($desktopRuntime.runtimeOptions.framework.name -ne 'Microsoft.NETCore.App' -or $desktopRuntime.runtimeOptions.framework.version -ne $runtimeVersion) {
    throw 'The desktop runtime configuration does not match the packaged .NET runtime. Rebuild with the pinned SDK.'
}
foreach ($relative in @('.', 'tools/data', 'tools/vntextpatch', 'tools/adapter-host', 'tools/parser-compat')) {
    if (Test-Path -LiteralPath (Join-Path $output "$relative/coreclr.dll")) {
        throw 'Duplicate runtime files remain in the output. Run publish.ps1 without -SkipBuild to rebuild the package.'
    }
}
& (Join-Path $PSScriptRoot 'package-vntextpatch-assets.ps1') -OutputDirectory (Join-Path $output 'tools/vntextpatch') -Configuration Release -SkipBuild:$SkipBuild
& (Join-Path $PSScriptRoot 'package-adapter-components.ps1') -OutputDirectory (Join-Path $output 'tools/adapter-components')
$engineToolsCache = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $EngineToolsCacheDirectory))
& (Join-Path $PSScriptRoot 'dotnet.ps1') run --project (Join-Path $repoRoot 'tools/GameL10n.Diagnostics/GameL10n.Diagnostics.csproj') -c Release -- prepare-engine-tools $engineToolsCache (Join-Path $output 'tools/engine-tools')
if ($LASTEXITCODE -ne 0) { throw 'Shared engine tools are missing; publishing stopped instead of deferring downloads to game analysis.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'GameL10n.Desktop.exe'))) { throw 'The desktop executable is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'tools/data/GameL10n.DataTool.exe'))) { throw 'The data tool is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'tools/adapter-host/GameL10n.AdapterHost.dll'))) { throw 'The precompiled adapter host is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output 'tools/parser-compat/GameL10n.ParserCompat.dll'))) { throw 'The legacy parser compatibility tool is missing.' }
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
Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md'), (Join-Path $repoRoot 'THIRD-PARTY-NOTICES.md'), (Join-Path $repoRoot 'logo.png') -Destination $output -Force
New-Item -ItemType Directory -Path (Join-Path $output 'docs') -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs') -Force | Copy-Item -Destination (Join-Path $output 'docs') -Recurse -Force
New-Item -ItemType Directory -Path (Join-Path $output 'contracts') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'contracts/adapter-v1.md') -Destination (Join-Path $output 'contracts') -Force
New-Item -ItemType Directory -Path (Join-Path $output 'scripts') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts/analyze-adaptation-log.ps1') -Destination (Join-Path $output 'scripts') -Force
Write-Output "Desktop package: $output"
