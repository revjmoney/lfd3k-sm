# Makes sim/testdata/speech-16k.wav: 20 s of speech, 16 kHz, 16 bit, mono,
# with the Windows speech synthesizer (System.Speech, no download). Used by
# lfd3k-voicesim as the caller's mic.
#
#   powershell -ExecutionPolicy Bypass -File tools\make_speech_wav.ps1 [-Out <path>] [-Seconds 20]
#
# The text is spoken at the default voice and rate, then cut (or padded with
# silence) to exactly the length asked for.

param(
    [string]$Out = (Join-Path $PSScriptRoot '..\sim\testdata\speech-16k.wav'),
    [int]$Seconds = 20
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$Out = [System.IO.Path]::GetFullPath($Out)

# Some Windows installs register desktop (SAPI) voices for 32 bit programs
# only; then this script runs itself again in the 32 bit PowerShell.
# A voice whose engine is missing is listed but cannot speak, so each one
# is tried. The SAPI sample voice is a last resort (it is not real speech).
function Test-Voice([string]$name) {
    try {
        $t = New-Object System.Speech.Synthesis.SpeechSynthesizer
        $t.SelectVoice($name)
        $t.SetOutputToNull()
        $t.Speak('test')
        $t.Dispose()
        return $true
    } catch { return $false }
}
$names = @()
try {
    $probe = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $names = @($probe.GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo.Name })
    $probe.Dispose()
} catch { $names = @() }
$voice = $names | Where-Object { $_ -notlike 'Sample*' -and (Test-Voice $_) } | Select-Object -First 1
if (-not $voice) {
    $wow = Join-Path $env:WINDIR 'SysWOW64\WindowsPowerShell\v1.0\powershell.exe'
    if ([Environment]::Is64BitProcess -and (Test-Path $wow)) {
        & $wow -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Out $Out -Seconds $Seconds
        exit $LASTEXITCODE
    }
    $voice = $names | Where-Object { $_ -like 'Sample*' -and (Test-Voice $_) } | Select-Object -First 1
    if (-not $voice) { throw 'no System.Speech voice is installed' }
    Write-Warning "no real voice is installed; using $voice, which is not real speech. Install a Windows voice and run this again."
}

$text = @'
This is the LORA FLOWTRON DATAMASTER 3000 serial modem, testing a voice call.
One, two, three, four, five, six, seven, eight, nine, ten.
The quick brown fox jumps over the lazy dog, and the five boxing wizards jump quickly.
On the fast tiers this is a normal phone call. On the slow tiers you hold the talk button,
say what you need to say, and let go so the other side can answer.
Please check the two words on both screens before you talk about anything private.
Over and out.
'@

$rate = 16000
$tmp = [System.IO.Path]::GetTempFileName()
try {
    $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
        $rate,
        [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
        [System.Speech.AudioFormat.AudioChannel]::Mono)
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $synth.SelectVoice($voice)
    $synth.SetOutputToWaveFile($tmp, $fmt)
    $synth.Speak($text)
    $synth.SetOutputToNull()
    $synth.Dispose()

    # Find the data chunk of what the synthesizer wrote.
    $b = [System.IO.File]::ReadAllBytes($tmp)
    $at = 12
    $data = $null
    while ($at + 8 -le $b.Length) {
        $id = [System.Text.Encoding]::ASCII.GetString($b, $at, 4)
        $len = [System.BitConverter]::ToInt32($b, $at + 4)
        if ($id -eq 'data') {
            $n = [Math]::Min($len, $b.Length - $at - 8)
            $data = New-Object byte[] $n
            [Array]::Copy($b, $at + 8, $data, 0, $n)
            break
        }
        $at += 8 + $len + ($len -band 1)
    }
    if ($null -eq $data) { throw 'no data chunk in the synthesized WAV' }

    # Exactly $Seconds long: cut, or pad with silence.
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
    Write-Host "wrote $Out ($Seconds s; the voice spoke for $spoken s)"
}
finally {
    Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
}
