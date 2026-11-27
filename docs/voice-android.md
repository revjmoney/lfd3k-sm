# Voice calls on Android: lfd3k-voice-android

**Your own encrypted radio (cell) phone network!! Call anyone with a modem. No carrier. No tower. No one listening.**

`lfd3k-voice-android` is the phone voice app of the LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM. It is the same calls as [`lfd3k-voice-pc`](voice-pc.md): call a person by name, push to talk on the slow tiers, the FLOWTRON SCRAMBLER and its two word check, the directory and scan, the settings tabs, and linking a second device. The call engine and the modem core are the Rust crates `voice/` and `core/`, linked in through a small C ABI (`apps/android/engine`). They are not rewritten in C++ or Java.

Ham mode does not exist yet, so the scrambler is always on. There is no echo canceller in this app. Use a headset for CALL mode. The streams are plain AAudio PCM. The voice communication usage and the voice input preset are Android 9 APIs, and this app's minimum is Android 8, so they are not requested.

## What was actually run

The engine crate's host tests were run on Linux x86_64 (`cargo test -p lfd3k-voice-android`): the `AT+VOICE` answer parser, the Wi-Fi read-only AT filter, the tier cap save order, and one simulated modem that answers `ATI`.

`./gradlew :app:assembleDebug` was run on the same machine and produced `app-debug.apk` (arm64-v8a, about 6 MB, `liblfd3kvoice.so` linked against the platform EGL, GLESv3, and AAudio libraries). That APK was not installed on a phone, a tablet, or an emulator. USB, Wi-Fi TCP, mDNS find, the microphone, and the speaker were not exercised on hardware. Nothing below that needs a real modem or a real phone should be read as a bench result.

## How it is built

The UI is a `NativeActivity`. C++ (NDK, CMake) draws the screens with Dear ImGui on OpenGL ES 3, opens audio with AAudio, and moves USB bulk bytes with the Linux `usbdevfs` ioctls. Rust owns the call.

A small Java class is still required, and only for things Android does not expose to native code:

- USB host permission is a system dialog. `UsbManager.requestPermission` and the broadcast that carries the answer are Java. The activity then opens the device, claims the interface, and hands the file descriptor to C++. Without that, the kernel will not let the app talk to the modem.
- mDNS (`find`) needs a `WifiManager.MulticastLock` or the phone drops the multicast replies. That lock is Java.
- `RECORD_AUDIO` is a runtime permission, so the activity asks for it.

There is no Kotlin. There is no AndroidX. The activity subclasses `android.app.NativeActivity` and does not draw.

Audio is AAudio (API 26 and newer), 16 kHz mono when the device allows it, otherwise resampled. OpenSL ES is not used. `minSdk` is 26 because that is the first release with AAudio.

The debug APK links a release build of the Rust engine. Opus and Codec2 are slow in a debug Rust build, same as the PC app. The APK itself is the Gradle debug variant (debuggable, debug signature).

Only `arm64-v8a` is built.

## Build

You need a JDK 17, the Android SDK platform 34, build-tools 34.0.0, NDK `26.3.11579264`, CMake 3.22.1, Rust stable, and `cargo-ndk` 4.1.2. The voice crates also need CMake on `PATH` for libopus (see the README). Point `ANDROID_HOME` at the SDK, or put `sdk.dir` in `apps/android/local.properties` (that file is not committed).

```text
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
cd apps/android
chmod +x gradlew scripts/cmake-android.sh
./gradlew :app:assembleDebug --no-daemon
```

The APK is `apps/android/app/build/outputs/apk/debug/app-debug.apk`.

```text
adb install -r apps/android/app/build/outputs/apk/debug/app-debug.apk
```

CI does this on every pull request and uploads the APK as the artifact `lfd3k-voice-android-debug`.

`cargo build --workspace` on the host also builds this crate as a normal library, so a mistake in the C ABI fails the existing Rust job. The Android target is only the APK job.

libopus is cross compiled by a small `cmake` wrapper (`apps/android/scripts/cmake-android.sh`). The NDK toolchain file otherwise defaults the ABI to `armeabi-v7a`, which will not link into an arm64 static library.

## Install and the first screen

The app is `LFD3K Voice`, package `com.jmscnc.lfd3k.voice`. It stays in portrait. The first screen asks which modem to use, then CONNECT.

