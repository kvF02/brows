use crate::{ApplicationError, ApplicationResult};
use brows_contracts::ErrorCode;
use brows_domain::ProfileId;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

/// Shared by all use cases that read or mutate a profile's browser data.
/// Reservations fail immediately; no mutex is held during filesystem or process I/O.
#[derive(Debug, Default)]
pub struct ProfileOperations {
    reserved: Mutex<BTreeSet<ProfileId>>,
}

impl ProfileOperations {
    pub fn reserve(
        self: &Arc<Self>,
        profile_id: &ProfileId,
    ) -> ApplicationResult<ProfileReservation> {
        let mut reserved = self
            .reserved
            .lock()
            .map_err(|_| ApplicationError::poisoned("profile operations"))?;
        if !reserved.insert(profile_id.clone()) {
            return Err(ApplicationError::new(
                ErrorCode::ProfileLocked,
                "环境正在运行、排队或执行数据操作，请等待完成后重试",
            ));
        }
        Ok(ProfileReservation {
            operations: self.clone(),
            profile_id: profile_id.clone(),
        })
    }
}

/// A session retains its reservation while queued, running or stopping.
#[derive(Debug)]
pub struct ProfileReservation {
    operations: Arc<ProfileOperations>,
    profile_id: ProfileId,
}

impl Drop for ProfileReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = self.operations.reserved.lock() {
            reserved.remove(&self.profile_id);
        }
    }
}
