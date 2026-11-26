[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$runtime = [IO.Path]::GetFullPath((Join-Path $repo 'target/runtime'))
$references = [IO.Path]::GetFullPath((Join-Path $repo 'third_party/engine-tools'))
$engineToolsDest = Join-Path $runtime 'engine-tools'
$expectedReferences = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($pinFile in Get-ChildItem -LiteralPath $references -File -Filter '*.json') {
    $pin = Get-Content -LiteralPath $pinFile.FullName -Raw | ConvertFrom-Json
    $revision = [string]$pin.revision
    if ($revision -notmatch '^[0-9a-fA-F]{40}$') {
        throw "Invalid engine-tools revision in $($pinFile.Name): $revision"
    }
    $pinDest = [IO.Path]::GetFullPath((Join-Path $engineToolsDest $pinFile.Name))
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($pinDest))
    [IO.File]::Copy($pinFile.FullName, $pinDest, $true)

    $relativeLibrary = Join-Path 'references' (Join-Path $revision 'library.zip')
    $source = Join-Path $references $relativeLibrary
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Missing pinned engine-tools file: $relativeLibrary"
    }
    $destination = [IO.Path]::GetFullPath((Join-Path $engineToolsDest $relativeLibrary))
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
    [IO.File]::Copy($source, $destination, $true)
    $null = $expectedReferences.Add($destination)
}
$referencesDest = Join-Path $engineToolsDest 'references'
if (Test-Path -LiteralPath $referencesDest) {
    foreach ($file in Get-ChildItem -LiteralPath $referencesDest -Recurse -File) {
        if (-not $expectedReferences.Contains($file.FullName)) {
            Remove-Item -LiteralPath $file.FullName -Force
        }
    }
    Get-ChildItem -LiteralPath $referencesDest -Directory -Recurse |
        Sort-Object { $_.FullName.Length } -Descending |
        ForEach-Object {
            if (@(Get-ChildItem -LiteralPath $_.FullName -Force).Count -eq 0) {
                Remove-Item -LiteralPath $_.FullName -Force
            }
        }
}
$hostDirectory = Join-Path $runtime 'host'
$null = [IO.Directory]::CreateDirectory($hostDirectory)
[IO.File]::Copy((Join-Path $repo 'crates/core/python/credentials.py'), (Join-Path $hostDirectory 'credentials.py'), $true)
