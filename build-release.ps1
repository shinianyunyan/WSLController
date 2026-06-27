$ErrorActionPreference = "Stop"

cargo build --release

$exe = Join-Path $PSScriptRoot "target\release\wsl_controller.exe"
Write-Host "Built: $exe"
