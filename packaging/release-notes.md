version: 0.2.3

This security update replaces the ZIP library used by both Windows update paths. Update your installed or portable copy of BamDude Bridge to 0.2.3.

## Security

- The installer updater and the portable updater now use ZIP 9.8.2, which includes the fix for [GHSA-m665-v7pm-7grj](https://github.com/advisories/GHSA-m665-v7pm-7grj).
- Updated rustls to 0.23.46 and the frontend dependencies Vite to 6.4.5 and source-map-js to 1.2.2.

## Other fixes

- The setup hint now uses the current `bd_` API-key prefix. Existing `bb_` keys still work.
- The uninstaller explains what choosing to delete application data removes.
