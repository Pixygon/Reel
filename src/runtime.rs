//! The process-wide tokio runtime for everything portal-shaped (rfd file
//! dialogs, ashpd screenshot/screencast). One runtime, alive for the whole
//! process: ashpd/zbus cache their D-Bus session connection globally and bind
//! it to the reactor of the runtime that created it — a per-call runtime
//! works exactly once, then every later portal call waits forever on the
//! dead reactor. (That was the "Open… only works once" bug.)

#![cfg(target_os = "linux")]

use std::sync::OnceLock;

pub fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("reel-portal")
            .build()
            .expect("tokio runtime")
    })
}
