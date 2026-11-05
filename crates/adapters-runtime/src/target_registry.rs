//! Bounded target identity and generation tracking, separate from protocol I/O.
use super::internal;
use brows_application::PortResult;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub(super) const MAX_TARGETS: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct Target {
    pub session: String,
    pub kind: String,
    generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct TargetJob {
    pub id: String,
    pub target: Target,
    pub extension: bool,
    pub gate: Arc<Mutex<()>>,
}

#[derive(Debug, Default)]
pub(super) struct Targets {
    configured: BTreeMap<String, Target>,
    attached: BTreeMap<String, TargetJob>,
    dormant: BTreeSet<String>,
    generation: u64,
    worker_versions: BTreeMap<String, BTreeSet<String>>,
    retired_workers: BTreeSet<String>,
}

impl Targets {
    fn next_generation(&mut self) -> PortResult<u64> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| internal("target generation exhausted"))?;
        Ok(self.generation)
    }
    pub fn attach(
        &mut self,
        id: &str,
        session: &str,
        kind: &str,
        extension: bool,
    ) -> PortResult<Option<TargetJob>> {
        if kind == "service_worker" && self.retired_workers.contains(id) {
            return Ok(None);
        }
        if self.attached.contains_key(session) {
            return Ok(None);
        }
        if self.attached.len() >= MAX_TARGETS
            || id.len() > 128
            || session.len() > 128
            || kind.len() > 64
            || self.attached.values().any(|job| job.id == id)
        {
            return Err(internal("target identity or capacity violation"));
        }
        let job = TargetJob {
            id: id.to_owned(),
            target: Target {
                session: session.to_owned(),
                kind: kind.to_owned(),
                generation: self.next_generation()?,
            },
            extension,
            gate: Arc::new(Mutex::new(())),
        };
        self.attached.insert(session.to_owned(), job.clone());
        Ok(Some(job))
    }
    pub fn is_current(&self, job: &TargetJob) -> bool {
        self.target_current(&job.target)
            && self
                .attached
                .get(&job.target.session)
                .is_some_and(|current| {
                    current.target.generation == job.target.generation && current.id == job.id
                })
    }
    pub fn target_current(&self, target: &Target) -> bool {
        !self.dormant.contains(&target.session)
            && self
                .attached
                .get(&target.session)
                .is_some_and(|current| current.target.generation == target.generation)
    }
    pub fn ready(&mut self, job: &TargetJob) -> bool {
        if !self.is_current(job) {
            return false;
        }
        self.configured.insert(job.id.clone(), job.target.clone());
        true
    }
    pub fn detach(&mut self, session: &str) {
        self.attached.remove(session);
        self.dormant.remove(session);
        self.configured
            .retain(|_, target| target.session != session);
    }
    pub fn suspend(&mut self, session: &str) -> PortResult<()> {
        if self.attached.contains_key(session) {
            let generation = self.next_generation()?;
            self.attached.get_mut(session).unwrap().target.generation = generation;
            self.dormant.insert(session.to_owned());
            self.configured
                .retain(|_, target| target.session != session);
        }
        Ok(())
    }
    pub fn reload(&mut self, session: &str) -> PortResult<Option<TargetJob>> {
        // WorkerReadyForInspection also emits this notification on initial
        // startup. Only a previously stopped generation needs a new job.
        if !self.attached.contains_key(session) || !self.dormant.contains(session) {
            return Ok(None);
        }
        let generation = self.next_generation()?;
        let job = self.attached.get_mut(session).unwrap();
        job.target.generation = generation;
        let job = job.clone();
        self.dormant.remove(session);
        self.configured
            .retain(|_, target| target.session != session);
        Ok(Some(job))
    }
    // A redundant version cannot run again. Stopped versions are deliberately
    // retained: they can restart and must still receive policy before execution.
    pub fn worker_version(
        &mut self,
        version: &str,
        target: Option<&str>,
        redundant: bool,
    ) -> PortResult<Vec<String>> {
        const LIMIT: usize = MAX_TARGETS * 4;
        if version.is_empty()
            || version.len() > 128
            || target.is_some_and(|id| id.is_empty() || id.len() > 128)
        {
            return Err(internal("invalid Service Worker version identity"));
        }
        if let Some(id) = target {
            self.worker_versions
                .entry(version.to_owned())
                .or_default()
                .insert(id.to_owned());
        }
        if self
            .worker_versions
            .values()
            .map(BTreeSet::len)
            .sum::<usize>()
            > LIMIT
        {
            return Err(internal("Service Worker version capacity exceeded"));
        }
        if !redundant {
            return Ok(Vec::new());
        }
        let ids = self.worker_versions.remove(version).unwrap_or_default();
        let mut sessions = Vec::new();
        for id in ids {
            self.retired_workers.insert(id.clone());
            if self.retired_workers.len() > LIMIT {
                return Err(internal("retired Service Worker capacity exceeded"));
            }
            sessions.extend(
                self.attached
                    .values()
                    .filter(|job| job.id == id && job.target.kind == "service_worker")
                    .map(|job| job.target.session.clone()),
            );
        }
        for session in &sessions {
            self.detach(session);
        }
        Ok(sessions)
    }
    pub fn get(&self, id: &str) -> Option<&Target> {
        self.configured.get(id)
    }
    pub fn snapshot(&self) -> Vec<Target> {
        self.configured.values().cloned().collect()
    }
    pub fn current_values<T>(&self, values: Vec<(Target, T)>) -> Vec<T> {
        values
            .into_iter()
            .filter(|(target, _)| self.target_current(target))
            .map(|(_, value)| value)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_workers_leave_observation_and_old_jobs_cannot_republish() {
        let mut state = Targets::default();
        let first = state
            .attach("target", "session", "service_worker", false)
            .unwrap()
            .unwrap();
        assert!(state.ready(&first));
        state.suspend("session").unwrap();
        assert!(state.snapshot().is_empty());
        assert!(!state.ready(&first));
        let restarted = state.reload("session").unwrap().unwrap();
        assert_eq!(restarted.id, first.id);
        assert_eq!(restarted.target.kind, "service_worker");
        assert!(!state.ready(&first));
        assert!(state.ready(&restarted));
        assert_eq!(state.snapshot().len(), 1);
    }
    #[test]
    fn superseded_restarts_and_detached_sessions_are_invalidated() {
        let mut state = Targets::default();
        let first = state.attach("t", "s", "worker", false).unwrap().unwrap();
        state.suspend("s").unwrap();
        let second = state.reload("s").unwrap().unwrap();
        state.suspend("s").unwrap();
        let third = state.reload("s").unwrap().unwrap();
        assert!(!state.ready(&first));
        assert!(!state.ready(&second));
        assert!(state.ready(&third));
        state.detach("s");
        assert!(!state.is_current(&third));
        assert!(state.snapshot().is_empty());
        assert!(state.reload("s").unwrap().is_none());
    }
    #[test]
    fn initial_and_duplicate_ready_notifications_preserve_the_current_job() {
        let mut state = Targets::default();
        let initial = state
            .attach("t", "s", "service_worker", false)
            .unwrap()
            .unwrap();
        assert!(state.reload("s").unwrap().is_none());
        assert!(state.is_current(&initial));
        assert!(state.ready(&initial));
        assert!(state.reload("s").unwrap().is_none());
        assert_eq!(state.snapshot().len(), 1);
        state.suspend("s").unwrap();
        let restarted = state.reload("s").unwrap().unwrap();
        assert!(!state.is_current(&initial));
        assert!(state.reload("s").unwrap().is_none());
        assert!(state.is_current(&restarted));
        assert!(state.ready(&restarted));
    }
    #[test]
    fn completed_reads_are_revalidated_after_later_targets_are_read() {
        let mut state = Targets::default();
        let first = state.attach("a", "sa", "worker", false).unwrap().unwrap();
        let second = state.attach("b", "sb", "page", false).unwrap().unwrap();
        assert!(state.ready(&first));
        assert!(state.ready(&second));
        let reads = vec![
            (first.target.clone(), "old a"),
            (second.target.clone(), "b"),
        ];
        // The first read succeeded before a later read blocked. Its Worker
        // then restarted before the whole observation was ready to return.
        state.suspend("sa").unwrap();
        let restarted = state.reload("sa").unwrap().unwrap();
        assert!(state.ready(&restarted));
        assert_eq!(state.current_values(reads), vec!["b"]);
        assert_eq!(
            state.current_values(vec![(restarted.target, "new a")]),
            vec!["new a"]
        );
    }
    #[test]
    fn reused_target_and_session_ids_do_not_restore_detached_readings() {
        let mut state = Targets::default();
        let old = state.attach("t", "s", "worker", false).unwrap().unwrap();
        assert!(state.ready(&old));
        state.detach("s");
        let new = state.attach("t", "s", "worker", false).unwrap().unwrap();
        assert!(state.ready(&new));
        assert!(state.current_values(vec![(old.target, "stale")]).is_empty());
        assert_eq!(
            state.current_values(vec![(new.target, "current")]),
            vec!["current"]
        );
    }
    #[test]
    fn redundant_versions_invalidate_only_their_workers_even_without_a_target_id() {
        let mut state = Targets::default();
        let old = state
            .attach("old", "old-session", "service_worker", false)
            .unwrap()
            .unwrap();
        let new = state
            .attach("new", "new-session", "service_worker", false)
            .unwrap()
            .unwrap();
        let page = state
            .attach("page", "page-session", "page", false)
            .unwrap()
            .unwrap();
        assert!(state.ready(&old));
        assert!(state.ready(&new));
        assert!(state.ready(&page));
        state.worker_version("v1", Some("old"), false).unwrap();
        state.worker_version("v2", Some("new"), false).unwrap();
        assert!(state.target_current(&old.target));
        assert_eq!(
            state.worker_version("v1", None, true).unwrap(),
            vec!["old-session"]
        );
        assert!(!state.ready(&old));
        assert!(state.reload("old-session").unwrap().is_none());
        assert!(state
            .attach("old", "late-session", "service_worker", false)
            .unwrap()
            .is_none());
        assert!(state.target_current(&new.target));
        assert!(state.target_current(&page.target));
        assert_eq!(state.snapshot().len(), 2);
        assert!(state
            .worker_version("v1", Some("old"), true)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn version_tracking_is_bounded_and_never_retires_a_page() {
        let mut state = Targets::default();
        let page = state.attach("page", "s", "page", false).unwrap().unwrap();
        assert!(state.ready(&page));
        state.worker_version("v", Some("page"), true).unwrap();
        assert!(state.target_current(&page.target));
        assert!(state.worker_version("", Some("t"), false).is_err());
        for index in 0..MAX_TARGETS * 4 {
            state
                .worker_version(&index.to_string(), Some(&index.to_string()), false)
                .unwrap();
        }
        assert!(state
            .worker_version("overflow", Some("overflow"), false)
            .is_err());
    }
    #[test]
    fn capacity_duplicate_session_and_duplicate_owner_are_distinct() {
        let mut state = Targets::default();
        for index in 0..MAX_TARGETS {
            state
                .attach(&index.to_string(), &index.to_string(), "worker", false)
                .unwrap();
        }
        assert!(state.attach("0", "0", "worker", false).unwrap().is_none());
        assert!(state.attach("new", "new", "worker", false).is_err());
        state.detach("0");
        assert!(state.attach("1", "new", "worker", false).is_err());
        assert!(state
            .attach("new", "new", "worker", false)
            .unwrap()
            .is_some());
    }
}
