[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',
    [string]$OutputDirectory,
    [switch]$Test,
    [string]$TestFont,
    [string]$TestFace = 'Segoe UI'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$source = Join-Path $repo 'tools/GameL10n.FontBridge'
$build = Join-Path $repo "target/fontbridge/$Configuration"
$cargoTarget = Join-Path $repo 'target/fontbridge/cargo'
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repo 'target/runtime/adapter-components' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $build, $OutputDirectory | Out-Null

$common = @('rustc', '--manifest-path', (Join-Path $source 'Cargo.toml'), '--locked',
    '--target', 'i686-pc-windows-msvc', '--target-dir', $cargoTarget)
if ($Configuration -eq 'Release') { $common += '--release' }
$compiled = Join-Path $cargoTarget ('i686-pc-windows-msvc/' + $(if ($Configuration -eq 'Release') { 'release' } else { 'debug' }))
# The DLL is loaded by games without a separately installed CRT or Rust runtime.
& cargo @common --lib -- -C target-feature=+crt-static
if ($LASTEXITCODE -ne 0) { throw "Font bridge build failed ($LASTEXITCODE)." }
$dll = Join-Path $OutputDirectory 'GameL10n.FontBridge.dll'
[IO.File]::Copy((Join-Path $compiled 'gamel10n_fontbridge.dll'), $dll, $true)
Copy-Item -LiteralPath (Join-Path $source 'LICENSE') -Destination (Join-Path $OutputDirectory 'GameL10n.FontBridge.LICENSE') -Force

if ($Test) {
    if (-not $TestFont) { $TestFont = Join-Path $env:WINDIR 'Fonts/segoeui.ttf' }
    if (-not (Test-Path -LiteralPath $TestFont -PathType Leaf)) { throw "Test font not found: $TestFont" }
    $exe = Join-Path $build 'FontBridgeTests.exe'
    & cargo @common --bin fontbridge-check -- -C target-feature=+crt-static
    if ($LASTEXITCODE -ne 0) { throw "Font bridge test build failed ($LASTEXITCODE)." }
    [IO.File]::Copy((Join-Path $compiled 'fontbridge-check.exe'), $exe, $true)
    Copy-Item -LiteralPath $dll -Destination (Join-Path $build 'GameL10n.FontBridge.dll') -Force
    Copy-Item -LiteralPath $TestFont -Destination (Join-Path $build 'test-font.bin') -Force
    $ini = Join-Path $build 'GameL10n.FontBridge.ini'
    $encoding = [Text.UnicodeEncoding]::new($false, $true)
    $scenarios = @(
        @{ Name = 'replace'; File = 'test-font.bin'; Face = $TestFace; Match = 'BridgeAlias'; Mode = 'replace-matching' },
        @{ Name = 'concurrent'; File = 'test-font.bin'; Face = $TestFace; Match = 'BridgeAlias'; Mode = 'replace-matching' },
        @{ Name = 'fallback'; File = 'test-font.bin'; Face = $TestFace; Match = 'Terminal'; Mode = 'fallback' },
        @{ Name = 'missing-font'; File = 'missing-font.ttf'; Face = $TestFace; Match = 'BridgeAlias'; Mode = 'replace-matching' },
        @{ Name = 'wrong-face'; File = 'test-font.bin'; Face = 'Nonexistent FontBridge Family'; Match = 'BridgeAlias'; Mode = 'replace-matching' },
        @{ Name = 'invalid-font'; File = 'GameL10n.FontBridge.ini'; Face = $TestFace; Match = 'BridgeAlias'; Mode = 'replace-matching' },
        @{ Name = 'bad-config'; File = '../test-font.bin'; Face = $TestFace; Match = 'BridgeAlias'; Mode = 'replace-matching' }
    )
    foreach ($scenario in $scenarios) {
        $text = "[Font]`r`nFile=$($scenario.File)`r`nFace=$($scenario.Face)`r`nMatchFace=$($scenario.Match)`r`nMode=$($scenario.Mode)`r`n"
        [IO.File]::WriteAllText($ini, $text, $encoding)
        & $exe direct $scenario.Name $TestFace
        if ($LASTEXITCODE -ne 0) { throw "Font bridge scenario '$($scenario.Name)' failed ($LASTEXITCODE)." }
    }
    Remove-Item -LiteralPath $ini
    & $exe direct missing-config $TestFace
    if ($LASTEXITCODE -ne 0) { throw "Font bridge missing-config scenario failed ($LASTEXITCODE)." }
    # Leave a useful fixture for an independent PE-import-retargeting check.
    [IO.File]::WriteAllText($ini, "[Font]`r`nFile=test-font.bin`r`nFace=$TestFace`r`nMatchFace=BridgeAlias`r`nMode=replace-matching`r`n", $encoding)
}
Get-Item -LiteralPath $dll | Select-Object FullName, Length
