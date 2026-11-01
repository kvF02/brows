use brows_application::{PortError, PortErrorKind, PortResult};
use std::{
    collections::BTreeMap,
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

pub(super) fn cancelled() -> PortError {
    PortError::new(PortErrorKind::Unavailable, "proxy session is stopping")
}

#[derive(Debug, Default)]
struct SocketRegistry {
    next_id: u64,
    sockets: BTreeMap<u64, TcpStream>,
}

/// Cloned handles let stop interrupt blocking reads and writes in either direction.
/// Registration and cancellation share a lock, including sockets arriving during stop.
#[derive(Debug, Default)]
pub(super) struct BrokerShutdown {
    stopping: AtomicBool,
    registry: Mutex<SocketRegistry>,
}

impl BrokerShutdown {
    #[cfg(test)]
    pub fn tracked_count(&self) -> usize {
        self.registry.lock().unwrap().sockets.len()
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub fn check(&self) -> PortResult<()> {
        if self.is_stopping() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }

    pub fn track(self: &Arc<Self>, stream: &TcpStream) -> PortResult<SocketRegistration> {
        let socket = stream.try_clone().map_err(|_| {
            PortError::new(PortErrorKind::Unavailable, "cannot track proxy connection")
        })?;
        let mut registry = self.registry.lock().map_err(|_| cancelled())?;
        if self.is_stopping() {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(cancelled());
        }
        let id = registry.next_id;
        registry.next_id = registry.next_id.wrapping_add(1);
        registry.sockets.insert(id, socket);
        Ok(SocketRegistration {
            shutdown: self.clone(),
            id,
        })
    }

    pub fn cancel(&self) {
        self.stopping.store(true, Ordering::Release);
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for socket in registry.sockets.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Debug)]
pub(super) struct SocketRegistration {
    shutdown: Arc<BrokerShutdown>,
    id: u64,
}

impl Drop for SocketRegistration {
    fn drop(&mut self) {
        self.shutdown
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sockets
            .remove(&self.id);
    }
}

#[derive(Debug)]
struct GateState {
    available: usize,
    closed: bool,
}

#[derive(Debug)]
pub(super) struct ConnectionGate {
    state: Mutex<GateState>,
    changed: Condvar,
    capacity: usize,
}

impl ConnectionGate {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                available: capacity,
                closed: false,
            }),
            changed: Condvar::new(),
            capacity,
        }
    }

    pub fn acquire(&self, timeout: Duration) -> PortResult<ConnectionPermit<'_>> {
        let state = self.state.lock().map_err(|_| cancelled())?;
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.available == 0 && !state.closed
            })
            .map_err(|_| cancelled())?;
        if state.closed {
            return Err(cancelled());
        }
        if state.available == 0 {
            return Err(PortError::new(
                PortErrorKind::ProxyConnect,
                "upstream proxy connection limit remained busy",
            ));
        }
        state.available -= 1;
        Ok(ConnectionPermit { gate: self })
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        self.changed.notify_all();
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.available = (state.available + 1).min(self.capacity);
        self.changed.notify_one();
    }
}

pub(super) struct ConnectionPermit<'a> {
    gate: &'a ConnectionGate,
}

impl Drop for ConnectionPermit<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// Owned by the listener; even an unwinding listener cancels and joins its workers.
pub(super) struct Workers {
    pub handles: Vec<JoinHandle<()>>,
    pub shutdown: Arc<BrokerShutdown>,
    pub gate: Arc<ConnectionGate>,
}

impl Workers {
    pub fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.handles.len() {
            if self.handles[index].is_finished() {
                let _ = self.handles.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.gate.close();
        for worker in self.handles.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, sync::mpsc, thread};

    #[test]
    fn closing_gate_wakes_all_waiters_without_waiting_for_permits() {
        let gate = Arc::new(ConnectionGate::new(1));
        let _permit = gate.acquire(Duration::from_secs(30)).unwrap();
        let (tx, rx) = mpsc::channel();
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let gate = gate.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    tx.send(gate.acquire(Duration::from_secs(30)).is_err())
                        .unwrap();
                })
            })
            .collect();
        gate.close();
        for _ in 0..4 {
            assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(gate.acquire(Duration::from_secs(30)).is_err());
    }

    #[test]
    fn registration_racing_cancellation_never_leaves_an_open_socket() {
        for _ in 0..20 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let shutdown = Arc::new(BrokerShutdown::default());
            let cancel = shutdown.clone();
            let thread = thread::spawn(move || cancel.cancel());
            let registration = shutdown.track(&client);
            thread.join().unwrap();
            let result = peer.read(&mut [0]);
            assert!(
                matches!(result, Ok(0))
                    || matches!(result, Err(ref e) if !matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock))
            );
            drop(registration);
            assert_eq!(shutdown.tracked_count(), 0);
            assert!(shutdown.track(&client).is_err());
        }
    }
}
