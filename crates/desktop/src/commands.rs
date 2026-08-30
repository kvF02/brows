use crate::state::AppState;
use brows_adapters_morelogin::{inspect_export as inspect_morelogin, MoreLoginPreview};
use brows_application::{
    persona_fingerprint::PersonaFingerprint, ApplicationResult, DiagnosticRequest,
    DiagnosticRunner, LegacyProfileDataSummary, MigrationRecord, MigrationRecordStatus,
    SessionLaunchOutcome,
};
use brows_contracts::{
    BrowserBuildIdDto, BrowserRuntimeDto, BrowserSelectorDto, DiagnosticListRequest,
    DiagnosticReportDto, DiagnosticRunRequest, ErrorCode, ErrorDto, EventEnvelope,
    FingerprintPreviewDto, LegacyMigrationExecuteRequest, LegacyMigrationInspectRequest,
    LegacyMigrationInspectionDto, LegacyMigrationListRequest, LegacyMigrationRecordDto,
    LegacyMigrationRollbackRequest, LegacyMigrationStatusDto, LocaleDto, PersonaDto, PresetIdDto,
    ProfileCreateRequest, ProfileDraftDto, ProfileDto, ProfileIdRequest, ProfileListRequest,
    ProfileRevisionRequest, ProfileUpdateRequest, ProxyConnectivityCheckDto, ProxyDto,
    ProxyRemoveRequest, ProxySaveRequest, ProxyTestRequest, SessionCancelQueuedRequest, SessionDto,
    SessionLaunchRequest, SessionStopRequest, WindowDto,
};
use brows_domain::ProfileId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tauri::{Emitter, State};

type IpcResult<T> = Result<T, ErrorDto>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FingerprintPreviewRequest {
    persona: PersonaDto,
    accept_languages: Vec<String>,
    window_width: u32,
    window_height: u32,
}

#[tauri::command]
pub(crate) fn list_runtimes(state: State<'_, AppState>) -> IpcResult<Vec<BrowserRuntimeDto>> {
    let builds = state
        .browsers
        .list_builds()
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    Ok(builds.iter().map(Into::into).collect())
}

#[tauri::command]
pub(crate) fn fingerprint_preview(
    state: State<'_, AppState>,
    request: FingerprintPreviewRequest,
) -> IpcResult<FingerprintPreviewDto> {
    let persona = fingerprint_persona(request.persona)?;
    let brows_domain::FingerprintPersona::SeedableNoise(seedable) = persona else {
        return Err(ErrorDto {
            code: ErrorCode::ValidationFailed,
            message: "fingerprint preview requires a SeedableNoise persona".to_owned(),
            violations: Vec::new(),
        });
    };
    seedable.validate().map_err(|error| ErrorDto {
        code: ErrorCode::ValidationFailed,
        message: error.to_string(),
        violations: Vec::new(),
    })?;
    let fingerprint = PersonaFingerprint::derive(&seedable).map_err(|error| ErrorDto {
        code: ErrorCode::ValidationFailed,
        message: error.to_string(),
        violations: Vec::new(),
    })?;
    let build = state
        .browsers
        .list_builds()
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?
        .into_iter()
        .max_by_key(|build| build.installed_at_ms)
        .ok_or_else(|| ErrorDto {
            code: ErrorCode::CapabilityUnsupported,
            message: "fingerprint preview requires an installed browser runtime".to_owned(),
            violations: Vec::new(),
        })?;
    let mut declared_only_fields = Vec::new();
    if seedable.spoof_canvas {
        declared_only_fields.push("canvas".to_owned());
    }
    if seedable.spoof_audio {
        declared_only_fields.push("audio".to_owned());
    }
    Ok(FingerprintPreviewDto {
        effective_user_agent: fingerprint.user_agent_for_chromium(&build.version),
        effective_platform: fingerprint.platform_string.to_owned(),
        effective_languages: request.accept_languages,
        effective_window_width: request.window_width,
        effective_window_height: request.window_height,
        effective_webgl_vendor: fingerprint.webgl_vendor,
        effective_webgl_renderer: fingerprint.webgl_renderer,
        hardware_concurrency: fingerprint.hardware_concurrency,
        device_memory_gb: fingerprint.device_memory_gb,
        declared_only_fields,
        host_shared_fields: Vec::new(),
    })
}

