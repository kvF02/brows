#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod commands;
mod state;

use commands::{
    diagnostic_list, diagnostic_run, legacy_migration_execute, legacy_migration_inspect,
    legacy_migration_list, legacy_migration_rollback, morelogin_import, morelogin_preview,
    profile_archive, profile_clone, profile_create, profile_get, profile_list, profile_restore,
    profile_update, proxy_list, proxy_remove, proxy_save, proxy_test, session_cancel_queued,
    session_launch, session_list, session_stop,
};
use state::AppState;
use tauri::{Manager, Url, WebviewUrl, WebviewWindowBuilder};

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
                .on_navigation(is_allowed_navigation);
            #[cfg(all(windows, feature = "webview-diagnostics"))]
            let window = window.additional_browser_args("--remote-debugging-port=9223");
            window.build()?;
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
        ])
        .run(tauri::generate_context!())
        .expect("Brows desktop runtime failed");
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
}
