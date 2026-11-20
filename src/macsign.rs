//! macOS code-signature checks: who signed a Reel.app, and does the bundled
//! engine load under the hardened runtime.
//!
//! `verify_team` is the gate any updater MUST pass before it swaps a
//! downloaded Reel.app in: only a Developer ID build signed by Pixygon's
//! team is accepted — an ad-hoc build, another team's, or a bundle whose
//! seal is broken is refused. `scripts/mac.sh` runs `selftest` against the
//! freshly signed bundle (`REEL_MAC_SELFTEST=1`, or `=team` to also demand
//! the team signature).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pixygon's Apple Developer team (the Developer ID Application certificate).
pub const TEAM_ID: &str = "78FMJHZ97T";

/// The code requirement a release must satisfy: an Apple-anchored Developer
/// ID chain (intermediate marker 6.2.6, leaf marker 6.1.13 — the same
/// shape as a Developer ID app's designated requirement) whose leaf belongs
/// to `team`.
pub fn requirement(team: &str) -> String {
    format!(
        "anchor apple generic \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// Refuse anything not signed by Pixygon's Developer ID: the strict, deep
/// verification must pass (every nested dylib and binary sealed) AND the
/// signature must meet `requirement(TEAM_ID)`.
pub fn verify_team(bundle: &Path) -> Result<()> {
    if !bundle.exists() {
        bail!("{} does not exist", bundle.display());
    }
    let out = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "--deep"])
        .arg(format!("-R={}", requirement(TEAM_ID)))
        .arg(bundle)
        .output()
        .context("running codesign")?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    bail!(
        "{} is not a build signed by team {TEAM_ID}: {}",
        bundle.display(),
        why.trim()
    )
}

/// Reel.app for the running binary (Contents/MacOS/reel → Reel.app).
fn own_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?.to_path_buf();
    (app.extension().and_then(|e| e.to_str()) == Some("app")).then_some(app)
}

/// Load the bundled libmpv for real (library validation refuses a dylib not
/// signed by the app's own team under the hardened runtime) and report the
/// bundle's signer. Returns the process exit code.
pub fn selftest() -> i32 {
    let Some(app) = own_bundle() else {
        eprintln!("mac selftest: not running from inside a .app bundle");
        return 1;
    };
    let lib = app.join("Contents/Frameworks/libmpv.2.dylib");
    // Load by absolute path: the normal lookup would quietly fall through to
    // Homebrew's copy and hide a refusal of the bundled one.
    match unsafe { libloading::Library::new(&lib) } {
        Ok(l) => {
            let sym: Result<libloading::Symbol<unsafe extern "C" fn() -> u64>, _> =
                unsafe { l.get(b"mpv_client_api_version\0") };
            match sym {
                Ok(f) => {
                    let v = unsafe { f() };
                    println!("• libmpv loads under the app's signature (client API {}.{})", v >> 16, v & 0xffff);
                }
                Err(e) => {
                    eprintln!("mac selftest: {} has no mpv_client_api_version: {e}", lib.display());
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("mac selftest: the bundled libmpv was refused: {e}");
            return 1;
        }
    }
    let want_team = std::env::var("REEL_MAC_SELFTEST").as_deref() == Ok("team");
    match verify_team(&app) {
        Ok(()) => println!("• signed by team {TEAM_ID}"),
        Err(e) if want_team => {
            eprintln!("mac selftest: {e}");
            return 1;
        }
        Err(_) => println!("• not signed by team {TEAM_ID} (an updater would refuse this build)"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_requirement_pins_the_team_and_a_developer_id_chain() {
        let r = requirement(TEAM_ID);
        assert!(r.contains("certificate leaf[subject.OU] = \"78FMJHZ97T\""));
        assert!(r.starts_with("anchor apple generic"));
        assert!(r.contains("1.2.840.113635.100.6.1.13"), "Developer ID Application leaf marker");
        // codesign must accept the requirement's syntax: an unparseable one
        // would make every build fail "verification" for the wrong reason.
        let out = Command::new("/usr/bin/csreq")
            .args(["-r", &format!("={r}"), "-t"])
            .output()
            .expect("csreq");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn apples_own_binaries_are_refused() {
        // Validly signed — but by Apple, not by Pixygon's team.
        assert!(verify_team(Path::new("/bin/ls")).is_err());
    }

    #[test]
    fn an_ad_hoc_build_is_refused() {
        let dir = std::env::temp_dir().join(format!("reel-macsign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("adhoc");
        std::fs::copy("/usr/bin/true", &bin).unwrap();
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&bin)
            .status()
            .unwrap();
        assert!(signed.success());
        // It IS a valid signature, so only the team requirement refuses it.
        let plain = Command::new("/usr/bin/codesign").args(["--verify", "--strict"]).arg(&bin).status().unwrap();
        assert!(plain.success());
        let err = verify_team(&bin).unwrap_err().to_string();
        assert!(err.contains(TEAM_ID), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_bundle_is_refused() {
        assert!(verify_team(Path::new("/nonexistent/Reel.app")).is_err());
    }
}
