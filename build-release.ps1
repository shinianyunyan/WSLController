$ErrorActionPreference = "Stop"

cargo build --release

$source = Join-Path $PSScriptRoot "target\release\wsl_controller.exe"
$dist = Join-Path $PSScriptRoot "dist"
$exe = Join-Path $dist "WSLController.exe"

New-Item -ItemType Directory -Path $dist -Force | Out-Null
Copy-Item -LiteralPath $source -Destination $exe -Force

Write-Host "Built: $source"
Write-Host "Packaged: $exe"