fn fingerprint_persona(value: PersonaDto) -> IpcResult<brows_domain::FingerprintPersona> {
    Ok(match value {
        PersonaDto::SystemNative {
            preserve_system_timezone,
            preserve_system_fonts,
            preserve_system_gpu,
            preserve_system_audio,
        } => brows_domain::FingerprintPersona::SystemNative(brows_domain::SystemNativePersona {
            preserve_system_timezone,
            preserve_system_fonts,
            preserve_system_gpu,
            preserve_system_audio,
        }),
        PersonaDto::SeedableNoise {
            seed,
            platform,
            hardware_concurrency,
            device_memory,
            touch_points,
            screen,
            webgl_vendor,
            webgl_renderer,
            spoof_canvas,
            spoof_audio,
        } => brows_domain::FingerprintPersona::SeedableNoise(brows_domain::SeedableNoisePersona {
            seed,
            platform: platform.into(),
            hardware_concurrency,
            device_memory: device_memory.into(),
            touch_points,
            screen: screen.into(),
            webgl: brows_domain::WebGLProfile {
                vendor: webgl_vendor,
                unmasked_renderer: webgl_renderer,
            },
            spoof_canvas,
            spoof_audio,
        }),
    })
}

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

fn migrations(state: &AppState) -> IpcResult<&brows_application::MigrationApplicationService> {
    state.migrations.as_deref().ok_or_else(|| ErrorDto {
        code: ErrorCode::CapabilityUnsupported,
        message: "完整环境迁移在当前运行模式下不可用".to_owned(),
        violations: Vec::new(),
    })
}

fn proxy_management(state: &AppState) -> IpcResult<&brows_application::ProxyApplicationService> {
    state.proxy_management.as_deref().ok_or_else(|| ErrorDto {
        code: ErrorCode::CapabilityUnsupported,
        message: "代理管理在当前运行模式下不可用".to_owned(),
        violations: Vec::new(),
    })
}

fn diagnostics(
    state: &AppState,
) -> IpcResult<&brows_adapters_diagnostic::LocalNetworkDiagnosticRunner> {
    state.diagnostics.as_deref().ok_or_else(|| ErrorDto {
        code: ErrorCode::CapabilityUnsupported,
        message: "诊断在当前运行模式下不可用".to_owned(),
        violations: Vec::new(),
    })
}

#[tauri::command]
pub(crate) fn diagnostic_run(
    state: State<'_, AppState>,
    request: DiagnosticRunRequest,
) -> IpcResult<DiagnosticReportDto> {
    let profile = ipc(state.profiles.get(ProfileIdRequest {
        profile_id: request.profile_id,
    }))?;
    let profile: brows_domain::ProfileSpec =
        profile
            .try_into()
            .map_err(|error: brows_contracts::ContractError| match error.field {
                Some(field) => ErrorDto::invalid_field(field, error.reason),
                None => ErrorDto {
                    code: ErrorCode::ValidationFailed,
                    message: error.reason,
                    violations: Vec::new(),
                },
            })?;
    let report = diagnostics(&state)?
        .run(&DiagnosticRequest {
            profile,
            session_id: None,
            include_external_checks: request.external,
        })
        .map_err(|error| ErrorDto {
            code: ErrorCode::Internal,
            message: error.to_string(),
            violations: Vec::new(),
        })?;
    Ok(DiagnosticReportDto::from(&report))
}

#[tauri::command]
pub(crate) fn diagnostic_list(
    state: State<'_, AppState>,
    request: DiagnosticListRequest,
) -> IpcResult<Vec<DiagnosticReportDto>> {
    let reports = diagnostics(&state)?.reports().map_err(|error| ErrorDto {
        code: ErrorCode::Internal,
        message: error.to_string(),
        violations: Vec::new(),
    })?;
    Ok(reports
        .into_iter()
        .filter(|report| {
            request
                .profile_id
                .as_ref()
                .is_none_or(|id| id.as_str() == report.profile_id.as_str())
        })
        .map(|report| DiagnosticReportDto::from(&report))
        .collect())
}

#[tauri::command]
pub(crate) fn proxy_list(state: State<'_, AppState>) -> IpcResult<Vec<ProxyDto>> {
    ipc(proxy_management(&state)?.list())
}

#[tauri::command]
pub(crate) fn proxy_save(
    state: State<'_, AppState>,
    request: ProxySaveRequest,
) -> IpcResult<ProxyDto> {
    ipc(proxy_management(&state)?.save(request))
}

#[tauri::command]
pub(crate) fn proxy_test(
    state: State<'_, AppState>,
    request: ProxyTestRequest,
) -> IpcResult<ProxyConnectivityCheckDto> {
    let proxy_id = brows_domain::ProxyId::new(request.proxy_id.as_str().to_owned())
        .map_err(ErrorDto::from_domain)?;
    let result = ipc(proxy_management(&state)?.test(proxy_id.clone()))?;
    Ok(ProxyConnectivityCheckDto {
        proxy_id: proxy_id.into(),
        reachable: result.reachable,
        latency_ms: result.latency_ms,
        detail: result.detail,
        exit_ip: result.exit_ip,
        exit_ip_checked: result.exit_ip_checked,
    })
}

