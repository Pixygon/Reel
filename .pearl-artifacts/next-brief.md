Follow-up to decision 6ae472217b80cb8b592a9b54 (Reel silent auto-update), which stopped because Reel could not depend on pixygon-updater. Do what Anders chose.

- public: move crates/pixygon-updater out of private Dyson into its own public Pixygon repo under Reel's MIT licence, with Dyson depending on the new home, so there is one crate and no copy. Nothing else from Dyson goes public. `dyson repo` only makes private repos, so if the public flip needs Anders, file it as a founder step and report blocked. Then run the Reel order (below).
- token: add a Dyson-read token as a Reel CI secret (a credential step for Anders), set net.git-fetch-with-cli in Reel's .cargo/config.toml, and change README/CONTRIBUTING to say outsiders cannot build from source. Then run the Reel order.
- own: Reel carries its own update core, with the "never a copy" exception recorded. Then run the Reel order.

The Reel order, unchanged: check at launch and every 24 h, background download, sha256, install on next launch or quit with no dialog and one small Updated line; never interrupt playback, editing or a render; Linux tarball, AppImage, Windows zip and Mac app update in place; Flatpak and package-manager installs left alone; a bad download or a new build that fails to start rolls back; Mac carries no quarantine flag, plus a build-config switch for signature and team ID checks; Settings: auto-update on/off and Check now; README and CHANGELOG.md.

Known traps, from attempt 1:
- reel.pixygon.io/latest.json lists Linux, Windows and AppImage at v1.15.1 while Reel is 2.1.0, so an updater reading it finds nothing newer until the pointer moves. It has no size field; pixygon_updater::verify_sha256 takes a size.
- Settings.auto_install defaults OFF in the crate, and this order wants auto-update ON by default.
- The crate's rollback is only a verdict (after_restart). The app supplies the swap, guard and health report.
- Default cargo gets 401 on a private git dep even with gh logged in; it needs net.git-fetch-with-cli.
