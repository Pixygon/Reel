Finish @grok's order from decision 6ae47a717b80cb8b592e11e2: Reel silent auto-update on every platform, using the shared pixygon-updater crate. Same Done-when list as that decision: download silently, verify sha256, run the new version on next launch; a bad checksum or failing new build leaves the previous version running (test); each platform updates in place; package-manager installs do not self-update; no quarantine flag on Mac; the signing switch, when on, refuses an unsigned build (test); full test run passes.

Do what the answer to this decision says:
- public: bootstrap the public repo Pixygon/pixygon-updater (MIT) under this tap (`dyson repo bootstrap`), move the crate there, and depend on it from Reel by pinned rev. Dyson's own move to the new repo is a separate order. Stop and report blocked if the bootstrap needs more than this tap.
- private: git dep on https://github.com/Pixygon/Dyson.git pinned by rev, plus `[net] git-fetch-with-cli = true` in .cargo/config.toml, and say in README, CONTRIBUTING and CI that outside builds no longer work.
- own: write the updater in src/updater.rs with no shared crate.

Already learned (attempt 1, nothing built):
- The crate is pure core: Version, verify_sha256, Settings, after_restart. Reel owns manifest, download, swap, restart. Its auto_install defaults OFF: treat a missing settings file as ON for Reel. No Offer prompt: the order wants no dialog.
- verify_sha256 needs a size and latest.json has none: pass bytes.len(); the sha256 still decides. Each .sha256 is sha256sum output ("hash  name"). Set a ureq body limit; the default is 10 MB.
- Artifacts: Linux tarball is the bare `reel` binary, Windows zip is the bare `reel.exe` (zip -j), AppImage is one file at $APPIMAGE, Mac zip is Reel.app built with ditto (use ditto to unpack, it has dylibs).
- Suggested flow: stage and run the new binary with a probe env var (prints its version, exits 0) BEFORE any swap. Swap by sibling renames (X -> X.update-old, X.update-new -> X): same filesystem, works on a running Windows exe, rollback is the reverse. On a first boot the GUI keeps the LaunchServices identity and spawns a watchdog child holding a stdin pipe; EOF before the health file, or timeout, means restore and relaunch; feed that to after_restart.
- Skip self-update for: $FLATPAK_ID or /.flatpak-info, /usr, /nix, /snap, ~/.cargo/bin, Homebrew Cellar, scoop/choco/winget paths, cargo target/ dirs, unwritable install dirs.
- reel.pixygon.io/latest.json showed top-level v1.15.1 with only Mac at v2.1.0: Linux and Windows get nothing until that pointer is current, and ReelSite is not in ~/repos on this machine.
- The session proxy refused pixygontech.b-cdn.net (CONNECT 403), so CDN reachability is unproven; test with a local manifest server.
- The guard refuses hand-run `gh api` and overriding git credential env.
