# BamDude Bridge

A small desktop app that connects a Windows workstation to a [BamDude](https://github.com/kainpl/bamdude) server.

BamDude runs in a container, usually on another machine. That means there are things it structurally cannot do: it cannot read a file your slicer just wrote to your local disk, and it cannot talk to a label printer plugged into your USB port. This app does those things and hands the result to the server over its normal HTTP API.

It is a bridge, not a second product. It holds no database and has no opinion about what your library contains.

## What it does

### Accepts sliced plates from BambuStudio

BambuStudio can hand a sliced plate to a farm-management app. On Windows it decides whether one is installed by looking for a registry key, then launches a custom URL carrying the path to a temporary 3MF it just exported. Bridge registers itself as the receiver of that URL, reads the file, and uploads it into your BamDude library.

⚠️ **This means taking over a URL scheme that belongs to Bambu Lab's own farm client.** A system has one handler per scheme, so if you have Bambu's client installed, only one of the two can receive those files. Bridge will tell you when it finds an existing registration and will not take it over silently — registering is a deliberate action you take from the settings window, never something the installer does behind your back.

### Reaches hardware the server cannot

Niimbot label printers over USB. Switch the role on in the settings window, pick the port, and Bridge will read the printer and print a test label. It is **off unless you turn it on** — a Bridge installed only to catch plates from the slicer never opens a serial port, and someone without a label printer is never asked about one.

What it shows you: which model it is and whether that model is supported, firmware and serial, whether paper is loaded, and what the cassette tag says — barcode, consumable type, how much is used.

⚠️ **The NIIMBOT desktop app holds the port exclusively.** While it is running nothing else can open the printer, and the error is a bare "Access is denied". Close it first.

⚠️ **The cassette does not report its size in millimetres.** No Niimbot tag does. Bridge shows you the barcode; turning that into a size is BamDude's job, and it asks you once for a cassette it has not seen. There is deliberately no size field here — a size that can be set in two places becomes two sizes.

Taking jobs from BamDude's queue automatically is not wired up yet; today the role gives you the device, its state and a test print.

## How it behaves

It lives in the system tray. **Closing the window hides it rather than quitting** — left-click the tray icon to bring it back, right-click for Open and Quit. Quit from that menu is the only real exit.

That is deliberate rather than merely tidy: staying resident is what lets a plate sent from the slicer reach the running instance instead of paying a cold start every time. Registering as the receiver also adds a per-user autostart entry, so the same is true after a reboot; unregistering removes it again.

**A handover does not raise the window.** The slicer is in front of you and jumping over it would be rude. A success pops a notification instead; only a failure takes the window, because a failure nobody sees is the same as no upload at all.

The tray tooltip carries the last outcome regardless — a notification can be suppressed by Focus Assist, or refused on a machine where this app has no Start Menu identity, and the tooltip cannot. Everything also goes to a log beside the settings, including whether the notification was accepted, so "no pop-up appeared" is never confused with "nothing happened".

## Downloads

Two builds, both doing the same job — registration is a button inside the app, never installer work, so neither is the lesser one.

- **Installer** (`…-setup.exe`) — Start Menu entry, uninstaller, and it fetches the WebView2 runtime if the machine lacks it.
- **Portable** (`…-portable.zip`) — unpack anywhere and run. "Portable" means no installer and nothing written next to the executable; settings and logs still live under `%APPDATA%` / `%LOCALAPPDATA%`, and registering still touches the registry. The archive's README says so plainly.

⚠️ **Neither is signed**, so SmartScreen warns on first run: **More info → Run anyway**.

## Status

Early. Working end to end on Windows, not yet released, not signed.

## Requirements

- Windows 10/11 (the BambuStudio integration is Windows-only — Bambu never implemented it for macOS)
- A reachable BamDude server and an API key with library-upload scope

## Building

Prerequisites: [Rust](https://rustup.rs), MSVC build tools, Node.js 22+, and WebView2 (preinstalled on Windows 11).

```bash
npm install
npm run typecheck        # frontend only, no toolchain needed
npm run tauri icon src-tauri/icon-source.png   # once, generates src-tauri/icons/
npm run tauri dev        # run against a dev server
npm run tauri build      # produce an installer
```

`tauri build` will not start until `src-tauri/icons/` exists — the icon step
above generates every size the bundler asks for from one source PNG. (They are
committed, so a fresh clone does not need it; run it only after changing the
source image.)

### Where the output lands

Build output goes to `target/` at the repository root, not under `src-tauri/` —
see `.cargo/config.toml`.

| | |
|---|---|
| Debug binary (`tauri dev`) | `target/debug/bamdude-bridge.exe` |
| Release binary | `target/release/bamdude-bridge.exe` |
| Installer | `target/release/bundle/nsis/BamDude Bridge_<version>_x64-setup.exe` |

⚠️ **Registering the protocol handler writes the path of the binary that did
it.** Register from a dev build and the registry points into `target/debug/`;
a later `cargo clean` or a move to an installed copy leaves the scheme pointing
at nothing, and files sent from the slicer quietly fail to arrive. Register
again from whichever build you actually use.

## Contributing

Branching, release channels and what to test before tagging: [CONTRIBUTING.md](CONTRIBUTING.md). Same rules as BamDude — work in `feature/*`, merge to `dev`, fast-forward `main` for a stable release.

## Licence

AGPL-3.0, matching BamDude. See [LICENSE](LICENSE).
