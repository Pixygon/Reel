Follow-up to decision 6ae69d8ef43a6ffce6be1cee (D-33), which stopped because wpm cannot hold a Rust crate. Do what Anders chose. The Pixygon/pixygon-updater repo stays private in both cases; never change its visibility.

- wpm-open: add a Cargo sparse-registry lane to Pixygon/wpm with public read and token-gated publish.
- wpm-private: the same lane with token-gated read as well. Reel's CI and every build node then need a read token (a credential step for Anders), and Reel's README and CONTRIBUTING must say outsiders cannot build from source.

Keep every existing Weft route and the Weft verification as they are. Crate versions must be immutable, unlike the pointer-moving Weft names. Publish pixygon-updater v0.1.0 (its Cargo.toml has publish = false, so it needs an allowed registry) and quote the published name and version in the receipt. Prove it with a fresh `cargo fetch` from a scratch crate.

Then run the Reel order in decision 6ae4a7397b80cb8b593c1e50, depending on the crate only through wpm. Reel has no updater today, so this is a new feature, not a repoint. Its traps are in that brief: latest.json lags, no size field, auto_install off by default, rollback is only a verdict. Done when `timeout 45m cargo test --workspace` passes in Reel.
