use crate::state::AppState;
use brows_application::{ApplicationResult, SessionLaunchOutcome};
use brows_contracts::{
    ErrorDto, ProfileCreateRequest, ProfileDto, ProfileIdRequest, ProfileListRequest,
    ProfileRevisionRequest, ProfileUpdateRequest, SessionCancelQueuedRequest, SessionDto,
    SessionLaunchRequest, SessionStopRequest,
};
use tauri::State;

type IpcResult<T> = Result<T, ErrorDto>;

fn ipc<T>(result: ApplicationResult<T>) -> IpcResult<T> {
    result.map_err(|error| error.to_contract())
}

#[tauri::command]
pub(crate) fn profile_list(
    state: State<'_, AppState>,
    request: ProfileListRequest,
) -> IpcResult<Vec<ProfileDto>> {
    ipc(state.profiles.list(request))
}

#[tauri::command]
pub(crate) fn profile_get(
    state: State<'_, AppState>,
    request: ProfileIdRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.get(request))
}

#[tauri::command]
pub(crate) fn profile_create(
    state: State<'_, AppState>,
    request: ProfileCreateRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.create(request))
}

#[tauri::command]
pub(crate) fn profile_update(
    state: State<'_, AppState>,
    request: ProfileUpdateRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.update(request))
}

#[tauri::command]
pub(crate) fn profile_clone(
    state: State<'_, AppState>,
    request: ProfileIdRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.clone_profile(request))
}

#[tauri::command]
pub(crate) fn profile_archive(
    state: State<'_, AppState>,
    request: ProfileRevisionRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.archive(request))
}

#[tauri::command]
pub(crate) fn profile_restore(
    state: State<'_, AppState>,
    request: ProfileRevisionRequest,
) -> IpcResult<ProfileDto> {
    ipc(state.profiles.restore(request))
}

#[tauri::command]
pub(crate) fn session_launch(
    state: State<'_, AppState>,
    request: SessionLaunchRequest,
) -> IpcResult<SessionDto> {
    ipc(state.sessions.launch(request)).map(|outcome| match outcome {
        SessionLaunchOutcome::Started { session, .. }
        | SessionLaunchOutcome::Queued { session, .. } => session,
    })
}

#[tauri::command]
pub(crate) fn session_stop(
    state: State<'_, AppState>,
    request: SessionStopRequest,
) -> IpcResult<SessionDto> {
    ipc(state.sessions.stop(request))
}

#[tauri::command]
pub(crate) fn session_cancel_queued(
    state: State<'_, AppState>,
    request: SessionCancelQueuedRequest,
) -> IpcResult<SessionDto> {
    ipc(state.sessions.cancel_queued(request))
}

#[tauri::command]
pub(crate) fn session_list(state: State<'_, AppState>) -> IpcResult<Vec<SessionDto>> {
    ipc(state.sessions.list())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_contracts::{ErrorCode, ProfileListRequest};

    #[test]
    fn application_errors_are_exposed_as_stable_contract_errors() {
        let error = brows_application::ApplicationError::new(
            ErrorCode::ProfileLocked,
            "profile is already locked",
        );
        let mapped: IpcResult<()> = ipc(Err(error));
        let mapped = mapped.unwrap_err();
        assert_eq!(mapped.code, ErrorCode::ProfileLocked);
        assert_eq!(mapped.message, "profile is already locked");
    }

    #[test]
    fn invalid_list_request_keeps_validation_error_code() {
        let state = AppState::prototype(std::path::PathBuf::from("test-data")).unwrap();
        let error = ipc(state.profiles.list(ProfileListRequest {
            page: 0,
            ..ProfileListRequest::default()
        }))
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
    }
}
