# Contributing

Same rules as [BamDude](https://github.com/kainpl/bamdude), deliberately — one set of habits across the repos beats a second set here.

## Branches

| Branch | What it is |
|---|---|
| `main` | Production. Only ever fast-forwarded from `dev`; nothing is written here directly. |
| `dev` | Where work lands. CI runs on every push and PR. |
| `feature/*`, `fix/*`, `docs/*`, `refactor/*`, `test/*` | Short-lived, branched from `dev`, merged back into `dev`. |

**PRs target `dev`, never `main`.**

Commits follow [Conventional Commits](https://www.conventionalcommits.org/) — `feat`, `fix`, `docs`, `test`, `refactor`, `chore`.

## Releases

| Channel | Tag | Branch |
|---|---|---|
| Stable | `vX.Y.Z` | `main` |
| Beta | `vX.Y.ZbN` | `dev` |

The release workflow reads the tag shape: `vX.Y.ZbN` is published as a **pre-release**, anything else as a full release. It also **refuses to build a stable tag that is not on `main`**, or a beta that is not on `dev` — the rule is enforced rather than remembered.

### Cutting a release

1. Bump the version in **`src-tauri/tauri.conf.json`** — that file is what CI compares the tag against — and keep `src-tauri/Cargo.toml` and `package.json` in step with it.
2. Land it on `dev` and **wait for CI there to be green**:
   ```bash
   git push origin dev
   gh run watch --exit-status
   ```
3. For a beta, tag `dev` and stop here.
4. For a stable release, fast-forward `main` and tag that:
   ```bash
   git checkout main && git merge --ff-only dev && git push origin main
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```

⚠️ **The gate is the run on `dev`, before the fast-forward.** Main receives a commit that has already been proved; checking only after it lands there means the fix for anything red has to be written on top of production.

⚠️ **Tags are immutable — never force-push one.** A tag that shipped the wrong thing is retired by publishing `X.Y.Z.1` or the next beta, not by moving it. Something has already downloaded the old artifact.

### Naming

Tag `v<version>`; release title `BamDude Bridge v<version>`, plus ` (pre-release)` for a beta. No subtitles — the body says what is in it.

## Testing

Everything below runs on Windows. The registry module is `#[cfg(windows)]` and the whole point of the app is a Windows integration, so a green run anywhere else would be green on code that never compiled.

```bash
npm run typecheck                              # frontend; never a bare `tsc`
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --lib
```

CI runs exactly those four, and the release workflow runs them again — a tag push does not trigger `ci.yml`, which fires on branches.

### The part CI cannot do

⚠️ **A handover must be tested through Windows' own URL routing, not by launching the executable with the URL as an argument.** The two are not equivalent, and the difference is not academic: `ShellExecute` normalises a URL that has no path component by appending `/`, which lands glued to the end of the last parameter. Every probe that ran the binary directly passed while the real thing failed — that is how the first release shipped a handover that could not work.

So, after `npm run tauri build`, and with the app registered as the receiver:

```powershell
# Note: no path to the exe. Windows resolves the handler from the registry,
# which is the entire point.
Start-Process 'bambu-farm-client://upload-file%3Fversion%3Dv1.6.0%26path%3DC%3A%2Fpath%2Fto%2Fany.3mf%26name%3DProbe_plate_1'
Get-Content "$env:LOCALAPPDATA\top.bamdude.bridge\logs\bridge.log" -Tail 10
```

The log should end with `server answered 200 OK` and `is in the library`. Anything else names the boundary that broke — that log exists because a failed handover used to leave nothing to read anywhere, a release build having no console to print to.

Worth exercising by hand at least once per release, because nothing else covers them:

- **closing the window** hides it instead of quitting, and **Quit** in the tray menu actually quits;
- **Register** when another program already owns the scheme — it must say so and refuse to proceed without an explicit tick;
- **autostart** after a reboot, coming up in the tray with no window;
- **Test connection** with a key that lacks the library scope — it must fail *there*, not at the first plate.
