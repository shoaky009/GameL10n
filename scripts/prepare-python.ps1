[CmdletBinding()]
param([string]$ArchivePath)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$version = '3.14.10'
$expectedHash = '90905e64a840557d8c1bc4f5b74db3d7ee85e3ffb217611cac1c7813acea4159'
$output = Join-Path $repo 'tools/python'
if (Test-Path -LiteralPath (Join-Path $output 'python.exe')) {
    & (Join-Path $output 'python.exe') -I --version
    if ($LASTEXITCODE -ne 0) { throw 'Existing portable Python could not start.' }
    return
}
if ((Test-Path -LiteralPath $output) -and @(Get-ChildItem -LiteralPath $output -Force).Count -gt 0) {
    throw 'tools/python is incomplete. Move or remove it before restoring the pinned interpreter.'
}
if (-not $ArchivePath) {
    $cache = Join-Path $repo '.tools'
    $null = [IO.Directory]::CreateDirectory($cache)
    $ArchivePath = Join-Path $cache "python-$version-embed-amd64.zip"
    if (-not (Test-Path -LiteralPath $ArchivePath)) {
        Invoke-WebRequest -Uri "https://www.python.org/ftp/python/$version/python-$version-embed-amd64.zip" -OutFile $ArchivePath
    }
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
    throw 'Portable Python archive SHA-256 does not match the pinned distribution.'
}
Expand-Archive -LiteralPath $ArchivePath -DestinationPath $output
& (Join-Path $output 'python.exe') -I --version
if ($LASTEXITCODE -ne 0) { throw 'Restored portable Python could not start.' }
