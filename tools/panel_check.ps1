# Compiles and runs the PC test of the panel renderer, then writes the PNG
# mockups into docs\phase2-bench\mockups.
#   powershell -ExecutionPolicy Bypass -File tools\panel_check.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gcc = 'D:\onerous\msys64\mingw64\bin\gcc.exe'
$build = Join-Path $root 'target\panel_check'
$mock = Join-Path $root 'docs\phase2-bench\mockups'
New-Item -ItemType Directory -Force $build | Out-Null
New-Item -ItemType Directory -Force $mock | Out-Null
$env:PATH = 'D:\onerous\msys64\mingw64\bin;' + $env:PATH
$exe = Join-Path $build 'panel_check.exe'
& $gcc -std=c11 -O1 -g -Wall -Wextra -Werror `
    -I (Join-Path $root 'firmware\src') -I (Join-Path $root 'firmware\include') `
    -o $exe (Join-Path $root 'tools\panel_check.c') (Join-Path $root 'firmware\src\panel_render.c')
if ($LASTEXITCODE -ne 0) { throw "compile failed ($LASTEXITCODE)" }
& $exe --dump $build
if ($LASTEXITCODE -ne 0) { throw "panel_check failed ($LASTEXITCODE)" }
python (Join-Path $root 'tools\panel_mockups.py') $build $mock
if ($LASTEXITCODE -ne 0) { throw "mockups failed ($LASTEXITCODE)" }
