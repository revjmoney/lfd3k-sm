# V223.LRA: the LFD3K-SM protocol standards

These are the formal specifications of the protocols spoken by the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM (LFD3K-SM™): the LoRa® air protocol between modems and the voice host protocol between a voice app and its modem. They are written so that anyone can build an interoperable, independent implementation from the documents alone.

**These are AXIS / LFD3K-SM specifications published by Rev. J. Money. They are not ITU-T Recommendations** and are not issued or endorsed by the ITU, ANSI, NIST or any standards body; the ITU-T style is only a format.

LoRa® is a registered trademark of Semtech Corporation. V223.LRA™, LFD3K-SM™ and FLOWTRON SCRAMBLER™ are trademarks of Rev. J. Money.

## Index

| Designation | Title | File | Revision | Status |
|---|---|---|---|---|
| V223.LRA | The LFD3K-SM protocol stack (umbrella: architecture, layers, conformance classes, capability signalling, interoperability, registries, patent promise, naming rule, regulatory notes) | [V223.LRA.md](V223.LRA.md) | 1.0 (draft D1) | Draft D1, not yet published |
| V223.LRA.LINK | Data link: physical layer, tiers SF12 to SF5, frame format, call setup and training, selective repeat ARQ, keepalive, retrain, resync, hang up, discovery and privacy modes | [V223.LRA.LINK.md](V223.LRA.LINK.md) | 1.0 (draft D1) | Draft D1, not yet published |
| V223.LRA.CMPRS | Compression on the air: negotiation, LZ bitstream, preset dictionary, the 8 byte rule | [V223.LRA.CMPRS.md](V223.LRA.CMPRS.md) | 1.0 (draft D1) | Draft D1, not yet published |
| V223.LRA.90 | Voice: type 15, voice names and discovery, voice call setup, talk plans and codecs, voice scheduler, voice host protocol (VHP) | [V223.LRA.90.md](V223.LRA.90.md) | 1.0 (draft D1) | Draft D1, not yet published; may change (voice-1 branch) |
| V223.LRA.92 | The FLOWTRON SCRAMBLER: keys and certs, commit then reveal handshake, HKDF, sealed frames, the two words, trust rules, device linking | [V223.LRA.92.md](V223.LRA.92.md) | 1.0 (draft D1) | Draft D1, not yet published; may change (voice-1 branch) |
| V223.LRA.973 | super secret mystery mode | (none) | | Reserved |

**Release mapping:** LFD3K-SM release **r.420 implements V223.LRA Revision 1.0**. All parts will be published together as Revision 1.0.

## Conformance classes (summary)

- **Class D** (data): a station implementing V223.LRA.LINK (and V223.LRA.CMPRS if it offers compression).
- **Class V** (voice): Class D plus the air and modem side of V223.LRA.90; or, for an app, the app side of the VHP.
- **Class S** (scrambler): a voice app implementing V223.LRA.92.

See V223.LRA clause 9.

## How to cite

Cite the designation, the title, the revision and the date, for example:

> V223.LRA.LINK, *Data link of LFD3K-SM*, Revision 1.0, Rev. J. Money, 2026.

Clauses are cited as "V223.LRA.LINK clause 8.5" or "V223.LRA part LINK, clause 8.5". Until publication, cite the draft: "Revision 1.0 (draft D1)".

## License, patents and the name

- **License:** each document is Copyright 2026 Rev. J. Money (jmscnc.com) and licensed under the Creative Commons Attribution 4.0 International License (CC BY 4.0), https://creativecommons.org/licenses/by/4.0/. Anyone may implement the specification, in open or closed products, without royalty.
- **Patents:** Rev. J. Money will not assert any patent he holds or may hold against anyone for implementing this specification (V223.LRA and its parts).
- **The name:** anyone may implement the specification freely; only implementations that pass the published V223.LRA conformance tests (the test vectors and conformance clauses of each part; a test suite will be published with the reference implementation) may describe themselves as "V223.LRA compatible" or use the V223.LRA name for their product.
- **Reference code:** the LFD3K-SM reference implementation is licensed separately under the GNU GPL version 3 or later with the attribution term in its NOTICE.md.

## Revisions

| Date | V223.LRA | LINK | CMPRS | 90 | 92 | Notes |
|---|---|---|---|---|---|---|
| 2026-11-12 | 1.0 D1 | 1.0 D1 | 1.0 D1 | 1.0 D1 | 1.0 D1 | First drafts, written from LFD3K-SM r.420 and its voice-1 branch |

Report errors and ask for registry numbers through the editor, Rev. J. Money (jmscnc.com).
