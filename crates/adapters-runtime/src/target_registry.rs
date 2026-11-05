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
        if !self.attached.contains_key(session) {
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
        let second = state.reload("s").unwrap().unwrap();
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
