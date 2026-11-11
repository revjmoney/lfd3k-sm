# Builds and runs the multimode PC tests with MinGW gcc. Touches no port,
# flashes nothing.
#
#   powershell -ExecutionPolicy Bypass -File multimode\tests\run.ps1
#
# test_choose       bootloader decision table (choose.c) and its source order promises
# test_picker       picker menu state machine (picker_menu.c)
# test_modem_side   the real firmware/src/mode_switch.c and settings_store.c on a fake
#                   ESP-IDF layer, with choose.c and picker_menu.c in the same binary
# guards            the #error guards in mode_switch.c must fire for each forbidden
#                   sdkconfig option (the compile is expected to fail)
[CmdletBinding()]
param([string]$Gcc = 'D:\onerous\msys64\mingw64\bin\gcc.exe')

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$mm = Split-Path -Parent $here
$root = Split-Path -Parent $mm
$out = Join-Path $here 'build'
New-Item -ItemType Directory -Force $out | Out-Null
$env:PATH = (Split-Path -Parent $Gcc) + ';' + $env:PATH
$srcdir = ($mm -replace '\\', '/')

$common = @('-std=gnu11', '-Wall', '-Wextra', '-Werror', '-Wno-unterminated-string-initialization',
    "-DSRC_DIR=`"$srcdir`"".Replace('"', '\"'), "-I$here", "-I$mm\bootloader", "-I$mm\picker\src")
$fake = @("-I$here\fake", "-I$root\firmware\src")

$failed = 0
function Build-And-Run($name, $sources, $extra) {
    $exe = Join-Path $out "$name.exe"
    if (Test-Path $exe) { Remove-Item $exe }
    & $Gcc @common @extra @sources -o $exe
    if ($LASTEXITCODE -ne 0) { Write-Host "BUILD FAILED: $name" -ForegroundColor Red; $script:failed++; return }
    Write-Host "== $name"
    & $exe
    if ($LASTEXITCODE -ne 0) { Write-Host "TESTS FAILED: $name" -ForegroundColor Red; $script:failed++ }
}

Build-And-Run 'test_choose' @("$here\test_choose.c", "$mm\bootloader\choose.c") @()
Build-And-Run 'test_picker' @("$here\test_picker.c", "$mm\picker\src\picker_menu.c") @()
Build-And-Run 'test_modem_side' @("$here\test_modem_side.c", "$here\fake\fake_esp.c", "$mm\bootloader\choose.c",
    "$mm\picker\src\picker_menu.c", "$root\firmware\src\mode_switch.c", "$root\firmware\src\settings_store.c") $fake

# The guards: each forbidden option must stop the build of mode_switch.c (and the
# Wi-Fi NVS option must stop settings_store.c).
Write-Host '== guards'
$guards = @(
    @('CONFIG_ESP_SYSTEM_ALLOW_RTC_FAST_MEM_AS_HEAP', 'mode_switch.c'),
    @('CONFIG_BOOTLOADER_RESERVE_RTC_MEM', 'mode_switch.c'),
    @('CONFIG_BOOTLOADER_CUSTOM_RESERVE_RTC', 'mode_switch.c'),
    @('CONFIG_BOOTLOADER_SKIP_VALIDATE_IN_DEEP_SLEEP', 'mode_switch.c'),
    @('CONFIG_ESP32S3_RTCDATA_IN_FAST_MEM', 'mode_switch.c'),
    @('CONFIG_ESP_WIFI_NVS_ENABLED', 'settings_store.c')
)
foreach ($g in $guards) {
    $obj = Join-Path $out 'guard.o'
    if (Test-Path $obj) { Remove-Item $obj }
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $Gcc -std=gnu11 -c "-D$($g[0])=1" "-I$here\fake" "-I$mm\bootloader" "-I$root\firmware\src" "$root\firmware\src\$($g[1])" -o $obj 2>$null
    $rc = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($rc -eq 0) { Write-Host "guard $($g[0]) did NOT stop $($g[1])" -ForegroundColor Red; $failed++ }
    else { Write-Host "ok     $($g[0]) stops $($g[1])" }
}
# And the clean compile of the same files must work (so the guard failures above are the guards).
foreach ($f in 'mode_switch.c', 'settings_store.c') {
    & $Gcc -std=gnu11 -c "-I$here\fake" "-I$mm\bootloader" "-I$root\firmware\src" "$root\firmware\src\$f" -o (Join-Path $out 'guard.o') 2>$null
    if ($LASTEXITCODE -ne 0) { Write-Host "clean compile of $f failed" -ForegroundColor Red; $failed++ }
}

if ($failed -gt 0) { Write-Host "multimode tests: $failed FAILED" -ForegroundColor Red; exit 1 }
Write-Host 'multimode tests: all passed'
exit 0
