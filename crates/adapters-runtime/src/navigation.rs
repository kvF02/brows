//! Correlate first-navigation start, independently of the website's response.
use url::Url;

#[cfg(any(test, all(windows, debug_assertions)))]
pub(super) fn document_result_code(result: &serde_json::Value) -> Option<&str> {
    result["errorText"].as_str().filter(|code| {
        code.len() <= 64
            && code.starts_with("net::ERR_")
            && !code[9..].is_empty()
            && code[9..]
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

#[cfg(any(test, all(windows, debug_assertions)))]
pub(super) fn document_url_kind(url: &str) -> &'static str {
    if url == "about:blank" {
        "blank"
    } else if url.starts_with("chrome-error://") {
        "browser-error"
    } else if url.starts_with("http://") || url.starts_with("https://") {
        "web"
    } else {
        "other"
    }
}

pub(super) fn navigation_reply_acknowledged(result: &serde_json::Value, frame: &str) -> bool {
    // errorText/isDownload describe the website outcome, not protocol failure.
    // Website success is verified separately, never inferred from this ack.
    result["frameId"].as_str() == Some(frame)
}

pub(super) struct NavigationStart {
    session: String,
    frame: String,
    url: Url,
    require_network: bool,
    started: bool,
}
impl std::fmt::Debug for NavigationStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavigationStart")
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}
impl NavigationStart {
    #[cfg(windows)]
    pub fn requires_network_proof(url: &str) -> bool {
        if !cfg!(debug_assertions) || std::env::var_os("BROWS_ACCEPTANCE_BROWSER").is_none() {
            return false;
        }
        Url::parse(url).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && match url.host() {
                    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                    Some(url::Host::Domain(host)) => host == "localhost",
                    None => false,
                }
        })
    }
    pub fn new(session: &str, frame: &str, url: &str, require_network: bool) -> Option<Self> {
        let url = Url::parse(url).ok()?;
        if session.is_empty() || frame.is_empty() || !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        Some(Self {
            session: session.to_owned(),
            frame: frame.to_owned(),
            url,
            require_network,
            started: false,
        })
    }
    pub fn observe(&mut self, session: &str, frame: &str, url: &str) {
        if !self.require_network
            && session == self.session
            && frame == self.frame
            && Url::parse(url).ok().as_ref() == Some(&self.url)
        {
            self.started = true;
        }
    }
    pub fn started(&self) -> bool {
        self.started
    }

    #[cfg(any(windows, test))]
    pub fn observe_document_request(&mut self, event: &serde_json::Value) {
        let params = &event["params"];
        if !self.require_network || params["type"] != "Document" {
            return;
        }
        // The fragment is client-side and may be absent from the network event.
        let without_fragment = |url: &Url| {
            let mut url = url.clone();
            url.set_fragment(None);
            url
        };
        if event["sessionId"].as_str() == Some(&self.session)
            && params["frameId"].as_str() == Some(&self.frame)
            && params["request"]["url"]
                .as_str()
                .and_then(|url| Url::parse(url).ok())
                .is_some_and(|url| without_fragment(&url) == without_fragment(&self.url))
        {
            self.started = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_diagnostics_keep_fixed_codes_and_url_classes_without_private_values() {
        for code in ["net::ERR_ABORTED", "net::ERR_HTTP2_PROTOCOL_ERROR"] {
            assert_eq!(
                document_result_code(&serde_json::json!({"errorText":code})),
                Some(code)
            );
        }
        for code in [
            "net::ERR_",
            "net::ERR_ABORTED https://private.test/?token=secret",
            "anything",
        ] {
            assert!(document_result_code(&serde_json::json!({"errorText":code})).is_none());
        }
        assert_eq!(
            document_url_kind("https://user:secret@private.test/path"),
            "web"
        );
        assert_eq!(document_url_kind("about:blank"), "blank");
        assert_eq!(
            document_url_kind("chrome-error://chromewebdata/"),
            "browser-error"
        );
    }
    #[test]
    fn website_errors_acknowledge_the_request_without_claiming_a_loaded_document() {
        for outcome in [
            serde_json::json!({"frameId":"main","errorText":"net::ERR_ABORTED"}),
            serde_json::json!({"frameId":"main","errorText":"net::ERR_CERT_AUTHORITY_INVALID"}),
            serde_json::json!({"frameId":"main","isDownload":true}),
        ] {
            assert!(navigation_reply_acknowledged(&outcome, "main"));
            assert!(!navigation_reply_acknowledged(&outcome, "other"));
        }
        assert!(!navigation_reply_acknowledged(
            &serde_json::json!({}),
            "main"
        ));
    }
    #[test]
    fn only_the_requested_url_in_the_requested_main_frame_can_start_navigation() {
        let mut state =
            NavigationStart::new("session", "main", "https://example.test/private", false).unwrap();
        for (session, frame, url) in [
            ("other", "main", "https://example.test/private"),
            ("session", "child", "https://example.test/private"),
            ("session", "main", "about:blank"),
            ("session", "main", "https://example.test/another"),
            ("session", "main", "https://elsewhere.test/private"),
        ] {
            state.observe(session, frame, url);
            assert!(!state.started());
        }
        state.observe("session", "main", "https://example.test/private");
        assert!(state.started());
    }
    #[test]
    fn browser_url_normalization_does_not_lose_exact_origin_or_path_correlation() {
        let mut state = NavigationStart::new("s", "f", "https://EXAMPLE.test:443", false).unwrap();
        state.observe("s", "f", "https://example.test/");
        assert!(state.started());
        assert!(NavigationStart::new("s", "f", "not-a-url", false).is_none());
        assert!(NavigationStart::new("s", "f", "file:///private", false).is_none());
        assert!(NavigationStart::new("s", "", "https://example.test/", false).is_none());
        assert!(!format!("{state:?}").contains("example.test"));
    }

    #[test]
    fn only_the_main_document_network_event_proves_strict_startup() {
        let mut request =
            NavigationStart::new("s", "f", "http://localhost:3333/page#anchor", true).unwrap();
        request.observe("s", "f", "http://localhost:3333/page#anchor");
        assert!(!request.started());
        let event = |session, frame, kind, url| {
            serde_json::json!({
                "sessionId": session, "params": {"frameId": frame, "type": kind, "request": {"url": url}}
            })
        };
        for invalid in [
            event("other", "f", "Document", "http://localhost:3333/page"),
            event("s", "child", "Document", "http://localhost:3333/page"),
            event("s", "f", "Script", "http://localhost:3333/page"),
            event("s", "f", "Document", "http://localhost:3333/other"),
        ] {
            request.observe_document_request(&invalid);
            assert!(!request.started());
        }
        request.observe_document_request(&event(
            "s",
            "f",
            "Document",
            "http://localhost:3333/page",
        ));
        assert!(request.started());
    }
}
