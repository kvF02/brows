use crate::{DomainError, ProfileId, SessionId, MAX_CONCURRENT_SESSIONS, MAX_QUEUED_SESSIONS};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTicket {
    pub session_id: SessionId,
    pub profile_id: ProfileId,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Started(LaunchTicket),
    Queued {
        ticket: LaunchTicket,
        position: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub limit: usize,
    pub active: usize,
    pub queued: usize,
}

/// Small, deterministic FIFO scheduler for browser sessions.
///
/// It only tracks IDs and capacity. Process handles and persistence belong to
/// adapters; this keeps scheduling unit-testable and independent of Windows.
#[derive(Debug, Clone)]
pub struct SessionScheduler {
    limit: usize,
    active: BTreeMap<SessionId, ProfileId>,
    queued: VecDeque<LaunchTicket>,
}

impl SessionScheduler {
    pub fn new(limit: usize) -> Result<Self, DomainError> {
        if limit == 0 || limit > MAX_CONCURRENT_SESSIONS {
            return Err(DomainError::SchedulerLimitInvalid);
        }
        Ok(Self {
            limit,
            active: BTreeMap::new(),
            queued: VecDeque::new(),
        })
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            limit: self.limit,
            active: self.active.len(),
            queued: self.queued.len(),
        }
    }

    pub fn enqueue(&mut self, ticket: LaunchTicket) -> Result<EnqueueOutcome, DomainError> {
        if self.active.contains_key(&ticket.session_id)
            || self
                .queued
                .iter()
                .any(|item| item.session_id == ticket.session_id)
        {
            return Err(DomainError::DuplicateSession);
        }
        if self
            .active
            .values()
            .any(|profile| profile == &ticket.profile_id)
            || self
                .queued
                .iter()
                .any(|item| item.profile_id == ticket.profile_id)
        {
            return Err(DomainError::ProfileAlreadyScheduled);
        }
        if self.active.len() < self.limit {
            self.active
                .insert(ticket.session_id.clone(), ticket.profile_id.clone());
            Ok(EnqueueOutcome::Started(ticket))
        } else {
            if self.queued.len() >= MAX_QUEUED_SESSIONS {
                return Err(DomainError::QueueFull);
            }
            self.queued.push_back(ticket.clone());
            Ok(EnqueueOutcome::Queued {
                ticket,
                position: self.queued.len(),
            })
        }
    }

    /// Release an active session and promote the oldest queued ticket, if any.
    pub fn release(&mut self, session_id: &SessionId) -> Result<Option<LaunchTicket>, DomainError> {
        if self.active.remove(session_id).is_none() {
            return Err(DomainError::SessionNotActive);
        }
        let promoted = self.queued.pop_front();
        if let Some(ticket) = &promoted {
            self.active
                .insert(ticket.session_id.clone(), ticket.profile_id.clone());
        }
        Ok(promoted)
    }

    pub fn cancel_queued(&mut self, session_id: &SessionId) -> Result<LaunchTicket, DomainError> {
        let index = self
            .queued
            .iter()
            .position(|ticket| &ticket.session_id == session_id)
            .ok_or(DomainError::SessionNotFound)?;
        self.queued
            .remove(index)
            .ok_or(DomainError::SessionNotFound)
    }

    pub fn is_active(&self, session_id: &SessionId) -> bool {
        self.active.contains_key(session_id)
    }

    pub fn queue_position(&self, session_id: &SessionId) -> Option<usize> {
        self.queued
            .iter()
            .position(|ticket| &ticket.session_id == session_id)
            .map(|i| i + 1)
    }

    pub fn active_profile(&self, session_id: &SessionId) -> Option<&ProfileId> {
        self.active.get(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u32) -> (SessionId, ProfileId) {
        (
            SessionId::new(format!("s-{n}")).unwrap(),
            ProfileId::new(format!("p-{n}")).unwrap(),
        )
    }

    #[test]
    fn fifo_promotes_when_slot_is_released() {
        let mut scheduler = SessionScheduler::new(2).unwrap();
        for n in 1..=2 {
            let (session_id, profile_id) = ids(n);
            assert!(matches!(
                scheduler.enqueue(LaunchTicket {
                    session_id,
                    profile_id,
                    requested_at_ms: n as u64
                }),
                Ok(EnqueueOutcome::Started(_))
            ));
        }
        let (session_id, profile_id) = ids(3);
        assert!(matches!(
            scheduler.enqueue(LaunchTicket {
                session_id,
                profile_id,
                requested_at_ms: 3
            }),
            Ok(EnqueueOutcome::Queued { position: 1, .. })
        ));
        let (first, _) = ids(1);
        let promoted = scheduler.release(&first).unwrap().unwrap();
        assert_eq!(promoted.session_id.as_str(), "s-3");
        assert_eq!(scheduler.snapshot().active, 2);
    }

    #[test]
    fn duplicate_profile_is_rejected_even_with_different_session() {
        let mut scheduler = SessionScheduler::new(2).unwrap();
        let (s1, p1) = ids(1);
        scheduler
            .enqueue(LaunchTicket {
                session_id: s1,
                profile_id: p1.clone(),
                requested_at_ms: 0,
            })
            .unwrap();
        let (s2, _) = ids(2);
        assert_eq!(
            scheduler.enqueue(LaunchTicket {
                session_id: s2,
                profile_id: p1,
                requested_at_ms: 1
            }),
            Err(DomainError::ProfileAlreadyScheduled)
        );
    }

    #[test]
    fn queued_cancellation_reorders_positions() {
        let mut scheduler = SessionScheduler::new(1).unwrap();
        let (s1, p1) = ids(1);
        scheduler
            .enqueue(LaunchTicket {
                session_id: s1,
                profile_id: p1,
                requested_at_ms: 0,
            })
            .unwrap();
        for n in 2..=3 {
            let (session_id, profile_id) = ids(n);
            scheduler
                .enqueue(LaunchTicket {
                    session_id,
                    profile_id,
                    requested_at_ms: n as u64,
                })
                .unwrap();
        }
        let (s2, _) = ids(2);
        scheduler.cancel_queued(&s2).unwrap();
        let (s3, _) = ids(3);
        assert_eq!(scheduler.queue_position(&s3), Some(1));
    }

    #[test]
    fn eleventh_launch_is_queued_at_the_default_limit() {
        let mut scheduler = SessionScheduler::new(crate::DEFAULT_MAX_CONCURRENT_SESSIONS).unwrap();
        for n in 1..=10 {
            let (session_id, profile_id) = ids(n);
            assert!(matches!(
                scheduler.enqueue(LaunchTicket {
                    session_id,
                    profile_id,
                    requested_at_ms: n as u64
                }),
                Ok(EnqueueOutcome::Started(_))
            ));
        }
        let (session_id, profile_id) = ids(11);
        let outcome = scheduler
            .enqueue(LaunchTicket {
                session_id,
                profile_id,
                requested_at_ms: 11,
            })
            .unwrap();
        assert!(matches!(
            outcome,
            EnqueueOutcome::Queued { position: 1, .. }
        ));
        assert_eq!(
            scheduler.snapshot(),
            QueueSnapshot {
                limit: 10,
                active: 10,
                queued: 1
            }
        );
    }

    #[test]
    fn scheduler_rejects_limits_above_the_v0_1_safety_cap() {
        assert_eq!(
            SessionScheduler::new(MAX_CONCURRENT_SESSIONS + 1).unwrap_err(),
            DomainError::SchedulerLimitInvalid
        );
    }
}
