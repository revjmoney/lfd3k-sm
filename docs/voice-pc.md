# Voice calls on the PC: lfd3k-voice-pc

**Your own encrypted radio (cell) phone network!! Call anyone with a modem. No carrier. No tower. No one listening.**

`lfd3k-voice-pc` is the PC voice app of the LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM. It calls people by name, like a phone. In this first part it carries its own modem inside: a real LFD3K-SM core on a simulated radio over UDP (SimNet, the same as `lfd3k-pcmodem`), so two copies of the app call each other with no radio at all, on one PC or on two. It can also drive a real modem over USB or Wi-Fi (see "With a real modem" below). The phone app comes later.

Everything in a call is scrambled by the FLOWTRON SCRAMBLER (end to end, keys made fresh for every call). Ham mode does not exist yet, so the scrambler is always on.

## Build

From PowerShell in the repo. The voice crate needs clang-cl on Windows (for Codec 2) and cmake (for Opus): see "Prerequisites for the voice crates" in the [README](../README.md#prerequisites-for-the-voice-crates). Point `LFD3K_CLANG_CL` at a clang-cl.exe (the Android NDK's clang-cl, or Visual Studio's C++ Clang tools), or put clang-cl on PATH:

```powershell
$env:LFD3K_CLANG_CL = 'C:\path\to\clang-cl.exe'
cargo build --release -p lfd3k-voice-pc
```

The program is `target\release\lfd3k-voice-pc.exe`. Use the release build for calls: Opus and Codec2 are slow in a debug build.

## Options

```
lfd3k-voice-pc --name REV --label PC --bind 127.0.0.1:4211 --peer 127.0.0.1:4210
    [--modem sim|COM11|/dev/ttyUSB0|tcp:<host>[:<port>]|find[:<station>]]
    [--snr <dB>] [--cap-tier SF9] [--data <dir>] [--station <NAME>]
    [--auto-answer] [--play <wav>] [--record <wav>]
    [--script <file> --shots <dir> [--script-unsafe]] [--trace <dir>]
```

| Option | Meaning |
|---|---|
| `--modem <where>` | Which modem the app uses. `sim` (the default) is the built-in one on SimNet. `COM11` or `/dev/ttyUSB0` is a real modem on USB. `tcp:192.168.4.1` is a real modem on Wi-Fi (port 4220 unless you add `:<port>`). `find` looks for a modem on the network, `find:<station>` for the one with that station name. See "With a real modem" |
| `--name`, `--label` | Your voice name (a person, like REV) and this device's label (like PC). Used the first time only, to make this device's keys; after that change them in Settings, Voice and keys. Default REV and PC |
| `--bind`, `--peer` | Built-in modem only. This app's UDP address and the other app's (both `addr:port`). Defaults 127.0.0.1:4211 and 127.0.0.1:4210 |
| `--snr <dB>` | Built-in modem only. What the other modem hears from ours (default 8). Lower is slower: the tier, codec and talk mode follow it |
| `--cap-tier SF9` | Caps our modem's tier (S21) for this run, to try a slow tier. ATZ (DISCARD) takes it away. On a real modem the app reads S21 first and puts the old value back when you close it (it never saves with AT&W) |
| `--data <dir>` | Where this device keeps its keys, checked contacts and the modem's saved settings. Default `%APPDATA%\lfd3k-voice-pc\<NAME>-<LABEL>` |
| `--station <NAME>` | Built-in modem only. Its station name. Default `PCMODEM-<port>`. A real modem has its own name (AT+NAME) |
| `--auto-answer` | Test helper: the app picks up by itself and talks anyway (the two words still show). The modem's S0 still never answers voice; this is the app's choice |
| `--play <wav>` | Test helper: the WAV is the mic (looped while we may talk). The real mic is not opened |
| `--record <wav>` | Test helper: what the app hears goes into this WAV (written at the end of each call). The speakers are not used |
| `--script <file> --shots <dir>` | Test helper: drives the window from a file, one action per line (call, words_match, talk down, set ATS30=5, save, shot <name>...), through the same code as the buttons; the command list is at the top of the script code in `apps/pc/src/ui.rs`. For a recorded call between two copies: `sync <file> [<ms>]` (both copies meet at one wall clock go mark through the file), `at_t <ms>` (wait until the go mark plus ms), `play <from_ms> <to_ms>` (play that part of the `--play` WAV from now, once, whatever the floor; `play 0 0` stops the looping before the call) |
| `--script-unsafe` | Test helper: lets a release build run the script command `link_code_file <path>`, which writes the link code (a secret) to a file in plain text for another copy's script. A debug build always allows it; without this flag a release build refuses it |
| `--trace <dir>` | Test helper for the testing video: `status.csv` (every 250 ms the front panel status fields and the last 64 scope samples), `host.log` (the AT lines the app sent, the modem's replies with passwords masked, and the core's own result line for each call state change: RING, CONNECT, RETRAIN, NO CARRIER, DECLINED) and `marks.csv` (where the `--record` WAV and each `play` start, Unix ms) |

## Two apps on this PC

Open two PowerShell windows in the repo.

Window 1, the far end, which answers by itself and plays a recording:

```powershell
target\release\lfd3k-voice-pc.exe --name ANN --label PC --bind 127.0.0.1:4210 --peer 127.0.0.1:4211 --auto-answer --play sim\testdata\speech-16k.wav --record ann-heard.wav
```

Window 2, you:

```powershell
target\release\lfd3k-voice-pc.exe --name REV --label PC --bind 127.0.0.1:4211 --peer 127.0.0.1:4210
```

The ANN copy uses no sound devices (`--play` replaces its mic, `--record` its speakers), so your mic never hears it and nothing feeds back. **Use a headset in CALL mode**: the PC app has no echo canceller in this part.

## With a real modem

Plug in a V4 or V3 modem, or put one on your Wi-Fi, and point the app at it with `--modem`. Everything else works the same: calls, the scope, Settings (they change the real modem).

On USB (Windows, find the COM port in Device Manager):

```powershell
target\release\lfd3k-voice-pc.exe --name REV --label PC --modem COM11
```

On Linux it is `--modem /dev/ttyUSB0` (the V3) or `/dev/ttyACM0` (the V4).

On Wi-Fi, by address or by looking it up:

```powershell
target\release\lfd3k-voice-pc.exe --name REV --label PC --modem tcp:192.168.4.1
target\release\lfd3k-voice-pc.exe --name REV --label PC --modem find
target\release\lfd3k-voice-pc.exe --name REV --label PC --modem find:SHOP
```

`find` looks for 5 seconds and takes the first modem that takes voice apps (its firmware has the TCP voice port), or the one with that station name.

Good to know:

- **Use a headset.** The app has no echo canceller, so speakers feed your own voice back into the call.
- **One app per modem session.** On USB one app has the modem; close the terminal program first (the port can only be open once). On Wi-Fi up to 3 apps can use one modem at a time.
- On Windows the app keeps the DTR and RTS lines low, so the board should not restart when the app opens the port (not yet checked on the bench). On Linux it leaves them as Linux sets them.
- `--bind`, `--peer`, `--snr` and `--station` are for the built-in modem only; with a real modem they are an error. The station name shown is the real modem's.
- The header shows the link, for example `USB COM11` or `TCP 192.168.4.1:4220`.
- If the cable comes out, Wi-Fi drops, or the modem restarts or goes quiet (nothing from it for 3 s, or 20 s if its radio is not working), the app shows **MODEM LOST** with the reason and goes back to the CALLS screen. Close it and start it again to reconnect.
- Closing the app ends its session cleanly; on USB the modem goes back to AT commands.

## Calling by name

1. Wait for ANN/PC to show under PEOPLE AND STATIONS NEARBY (a new name is announced within about 15 s; SCAN on the DIRECTORY screen asks right away). The list shows `CALL` (a phone call) or `PTT` (push to talk) for the best tier.
2. Press CALL on that row, or type the name under CALL BY NAME (`ANN` rings every device of ANN; add the label `PC` for one device).
3. The two words show once the scrambler is up, with `ANN/PC (NOT CHECKED)` the first time. Read them out; both screens show the same two. Press WORDS MATCH (remembered for next time) or TALK ANYWAY (this call only). Audio is held both ways until then. `NOT THE ANN YOU CHECKED` means a checked device now shows a different key, or a removed one: the call stays muted.
4. The line under the name shows the real tier, codec and mode, for example `SF7 BW500 21875   CODEC2 2400   CALL`. HANG UP ends it. ANSWERED ELSEWHERE, DECLINED, NO ANSWER and CALL ENDED show in the banner.
5. Someone calling you brings up the CALLS screen with INCOMING CALL, ANSWER and DECLINE, and the app rings.

## The signal scope and the connection sounds

The band at the bottom of every screen is the modem's own view of the radio (no SDR): MODEM SIGNAL with the modem's state and the newest RSSI, and a scope of the last 10 s. The grey line is the channel level the modem reads every 50 ms (on this PC's built-in modem: -110 dBm when nothing is heard, the level of a frame while it is being received); blue points are samples taken while our modem was sending. Every frame the modem hears shows as a grey span with its tier (SF5 to SF12), every frame it sends as a blue span. NO MODEM means no samples are coming from the modem.

While a call is being set up (dialing, ringing, training, a retrain) and when it ends (NO CARRIER), the app plays the frames as LoRa-like chirps, made live from what the modem sends and hears: slow sweeps for SF12 frames, a fast flutter for SF5, as long as each frame is on the air. Frames heard sound a little brighter and softer than frames sent. It goes quiet once the call is connected (only a retrain's own frames sound then, softly, under the call), and stays quiet while idle (beacons and scans make no sound). Turn it off or on with CONNECTION SOUNDS in Settings, About (remembered in `app-prefs.txt` in the data folder; on by default). A `--record` WAV carries the chirps of the call setup too.

## Slow tiers and push to talk

Start your copy with `--cap-tier SF9` (or `--snr -12`). From SF9 down the call is push to talk: hold the big TALK button, or the space bar, and let go to listen. `WAITING FOR THE FLOOR` means the other side holds it; you get it when they let go. At SF12 the line reads `PTT DELAYED`: the listener hears you about 2 s late. The `--auto-answer --play` copy takes the floor, plays the whole WAV, lets go for 8 s so you can answer, and starts again.

## Listening to the tiers without a call

`lfd3k-voicesim` runs the whole path in the simulator and writes one WAV per tier:

```powershell
cargo run --release -p lfd3k-sim --bin lfd3k-voicesim -- --tier all --in sim/testdata/speech-16k.wav --out target/voicesim
```

Play `target\voicesim\SF5.wav` to `SF12.wav` in any player. The table it prints gives the codec, mode, frames lost and the mouth-to-ear delay for each.

## Linking your own devices

Linking makes your second device count as checked for everyone who checked your first one.

1. On the device you set up first (it holds your person key): Settings, Voice and keys, LINK A DEVICE. A 6 digit code shows. It works once, for 5 minutes; CANCEL wipes it.
2. On the new device: type the code in the CODE box and press JOIN.
3. From the new device, call the first one by name. The link runs inside that call. When it is done both show LINKED, and the new device carries your name.

A wrong code fails with nothing learned (`link failed` after about a minute); start again with a new code.

## Settings

SETTINGS runs the whole modem, with the same AT lines the AT+MENU sends: tabs Radio (station name, S30 TX power with a warning above the safe default of 2, S21, S0, S31, S32, S20, S7, S22), Privacy (ON, QUIET, HIDDEN, pinned names), Wi-Fi (hotspot, client network, the live status; passwords never shown, and wiped from the app once sent), Forwards (8 slots, publish), Voice and keys (your name and label, linking, checked contacts, remove a lost device), About (ATI, AT&V, CONNECTION SOUNDS ON or OFF). The DIRECTORY screen dials, pins, deletes and scans.

A change is live at once. When the settings differ from the saved ones a bar shows SAVE (AT&W, kept after a restart: the built-in modem keeps its saved settings and directory in the data folder; it has no Wi-Fi, so the Wi-Fi settings do nothing here and no Wi-Fi password is ever written to disk) and DISCARD (ATZ, back to the saved ones; not during a call, as ATZ ends it).

Removed devices (REMOVE DEVICE on one of your own) are kept in `removed.json` in the data folder and told to each contact at your next call with them.

## Two PCs

Same as above with each app's `--bind` on its own LAN address and `--peer` the other PC's, for example `--bind 192.168.1.20:4210 --peer 192.168.1.30:4210`. Let UDP through the firewall on both.

## Bench recordings and screenshots

- `docs/voice-bench/timing.md`: the timing table of every tier (`lfd3k-voicesim`) and of the recorded test call.
- `docs/voice-bench/wav/`: `lfd3k-voicesim` recordings of every tier (SF5 to SF12).
- `docs/voice-bench/call/`: the recorded AXIS BBS test call between two copies of this app on SimNet, at SF5, SF7, SF9 and SF12 (stereo Opus, left what the caller heard, right what the answerer heard), how it was made, and the synthetic source conversation.
- Footage for the testing video (app windows, each modem's host side as a terminal, each modem's front panel, a timeline) is not kept in the repo (about 33 MB): `tools/voice_video.py` makes it from the runs of `tools/voice_call_bench/run_call.py` (each app's `--shots` and `--trace`).
- `docs/voice-bench/pc/`: the app's screens at 1280 x 720.
