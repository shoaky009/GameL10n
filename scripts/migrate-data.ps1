[CmdletBinding(DefaultParameterSetName = 'OneGame')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'OneGame')][string] $GameDirectory,
    [Parameter(Mandatory = $true, ParameterSetName = 'AllGames')][switch] $AllGames,
    [string] $SourceRoot = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'AvgTranslator'),
    [string] $DestinationRoot,
    [switch] $Activate,
    [string] $ReportPath,
    [switch] $VerifyOnly,
    [string] $BootstrapRoot
)

$ErrorActionPreference = 'Stop'
$migrationPackageRoot = Split-Path $PSScriptRoot -Parent
$migrationTool = Join-Path $migrationPackageRoot 'tools/data/AvgTranslator.DataTool.exe'
$migrationArguments = @()
if ($VerifyOnly) {
    if ($AllGames) { throw '-VerifyOnly requires -GameDirectory.' }
    $migrationDataRoot = if ($DestinationRoot) { $DestinationRoot } else { $SourceRoot }
    $migrationArguments = @('verify', '--game-directory', $GameDirectory, '--data-root', $migrationDataRoot)
} else {
    $migrationArguments = @('migrate', '--source-root', $SourceRoot)
    if ($AllGames) { $migrationArguments += '--all' }
    else { $migrationArguments += @('--game-directory', $GameDirectory) }
    if ($DestinationRoot) { $migrationArguments += @('--destination-root', $DestinationRoot) }
    if ($Activate) { $migrationArguments += '--activate' }
    if ($BootstrapRoot) { $migrationArguments += @('--bootstrap-root', $BootstrapRoot) }
}
if ($ReportPath) { $migrationArguments += @('--report', $ReportPath) }

if (Test-Path -LiteralPath $migrationTool) {
    & $migrationTool @migrationArguments
} else {
    $migrationProject = Join-Path $migrationPackageRoot 'tools/AvgTranslator.DataTool/AvgTranslator.DataTool.csproj'
    if (-not (Test-Path -LiteralPath $migrationProject)) { throw 'Packaged data tool is missing: tools/data/AvgTranslator.DataTool.exe' }
    & (Join-Path $PSScriptRoot 'dotnet.ps1') run --project $migrationProject -- @migrationArguments
}
if ($LASTEXITCODE -ne 0) { throw "Data tool failed with exit code $LASTEXITCODE. Original data was preserved; see the diagnostic above." }
