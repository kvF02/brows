#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod commands;
mod state;

use brows_contracts::SessionStateDto;
use commands::{
    backup_export, backup_import, backup_list, diagnostic_list, diagnostic_run,
    fingerprint_preview, legacy_migration_execute, legacy_migration_inspect, legacy_migration_list,
    legacy_migration_rollback, list_runtimes, morelogin_import, morelogin_preview,
    probe_runtime_fingerprint, profile_archive, profile_clone, profile_create, profile_get,
    profile_list, profile_restore, profile_update, proxy_list, proxy_remove, proxy_save,
    proxy_test, session_cancel_queued, session_launch, session_list, session_observe, session_stop,
};
use state::AppState;
use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

const TAURI_RUNTIME_MARKER_SCRIPT: &str = "window.__BROWS_IS_TAURI__ = true;";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_root = app.path().app_local_data_dir()?;
            let state = AppState::production(data_root)
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
            app.manage(state);

            let window = WebviewWindowBuilder::new(app, "main", main_window_url())
                .title("Brows")
                .inner_size(1280.0, 820.0)
                .min_inner_size(1040.0, 680.0)
                .on_navigation(is_allowed_navigation)
                // Inject a Tauri runtime marker via init_script so the frontend
                // can detect it's running inside the Tauri WebView, even when
                // the dev URL is loaded from an external Vite dev server. Do
                // not write Tauri's reserved __TAURI_INTERNALS__ object: the
                // framework owns it and uses it to expose the IPC bridge.
                .initialization_script(TAURI_RUNTIME_MARKER_SCRIPT);
            #[cfg(all(windows, feature = "webview-diagnostics"))]
            let window = window.additional_browser_args("--remote-debugging-port=9223");
            let window = window.build()?;
            window.show()?;
            window.set_focus()?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            profile_list,
            profile_get,
            profile_create,
            profile_update,
            profile_clone,
            profile_archive,
            profile_restore,
            morelogin_preview,
            morelogin_import,
            legacy_migration_inspect,
            legacy_migration_execute,
            legacy_migration_rollback,
            legacy_migration_list,
            proxy_list,
            proxy_save,
            proxy_test,
            proxy_remove,
            diagnostic_run,
            diagnostic_list,
            session_launch,
            session_stop,
            session_cancel_queued,
            session_list,
            session_observe,
            fingerprint_preview,
            probe_runtime_fingerprint,
            list_runtimes,
            backup_list,
            backup_export,
            backup_import,
        ])
        .build(tauri::generate_context!())
        .expect("Brows desktop runtime failed to build")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                cleanup_sessions(app.state::<AppState>().inner());
            }
        })
}

fn cleanup_sessions(state: &AppState) {
    let sessions = match state.sessions.list() {
        Ok(sessions) => sessions,
        Err(_) => return,
    };
    // Cancel queued work first.  Stopping an active session promotes the oldest
    // queued ticket synchronously; cancelling the snapshot's queued tickets
    // up front prevents a new browser from being launched during shutdown.
    for session in sessions
        .iter()
        .filter(|session| session.state == SessionStateDto::Queued)
    {
        let _ = state.sessions.stop(brows_contracts::SessionStopRequest {
            session_id: session.id.clone(),
        });
    }
    for session in sessions {
        if matches!(
            session.state,
            SessionStateDto::Starting | SessionStateDto::Running | SessionStateDto::Stopping
        ) {
            let _ = state.sessions.stop(brows_contracts::SessionStopRequest {
                session_id: session.id,
            });
        }
    }
}

fn main_window_url() -> WebviewUrl {
    #[cfg(debug_assertions)]
    {
        WebviewUrl::External(
            "http://127.0.0.1:4173"
                .parse()
                .expect("the static development URL is valid"),
        )
    }
    #[cfg(not(debug_assertions))]
    {
        WebviewUrl::App("index.html".into())
    }
}

fn is_allowed_navigation(url: &Url) -> bool {
    let packaged_asset = matches!(url.scheme(), "tauri" | "asset")
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"));
    if packaged_asset || (url.scheme() == "about" && url.path() == "blank") {
        return true;
    }

    cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(4173)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_allows_only_packaged_assets_and_exact_dev_origin() {
        assert!(is_allowed_navigation(
            &"http://tauri.localhost/index.html".parse().unwrap()
        ));
        assert!(is_allowed_navigation(
            &"tauri://localhost/".parse().unwrap()
        ));
        assert!(!is_allowed_navigation(
            &"https://example.com/".parse().unwrap()
        ));
        assert!(!is_allowed_navigation(
            &"file:///C:/Windows/System32/".parse().unwrap()
        ));

        let dev: Url = "http://127.0.0.1:4173/".parse().unwrap();
        assert_eq!(is_allowed_navigation(&dev), cfg!(debug_assertions));
        assert!(!is_allowed_navigation(
            &"http://127.0.0.1:4174/".parse().unwrap()
        ));
    }

    #[test]
    fn runtime_marker_does_not_overwrite_tauri_ipc_internals() {
        assert!(TAURI_RUNTIME_MARKER_SCRIPT.contains("__BROWS_IS_TAURI__"));
        assert!(!TAURI_RUNTIME_MARKER_SCRIPT.contains("__TAURI_INTERNALS__"));
    }

    #[test]
    fn development_origin_is_explicitly_authorized_for_ipc() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main-window.json")).unwrap();
        assert_eq!(capability["local"], true);
        assert_eq!(
            capability["remote"]["urls"],
            serde_json::json!(["http://127.0.0.1:4173/*"])
        );
    }
}
