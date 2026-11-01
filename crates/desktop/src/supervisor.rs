use brows_application::SessionApplicationService;
use brows_contracts::{EventEnvelope, EventV1, SessionStateChangedEvent};
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};
use tauri::Emitter;

/// Owns the poller so app shutdown can join it before stopping sessions.
#[derive(Debug)]
pub(crate) struct SessionSupervisor {
    stop: mpsc::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SessionSupervisor {
    pub(crate) fn start(
        app: tauri::AppHandle,
        sessions: Arc<SessionApplicationService>,
    ) -> std::io::Result<Self> {
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("brows-session-supervisor".into())
            .spawn(move || {
                while matches!(
                    receiver.recv_timeout(Duration::from_millis(500)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let previous = sessions.list().unwrap_or_default();
                    if matches!(sessions.reconcile_exits(), Ok(true)) {
                        // The UI refreshes its snapshot on the existing versioned event channel.
                        if let Ok(current) = sessions.list() {
                            for session in current.iter().filter(|s| !previous.contains(s)) {
                                if let Ok(event) = EventEnvelope::new(
                                    format!("session-{}", uuid::Uuid::new_v4()),
                                    EventV1::SessionStateChanged(SessionStateChangedEvent {
                                        session: session.clone(),
                                        at_ms: session.updated_at_ms,
                                    }),
                                ) {
                                    let _ = app.emit("brows://session", event);
                                }
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(crate) fn stop(&self) {
        let _ = self.stop.send(());
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for SessionSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}
