//! Correlate first-navigation start, independently of the website's response.
use url::Url;

pub(super) fn navigation_reply_acknowledged(result: &serde_json::Value, frame: &str) -> bool {
    // errorText/isDownload describe the website outcome, not protocol failure.
    // Website success is verified separately, never inferred from this ack.
    result["frameId"].as_str() == Some(frame)
}

pub(super) struct NavigationStart {
    session: String,
    frame: String,
    url: Url,
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
    pub fn new(session: &str, frame: &str, url: &str) -> Option<Self> {
        let url = Url::parse(url).ok()?;
        if session.is_empty() || frame.is_empty() || !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        Some(Self {
            session: session.to_owned(),
            frame: frame.to_owned(),
            url,
            started: false,
        })
    }
    pub fn observe(&mut self, session: &str, frame: &str, url: &str) {
        if session == self.session
            && frame == self.frame
            && Url::parse(url).ok().as_ref() == Some(&self.url)
        {
            self.started = true;
        }
    }
    pub fn started(&self) -> bool {
        self.started
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            NavigationStart::new("session", "main", "https://example.test/private").unwrap();
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
        let mut state = NavigationStart::new("s", "f", "https://EXAMPLE.test:443").unwrap();
        state.observe("s", "f", "https://example.test/");
        assert!(state.started());
        assert!(NavigationStart::new("s", "f", "not-a-url").is_none());
        assert!(NavigationStart::new("s", "f", "file:///private").is_none());
        assert!(NavigationStart::new("s", "", "https://example.test/").is_none());
        assert!(!format!("{state:?}").contains("example.test"));
    }
}
