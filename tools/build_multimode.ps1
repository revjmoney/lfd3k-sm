# Builds or collects every piece of the multimode install image, checks the
# layout, and merges one 16 MB image with esptool merge_bin.
#
#   powershell -ExecutionPolicy Bypass -File tools\build_multimode.ps1 [-Build]
#
# This script NEVER flashes: it does not call write_flash, erase_flash or
# read_flash, never runs pio upload or monitor, and never opens a serial port.
# merge_bin only reads files and writes multimode\out\full-16mb.bin.
[CmdletBinding()]
param(
    [switch]$Build,   # build each piece first (slow; Meshtastic alone is about an hour cold)

    # Picker project outputs (pio run -d multimode/picker)
    [string]$PickerDir = '',
    [string]$Bootloader = '',
    [string]$Partitions = '',
    [string]$Picker = '',

    # The other modes
    [string]$Modem = 'F:\lfd3k-sm\firmware\.pio\build\heltec_v4\firmware.bin',
    [string]$Simian = 'F:\simian-radar\.pio\build\heltec_v4_multimode\firmware.bin',
    [string]$Mesh = 'F:\meshtastic-axis\.pio\build\heltec-v4-axis\firmware-heltec-v4-axis-2.7.26.54e0d8d.bin',
    [string]$RnodeZip = 'F:\lfd3k-sm\vendor\rnode\rnode_firmware_heltec32v4pa.zip',
    [string]$Rnode = '',   # default: extracted from the zip into multimode\out\rnode
    [string]$CamDb = 'F:\simian-radar\build\camdb.bin',   # optional

    [string]$SimianRepo = 'F:\simian-radar',
    [string]$MeshRepo = 'F:\meshtastic-axis',
    [string]$MeshEnv = 'heltec-v4-axis',

    [string]$Python = 'python',
    [string]$Esptool = "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py",
    [string]$OutDir = '',
    [ValidateSet('qio', 'dio')][string]$FlashMode = 'dio',   # header stays DIO like every image this board ran; the bootloader switches itself to QIO
    [string]$FlashFreq = '80m'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$csv = Join-Path $root 'multimode\partitions-multimode.csv'
if (-not $PickerDir) { $PickerDir = Join-Path $root 'multimode\picker\.pio\build\picker' }
if (-not $Bootloader) { $Bootloader = Join-Path $PickerDir 'bootloader.bin' }
if (-not $Partitions) { $Partitions = Join-Path $PickerDir 'partitions.bin' }
if (-not $Picker) { $Picker = Join-Path $PickerDir 'firmware.bin' }
if (-not $OutDir) { $OutDir = Join-Path $root 'multimode\out' }
New-Item -ItemType Directory -Force $OutDir | Out-Null

function Run($exe, $argList, $dir) {
    Push-Location $dir
    try {
        & $exe @argList
        if ($LASTEXITCODE -ne 0) { throw "$exe failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
}

# ---- offsets come from the CSV, so the image can never drift from the table ----
function Get-Offsets($path) {
    $map = @{}
    foreach ($line in Get-Content $path) {
        $t = ($line -split '#')[0].Trim()
        if (-not $t) { continue }
        $c = $t -split ',' | ForEach-Object { $_.Trim() }
        if ($c.Count -ge 5) { $map[$c[0]] = [Convert]::ToInt64($c[3], 16) }
    }
    return $map
}
$off = Get-Offsets $csv
$expect = @{ factory = 0x20000; ota_0 = 0xA0000; ota_1 = 0x4A0000; ota_2 = 0x620000; ota_3 = 0x7A0000; camdb = 0xCB0000 }
foreach ($k in $expect.Keys) {
    if ($off[$k] -ne $expect[$k]) { throw ("CSV offset of {0} is 0x{1:X}, expected 0x{2:X}" -f $k, $off[$k], $expect[$k]) }
}

# ---- optional builds (never upload) ----
if ($Build) {
    Write-Host '== build picker'
    Run 'pio' @('run', '-d', 'multimode/picker') $root
    Write-Host '== build modem'
    Run 'powershell' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $root 'firmware\build_core.ps1')) $root
    Run 'pio' @('run', '-d', 'firmware', '-e', 'heltec_v4') $root
    Write-Host '== build simian'
    Run 'pio' @('run', '-e', 'heltec_v4_multimode') $SimianRepo
    Write-Host '== build meshtastic (long)'
    Run 'pio' @('run', '-e', $MeshEnv) $MeshRepo
}

# ---- RNode: take only rnode_firmware_heltec32v4pa.bin from the zip, never run anything in it ----
if (-not $Rnode) {
    $rdir = Join-Path $OutDir 'rnode'
    $Rnode = Join-Path $rdir 'rnode_firmware_heltec32v4pa.bin'
    if (Test-Path $RnodeZip) {
        New-Item -ItemType Directory -Force $rdir | Out-Null
        $py = @'
import sys, zipfile
z, member, dest = sys.argv[1:4]
with zipfile.ZipFile(z) as f:
    data = f.read(member)
open(dest, "wb").write(data)
print("extracted %s (%d bytes)" % (member, len(data)))
'@
        $pyfile = Join-Path $OutDir 'extract_rnode.py'
        Set-Content -Path $pyfile -Value $py -Encoding ascii
        Run $Python @('-I', $pyfile, $RnodeZip, 'rnode_firmware_heltec32v4pa.bin', $Rnode) $OutDir
        Remove-Item $pyfile
    }
}

# ---- collect ----
$pieces = @(
    @{ Label = 'bootloader'; Offset = 0x0;                 Path = $Bootloader; Name = 'bootloader' },
    @{ Label = 'partitions'; Offset = 0x8000;              Path = $Partitions; Name = 'partition table' },
    @{ Label = 'factory';    Offset = $off['factory'];     Path = $Picker;     Name = 'picker' },
    @{ Label = 'ota_0';      Offset = $off['ota_0'];       Path = $Mesh;       Name = 'MESH' },
    @{ Label = 'ota_1';      Offset = $off['ota_1'];       Path = $Modem;      Name = 'MODEM' },
    @{ Label = 'ota_2';      Offset = $off['ota_2'];       Path = $Simian;     Name = 'SIMIAN' },
    @{ Label = 'ota_3';      Offset = $off['ota_3'];       Path = $Rnode;      Name = 'RNODE' }
)
$missing = @()
$have = @()
foreach ($p in $pieces) {
    if (Test-Path $p.Path -PathType Leaf) { $have += $p } else { $missing += ("{0}: {1}" -f $p.Name, $p.Path) }
}
if ($CamDb -and (Test-Path $CamDb -PathType Leaf)) {
    $have += @{ Label = 'camdb'; Offset = $off['camdb']; Path = $CamDb; Name = 'camdb' }
} else {
    Write-Warning "camdb not found ($CamDb): Simian's camera database will be flashed separately (tools\flash_camdb.ps1 -Multimode)."
}
if ($missing.Count -gt 0) {
    Write-Host 'MISSING pieces:' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    # still check what exists, so the report is useful, then stop
}

# ---- layout check ----
$checkArgs = @('-I', (Join-Path $root 'tools\check_layout.py'), '--csv', $csv)
foreach ($p in $have) { $checkArgs += ("{0}={1}" -f $p.Label, $p.Path) }
& $Python @checkArgs
$checkRc = $LASTEXITCODE
if ($checkRc -ne 0) { throw 'layout check failed, no image written' }
if ($missing.Count -gt 0) { throw ("{0} piece(s) missing, no image written" -f $missing.Count) }

# ---- merge (reads files, writes one file, touches no port) ----
$full = Join-Path $OutDir 'full-16mb.bin'
if (Test-Path $full) { Remove-Item $full }
$m = @($Esptool, '--chip', 'esp32s3', 'merge_bin', '-o', $full,
    '--flash_mode', $FlashMode, '--flash_freq', $FlashFreq, '--flash_size', '16MB',
    '--fill-flash-size', '16MB')
foreach ($p in ($have | Sort-Object { $_.Offset })) { $m += @(('0x{0:X}' -f $p.Offset), $p.Path) }
& $Python @m
if ($LASTEXITCODE -ne 0) { throw "merge_bin failed ($LASTEXITCODE)" }
$size = (Get-Item $full).Length
if ($size -ne 16MB) { throw "merged image is $size bytes, expected 16777216" }

# ---- flash_args.txt: the line the controller uses later; COMn is a placeholder ----
$esptoolRel = '%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py'
$line = ('python "{0}" --chip esp32s3 -p COMn -b 460800 --before default_reset --after no_reset write_flash --flash_mode keep --flash_freq keep --flash_size keep 0x0 full-16mb.bin' -f $esptoolRel)
$argsFile = Join-Path $OutDir 'flash_args.txt'
Set-Content -Path $argsFile -Value @(
    '# Replace COMn with the verified port (VID 303A PID 1001, MAC 88:56:A6:96:16:FC). Run from the multimode\out folder.',
    '# Only after a full backup is verified and Rev has approved the wipe. After it, unplug and replug USB (no_reset: a watchdog reset would count as a crash).',
    $line) -Encoding ascii

# ---- manifest ----
function Sha($path) { (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower() }
$mf = @('LFD3K-SM multimode install image', ("built {0}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm')), '')
foreach ($p in ($have | Sort-Object { $_.Offset })) {
    $mf += ("{0}  0x{1:X7}  {2,9} bytes  {3,-16} {4}" -f (Sha $p.Path), $p.Offset, (Get-Item $p.Path).Length, $p.Name, $p.Path)
}
$mf += ''
$mf += ("{0}  0x0000000  {1,9} bytes  full-16mb.bin" -f (Sha $full), $size)
Set-Content -Path (Join-Path $OutDir 'manifest.txt') -Value $mf -Encoding ascii

Write-Host ''
Write-Host "wrote $full ($size bytes)"
Write-Host ("sha256 {0}" -f (Sha $full))
Write-Host "wrote $argsFile and manifest.txt"
