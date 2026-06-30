$ErrorActionPreference = "Stop"

$buildTarget = Join-Path $PSScriptRoot ".build-target"
$source = Join-Path $buildTarget "release\wsl_controller.exe"
$dist = Join-Path $PSScriptRoot "dist"
$exe = Join-Path $dist "WSLController.exe"

if (Test-Path -LiteralPath $buildTarget) {
    Remove-Item -LiteralPath $buildTarget -Recurse -Force
}

cargo build --release --target-dir $buildTarget

New-Item -ItemType Directory -Path $dist -Force | Out-Null
Copy-Item -LiteralPath $source -Destination $exe -Force

Remove-Item -LiteralPath $buildTarget -Recurse -Force

Write-Host "Packaged: $exe"
