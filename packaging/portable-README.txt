BamDude Bridge — portable
=========================

Unpack anywhere and run bamdude-bridge.exe. There is no installer and
nothing is registered until you ask for it.

FIRST RUN
---------
1. Enter your BamDude server address and an API key.
   The key needs the library-manage scope, and nothing else.
2. Press "Test connection". It checks the address, the key, AND that the
   key is actually allowed to add files to your library — so a green
   answer here means the first plate will land.
3. Press "Register Bridge as the receiver".
   This asks for administrator rights once, to write the single registry
   key BambuStudio looks for. It also adds a per-user autostart entry so
   the app is already running the next time you sign in.
4. Restart BambuStudio. It reads that key only when it starts, so the
   menu entry will not appear in an already-open slicer.

Then: slice a plate, and pick "Send to Bambu Farm Manager Client" from
the print button's dropdown.


IF YOU MOVE THIS FOLDER
-----------------------
Re-run the app and press Register again.

Registration stores the full path of the executable that performed it.
After a move, Windows would be pointing at a file that is no longer
there, and plates sent from the slicer would quietly fail to arrive. The
app notices and stops claiming it is registered, so the fix is one click
— but nothing warns you until you look.


WHAT "PORTABLE" DOES AND DOES NOT MEAN
--------------------------------------
It means no installer, and no files written next to the executable.

It does NOT mean no traces. Your settings live in
  %APPDATA%\top.bamdude.bridge\settings.json
and the log in
  %LOCALAPPDATA%\top.bamdude.bridge\logs\bridge.log

Registering additionally writes:
  HKCU\Software\Classes\bambu-farm-client            (the receiver)
  HKCU\...\CurrentVersion\Run                        (autostart)
  HKLM\SOFTWARE\Bambulab\Bambu Farm Manager Client   (needs admin)

"Stop receiving" in the app removes the first two. The third is the flag
that makes BambuStudio offer the button at all; it is left in place
because Bambu's own client uses the same key.


REQUIREMENTS
------------
Windows 10 or 11, and the Microsoft Edge WebView2 runtime — which ships
with Windows 11 and with any recent Windows 10. The installer would fetch
it if it were missing; this archive cannot, so on a machine without it
the window comes up blank. Installing "Microsoft Edge WebView2 Runtime"
from Microsoft fixes that.

The build is unsigned, so SmartScreen will warn on first run:
More info -> Run anyway.


Source, issues: https://github.com/kainpl/bamdude-bridge
Licence: AGPL-3.0-or-later
