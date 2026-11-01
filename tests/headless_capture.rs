//! `reel screenshot` with no display, end to end: the real binary, a stub
//! `spectacle` first on PATH that records being started. The stub, not the
//! real one, because a real run here aborts and wakes the crash reporter.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch dir holding the stub, an empty runtime dir, and a HOME.
struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("reel-headless-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["bin", "run", "home"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        // Writes the file spectacle was asked for (the argument after -o).
        let stub = dir.join("bin/spectacle");
        let marker = dir.join("spectacle-started");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho started > '{}'\nwhile [ $# -gt 1 ]; do shift; done\necho png > \"$1\"\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rig { dir }
    }

    fn runtime(&self) -> PathBuf {
        self.dir.join("run")
    }

    fn spectacle_started(&self) -> bool {
        self.dir.join("spectacle-started").exists()
    }

    fn shot(&self, display: &[(&str, &str)]) -> (Output, PathBuf) {
        let out = self.dir.join("shot.png");
        let path = format!("{}:{}", self.dir.join("bin").display(), std::env::var("PATH").unwrap());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_reel"));
        cmd.arg("screenshot")
            .arg(&out)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env("PATH", path)
            .env("HOME", self.dir.join("home"))
            .env("XDG_RUNTIME_DIR", self.runtime())
            .envs(display.iter().copied());
        (cmd.output().expect("run reel"), out)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assert_refused(rig: &Rig, output: &Output, out: &Path, names: &[&str]) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "must fail: {stderr}");
    assert!(!rig.spectacle_started(), "spectacle was started: {stderr}");
    assert!(!out.exists(), "no file without a capture");
    assert!(stderr.contains("headless: no reachable display"), "{stderr}");
    for name in names {
        assert!(stderr.contains(name), "error should name {name}: {stderr}");
    }
}

#[test]
fn no_display_variables_start_no_spectacle() {
    let rig = Rig::new("unset");
    let (output, out) = rig.shot(&[]);
    assert_refused(&rig, &output, &out, &["WAYLAND_DISPLAY is not set", "DISPLAY is not set"]);
}

/// The evidence case: the variables were inherited, the sockets were not.
#[test]
fn set_but_unreachable_displays_start_no_spectacle() {
    let rig = Rig::new("dead");
    let (output, out) = rig.shot(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":199")]);
    assert_refused(&rig, &output, &out, &["WAYLAND_DISPLAY=wayland-0", "DISPLAY=:199"]);
}

/// The control: the same rig with a live socket does reach spectacle, so the
/// two refusals above are the guard speaking and not a broken stub.
#[test]
fn a_live_wayland_socket_still_reaches_spectacle() {
    let rig = Rig::new("live");
    let _socket = UnixListener::bind(rig.runtime().join("wayland-0")).unwrap();
    let (output, out) = rig.shot(&[("WAYLAND_DISPLAY", "wayland-0")]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(rig.spectacle_started());
    assert!(out.exists());
}
