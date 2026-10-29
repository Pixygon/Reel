//! macOS: files handed over by Finder, `open`, the Dock or "Open With".
//!
//! A Mac app never sees those files in argv — AppKit delivers them to the
//! application delegate as `application:openURLs:`. winit 0.30 owns that
//! delegate (`WinitApplicationDelegate`) and doesn't implement the method,
//! so we add it to the class at runtime and forward each path to the event
//! loop as `UserEvent::OpenFile`, which goes through `app.open()` like every
//! other source.

use crate::UserEvent;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use std::ffi::{c_char, CStr};
use std::sync::OnceLock;
use winit::event_loop::EventLoopProxy;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

extern "C" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut AnyObject) {
    let Some(proxy) = PROXY.get() else { return };
    if urls.is_null() {
        return;
    }
    unsafe {
        let n: usize = msg_send![urls, count];
        for i in 0..n {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
            if url.is_null() {
                continue;
            }
            let path: *mut AnyObject = msg_send![url, path];
            if path.is_null() {
                continue;
            }
            let utf8: *const c_char = msg_send![path, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let path = CStr::from_ptr(utf8).to_string_lossy().into_owned();
            let _ = proxy.send_event(UserEvent::OpenFile(path));
        }
    }
}

/// Call after the event loop is built (that is when winit registers and
/// installs its delegate) and before `run_app`, so files that launched the
/// app arrive too.
pub fn install_open_handler(proxy: EventLoopProxy<UserEvent>) {
    let _ = PROXY.set(proxy);
    let Some(cls) = AnyClass::get("WinitApplicationDelegate") else {
        log::warn!("no WinitApplicationDelegate — Finder opens will be ignored");
        return;
    };
    type OpenUrls = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);
    let ok = unsafe {
        objc2::ffi::class_addMethod(
            cls as *const AnyClass as *mut objc2::ffi::objc_class,
            sel!(application:openURLs:).as_ptr(),
            Some(std::mem::transmute::<OpenUrls, unsafe extern "C" fn()>(open_urls)),
            c"v@:@@".as_ptr(),
        )
    };
    if ok {
        log::info!("macOS file-open handler installed");
    } else {
        log::warn!("application:openURLs: already defined — not replaced");
    }
}
