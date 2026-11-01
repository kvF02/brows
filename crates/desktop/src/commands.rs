use crate::state::AppState;
use brows_adapters_morelogin::{inspect_export as inspect_morelogin, MoreLoginPreview};
use brows_application::{
    persona_fingerprint::PersonaFingerprint, ApplicationResult, BackupExportSpec, BackupImportSpec,
    BackupService, DiagnosticRequest, DiagnosticRunner, LegacyProfileDataSummary, MigrationRecord,
    MigrationRecordStatus, SecretValue, SessionLaunchOutcome,
};
use brows_contracts::{
    BackupExportRequest, BackupImportRequest, BrowserBuildIdDto, BrowserRuntimeDto,
    BrowserSelectorDto, DiagnosticListRequest, DiagnosticReportDto, DiagnosticRunRequest,
    ErrorCode, ErrorDto, EventEnvelope, FingerprintPreviewDto, LegacyMigrationExecuteRequest,
    LegacyMigrationInspectRequest, LegacyMigrationInspectionDto, LegacyMigrationListRequest,
    LegacyMigrationRecordDto, LegacyMigrationRollbackRequest, LegacyMigrationStatusDto, LocaleDto,
    PersonaDto, PresetIdDto, ProfileCreateRequest, ProfileDraftDto, ProfileDto, ProfileIdRequest,
    ProfileListRequest, ProfileRevisionRequest, ProfileUpdateRequest, ProxyConnectivityCheckDto,
    ProxyDto, ProxyRemoveRequest, ProxySaveRequest, ProxyTestRequest, SessionCancelQueuedRequest,
    SessionDto, SessionLaunchRequest, SessionStopRequest, WindowDto,
};
use brows_domain::ProfileId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};

