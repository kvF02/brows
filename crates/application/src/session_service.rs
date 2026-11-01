use crate::{
    ApplicationError, ApplicationResult, AuditEvent, AuditOutcome, AuditSink, BrowserProcess,
    BrowserRuntime, Clock, IdGenerator, LaunchPlanCompiler, ProfileRepository, ProxyLease,
    ProxyRuntime,
};
use brows_contracts::{
    ErrorCode, EventV1, LaunchQueuedEvent, SessionCancelQueuedRequest, SessionDto,
    SessionLaunchRequest, SessionStateChangedEvent, SessionStopRequest,
};
use brows_domain::{
    EnqueueOutcome, LaunchTicket, Session, SessionId, SessionScheduler, SessionState,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLaunchOutcome {
    Started {
        session: SessionDto,
        event: EventV1,
    },
    Queued {
        session: SessionDto,
        position: usize,
        event: EventV1,
    },
}

#[derive(Debug)]
struct SessionRegistry {
    scheduler: SessionScheduler,
    reservations: BTreeMap<SessionId, crate::ProfileReservation>,
    shutting_down: bool,
    sessions: BTreeMap<SessionId, Session>,
    processes: BTreeMap<SessionId, BrowserProcess>,
    proxy_leases: BTreeMap<SessionId, ProxyLease>,
}

/// Coordinates the domain scheduler with browser and proxy lifetimes.
/// Mutating commands are serialized to make launch/stop promotion atomic from
/// the application's perspective; adapters remain free to parallelize the
/// actual Chromium processes they own.
#[derive(Debug)]
pub struct SessionApplicationService {
    profiles: Arc<dyn ProfileRepository>,
    compiler: Arc<LaunchPlanCompiler>,
    browsers: Arc<dyn BrowserRuntime>,
    proxies: Arc<dyn ProxyRuntime>,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    operations: Mutex<()>,
    profile_operations: Arc<crate::ProfileOperations>,
    registry: Mutex<SessionRegistry>,
}

impl SessionApplicationService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profiles: Arc<dyn ProfileRepository>,
        profile_operations: Arc<crate::ProfileOperations>,
        compiler: Arc<LaunchPlanCompiler>,
        browsers: Arc<dyn BrowserRuntime>,
        proxies: Arc<dyn ProxyRuntime>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        max_concurrent_sessions: usize,
    ) -> ApplicationResult<Self> {
        Ok(Self {
            profiles,
            profile_operations,
            compiler,
            browsers,
            proxies,
            audit,
            clock,
            ids,
            operations: Mutex::new(()),
            registry: Mutex::new(SessionRegistry {
                scheduler: SessionScheduler::new(max_concurrent_sessions)?,
                reservations: BTreeMap::new(),
                shutting_down: false,
                sessions: BTreeMap::new(),
                processes: BTreeMap::new(),
                proxy_leases: BTreeMap::new(),
            }),
        })
    }

    pub fn launch(&self, request: SessionLaunchRequest) -> ApplicationResult<SessionLaunchOutcome> {
        let _operation = self.lock_operations()?;
        let profile_id = request.profile_id.try_into()?;
        {
            let registry = self.lock_registry()?;
            if registry.shutting_down {
                return Err(ApplicationError::new(
                    ErrorCode::InvalidProfileState,
                    "管理器正在退出",
                ));
            }
            if registry.sessions.values().any(|s| {
                s.profile_id == profile_id
                    && (s.state.is_active() || s.state == SessionState::Queued)
            }) {
                return Err(ApplicationError::new(
                    ErrorCode::AlreadyExists,
                    "profile already scheduled",
                ));
            }
        }
        let reservation = self.profile_operations.reserve(&profile_id)?;
        let profile = self.profiles.get(&profile_id)?;
        if profile.archived {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "archived profiles cannot be launched",
            ));
        }

        let session_id = self.ids.next_session_id()?;
        let now = self.clock.now_ms();
        let ticket = LaunchTicket {
            session_id: session_id.clone(),
            profile_id,
            requested_at_ms: now,
        };
        let outcome = {
            let mut registry = self.lock_registry()?;
            let outcome = registry.scheduler.enqueue(ticket)?;
            let mut session = Session::new(session_id.clone(), profile.id, now);
            match &outcome {
                EnqueueOutcome::Started(_) => session.transition(SessionState::Starting, now)?,
                EnqueueOutcome::Queued { .. } => session.transition(SessionState::Queued, now)?,
            }
            registry.sessions.insert(session_id.clone(), session);
            registry
                .reservations
                .insert(session_id.clone(), reservation);
            outcome
        };

        match outcome {
            EnqueueOutcome::Started(_) => {
                let session = self.start_one(&session_id)?;
                let dto = SessionDto::from(&session);
                Ok(SessionLaunchOutcome::Started {
                    event: EventV1::SessionStateChanged(SessionStateChangedEvent {
                        session: dto.clone(),
                        at_ms: session.updated_at_ms,
                    }),
                    session: dto,
                })
            }
            EnqueueOutcome::Queued { position, .. } => {
                let session = self.session(&session_id)?;
                let dto = SessionDto::from(&session);
                self.audit(
                    "session.queue",
                    session.id.as_str(),
                    AuditOutcome::Succeeded,
                );
                Ok(SessionLaunchOutcome::Queued {
                    event: EventV1::LaunchQueued(LaunchQueuedEvent {
                        session_id: (&session.id).into(),
                        profile_id: (&session.profile_id).into(),
                        position,
                    }),
                    session: dto,
                    position,
                })
            }
        }
    }

    pub fn stop(&self, request: SessionStopRequest) -> ApplicationResult<SessionDto> {
        let _operation = self.lock_operations()?;
        self.stop_locked(request.session_id.try_into()?)
    }

    fn stop_locked(&self, session_id: SessionId) -> ApplicationResult<SessionDto> {
        let snapshot = self.session(&session_id)?;
        if snapshot.state == SessionState::Queued {
            let session = {
                let mut registry = self.lock_registry()?;
                registry.scheduler.cancel_queued(&session_id)?;
                registry.reservations.remove(&session_id);
                let session = registry.sessions.get_mut(&session_id).ok_or_else(|| {
                    ApplicationError::new(ErrorCode::NotFound, "session not found")
                })?;
                session.transition(SessionState::Stopped, self.clock.now_ms())?;
                session.clone()
            };
            self.audit(
                "session.cancel",
                session.id.as_str(),
                AuditOutcome::Succeeded,
            );
            return Ok(SessionDto::from(&session));
        }
        if !matches!(
            snapshot.state,
            SessionState::Starting | SessionState::Running | SessionState::Stopping
        ) {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                format!("cannot stop a {} session", snapshot.state.as_str()),
            ));
        }

        if snapshot.state != SessionState::Stopping {
            let mut registry = self.lock_registry()?;
            registry
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| ApplicationError::new(ErrorCode::NotFound, "session not found"))?
                .transition(SessionState::Stopping, self.clock.now_ms())?;
        }

        if let Err(error) = self.browsers.stop(&session_id) {
            let application_error: ApplicationError = error.into();
            // Keep the session in Stopping and retain its scheduler slot. A
            // caller can retry without accidentally exceeding the concurrency
            // limit while the process may still be alive.
            self.audit("session.stop", session_id.as_str(), AuditOutcome::Failed);
            return Err(application_error);
        }

        let (session, promoted, proxy_release_error) = {
            let mut registry = self.lock_registry()?;
            let lease = registry.proxy_leases.remove(&session_id);
            registry.processes.remove(&session_id);
            registry.reservations.remove(&session_id);
            let proxy_error = lease
                .as_ref()
                .and_then(|lease| self.proxies.release_session(lease).err())
                .map(ApplicationError::from);
            let session = registry
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| ApplicationError::new(ErrorCode::NotFound, "session not found"))?;
            session.transition(SessionState::Stopped, self.clock.now_ms())?;
            let session = session.clone();
            let promoted = registry.scheduler.release(&session_id)?;
            (session, promoted, proxy_error)
        };

        self.audit("session.stop", session.id.as_str(), AuditOutcome::Succeeded);
        if let Some(ticket) = promoted {
            let _ = self.start_one(&ticket.session_id);
        }
        if let Some(error) = proxy_release_error {
            return Err(error);
        }
        Ok(SessionDto::from(&session))
    }

    pub fn cancel_queued(
        &self,
        request: SessionCancelQueuedRequest,
    ) -> ApplicationResult<SessionDto> {
        self.stop(SessionStopRequest {
            session_id: request.session_id,
        })
    }

    pub fn list(&self) -> ApplicationResult<Vec<SessionDto>> {
        let registry = self.lock_registry()?;
        Ok(registry.sessions.values().map(SessionDto::from).collect())
    }

    pub fn has_active_profile(
        &self,
        profile_id: &brows_domain::ProfileId,
    ) -> ApplicationResult<bool> {
        let registry = self.lock_registry()?;
        Ok(registry
            .sessions
            .values()
            .any(|session| &session.profile_id == profile_id && session.state.is_active()))
    }

    /// Called by a process supervisor when Chromium exits without an explicit
    /// successful stop operation.
    pub fn record_unexpected_exit(
        &self,
        session_id: &SessionId,
        message: impl Into<String>,
    ) -> ApplicationResult<SessionDto> {
        let _operation = self.lock_operations()?;
        self.finish_exit(session_id, false, message.into())
    }

    fn finish_exit(
        &self,
        session_id: &SessionId,
        success: bool,
        message: String,
    ) -> ApplicationResult<SessionDto> {
        // Reap the exited process (and its owned tree) before releasing data or capacity.
        if let Err(error) = self.browsers.stop(session_id) {
            if error.kind != crate::PortErrorKind::NotFound {
                return Err(error.into());
            }
        }
        let (session, promoted, lease) = {
            let mut registry = self.lock_registry()?;
            registry.processes.remove(session_id);
            let lease = registry.proxy_leases.remove(session_id);
            let session = registry
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| ApplicationError::new(ErrorCode::NotFound, "session not found"))?;
            if success {
                if session.state != SessionState::Stopping {
                    session.transition(SessionState::Stopping, self.clock.now_ms())?;
                }
                session.transition(SessionState::Stopped, self.clock.now_ms())?;
            } else {
                session.fail("BROWSER_EXITED", message, self.clock.now_ms())?;
            }
            let session = session.clone();
            registry.reservations.remove(session_id);
            let promoted = registry.scheduler.release(session_id)?;
            (session, promoted, lease)
        };
        if let Some(lease) = lease {
            let _ = self.proxies.release_session(&lease);
        }
        self.audit(
            "session.exit",
            session.id.as_str(),
            if success {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
        );
        if let Some(ticket) = promoted {
            let _ = self.start_one(&ticket.session_id);
        }
        Ok(SessionDto::from(&session))
    }

    /// Poll every owned process, including ones manually closed by the user.
    /// A failed query retains ownership and capacity for a later retry.
    pub fn reconcile_exits(&self) -> ApplicationResult<bool> {
        let _operation = self.lock_operations()?;
        let ids = {
            let registry = self.lock_registry()?;
            if registry.shutting_down {
                return Ok(false);
            }
            registry.processes.keys().cloned().collect::<Vec<_>>()
        };
        let mut changed = false;
        for id in ids {
            if let Ok(Some(exit)) = self.browsers.poll_exit(&id) {
                changed |= self
                    .finish_exit(&id, exit.success, "浏览器进程已退出".into())
                    .is_ok();
            }
        }
        Ok(changed)
    }

    pub fn reserve_stopped_profile(
        &self,
        id: &brows_domain::ProfileId,
    ) -> ApplicationResult<crate::ProfileReservation> {
        self.profile_operations.reserve(id)
    }

    /// Stop accepting launches before cancelling the queue, so shutdown cannot promote work.
    pub fn shutdown(&self) -> ApplicationResult<()> {
        let _operation = self.lock_operations()?;
        self.lock_registry()?.shutting_down = true;
        let sessions = self.list()?;
        let mut first_error = None;
        for queued in [true, false] {
            for session in &sessions {
                let should_stop = if queued {
                    session.state == brows_contracts::SessionStateDto::Queued
                } else {
                    matches!(
                        session.state,
                        brows_contracts::SessionStateDto::Running
                            | brows_contracts::SessionStateDto::Starting
                            | brows_contracts::SessionStateDto::Stopping
                    )
                };
                if should_stop {
                    if let Err(error) = self.stop_locked(session.id.clone().try_into()?) {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn queue_position(&self, session_id: &SessionId) -> ApplicationResult<Option<usize>> {
        Ok(self.lock_registry()?.scheduler.queue_position(session_id))
    }

    fn start_one(&self, session_id: &SessionId) -> ApplicationResult<Session> {
        {
            let mut registry = self.lock_registry()?;
            let session = registry
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| ApplicationError::new(ErrorCode::NotFound, "session not found"))?;
            if session.state == SessionState::Queued {
                session.transition(SessionState::Starting, self.clock.now_ms())?;
            }
        }

        let result = (|| -> ApplicationResult<(BrowserProcess, Option<ProxyLease>)> {
            let profile_id = self.session(session_id)?.profile_id;
            let profile = self.profiles.get(&profile_id)?;
            let prepared = self.compiler.compile(&profile, session_id)?;
            match self.browsers.launch(&prepared.plan) {
                Ok(process) => Ok((process, prepared.proxy_lease)),
                Err(error) => {
                    if let Some(lease) = &prepared.proxy_lease {
                        let _ = self.proxies.release_session(lease);
                    }
                    Err(error.into())
                }
            }
        })();

        match result {
            Ok((process, lease)) => {
                let session = {
                    let mut registry = self.lock_registry()?;
                    registry.processes.insert(session_id.clone(), process);
                    if let Some(lease) = lease {
                        registry.proxy_leases.insert(session_id.clone(), lease);
                    }
                    let session = registry.sessions.get_mut(session_id).ok_or_else(|| {
                        ApplicationError::new(ErrorCode::NotFound, "session not found")
                    })?;
                    session.transition(SessionState::Running, self.clock.now_ms())?;
                    session.clone()
                };
                self.audit(
                    "session.launch",
                    session.id.as_str(),
                    AuditOutcome::Succeeded,
                );
                Ok(session)
            }
            Err(error) => {
                let promoted = {
                    let mut registry = self.lock_registry()?;
                    let session = registry.sessions.get_mut(session_id).ok_or_else(|| {
                        ApplicationError::new(ErrorCode::NotFound, "session not found")
                    })?;
                    session.fail(
                        format!("{:?}", error.code()).to_ascii_uppercase(),
                        error.message(),
                        self.clock.now_ms(),
                    )?;
                    registry.reservations.remove(session_id);
                    registry.scheduler.release(session_id)?
                };
                self.audit("session.launch", session_id.as_str(), AuditOutcome::Failed);
                if let Some(ticket) = promoted {
                    let _ = self.start_one(&ticket.session_id);
                }
                Err(error)
            }
        }
    }

    fn session(&self, id: &SessionId) -> ApplicationResult<Session> {
        self.lock_registry()?
            .sessions
            .get(id)
            .cloned()
            .ok_or_else(|| ApplicationError::new(ErrorCode::NotFound, "session not found"))
    }

    fn audit(&self, action: &str, entity_id: &str, outcome: AuditOutcome) {
        let _ = self.audit.record(AuditEvent {
            at_ms: self.clock.now_ms(),
            action: action.to_owned(),
            entity_id: entity_id.to_owned(),
            outcome,
        });
    }

    fn lock_operations(&self) -> ApplicationResult<MutexGuard<'_, ()>> {
        self.operations
            .lock()
            .map_err(|_| ApplicationError::poisoned("session operation"))
    }

    fn lock_registry(&self) -> ApplicationResult<MutexGuard<'_, SessionRegistry>> {
        self.registry
            .lock()
            .map_err(|_| ApplicationError::poisoned("session registry"))
    }
}
