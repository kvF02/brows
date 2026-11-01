//! Winsock does not reliably wake an already blocking recv when another clone
//! calls shutdown. Short OS I/O waits provide cancellation checkpoints without
//! imposing an idle timeout on an established tunnel.
use super::BrokerShutdown;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const IO_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) struct CancellableIo {
    stream: TcpStream,
    shutdown: Arc<BrokerShutdown>,
    deadline: Option<Instant>,
}
impl CancellableIo {
    pub fn new(
        stream: TcpStream,
        shutdown: Arc<BrokerShutdown>,
        timeout: Duration,
    ) -> io::Result<Self> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(IO_POLL_INTERVAL))?;
        stream.set_write_timeout(Some(IO_POLL_INTERVAL))?;
        Ok(Self {
            stream,
            shutdown,
            deadline: Some(Instant::now() + timeout),
        })
    }
    pub fn establish(&mut self) {
        self.deadline = None;
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            shutdown: self.shutdown.clone(),
            deadline: self.deadline,
        })
    }
    pub fn shutdown(&self, direction: Shutdown) -> io::Result<()> {
        self.stream.shutdown(direction)
    }
    fn check(&self) -> io::Result<()> {
        if self.shutdown.is_stopping() {
            // copy/read_exact retry Interrupted. Cancellation must terminate them.
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "proxy session is stopping",
            ))
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "proxy handshake timed out",
            ))
        } else {
            Ok(())
        }
    }
    fn retry(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        )
    }
}
impl Read for CancellableIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.read(bytes) {
                Err(error) if Self::retry(&error) => continue,
                result => return result,
            }
        }
    }
}
impl Write for CancellableIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.write(bytes) {
                Err(error) if Self::retry(&error) => continue,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread};

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (client, listener.accept().unwrap().0)
    }
    #[test]
    fn cancellation_is_observed_even_without_socket_shutdown() {
        let (socket, _peer) = pair();
        let shutdown = Arc::new(BrokerShutdown::default());
        // Deliberately do not register the socket: cancellation cannot call
        // shutdown on it, so this proves the timed-read checkpoint itself.
        let mut stream =
            CancellableIo::new(socket, shutdown.clone(), Duration::from_secs(30)).unwrap();
        let (started, ready) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            done.send(stream.read(&mut [0]).unwrap_err().kind())
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(finished.recv_timeout(Duration::from_millis(200)).is_err());
        shutdown.cancel();
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(2)).unwrap(),
            io::ErrorKind::ConnectionAborted
        );
        worker.join().unwrap();
    }
    #[test]
    fn cancellation_interrupts_a_writer_when_the_peer_never_reads() {
        let (socket, _peer) = pair();
        let shutdown = Arc::new(BrokerShutdown::default());
        let mut stream =
            CancellableIo::new(socket, shutdown.clone(), Duration::from_secs(30)).unwrap();
        stream.establish();
        let (started, ready) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            let bytes = [0u8; 64 * 1024];
            loop {
                if let Err(error) = stream.write_all(&bytes) {
                    done.send(error.kind()).unwrap();
                    break;
                }
            }
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(finished.recv_timeout(Duration::from_millis(250)).is_err());
        shutdown.cancel();
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(2)).unwrap(),
            io::ErrorKind::ConnectionAborted
        );
        worker.join().unwrap();
    }
    #[test]
    fn established_stream_can_receive_after_the_handshake_deadline() {
        let (socket, mut peer) = pair();
        let mut stream =
            CancellableIo::new(socket, Arc::default(), Duration::from_millis(30)).unwrap();
        stream.establish();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            peer.write_all(b"still open").unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
        });
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"still open");
        writer.join().unwrap();
    }
    #[test]
    fn incomplete_handshake_keeps_an_absolute_deadline() {
        let (socket, _peer) = pair();
        let mut stream =
            CancellableIo::new(socket, Arc::default(), Duration::from_millis(30)).unwrap();
        let started = Instant::now();
        assert_eq!(
            stream.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
