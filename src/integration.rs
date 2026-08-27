//! Desktop integration — Reel's front door is a double-click on a media file,
//! so being present in "Open with" menus and claimable as the default handler
//! IS the product. This module installs the .desktop entry + icon into the
//! user's home (skipped when the system package already provides them) and
//! sets per-category defaults via xdg-mime.

#![cfg(target_os = "linux")]

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::Command;

pub const VIDEO_MIMES: &[&str] = &[
    "video/mp4", "video/x-matroska", "video/webm", "video/quicktime",
    "video/x-msvideo", "video/mpeg", "video/ogg", "video/x-flv",
    "video/x-ms-wmv", "video/mp2t", "video/3gpp",
];
pub const AUDIO_MIMES: &[&str] = &[
    "audio/mpeg", "audio/flac", "audio/ogg", "audio/opus", "audio/x-wav",
    "audio/wav", "audio/mp4", "audio/aac", "audio/x-m4a", "audio/webm",
];
pub const IMAGE_MIMES: &[&str] = &[
    "image/png", "image/jpeg", "image/webp", "image/bmp", "image/svg+xml",
    "image/tiff", "image/gif",
];

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        })
}

const DESKTOP_ID: &str = "reel.desktop";

fn all_mimes() -> Vec<&'static str> {
    [VIDEO_MIMES, AUDIO_MIMES, IMAGE_MIMES].concat()
}

/// Write reel.desktop + the icon under ~/.local/share so file managers list
/// Reel in "Open with" — unless the system package already installed them.
/// Idempotent; refreshes Exec to the currently running binary.
pub fn install_desktop_entry() -> Result<()> {
    if PathBuf::from("/usr/share/applications").join(DESKTOP_ID).exists() {
        return Ok(()); // the distro package owns it
    }
    let exe = std::env::current_exe()?;
    let apps = data_home().join("applications");
    let icons = data_home().join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&apps)?;
    std::fs::create_dir_all(&icons)?;
    std::fs::write(icons.join("reel.svg"), include_str!("../assets/reel-icon.svg"))?;

    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Reel\n\
         GenericName=Media Player\n\
         Comment=Play, edit, convert and capture media\n\
         Exec={} %f\n\
         Icon=reel\n\
         Terminal=false\n\
         Categories=AudioVideo;Player;Video;Audio;Graphics;\n\
         MimeType={};\n",
        exe.to_string_lossy(),
        all_mimes().join(";"),
    );
    std::fs::write(apps.join(DESKTOP_ID), entry)?;
    // Best-effort cache refresh; file managers pick it up regardless on next scan.
    let _ = Command::new("update-desktop-database").arg(&apps).output();
    Ok(())
}

/// Make Reel the default handler for the given mime types.
pub fn set_default_for(mimes: &[&str]) -> Result<()> {
    install_desktop_entry()?;
    let out = Command::new("xdg-mime")
        .arg("default")
        .arg(DESKTOP_ID)
        .args(mimes)
        .output()
        .map_err(|e| anyhow!("xdg-mime not available: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!("xdg-mime failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// Is Reel currently the default for a representative mime of the category?
pub fn is_default_for(probe_mime: &str) -> bool {
    Command::new("xdg-mime")
        .args(["query", "default", probe_mime])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == DESKTOP_ID)
        .unwrap_or(false)
}

/// Settings persisted between runs (~/.config/reel/settings.json).
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Settings {
    /// The "make Reel your default player?" banner was answered or dismissed.
    pub defaults_prompted: bool,
}

fn settings_path() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("reel/settings.json")
}

pub fn load_settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    let p = settings_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(p, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_lands_in_a_fake_home() {
        let tmp = std::env::temp_dir().join(format!("reel-integ-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("XDG_DATA_HOME", &tmp);
        install_desktop_entry().expect("install desktop entry");
        let entry = std::fs::read_to_string(tmp.join("applications/reel.desktop")).expect("entry");
        assert!(entry.contains("MimeType=video/mp4;"));
        assert!(entry.contains("Exec=") && entry.contains(" %f"));
        assert!(tmp.join("icons/hicolor/scalable/apps/reel.svg").exists());
        std::env::remove_var("XDG_DATA_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
