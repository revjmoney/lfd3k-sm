# Makes sim/testdata/speech-16k.wav: 20 s of speech, 16 kHz, 16 bit, mono,
# with the voices built into Windows (no download). Used by lfd3k-voicesim
# as the caller's mic.
#
#   powershell -ExecutionPolicy Bypass -File tools\make_speech_wav.ps1 [-Out <path>] [-Seconds 20]
#
# Voices, first that speaks wins:
#   1. a desktop voice through System.Speech (SAPI);
#   2. a OneCore voice through WinRT (Windows.Media.SpeechSynthesis), the
#      voices in Settings, Time and language, Speech;
#   3. a desktop voice in the 32 bit PowerShell (some installs register
#      them for 32 bit programs only; the script runs itself again there);
#   4. the SAPI sample voice, with a warning: it is not real speech.
# A voice whose engine is missing is listed but cannot speak, so each one is
# tried. The text is spoken at the voice's default rate, resampled to 16 kHz
# mono if needed, then cut (or padded with silence) to exactly the length
# asked for.

param(
    [string]$Out = (Join-Path $PSScriptRoot '..\sim\testdata\speech-16k.wav'),
    [int]$Seconds = 20,
    [switch]$Wow
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$Out = [System.IO.Path]::GetFullPath($Out)
$rate = 16000

$text = @'
This is the LORA FLOWTRON DATAMASTER 3000 serial modem, testing a voice call.
One, two, three, four, five, six, seven, eight, nine, ten.
The quick brown fox jumps over the lazy dog, and the five boxing wizards jump quickly.
On the fast tiers this is a normal phone call. On the slow tiers you hold the talk button,
say what you need to say, and let go so the other side can answer.
Please check the two words on both screens before you talk about anything private.
Over and out.
'@

# The fmt (rate, channels) and data chunk of a 16 bit PCM WAV, or $null.
function Read-Wav([byte[]]$b) {
    $at = 12
    $ch = 1; $wr = 0; $data = $null
    while ($at + 8 -le $b.Length) {
        $id = [System.Text.Encoding]::ASCII.GetString($b, $at, 4)
        $len = [System.BitConverter]::ToInt32($b, $at + 4)
        if ($id -eq 'fmt ') {
            $ch = [System.BitConverter]::ToInt16($b, $at + 10)
            $wr = [System.BitConverter]::ToInt32($b, $at + 12)
            if ([System.BitConverter]::ToInt16($b, $at + 22) -ne 16) { return $null }
        } elseif ($id -eq 'data') {
            $n = [Math]::Min($len, $b.Length - $at - 8)
            $data = New-Object byte[] $n
            [Array]::Copy($b, $at + 8, $data, 0, $n)
            break
        }
        $at += 8 + $len + ($len -band 1)
    }
    if ($null -eq $data -or $wr -le 0) { return $null }
    return @{ Rate = $wr; Channels = [int]$ch; Data = $data }
}

# 16 bit PCM of any rate and channel count to 16 kHz mono (linear).
function To-16kMono($w) {
    $ch = $w.Channels
    $frames = [int]($w.Data.Length / (2 * $ch))
    $mono = New-Object 'double[]' $frames
    for ($i = 0; $i -lt $frames; $i++) {
        $s = 0.0
        for ($c = 0; $c -lt $ch; $c++) { $s += [System.BitConverter]::ToInt16($w.Data, ($i * $ch + $c) * 2) }
        $mono[$i] = $s / $ch
    }
    if ($w.Rate -eq $rate -and $ch -eq 1) { return $w.Data }
    $n = [int]([long]$frames * $rate / $w.Rate)
    $outb = New-Object byte[] ($n * 2)
    $step = $w.Rate / $rate
    for ($i = 0; $i -lt $n; $i++) {
        $x = $i * $step
        $k = [int][Math]::Floor($x)
        $f = $x - $k
        $a = if ($k -lt $frames) { $mono[$k] } else { 0.0 }
        $bb = if ($k + 1 -lt $frames) { $mono[$k + 1] } else { $a }
        $v = [int][Math]::Round($a + ($bb - $a) * $f)
        $v = [Math]::Max(-32768, [Math]::Min(32767, $v))
        $bytes = [System.BitConverter]::GetBytes([int16]$v)
        $outb[2 * $i] = $bytes[0]; $outb[2 * $i + 1] = $bytes[1]
    }
    return $outb
}

# Write exactly $Seconds of 16 kHz mono PCM (cut or padded).
function Write-Out([byte[]]$data, [string]$who) {
    $want = $rate * 2 * $Seconds
    $pcm = New-Object byte[] $want
    [Array]::Copy($data, 0, $pcm, 0, [Math]::Min($data.Length, $want))
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($ms)
    $w.Write([System.Text.Encoding]::ASCII.GetBytes('RIFF'))
    $w.Write([int](36 + $want))
    $w.Write([System.Text.Encoding]::ASCII.GetBytes('WAVEfmt '))
    $w.Write([int]16)
    $w.Write([int16]1)
    $w.Write([int16]1)
    $w.Write([int]$rate)
    $w.Write([int]($rate * 2))
    $w.Write([int16]2)
    $w.Write([int16]16)
    $w.Write([System.Text.Encoding]::ASCII.GetBytes('data'))
    $w.Write([int]$want)
    $w.Write($pcm)
    $w.Flush()
    $dir = Split-Path -Parent $Out
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
    [System.IO.File]::WriteAllBytes($Out, $ms.ToArray())
    $w.Dispose()
    $spoken = [Math]::Round($data.Length / ($rate * 2), 1)
    Write-Host "wrote $Out ($Seconds s; $who spoke for $spoken s)"
}

function Test-SapiVoice([string]$name) {
    try {
        $t = New-Object System.Speech.Synthesis.SpeechSynthesizer
        $t.SelectVoice($name)
        $t.SetOutputToNull()
        $t.Speak('test')
        $t.Dispose()
        return $true
    } catch { return $false }
}

# Speak $text with a SAPI voice at 16 kHz mono; the PCM bytes.
function Speak-Sapi([string]$name) {
    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
            $rate,
            [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
            [System.Speech.AudioFormat.AudioChannel]::Mono)
        $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
        $synth.SelectVoice($name)
        $synth.SetOutputToWaveFile($tmp, $fmt)
        $synth.Speak($text)
        $synth.SetOutputToNull()
        $synth.Dispose()
        $w = Read-Wav ([System.IO.File]::ReadAllBytes($tmp))
        if ($null -eq $w) { throw 'no data chunk in the synthesized WAV' }
        return (To-16kMono $w)
    } finally {
        Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
    }
}

