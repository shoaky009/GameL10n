[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$sources = Get-Content -LiteralPath (Join-Path $repo 'third_party/qlie-fonts/sources.json') -Raw | ConvertFrom-Json
$directory = Join-Path $repo 'target/runtime/adapter-components/qlie-fonts'
$null = [IO.Directory]::CreateDirectory($directory)

foreach ($item in $sources.files.PSObject.Properties) {
    $font = Join-Path $directory $item.Name
    if ((Test-Path -LiteralPath $font -PathType Leaf) -and
        (Get-FileHash -LiteralPath $font -Algorithm SHA256).Hash -eq $item.Value.sha256) {
        continue
    }
    $temporary = "$font.$([guid]::NewGuid().ToString('N')).download"
    try {
        $uri = "https://raw.githubusercontent.com/google/fonts/$($sources.revision)/$($item.Value.path)"
        Invoke-WebRequest -Uri $uri -OutFile $temporary -TimeoutSec 120
        if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $item.Value.sha256) {
            throw "QLIE font asset SHA-256 mismatch: $($item.Name)"
        }
        [IO.File]::Move($temporary, $font, $true)
    } finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary
        }
    }
}
