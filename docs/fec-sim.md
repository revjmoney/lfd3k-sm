# Burst parity, simulated

These numbers are from the seeded simulator (`sim/tests/fec.rs`), not from a radio. The channel model drops a frame from its SNR margin (`m` in dB times 4: 0.5 percent at `m >= 8`, 10 percent at `0 <= m < 8`, 60 percent below that down to -8, and every frame below that). The loss rate tests add a second, independent drop of Data frames (chunks and parity frames) at the rate named below. Setup frames are not dropped by that hook. Throughput is delivered host bytes divided by simulated milliseconds, one direction, after connect.

The codec is a one parity symbol Reed-Solomon code over GF(256). In characteristic 2 the parity byte is the XOR of the data bytes. One missing chunk in a group is rebuilt. Two missing chunks are resent by ARQ.

## Same tier (SF5, 12 dB, 8192 incompressible bytes)

Both modems stay on SF5. FEC is S31 = 1, so the capability byte offers LZ, stream LZ, CAP_BULK and CAP_FEC together. ARQ is S31 = 0, which sends no capability byte, so the window stays 8 and the burst stays 4. The bytes are incompressible, so stream LZ only adds its check byte and literal headers. The FEC side can also use the bulk burst of 8 and the window of 16.

Hook drop 0 percent. FEC 1223 ms, 0 resends, 0 parity frames, 0 chunks lost, 42 data frames. ARQ 1218 ms, 0 resends, 0 chunks lost. Parity stayed off. The few milliseconds are the bulk window, not parity.

Hook drop 5 percent. FEC: 5927 B/s, 1382 ms, 1 resend, 4 parity frames, 2 chunks lost. ARQ: 6210 B/s, 1319 ms, 2 resends, 2 chunks lost.

Hook drop 10 percent. FEC: 5475 B/s, 1496 ms, 2 resends, 5 parity frames, 3 chunks lost. ARQ: 5516 B/s, 1485 ms, 4 resends, 4 chunks lost.

Hook drop 20 percent. FEC: 3615 B/s, 2266 ms, 3 resends, 7 parity frames, 7 chunks lost. ARQ: 4804 B/s, 1705 ms, 8 resends, 7 chunks lost.

At a fixed fast tier the parity frames cost airtime, so 5, 10, and 20 percent are a bit slower than ARQ alone. Resends fall (4 to 2 at 10 percent, 8 to 3 at 20 percent).

A clean run sends no parity frame.

## Marginal SNR (the step down parity is for)

Connect at 12 dB on SF5, then the SNR steps to -1 dB at 3.5 s. That is under SF5's step down line. In this model the margin is about 1.5 dB, in the 10 percent loss band. 65536 incompressible bytes, one direction.

ARQ only (S31 = 0): ends on SF6, 15851 ms, 6 resends, 0 parity frames. The step is at 4238 ms.

Parity agreed (also CAP_BULK and stream LZ): stays on SF5 the whole way, 13459 ms, 19 resends, 37 parity frames.

SF6's air rate is 37500 bps against SF5's 62500, about 40 percent less. The ARQ only call steps down to SF6, where the loss band is about 0.5 percent, so it resends little and is slow because of the tier. The parity call stays on SF5, with a bulk burst of eight (seven chunks and one parity frame) and a window of 16. It resends more and still finishes sooner: 65536 bytes in 13.5 s is about 4870 B/s, against about 4130 B/s after the step down.

## Bulk burst and parity together

Simulated, SF5, 12 dB, no SNR loss draw, a 10 percent hook drop of Data frames, 8192 incompressible bytes, S31 = 1 so CAP_BULK, CAP_FEC, and stream LZ are all on. The call accepts a window of 16 (the first feed takes more than 8 chunks). The longest back to back Data run is 8 frames, which is the bulk burst with the parity frame as the eighth, not a ninth. 5 parity frames, 1 resend, and the bytes arrive intact. A classic burst with parity would have been 5 frames (four chunks and one extra).

A second run uses a repeated menu line (12298 bytes) so stream LZ is doing real work, and drops every 5th data chunk. The air load of the first sends is 2251 bytes. One parity frame is sent, the burst is still 8, the ack carries two SACK bytes, and the rebuilt compressed bytes decode back to the same line. Normal data frames do not carry the per frame LZ flag.

RETRAIN is held off the data stream until the host escapes, so the test reads the tier the modem is actually on.

## Cost on the ESP32-S3

The Heltec V3 and V4 use an ESP32-S3 (240 MHz, 512 KB SRAM). `Group` in `core/src/fec.rs` is the only extra buffer: seven sequence numbers, seven lengths, and one 200 byte column. It is 217 bytes, inside `Link`, with no heap and no second copy of the chunks. The column holds up to seven chunks so a bulk burst of eight can end with the parity frame. The receiver reads the siblings back out of the ARQ window. Building or applying one parity frame is at most 4 times 200 byte XORs, a few microseconds, under one SF5 symbol (64 us). There is no lookup table.

While parity is on, the resend timeout grows by one chunk airtime so the extra frame in the burst is not treated as a loss. After 16 new chunks with no new hole, and while the SNR is not under the step down line, parity turns off.
