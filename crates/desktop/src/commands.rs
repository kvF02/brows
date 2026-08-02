use crate::morelogin::{inspect as inspect_morelogin, MoreLoginPreview};
use crate::state::AppState;
use brows_application::{ApplicationResult, SessionLaunchOutcome};
use brows_contracts::{
    BrowserBuildIdDto, BrowserSelectorDto, ErrorCode, ErrorDto, LocaleDto, PersonaDto, PresetIdDto,
    ProfileCreateRequest, ProfileDraftDto, ProfileDto, ProfileIdRequest, ProfileListRequest,
    ProfileRevisionRequest, ProfileUpdateRequest, SessionCancelQueuedRequest, SessionDto,
    SessionLaunchRequest, SessionStopRequest, WindowDto,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tauri::State;

type IpcResult<T> = Result<T, ErrorDto>;

fn ipc<T>(result: ApplicationResult<T>) -> IpcResult<T> {
    result.map_err(|error| error.to_contract())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoreLoginPreviewRequest {
    source_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoreLoginImportRequest {
    source_path: String,
    #[serde(default)]
    row_numbers: Vec<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoreLoginImportFailure {
    row_number: usize,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoreLoginImportResult {
    imported_count: usize,
    skipped_count: usize,
    failures: Vec<MoreLoginImportFailure>,
}

#[tauri::command]
pub(crate) fn morelogin_preview(request: MoreLoginPreviewRequest) -> IpcResult<MoreLoginPreview> {
    inspect_morelogin(&request.source_path).map_err(import_error)
}

#[tauri::command]
pub(crate) fn morelogin_import(
    state: State<'_, AppState>,
    request: MoreLoginImportRequest,
) -> IpcResult<MoreLoginImportResult> {
    let preview = inspect_morelogin(&request.source_path).map_err(import_error)?;
    let selected = request.row_numbers.into_iter().collect::<BTreeSet<_>>();
    let import_all = selected.is_empty();
    let mut imported_count = 0;
    let mut skipped_count = 0;
    let mut failures = Vec::new();

    for candidate in preview.candidates {
        if !import_all && !selected.contains(&candidate.row_number) {
            skipped_count += 1;
            continue;
        }
        let profile = ProfileDraftDto {
            name: candidate.name,
            tags: candidate.tags,
            preset_id: PresetIdDto::new("system-native").expect("static preset id is valid"),
            preset_version: 1,
            proxy_id: None,
            browser: BrowserSelectorDto::Pinned {
                build_id: BrowserBuildIdDto::new("google-chrome-stable-150")
                    .expect("static build id is valid"),
            },
            persona: PersonaDto::default(),
            locale: LocaleDto {
                accept_languages: vec!["zh-CN".to_owned(), "zh".to_owned()],
                timezone: None,
            },
            window: WindowDto {
                width: 1280,
                height: 800,
                start_maximized: false,
            },
            startup_urls: Vec::new(),
        };
        match state.profiles.create(ProfileCreateRequest { profile }) {
            Ok(_) => imported_count += 1,
            Err(error) => failures.push(MoreLoginImportFailure {
                row_number: candidate.row_number,
                reason: error.to_contract().message,
            }),
        }
    }
    Ok(MoreLoginImportResult {
        imported_count,
        skipped_count,
        failures,
    })
}

fn import_error(message: String) -> ErrorDto {
    ErrorDto {
        code: ErrorCode::ValidationFailed,
        message,
        violations: Vec::new(),
    }
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
