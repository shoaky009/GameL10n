$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$env:DOTNET_CLI_HOME = Join-Path $repoRoot '.tools/dotnet-home'
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'
$env:NUGET_PACKAGES = Join-Path $repoRoot '.packages'
$env:NUGET_HTTP_CACHE_PATH = Join-Path $repoRoot '.tools/nuget-http'
$env:NUGET_SCRATCH = Join-Path $repoRoot '.tools/nuget-tmp'
$localDotnet = Join-Path $repoRoot '.tools/dotnet/dotnet.exe'
if (Test-Path -LiteralPath $localDotnet) {
    & $localDotnet @args
} else {
    & dotnet @args
}
exit $LASTEXITCODE
