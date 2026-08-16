# BamDude Bridge

A small desktop app that connects a Windows workstation to a [BamDude](https://github.com/kainpl/bamdude) server.

BamDude runs in a container, usually on another machine. That means there are things it structurally cannot do: it cannot read a file your slicer just wrote to your local disk, and it cannot talk to a label printer plugged into your USB port. This app does those things and hands the result to the server over its normal HTTP API.

It is a bridge, not a second product. It holds no database and has no opinion about what your library contains.

## What it does

### Accepts sliced plates from BambuStudio

BambuStudio can hand a sliced plate to a farm-management app. On Windows it decides whether one is installed by looking for a registry key, then launches a custom URL carrying the path to a temporary 3MF it just exported. Bridge registers itself as the receiver of that URL, reads the file, and uploads it into your BamDude library.

⚠️ **This means taking over a URL scheme that belongs to Bambu Lab's own farm client.** A system has one handler per scheme, so if you have Bambu's client installed, only one of the two can receive those files. Bridge will tell you when it finds an existing registration and will not take it over silently — registering is a deliberate action you take from the settings window, never something the installer does behind your back.

### Reaches hardware the server cannot

Planned: Niimbot label printers over USB/Bluetooth. The server renders labels today and sends them to a system print driver through your browser; a directly-attached printer is a different path that needs a process on the machine the printer is plugged into.

## Status

Early scaffold. Not usable yet, not released, not signed.

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
above generates every size the bundler asks for from one source PNG.

## Licence

AGPL-3.0, matching BamDude. See [LICENSE](LICENSE).