#[tauri::command]
pub(crate) fn proxy_remove(
    state: State<'_, AppState>,
    request: ProxyRemoveRequest,
) -> IpcResult<()> {
    let proxy_id = brows_domain::ProxyId::new(request.proxy_id.as_str().to_owned())
        .map_err(ErrorDto::from_domain)?;
    ipc(proxy_management(&state)?.remove(proxy_id))
}

fn profile_id(value: &brows_contracts::ProfileIdDto) -> IpcResult<ProfileId> {
    ProfileId::new(value.as_str().to_owned()).map_err(ErrorDto::from_domain)
}

#[tauri::command]
pub(crate) fn legacy_migration_inspect(
    state: State<'_, AppState>,
    request: LegacyMigrationInspectRequest,
) -> IpcResult<LegacyMigrationInspectionDto> {
    let summary =
        ipc(migrations(&state)?
            .inspect(profile_id(&request.profile_id)?, request.source_profile_id))?;
    Ok(migration_inspection_dto(summary))
}

#[tauri::command]
pub(crate) fn legacy_migration_execute(
    state: State<'_, AppState>,
    request: LegacyMigrationExecuteRequest,
) -> IpcResult<LegacyMigrationRecordDto> {
    let record =
        ipc(migrations(&state)?
            .migrate(profile_id(&request.profile_id)?, request.source_profile_id))?;
    Ok(migration_record_dto(record))
}

#[tauri::command]
pub(crate) fn legacy_migration_rollback(
    state: State<'_, AppState>,
    request: LegacyMigrationRollbackRequest,
) -> IpcResult<LegacyMigrationRecordDto> {
    ipc(migrations(&state)?.rollback(request.migration_id)).map(migration_record_dto)
}

#[tauri::command]
pub(crate) fn legacy_migration_list(
    state: State<'_, AppState>,
    request: LegacyMigrationListRequest,
) -> IpcResult<Vec<LegacyMigrationRecordDto>> {
    ipc(migrations(&state)?.list(request.limit))
        .map(|records| records.into_iter().map(migration_record_dto).collect())
}

fn migration_inspection_dto(value: LegacyProfileDataSummary) -> LegacyMigrationInspectionDto {
    LegacyMigrationInspectionDto {
        source_profile_id: value.source_profile_id,
        file_count: value.file_count,
        total_bytes: value.total_bytes,
        has_history: value.has_history,
        has_cookies: value.has_cookies,
        has_login_data: value.has_login_data,
        has_local_storage: value.has_local_storage,
        has_indexed_db: value.has_indexed_db,
        source_may_be_running: value.source_may_be_running,
    }
}

fn migration_record_dto(value: MigrationRecord) -> LegacyMigrationRecordDto {
    let can_rollback = value.status == MigrationRecordStatus::Completed
        && value
            .backup_key
            .as_deref()
            .is_some_and(|key| !key.is_empty());
    LegacyMigrationRecordDto {
        id: value.id,
        profile_id: (&value.profile_id).into(),
        source_kind: value.source_kind,
        source_profile_id: value.source_profile_id,
        status: match value.status {
            MigrationRecordStatus::Running => LegacyMigrationStatusDto::Running,
            MigrationRecordStatus::Completed => LegacyMigrationStatusDto::Completed,
            MigrationRecordStatus::Failed => LegacyMigrationStatusDto::Failed,
            MigrationRecordStatus::RolledBack => LegacyMigrationStatusDto::RolledBack,
        },
        started_at_ms: value.started_at_ms,
        completed_at_ms: value.completed_at_ms,
        copied_files: value.copied_files,
        copied_bytes: value.copied_bytes,
        can_rollback,
        error_summary: value.error_summary,
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
    let id = profile_id(&request.profile_id)?;
    if state
        .sessions
        .has_active_profile(&id)
        .map_err(|error| error.to_contract())?
    {
        return Err(ErrorDto {
            code: ErrorCode::ProfileLocked,
            message: "stop the profile session before archiving".to_owned(),
            violations: Vec::new(),
        });
    }
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
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: SessionLaunchRequest,
) -> IpcResult<SessionDto> {
    let outcome = ipc(state.sessions.launch(request))?;
    let (session, event) = match outcome {
        SessionLaunchOutcome::Started { session, event } => (session, event),
        SessionLaunchOutcome::Queued { session, event, .. } => (session, event),
    };
    if let Ok(envelope) = EventEnvelope::new(format!("session-{}", uuid::Uuid::new_v4()), event) {
        let _ = app.emit("brows://session", envelope);
    }
    Ok(session)
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
