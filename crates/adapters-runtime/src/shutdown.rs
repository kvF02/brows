//! Bounded normal shutdown; the owner retains its process until exit is reaped.
use std::{
    io, thread,
    time::{Duration, Instant},
};

pub(super) trait OwnedProcess {
    fn exited(&mut self) -> io::Result<bool>;
    fn request_close(&mut self) -> io::Result<bool>;
    fn force(&mut self) -> io::Result<()>;
    fn reap(&mut self) -> io::Result<()>;
}

pub(super) fn stop(process: &mut impl OwnedProcess, grace: Duration) -> io::Result<()> {
    let deadline = Instant::now() + grace;
    let mut requested = false;
    while !process.exited()? {
        if Instant::now() >= deadline {
            break;
        }
        if !requested {
            match process.request_close() {
                Ok(sent) => requested = sent,
                Err(_) => break, // A failed close request still requires bounded reclamation.
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !process.exited()? {
        if let Err(error) = process.force() {
            // The process can exit between the query and termination request.
            if !process.exited()? {
                return Err(error);
            }
        }
    }
    process.reap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        exited: bool,
        closes: usize,
        kills: usize,
        reaps: usize,
        accepts_close: bool,
        reject_close: bool,
        reject_kill: bool,
    }
    impl OwnedProcess for Fake {
        fn exited(&mut self) -> io::Result<bool> {
            Ok(self.exited)
        }
        fn request_close(&mut self) -> io::Result<bool> {
            self.closes += 1;
            if self.reject_close {
                return Err(io::Error::other("close unavailable"));
            }
            self.exited = self.accepts_close;
            Ok(self.accepts_close)
        }
        fn force(&mut self) -> io::Result<()> {
            self.kills += 1;
            if self.reject_kill {
                return Err(io::Error::other("termination failed"));
            }
            self.exited = true;
            Ok(())
        }
        fn reap(&mut self) -> io::Result<()> {
            self.reaps += 1;
            Ok(())
        }
    }
    #[test]
    fn normal_close_reaps_without_forcing_and_already_exited_children_are_only_reaped() {
        let mut p = Fake {
            accepts_close: true,
            ..Fake::default()
        };
        stop(&mut p, Duration::from_secs(1)).unwrap();
        assert_eq!((p.closes, p.kills, p.reaps), (1, 0, 1));
        stop(&mut p, Duration::from_secs(1)).unwrap();
        assert_eq!((p.closes, p.kills, p.reaps), (1, 0, 2));
    }
    #[test]
    fn close_errors_and_expired_grace_still_force_and_reap_owned_processes() {
        let mut p = Fake {
            reject_close: true,
            ..Fake::default()
        };
        stop(&mut p, Duration::from_secs(1)).unwrap();
        assert_eq!((p.closes, p.kills, p.reaps), (1, 1, 1));
        let mut p = Fake::default();
        stop(&mut p, Duration::ZERO).unwrap();
        assert_eq!((p.closes, p.kills, p.reaps), (0, 1, 1));
        let mut p = Fake::default();
        stop(&mut p, Duration::from_millis(1)).unwrap();
        assert!(p.closes > 0);
        assert_eq!((p.kills, p.reaps), (1, 1));
    }
    #[test]
    fn failed_termination_is_not_reported_or_reaped_as_a_successful_stop() {
        let mut p = Fake {
            reject_kill: true,
            ..Fake::default()
        };
        assert!(stop(&mut p, Duration::ZERO).is_err());
        assert_eq!((p.kills, p.reaps), (1, 0));
    }
}
