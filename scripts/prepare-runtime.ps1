[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$runtime = Join-Path $repo 'target/runtime'
$references = Join-Path $repo 'third_party/engine-tools'
foreach ($file in Get-ChildItem -LiteralPath $references -Recurse -File) {
    $relative = [IO.Path]::GetRelativePath($references, $file.FullName)
    $destination = Join-Path $runtime (Join-Path 'engine-tools' $relative)
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
    [IO.File]::Copy($file.FullName, $destination, $true)
}
$hostDirectory = Join-Path $runtime 'host'
$null = [IO.Directory]::CreateDirectory($hostDirectory)
[IO.File]::Copy((Join-Path $repo 'crates/core/python/credentials.py'), (Join-Path $hostDirectory 'credentials.py'), $true)