# Speak $text with the default OneCore voice through WinRT; the PCM bytes
# at 16 kHz mono, or $null when WinRT speech does not work here.
function Speak-WinRT {
    try {
        Add-Type -AssemblyName System.Runtime.WindowsRuntime
        $null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType = WindowsRuntime]
        $null = [Windows.Storage.Streams.DataReader, Windows.Storage.Streams, ContentType = WindowsRuntime]
        $asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
            $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
            $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
        $await = {
            param($op, [Type]$t)
            $task = $asTask.MakeGenericMethod($t).Invoke($null, @($op))
            $null = $task.Wait(60000)
            $task.Result
        }
        $synth = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
        $who = $synth.Voice.DisplayName
        $stream = & $await ($synth.SynthesizeTextToStreamAsync($text)) ([Windows.Media.SpeechSynthesis.SpeechSynthesisStream])
        $reader = New-Object Windows.Storage.Streams.DataReader($stream.GetInputStreamAt(0))
        $n = & $await ($reader.LoadAsync([uint32]$stream.Size)) ([uint32])
        $bytes = New-Object byte[] $n
        $reader.ReadBytes($bytes)
        $w = Read-Wav $bytes
        if ($null -eq $w) { return $null }
        return @{ Who = $who; Data = (To-16kMono $w) }
    } catch {
        Write-Host "WinRT speech does not work here: $($_.Exception.Message.Trim())"
        return $null
    }
}

$names = @()
try {
    $probe = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $names = @($probe.GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo.Name })
    $probe.Dispose()
} catch { $names = @() }

# 1. A real desktop voice.
$voice = $names | Where-Object { $_ -notlike 'Sample*' -and (Test-SapiVoice $_) } | Select-Object -First 1
if ($voice) {
    Write-Out (Speak-Sapi $voice) $voice
    exit 0
}

if (-not $Wow) {
    # 2. A OneCore voice through WinRT.
    $r = Speak-WinRT
    if ($null -ne $r) {
        Write-Out $r.Data $r.Who
        exit 0
    }
    # 3. The 32 bit PowerShell's desktop voices.
    $wowPs = Join-Path $env:WINDIR 'SysWOW64\WindowsPowerShell\v1.0\powershell.exe'
    if ([Environment]::Is64BitProcess -and (Test-Path $wowPs)) {
        & $wowPs -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Out $Out -Seconds $Seconds -Wow
        exit $LASTEXITCODE
    }
}

# 4. The SAPI sample voice.
$voice = $names | Where-Object { $_ -like 'Sample*' -and (Test-SapiVoice $_) } | Select-Object -First 1
if (-not $voice) { throw 'no Windows speech voice works on this PC' }
Write-Warning "no real voice works on this PC; using $voice, which is not real speech. Install a Windows voice (Settings, Time and language, Speech) and run this again."
Write-Out (Speak-Sapi $voice) $voice
