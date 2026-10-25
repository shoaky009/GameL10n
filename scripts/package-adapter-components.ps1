param([string]$OutputDirectory = 'artifacts/AvgTranslator/tools/adapter-components')
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath([IO.Path]::Combine($repoRoot, $OutputDirectory))
$source = Join-Path $repoRoot 'adapters/qlie-filepack3-systemfont/tools/QlieLauncher.cs'
$framework = Join-Path $env:SystemRoot 'Microsoft.NET/Framework/v4.0.30319'
$compiler = Join-Path $repoRoot '.tools/dotnet/sdk/10.0.402/Roslyn/bincore/csc.dll'
if (-not (Test-Path -LiteralPath $compiler)) { throw 'The pinned compiler is required to package the independent QLIE launcher.' }
New-Item -ItemType Directory -Path $output -Force | Out-Null
$executable = Join-Path $output 'QlieLauncher.exe'
$compilerArguments = @('/noconfig', '/nologo', '/nostdlib+', '/target:winexe', '/platform:x86', '/optimize+', '/deterministic+', '/langversion:5', '/codepage:65001', "/pathmap:$repoRoot=/sources", "/out:$executable")
foreach ($name in @('mscorlib.dll', 'System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Xml.dll')) {
    $reference = Join-Path $framework $name
    if (-not (Test-Path -LiteralPath $reference)) { throw "Missing Windows .NET Framework assembly: $name" }
    $compilerArguments += "/reference:$reference"
}
& (Join-Path $PSScriptRoot 'dotnet.ps1') $compiler @compilerArguments $source
if ($LASTEXITCODE -ne 0) { throw 'Reusable QLIE launcher compilation failed.' }
if (-not (Test-Path -LiteralPath $executable)) { throw 'The compiler did not produce the reusable QLIE launcher.' }
Write-Output "Adapter components: $output"