| Choice | What it does |
|---|---|
| SIMNET | The built-in modem on SimNet over UDP, the same path as the PC app. Set this phone's bind address and the other end's peer. SNR is what the other modem hears. |
| TCP | A real modem on Wi-Fi. Host, and port 4220 unless you change it. Example host `192.168.4.1` (the modem's own hotspot). Settings are locked, same as the PC app on `tcp:`. |
| FIND | Browses for five seconds for `_lfd3k._tcp` with a `voice` port. Optional station name. The phone has to be on that Wi-Fi. The multicast lock is held while this runs. |
| USB | Android USB host. Allow the system dialog. The V4 is opened as CDC-ACM (that is the port Linux calls `ttyACM`). The V3 is a CP210x bridge, not CDC-ACM, and is opened with that chip's line setup at 115200 8N1. DTR and RTS are held low so a toggle does not reset the board. |

The first launch makes this install's keys in the app's private files (`voice/identity.bin`, plus `trust.json`, `removed.json`, `app-prefs.txt`, and, for SimNet, `modem-settings.bin` and `modem-dir.bin`). A name is 1 to 16 printable characters without `:`. A label is 1 to 8 without `:` or `/`. Defaults are REV and PHONE. Change them later under Settings, Voice.

Launch extras, for a scripted start, are `modem` (`sim`, `tcp`, `find`, `usb`), `name`, `label`, `bind`, `peer`, `host`, `port`, `station`, `find`, `snr`, and `autoconnect` (`1` connects immediately). They are not a substitute for a bench run.

A tier cap (spreading factor 5 to 12, or 0 for none) matches the PC app's cap: on a real modem it is put back when you leave, and SAVE keeps the old S21. On SimNet it is this run's S21.

"answer incoming calls" picks up and talks anyway. It does not play a WAV. The PC app's script, screenshot, trace, and WAV play and record helpers are not in this app.

## Using it

CALLS lists people nearby (CALL or PTT for the best tier), CALL BY NAME, an incoming call (ANSWER, DECLINE), and the live call: the two words, WORDS MATCH, TALK ANYWAY, mute, hang up, and the TALK button on a push to talk tier (hold it). `WAITING FOR THE FLOOR` means the other side has it.

DIRECTORY scans, pins, unpins, deletes a station, and clears unpinned names. Over Wi-Fi only call and scan stay available.

SETTINGS has the same tabs as the PC app: Radio, Privacy, Wi-Fi, Forwards, Voice and keys, About. A change is one AT line and is live at once. When the settings differ from the saved ones, SAVE (AT&W) and DISCARD (ATZ, not during a call) show. Passwords are not shown again after they are sent. The SimNet modem has no Wi-Fi, so those lines do nothing there and no Wi-Fi password is written to disk.

Voice and keys shows a link code (one use, five minutes) and JOIN. After JOIN, call the first device by name. The link runs inside that call. Both sides then show LINKED.

About shows ATI, AT&V, connection sounds, the data folder, and the attribution required by `NOTICE.md`: **Based on LFD3K-SM by Rev. J. Money / jmscnc.com**. The same line is on every screen. Connection sounds are the chirps while a call is set up, on unless `app-prefs.txt` says off.

The band under the screens is the modem's own signal (the last samples, transmit in the accent color). NO MODEM SIGNAL means no samples are arriving.

If the cable comes out, Wi-Fi drops, or the modem goes quiet (3 s after a scope sample, otherwise 20 s), the app shows MODEM LOST and BACK returns to the connect screen.

The on-screen keyboard is there because a `NativeActivity` does not get Unicode from the system keyboard in a way this UI can trust. Hardware keys still go to the text fields.

## What is not in this build

- No run on a device or an emulator, so USB, TCP, find, and audio are untested on hardware.
- No echo canceller of our own.
- No PC test helpers (script, screenshots, trace, WAV mic, WAV record, the auto-answer WAV push to talk cycle).
- One ABI, arm64-v8a, portrait only.
- The screens are a phone layout of the same controls, not a copy of the 1280 x 720 PC window, and they do not draw the Simian logo.
- If a V4 enumerates as something other than CDC-ACM or a vendor interface with bulk endpoints, USB open fails with the error on the connect screen. That path was not tried on a board.
