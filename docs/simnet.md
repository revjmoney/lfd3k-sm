# SimNet: call the V4 from the PC over Wi-Fi

You have one V4 and no second LoRa radio. SimNet lets the PC be the far
modem. The PC runs the same modem core as the V4 (`lfd3k-pcmodem`), and the
two talk over Wi-Fi (UDP port 4210) instead of over the air. Everything else
is real: dial, RING, training, CONNECT, data, retrain, `+++`, hang up, and
the `AT+PUB` bridge to an AXIS BBS. Only the radio itself is simulated, and
the simulation keeps LoRa timing (each frame takes its real airtime).

While SimNet is on, the V4 sends nothing on RF (the SX1262 sits in standby),
AT&V shows `RADIO SIMNET <ip>`, and the OLED labels the HS light `SI`.
SimNet is never saved: every boot starts on the real radio.

## What you need

- The V4 on your home Wi-Fi (client mode).
- The PC on the same network.
- The repo at `F:\lfd3k-sm` with Rust installed (the same setup as the demo).

The first time the PC tool opens UDP port 4210, Windows may ask whether to
allow it through the firewall. Allow it, or the V4's datagrams never reach
the PC. Nothing here changes firewall settings for you. Two things to know:

- The allow applies to one network profile. If your home network is set to
  Public in Windows, allowing "private networks" is not enough: set the
  network to Private, or allow `lfd3k-pcmodem.exe` for the Public profile.
- The debug build (`cargo run`) and a release build (`cargo run --release`)
  are two different programs to Windows, so each asks once.

If the V4 shows nothing when you dial, check this first.

## Plain call: PC dials the V4

1. In your terminal on the V4, join your Wi-Fi (you type your own password):

   `AT+WSTA=<your ssid>,<your password>`

2. Read the V4's IP address from the status line:

   `AT+WIFI?`

   The `STA` part shows the address, for example `192.168.1.57`.

3. Find the PC's IP address (in PowerShell: `ipconfig`, the IPv4 address of
   the Wi-Fi or Ethernet adapter), for example `192.168.1.20`.

4. On the V4, switch to the simulated radio, using the PC's address:

   `AT+SIMNET=192.168.1.20`

   It answers OK. `AT&V` now shows `RADIO SIMNET 192.168.1.20`.

5. On the PC, in PowerShell in `F:\lfd3k-sm`, start the PC modem with the
   V4's address:

   `cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer 192.168.1.57`

6. Type in the PC window:

   `ATDT AXISMODEM-16FC`

   You see `DIALING`, the V4 shows `RING`, and if the V4 answers (type `ATA`
   on the V4, or set `ATS0=1` first) both sides train and print `CONNECT`.
   Lines you type on one side now show on the other.

7. To hang up from the PC: wait a second, type `+++` alone on a line, wait for
   `OK`, then type `ATH`. Both sides print `NO CARRIER`.

8. Back to the real radio on the V4: `AT+SIMNET=0`.

The PC window works a line at a time: each line is sent when you press
Enter. A line that is only `+++` is sent without Enter, so it works as the
escape. The modem echoes your commands, so you may see a command twice.
Ctrl+C closes the PC modem.

## Retrain test

Start the PC modem with `--profile dip`:

`cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer 192.168.1.57 --profile dip`

From 15 s to 35 s into each call, the signal the V4 hears from the PC drops
by 12 dB, then comes back. The link steps down and back up (RETRAIN, shown
after `+++` and in `AT&V`). `--snr 3` sets a fixed signal level instead
(the default is 8 dB).

## AXIS variant: the PC calls your BBS through the V4

1. Steps 1 to 4 above.
2. On the V4, add the forward to your AXIS board, publish it, and turn on
   auto answer:

   `AT+FWD=AXIS,<axis ip>,23`

   `AT+PUB=AXIS`

   `ATS0=1`

3. On the PC: `cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer 192.168.1.57`
4. Type `ATDT AXISMODEM-16FC` on the PC. The V4 answers by itself, the PC
   prints `CONNECT`, and the V4 connects the call to AXIS (its terminal shows
   `BRIDGE AXIS`). The AXIS login screen appears in the PC window.
5. Log off, or `+++` and `ATH`, to end the call.

## PC modem options

| Option | Meaning |
|--------|---------|
| `--peer <ip>` | The V4's address (required) |
| `--port <n>` | The peer's UDP port (default 4210) |
| `--bind <addr:port>` | The PC's own UDP socket (default `0.0.0.0:4210`). When calling the V4, keep port 4210: the V4 always sends to port 4210 of the PC. Other ports are only for two PC modems on one PC |
| `--name <NAME>` | The PC modem's station name (default `PCMODEM`) |
| `--snr <dB>` | What the V4 hears from the PC (default 8) |
| `--profile dip` | 12 dB dip from 15 s to 35 s into each call |
| `--fwd NAME=host:port[,T\|R]` | A forward set at start, so `ATDT NAME` calls a TCP host from the PC itself (repeatable) |
| `--script <file>` | Unattended run: `send`, `sendraw`, `expect <text> <seconds>`, `sleep <ms>` lines, timestamped log, exit 0 or 1 |
| `--section <name>` | Run only that `[name]` part of the script |

`tools/simnet-loopback.txt` is a script for two PC modems on one PC
(127.0.0.1, ports 4211 and 4212); the commands to run it are at its top.
