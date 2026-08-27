//! System tray (StatusNotifierItem) — where screen capture lives. Screenshots
//! and recordings are about the *screen*, not the file in the player, so
//! their home is the tray: always reachable, even with Reel's window buried.
//! Menu clicks wake the winit loop through an EventLoopProxy.

#![cfg(target_os = "linux")]

use crate::capture::ShotMode;
use crate::runtime::rt;
use crate::UserEvent;
use ksni::TrayMethods;
use winit::event_loop::EventLoopProxy;

pub struct ReelTray {
    proxy: EventLoopProxy<UserEvent>,
    /// Mirrored from the app so the menu shows Start vs Stop.
    pub recording: bool,
}

impl ksni::Tray for ReelTray {
    fn id(&self) -> String {
        "reel".into()
    }

    fn title(&self) -> String {
        if self.recording { "Reel — recording".into() } else { "Reel".into() }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![logo_icon()]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        fn send(proxy: &EventLoopProxy<UserEvent>, ev: UserEvent) {
            let _ = proxy.send_event(ev);
        }
        vec![
            StandardItem {
                label: "Open Reel".into(),
                activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: "Screenshot".into(),
                submenu: vec![
                    StandardItem {
                        label: "Full screen".into(),
                        activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::Shot(ShotMode::Full))),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Region…".into(),
                        activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::Shot(ShotMode::Region))),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Window…".into(),
                        activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::Shot(ShotMode::Window))),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.recording { "⏹ Stop recording".into() } else { "⏺ Record screen…".into() },
                activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::ToggleRecord)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Reel".into(),
                activate: Box::new(|t: &mut Self| send(&t.proxy, UserEvent::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Rasterize the Reel logo for the tray (SNI wants ARGB32, big-endian).
fn logo_icon() -> ksni::Icon {
    const SIZE: u32 = 64;
    let svg = include_str!("../assets/reel-icon.svg");
    let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];
    if let Ok(tree) = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()) {
        if let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(SIZE, SIZE) {
            let scale = SIZE as f32 / tree.size().width().max(tree.size().height());
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            // tiny-skia: premultiplied RGBA → SNI: ARGB.
            for (dst, px) in data.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
                dst[0] = px[3];
                dst[1] = px[0];
                dst[2] = px[1];
                dst[3] = px[2];
            }
        }
    }
    ksni::Icon { width: SIZE as i32, height: SIZE as i32, data }
}

/// Register the tray. `None` when no StatusNotifier host exists — the UI
/// then keeps capture reachable in-app instead.
pub fn spawn(proxy: EventLoopProxy<UserEvent>) -> Option<ksni::Handle<ReelTray>> {
    match rt().block_on(ReelTray { proxy, recording: false }.spawn()) {
        Ok(handle) => Some(handle),
        Err(e) => {
            log::warn!("no system tray available ({e}); capture stays in-app");
            None
        }
    }
}

/// Keep the tray's Start/Stop label in step with the app.
pub fn set_recording(handle: &ksni::Handle<ReelTray>, recording: bool) {
    let h = handle.clone();
    rt().spawn(async move {
        let _ = h.update(move |t| t.recording = recording).await;
    });
}
