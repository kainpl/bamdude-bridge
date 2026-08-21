version: 0.2.0

BamDude Bridge learns its second job: printing labels on a printer plugged into this computer.

## Label printing

BamDude draws a spool label and puts it in a queue; this app comes and takes it. Nothing connects *to* your desktop, so it works on a laptop, behind NAT, or through a firewall that would never allow the reverse.

Turn the role on under **Label printer**, pick the serial port, and the machine appears in BamDude under **Settings → Filament → Marking** — listed, but switched off. It stays that way until somebody enables it there: signing in proves the app is yours, not that this particular printer should be given your labels. The id you match it by is shown in the app, with a copy button.

Currently the Niimbot B1 over USB. The app names the model it finds and says plainly when it meets one it cannot drive yet.

⚠️ Close the NIIMBOT desktop app first — it holds the serial port exclusively, and nothing else can open it while it is running.

## Updates

The app now updates itself. It looks shortly after starting and every few hours after that, shows what changed, and installs on a button.

Both builds are covered, and they work differently on purpose. The **installer** build runs the new installer with a progress bar and comes back. The **portable** build replaces its own executable in place and restarts — it never turns itself into an installed copy, and it refuses rather than half-applying if the folder it sits in is not writable.

⚠️ **This part only helps from here on.** Version 0.1.0 has no updater, so if that is what you are running, this one has to be downloaded by hand. Everything after it updates itself.

## Smaller things

- The tray icon is legible now. It was the app icon — a plate with the mark taking 41% of its width — which beside other tray icons read as a small badge rather than a peer.
- The window remembers the size you left it at. It no longer offers a maximise button, which a settings window has no use for; resizing still works.
- **Updates** has its own tab instead of sitting under whichever one you happened to be on.
