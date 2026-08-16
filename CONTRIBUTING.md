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
| Pre-release | `vX.Y.Z-beta.N` | `dev` |

The release workflow reads the tag shape: anything with a SemVer suffix after the version core is published as a **pre-release**, anything else as a full release. It also **refuses to build a stable tag that is not on `main`**, or a pre-release that is not on `dev` — the rule is enforced rather than remembered.

⚠️ **The one deliberate divergence from BamDude: `-beta.N`, not `bN`.** BamDude's `0.5.3b1` is legal Python and illegal SemVer, and Cargo refuses to parse a manifest containing it. Rather than keep two spellings and a mapping between them — which is a drift waiting to happen — this repo uses the SemVer form everywhere: manifest, tag, installer filename and the version shown in the app are one identical string.

### Cutting a release

1. Bump the version — one command, four files:
   ```bash
   node scripts/set_version.js 0.2.0-beta.1
   ```
   It writes `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and the lockfile, and refuses anything that is not SemVer. Editing by hand invites the release workflow's tag-vs-version check to stop you later, which is the good outcome; the bad one is an app reporting a version that was never built.
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

Tag `v<version>`; release title `BamDude Bridge v<version>`, plus ` (pre-release)` for a pre-release. No subtitles — the body says what is in it.

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

⚠️ **Quit the app before building.** It lives in the tray, so it is usually
running — and Windows will not let cargo replace an executable that is open.
The failure reads `error: failed to remove file … Access is denied (os error
5)`, which looks like a permissions problem and is really just the last build
still running. Quit from the tray menu; if it was started elevated, a normal
shell cannot stop it at all and the tray is the only way out.

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
