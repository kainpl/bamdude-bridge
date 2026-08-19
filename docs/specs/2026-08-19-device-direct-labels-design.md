# Device-direct label printing, bridge side (phase 2) — Design

**Date:** 2026-08-19
**Status:** Approved (design), pending implementation plan
**Repo:** `kainpl/bamdude-bridge` — **not** this one. Recorded here because the contract it consumes is defined here and the two specs must be read together.
**Area:** `src-tauri/src/label/` (new), settings, a second tab in the window
**Builds on:** `2026-08-19-device-direct-labels-server-design.md` — the queue, the raster and the HTTP contract exist before any of this runs
**Vault:** `10-repos/bamdude-bridge/bamdude-bridge-overview` · `60-specs/niimbot-protocol`

## Goal

The bridge asks BamDude for label work, and prints what it is given on a Niimbot attached to the USB port.

This phase ends when a job queued in the Inventory page comes out of a real printer and its result is visible in BamDude. It renders nothing and decides nothing about content — it receives a 1-bit raster and a copy count.

## What the bridge already is

Seven Rust modules, working and verified on live BambuStudio since 2026-08-16: settings under `%APPDATA%` with a server address and an API key, `reqwest` on rustls, single-instance with deep-link argv dispatch, tray residency, Windows toasts, a log on disk, and registry work for the protocol handler.

Everything the label role needs from that already exists **except the loop**: today the app does work when a URL arrives and otherwise sits idle. This phase gives it a reason to be doing something while nothing is happening.

## Preflight findings

### There is no Niimbot crate

Neither on crates.io nor as a live GitHub project — the one `niimbot-rs` stopped in October 2024. The reference implementation is TypeScript (`MultiMote/niimbluelib`), with a community protocol wiki beside it. **So this is a port, and must be written as one**: module docs naming the upstream file each piece came from, because the rest of the model coverage will have to be brought over from the same place later.

### The port is small, and its size is measurable rather than guessed

The per-model divergence in niimbluelib is seven "print task" variants over roughly seventeen models, each 1–2 KB of TypeScript. The whole B1 flow is:

```
init:  setDensity → setLabelType → printStart (7-byte form)
page:  pageStart → setPageSize (6-byte form) → image rows → pageEnd
end:   poll status until the expected page count is reported
```

The `B1` variant also covers `D110_M`, `B21_C2B`, `M2_H`, `N1` and `D101`, so one ported variant is not one supported printer.

### The framing is uniform across models

`55 55`, command, length, data, XOR over the payload, `AA AA`. Only the *contents* of start-of-print and page-size differ by generation. That is what makes a driver split by "print task" rather than by model the right shape in Rust too.

### The row encoder is the fiddly part

Eight pixels to a byte, MSB first, width padded to a multiple of 8; runs of identical rows collapse into a repeat count; a bookkeeping row every 200; and rows with very few black pixels are sent as 16-bit big-endian indexes instead of a bitmap. This is where a port goes subtly wrong, and where the test vectors go.

### Serial and BLE both have mature crates

`serialport` for USB, `btleplug` (WinRT backend) for Bluetooth. Neither is exotic; both build on the Windows-only CI the repo already runs.

⚠️ B1 and B21 advertise **two** Bluetooth addresses and only the second one works, and a connection error does not reliably mean the print failed. That is BLE's problem and BLE is not in this phase.

## Design

### Component 1 — `label/protocol.rs`, the port

Packet framing, the command set this phase needs, and the `B1` print task behind a trait so the remaining six variants are additions rather than edits. Model and protocol version arrive from the printer's own reply and select the task, with the pair taking priority over the model alone — the same resolution order the reference uses, because one model on two firmwares can need two different flows.

### Component 2 — `label/encoder.rs`

Takes the PNG the server sent, decodes to 1-bit rows, and produces the wire rows. Kept apart from `protocol.rs` because it is pure, deterministic and the piece most worth testing exhaustively without a printer in the room.

### Component 3 — `label/serial.rs`

Open, write, read with a timeout. The port comes from settings; a scan helper lists candidates so the operator picks from a list rather than typing `COM3` and finding out later.

### Component 4 — `label/poller.rs`

A tokio task, started when the app starts and the label role is configured:

1. Read the printer's state — model, cassette RFID, paper, battery. Cheap, and it is what the server wants in the request body anyway.
2. `POST /label-devices/poll` with that state.
3. `200` → print it, then `POST …/jobs/{id}/result`. `204` → straight back to step 1. `409` → the subsystem is off server-side; back off hard, this is not an error to retry tightly.
4. Network failure → exponential backoff with a ceiling, and the tray tooltip says so.

The device state is read **before** each poll rather than on a separate schedule, so BamDude's view of the cassette can never be more stale than one poll.

⚠️ **A printer that is unplugged is not an error state to hide.** The poll still goes out, with `printer_reachable: false`. BamDude showing "bridge alive, printer gone" is worth more than silence, and it is the difference between "your USB cable" and "your server".

### Component 5 — settings and window

New settings: label role on/off, transport, port, and the `installation_id` — a UUID generated once on first use and never regenerated. It is what identifies the device row on the server; regenerating it would silently orphan the paired device and create a second one waiting for approval.

The window gains a second tab: transport and port, a **Test print**, and the current state as the bridge sees it. Test print is not a nicety — it separates "the printer works" from "the queue works", which are the two things anybody debugging this needs told apart.

### Component 6 — decouple autostart from protocol registration

Today autostart is installed alongside becoming the slicer's receiver. An operator who wants labels and not the BambuStudio integration would get a poller that only runs when they happen to open the app — which looks exactly like a broken feature.

Autostart becomes its own thing, switched on by either role and removed only when both are off.

⚠️ The elevation trap already documented for the bridge is unchanged and still applies: **run it as administrator and the whole product breaks silently**, because the slicer's unelevated process cannot message an elevated tray instance across the integrity boundary. The existing detect-and-warn banner covers this role too.

## Deliberately not in this phase

- **BLE.** Serial proven first. The two-address quirk and a sleeping radio are their own debugging session.
- **The other six print tasks.** Ported when there is hardware to verify them on. A variant written blind and shipped untested is worse than an honest "unsupported model".
- **Cassette size resolution.** The bridge reports the barcode; deciding what it means is the server's job and lives in its catalogue.
- **Rendering anything.** Ever, in this role. The moment the bridge owns a font it owns a layout, and the boundary in the vault note stops being true.
- **macOS.** The slicer role is Windows-only because Bambu never implemented the other side; the label role is not, and that is a later phase with its own testing.

## Testing

- **Framing:** round-trip encode/decode, checksum over known payloads.
- **Encoder:** fixed test images to expected row bytes — an all-white image, a single black pixel, a run of identical rows collapsing to a repeat, a sparse row taking the index encoding, and a width that is not a multiple of 8. These are the port's real regression suite and need no hardware.
- **Poller:** against a stub HTTP server — `204` loops, `200` prints and reports, `409` backs off, a transport error retries with backoff, and a result is posted exactly once per job.
- **No hardware in CI.** The repo's CI is Windows-only for a reason already learned there; it stays that way, and device verification is a manual checklist in `CONTRIBUTING.md`.

## Open questions

- Whether the poller should stop entirely when the tray window is closed. It should not — closing the window already means hide, not quit — but it is worth stating in the README, because "I closed it and labels stopped" is a support question waiting to happen.
- Whether a failed print should retry locally before reporting failure. Leaning no: the server already reclaims and re-queues, and two retry layers make the attempt count meaningless.
