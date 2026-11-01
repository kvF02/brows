#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod backup_jobs;
mod commands;
#[cfg(test)]
mod native_acceptance;
mod state;
mod supervisor;

use commands::{
    backup_cancel, backup_cleanup, backup_export, backup_import, backup_list, diagnostic_list,
    diagnostic_run, fingerprint_preview, legacy_migration_execute, legacy_migration_inspect,
    legacy_migration_list, legacy_migration_rollback, list_runtimes, measure_runtime_fingerprint,
    morelogin_import, morelogin_preview, probe_runtime_fingerprint, profile_archive, profile_clone,
    profile_create, profile_get, profile_list, profile_restore, profile_update, proxy_list,
    proxy_remove, proxy_save, proxy_test, runtime_install_cancel, runtime_install_official,
    runtime_install_status, session_cancel_queued, session_launch, session_list, session_stop,
    settings_get, settings_update,
};
use state::AppState;
use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

const TAURI_RUNTIME_MARKER_SCRIPT: &str = "window.__BROWS_IS_TAURI__ = true;";

fn startup_trace(stage: &'static str) {
    if std::env::var_os("BROWS_STARTUP_DIAGNOSTICS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        eprintln!("[brows-startup] {stage}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            startup_trace("setup-started");
            let data_root = app.path().app_local_data_dir()?;
            let state = AppState::production(data_root)
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
            let supervisor =
                supervisor::SessionSupervisor::start(app.handle().clone(), state.sessions.clone())?;
            app.manage(state);
            app.manage(supervisor);
            startup_trace("services-ready");

            let window = WebviewWindowBuilder::new(app, "main", main_window_url())
                .title(app.config().product_name.as_deref().unwrap_or("Brows"))
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
            startup_trace("window-built");
            window.show()?;
            window.set_focus()?;
            startup_trace("ready");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            settings_get,
            settings_update,
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
            fingerprint_preview,
            probe_runtime_fingerprint,
            measure_runtime_fingerprint,
            list_runtimes,
            runtime_install_official,
            runtime_install_status,
            runtime_install_cancel,
            backup_list,
            backup_cleanup,
            backup_cancel,
            backup_export,
            backup_import,
        ])
        .build(tauri::generate_context!())
        .expect("Brows desktop runtime failed to build")
        .run(|app, event| {
            match &event {
                RunEvent::WindowEvent {
                    event: tauri::WindowEvent::CloseRequested { .. },
                    ..
                } => startup_trace("close-requested"),
                RunEvent::WindowEvent {
                    event: tauri::WindowEvent::Destroyed,
                    ..
                } => startup_trace("window-destroyed"),
                RunEvent::Exit => startup_trace("exit"),
                _ => {}
            }
            if let RunEvent::ExitRequested { .. } = event {
                startup_trace("exit-requested");
                stop_accepting_background_work(app.state::<AppState>().inner());
                startup_trace("admission-stopped");
                app.state::<supervisor::SessionSupervisor>().stop();
                startup_trace("supervisor-stopped");
                shutdown_services(app.state::<AppState>().inner());
                startup_trace("services-stopped");
            }
        })
}

fn stop_accepting_background_work(state: &AppState) {
    state.backup_jobs.stop_accepting();
    state.browsers.stop_accepting_probes();
    if let Some(installer) = &state.runtime_installer {
        installer.stop_accepting();
    }
}

fn shutdown_services(state: &AppState) {
    stop_accepting_background_work(state);
    let _ = state.sessions.shutdown();
    state.backup_jobs.shutdown();
    if let Some(installer) = &state.runtime_installer {
        installer.shutdown();
    }
    state.browsers.wait_for_probes();
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
    fn alpha_build_uses_its_own_data_namespace_without_overriding_security_settings() {
        let alpha: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.alpha.conf.json")).unwrap();
        let stable: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_ne!(alpha["identifier"], stable["identifier"]);
        assert_eq!(alpha["identifier"], "local-brows-desktop-alpha");
        assert_eq!(alpha["productName"], "Brows Alpha");
        assert_eq!(alpha.as_object().unwrap().len(), 3);
        assert!(alpha["app"].is_null());
    }

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
