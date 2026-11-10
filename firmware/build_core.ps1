# Builds the Rust core as a static library for the ESP32-S3 and copies it to
# firmware/lib/liblfd3k_ffi.a, where the ESP-IDF build links it.
# Run from any directory:  powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1
$ErrorActionPreference = 'Stop'
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH
. "$env:USERPROFILE\export-esp.ps1"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $env:RUSTFLAGS = '-C panic=abort'
    cargo +esp build -p lfd3k-ffi --release --target xtensa-esp32s3-none-elf -Zbuild-std=core
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    $src = Join-Path $root 'target\xtensa-esp32s3-none-elf\release\liblfd3k_ffi.a'
    $dst = Join-Path $root 'firmware\lib\liblfd3k_ffi.a'
    Copy-Item $src $dst -Force
    Write-Host "copied $src -> $dst ($((Get-Item $dst).Length) bytes)"
} finally {
    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    Pop-Location
}