type IpcResult<T> = Result<T, ErrorDto>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupJobDto {
    id: String,
    profile_id: String,
    profile_name: String,
    status: &'static str,
    created_at: String,
    size_bytes: Option<u64>,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupImportResultDto {
    profile: ProfileDto,
    note: String,
}

fn backup_service(state: &AppState) -> IpcResult<&brows_adapters_backup::EncryptedBackupService> {
    state.backups.as_deref().ok_or_else(|| ErrorDto {
        code: ErrorCode::CapabilityUnsupported,
        message: "加密备份在当前运行模式下不可用".to_owned(),
        violations: Vec::new(),
    })
}

#[tauri::command]
pub(crate) fn backup_list(state: State<'_, AppState>) -> IpcResult<Vec<BackupJobDto>> {
    let entries = backup_service(&state)?
        .list_archives()
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    Ok(entries
        .into_iter()
        .map(|entry| BackupJobDto {
            id: format!("backup-{}", entry.name),
            profile_id: String::new(),
            profile_name: "加密归档".to_owned(),
            status: "completed",
            created_at: entry
                .modified_at_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            size_bytes: Some(entry.size_bytes),
            note: Some(format!("文件：{}；代理凭据未导出", entry.name)),
        })
        .collect())
}

#[tauri::command]
pub(crate) fn backup_export(
    state: State<'_, AppState>,
    request: BackupExportRequest,
) -> IpcResult<BackupJobDto> {
    let profile = ipc(state.profiles.get(ProfileIdRequest {
        profile_id: request.profile_id.clone(),
    }))?;
    let profile_name = profile.name.clone();
    let profile: brows_domain::ProfileSpec =
        profile
            .try_into()
            .map_err(|error: brows_contracts::ContractError| ErrorDto {
                code: ErrorCode::ValidationFailed,
                message: error.reason,
                violations: Vec::new(),
            })?;
    let password = SecretValue::new(request.password.expose().as_bytes().to_vec())
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    backup_service(&state)?
        .export(&BackupExportSpec {
            destination_name: request.destination_name.clone(),
            profile,
            password,
            include_proxy_credential: request.include_proxy_credential,
        })
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    let size_bytes = backup_service(&state)?
        .archive_size(&request.destination_name)
        .ok();
    Ok(BackupJobDto {
        id: format!("backup-{}", request.destination_name),
        profile_id: request.profile_id.as_str().to_owned(),
        profile_name,
        status: "completed",
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string(),
        size_bytes,
        note: Some("本地加密归档；代理凭据未导出".to_owned()),
    })
}

#[tauri::command]
pub(crate) fn backup_import(
    state: State<'_, AppState>,
    request: BackupImportRequest,
) -> IpcResult<BackupImportResultDto> {
    let password = SecretValue::new(request.password.expose().as_bytes().to_vec())
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    let payload = backup_service(&state)?
        .import(&BackupImportSpec {
            source_name: request.source_name,
            password,
        })
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    let imported = state
        .profiles
        .create_imported(payload.profile)
        .map_err(|error| error.to_contract())?;
    if let Err(error) = backup_service(&state)?
        .install_restored_data(imported.id.as_str(), &payload.restored_data_dir)
    {
        backup_service(&state)?.discard_restored_data(&payload.restored_data_dir);
        let _ = state.profiles.archive(ProfileRevisionRequest {
            profile_id: imported.id.clone(),
            expected_revision: imported.revision,
        });
        return Err(brows_application::ApplicationError::from(error).to_contract());
    }
    Ok(BackupImportResultDto {
        profile: imported,
        note: "已恢复为新的环境 UUID，未覆盖现有环境".to_owned(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FingerprintPreviewRequest {
    runtime_id: String,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FingerprintProbeRequest {
    runtime_id: String,
}

#[tauri::command]
pub(crate) async fn probe_runtime_fingerprint(
    state: State<'_, AppState>,
    request: FingerprintProbeRequest,
) -> IpcResult<brows_contracts::FingerprintProbeResultDto> {
    let id =
        brows_domain::BrowserBuildId::new(request.runtime_id).map_err(ErrorDto::from_domain)?;
    let browsers = state.browsers.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || browsers.probe_fingerprint_support(&id))
            .await
            .map_err(|_| ErrorDto {
                code: ErrorCode::Internal,
                message: "指纹能力检测任务未能完成".to_owned(),
                violations: Vec::new(),
            })?
            .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    Ok(brows_contracts::FingerprintProbeResultDto {
        runtime: (&result.build).into(),
        detail: result.detail,
    })
}

#[tauri::command]
pub(crate) fn fingerprint_preview(
    state: State<'_, AppState>,
    request: FingerprintPreviewRequest,
) -> IpcResult<FingerprintPreviewDto> {
    preview_fingerprint(state.browsers.as_ref(), request)
}

fn preview_fingerprint(
    browsers: &dyn brows_application::BrowserRuntime,
    request: FingerprintPreviewRequest,
) -> IpcResult<FingerprintPreviewDto> {
    let persona = fingerprint_persona(request.persona)?;
    let host_shared_fields = persona
        .host_shared_fields()
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<Vec<_>>();
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
    brows_domain::LocaleConfig {
        accept_languages: request.accept_languages.clone(),
        timezone: None,
    }
    .validate()
    .map_err(ErrorDto::from_domain)?;
    brows_domain::LaunchArgument::AcceptLanguages(request.accept_languages.clone())
        .validate()
        .map_err(ErrorDto::from_domain)?;
    brows_domain::WindowConfig {
        width: request.window_width,
        height: request.window_height,
        start_maximized: false,
    }
    .validate()
    .map_err(ErrorDto::from_domain)?;
    let runtime_id =
        brows_domain::BrowserBuildId::new(request.runtime_id).map_err(ErrorDto::from_domain)?;
    let build = browsers
        .resolve_build(&brows_domain::BrowserBuildSelector::Pinned(
            runtime_id.clone(),
        ))
        .map_err(|error| brows_application::ApplicationError::from(error).to_contract())?;
    build.validate().map_err(ErrorDto::from_domain)?;
    if build.id != runtime_id {
        return Err(ErrorDto {
            code: ErrorCode::RuntimeUntrusted,
            message: "browser runtime returned a different build than requested".to_owned(),
            violations: Vec::new(),
        });
    }
    if build.trust != brows_domain::RuntimeTrust::Verified {
        return Err(ErrorDto {
            code: ErrorCode::RuntimeUntrusted,
            message: "the selected browser runtime is not verified".to_owned(),
            violations: Vec::new(),
        });
    }
    let fingerprint =
        PersonaFingerprint::derive(&seedable, &build.version).map_err(ErrorDto::from_domain)?;
    let mut declared_only_fields = vec![
        "navigator".to_owned(),
        "screen".to_owned(),
        "webgl".to_owned(),
    ];
    if seedable.spoof_canvas {
        declared_only_fields.push("canvas".to_owned());
    }
    if seedable.spoof_audio {
        declared_only_fields.push("audio".to_owned());
    }
    let mut runtime_warnings = vec![
        "配置预览未启动浏览器，未观测页面实际指纹。".to_owned(),
        "MV3 扩展载入及各 API 覆盖尚未在所选运行时实测；生成配置不等于已生效。".to_owned(),
        "官方 Google Chrome 137+ 的 --load-extension 支持存在兼容限制；须验证运行时扩展加载能力。"
            .to_owned(),
    ];
    match build.capabilities.fingerprint_support {
        brows_domain::FingerprintSupport::Unverified => runtime_warnings.push(
            "运行时指纹能力未验证：可保存配置，但 SeedableNoise 启动前必须在设置中检测通过。".to_owned(),
        ),
        brows_domain::FingerprintSupport::Unsupported => runtime_warnings.push(
            "运行时指纹能力检测未通过：SeedableNoise 已阻止启动；请切换系统原生或检测通过的可信运行时。".to_owned(),
        ),
        brows_domain::FingerprintSupport::Supported => runtime_warnings.push(
            "运行时仅通过本地 document_start 样本检测，不代表所有 API、frame 或检测站均已通过。".to_owned(),
        ),
    }
    if !build.capabilities.supports_language {
        runtime_warnings
            .push("所选运行时未声明支持语言启动参数，网络请求语言可能与配置不同。".to_owned());
    }
    Ok(FingerprintPreviewDto {
        runtime_id: build.id.as_str().to_owned(),
        runtime_version: build.version,
        effective_user_agent: fingerprint.user_agent,
        effective_platform: fingerprint.platform_string.to_owned(),
        effective_languages: request.accept_languages,
        effective_window_width: request.window_width,
        effective_window_height: request.window_height,
        effective_screen_width: seedable.screen.width,
        effective_screen_height: seedable.screen.height,
        device_scale_factor_hundredths: seedable.screen.device_scale_factor_hundredths,
        effective_webgl_vendor: fingerprint.webgl_vendor,
        effective_webgl_renderer: fingerprint.webgl_renderer,
        hardware_concurrency: fingerprint.hardware_concurrency,
        device_memory_gb: fingerprint.device_memory_gb,
        declared_only_fields,
        host_shared_fields,
        runtime_warnings,
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
    use brows_application::memory::{InMemoryBrowserRuntime, ManualClock};
    use brows_contracts::{ErrorCode, ProfileListRequest};
    use std::{path::PathBuf, sync::Arc};

    fn preview_request() -> FingerprintPreviewRequest {
        FingerprintPreviewRequest {
            runtime_id: "chosen-runtime".to_owned(),
            persona: PersonaDto::SeedableNoise {
                seed: "preview-stable-seed".to_owned(),
                platform: brows_contracts::PlatformDto::Windows,
                hardware_concurrency: 8,
                device_memory: brows_contracts::DeviceMemoryDto::Half,
                touch_points: 0,
                screen: brows_contracts::ScreenConfigDto {
                    width: 1920,
                    height: 1080,
                    color_depth: 24,
                    device_scale_factor_hundredths: 125,
                },
                webgl_vendor: String::new(),
                webgl_renderer: String::new(),
                spoof_canvas: true,
                spoof_audio: false,
            },
            accept_languages: vec!["de-DE".to_owned(), "de".to_owned()],
            window_width: 1200,
            window_height: 800,
        }
    }

    fn preview_build(id: &str, version: &str, installed_at_ms: u64) -> brows_domain::BrowserBuild {
        brows_domain::BrowserBuild {
            id: brows_domain::BrowserBuildId::new(id).unwrap(),
            provider: "Google Chrome".to_owned(),
            version: version.to_owned(),
            channel: brows_domain::BrowserChannel::Stable,
            executable_path: PathBuf::from("C:\\Brows\\chrome.exe"),
            sha256: "a".repeat(64),
            publisher: "Google LLC".to_owned(),
            trust: brows_domain::RuntimeTrust::Verified,
            capabilities: brows_domain::BrowserCapabilities {
                supports_language: true,
                ..brows_domain::BrowserCapabilities::default()
            },
            installed_at_ms,
        }
    }

    fn preview_runtime() -> InMemoryBrowserRuntime {
        let runtime = InMemoryBrowserRuntime::new(Arc::new(ManualClock::new(100)));
        runtime
            .insert_build(preview_build("chosen-runtime", "149.0.1.2", 1))
            .unwrap();
        runtime
            .insert_build(preview_build("newer-runtime", "150.0.9.9", 99))
            .unwrap();
        runtime
    }

    #[test]
    fn fingerprint_preview_uses_selected_runtime_and_actual_profile_settings() {
        let runtime = preview_runtime();
        let preview = preview_fingerprint(&runtime, preview_request()).unwrap();
        assert_eq!(preview.runtime_id, "chosen-runtime");
        assert_eq!(preview.runtime_version, "149.0.1.2");
        assert!(preview.effective_user_agent.contains("Chrome/149.0.0.0"));
        assert_eq!(preview.effective_languages, ["de-DE", "de"]);
        assert_eq!(
            (
                preview.effective_window_width,
                preview.effective_window_height
            ),
            (1200, 800)
        );
        assert_eq!(
            (
                preview.effective_screen_width,
                preview.effective_screen_height
            ),
            (1920, 1080)
        );
        assert_eq!(preview.device_scale_factor_hundredths, 125);
        assert_eq!(preview.device_memory_gb, "0.5");
        assert_eq!(
            preview,
            preview_fingerprint(&runtime, preview_request()).unwrap()
        );
        assert!(runtime.running_plans().unwrap().is_empty());
    }

    #[test]
    fn fingerprint_preview_exposes_partial_coverage_and_runtime_limitations() {
        let runtime = preview_runtime();
        let preview = preview_fingerprint(&runtime, preview_request()).unwrap();
        for field in ["navigator", "screen", "webgl", "canvas"] {
            assert!(preview
                .declared_only_fields
                .iter()
                .any(|value| value == field));
        }
        assert!(!preview
            .declared_only_fields
            .iter()
            .any(|value| value == "audio"));
        for field in [
            "timezone",
            "client_hints",
            "workers",
            "webgl_pixels",
            "audio",
        ] {
            assert!(preview
                .host_shared_fields
                .iter()
                .any(|value| value == field));
        }
        assert!(preview
            .runtime_warnings
            .iter()
            .any(|warning| warning.contains("未观测")));
        assert!(preview
            .runtime_warnings
            .iter()
            .any(|warning| warning.contains("137+")));
    }

    #[test]
    fn fingerprint_preview_rejects_invalid_locale_window_and_missing_runtime() {
        let runtime = preview_runtime();
        let mut request = preview_request();
        request.accept_languages = Vec::new();
        assert_eq!(
            preview_fingerprint(&runtime, request).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        let mut request = preview_request();
        request.window_width = 100;
        assert_eq!(
            preview_fingerprint(&runtime, request).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        let mut request = preview_request();
        request.runtime_id = "unknown-runtime".to_owned();
        assert!(preview_fingerprint(&runtime, request).is_err());
    }

    #[test]
    fn fingerprint_preview_rejects_untrusted_and_non_numeric_runtimes() {
        let runtime = preview_runtime();
        let mut build = preview_build("chosen-runtime", "149.0.1.2", 1);
        build.trust = brows_domain::RuntimeTrust::Revoked;
        runtime.insert_build(build).unwrap();
        assert_eq!(
            preview_fingerprint(&runtime, preview_request())
                .unwrap_err()
                .code,
            ErrorCode::RuntimeUntrusted
        );
        runtime
            .insert_build(preview_build("chosen-runtime", "installed", 1))
            .unwrap();
        assert_eq!(
            preview_fingerprint(&runtime, preview_request())
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );
    }

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
