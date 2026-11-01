use super::*;
use std::{future::Future, time::Duration};

pub(super) struct HttpSource;
impl Source for HttpSource {
    fn fetch(
        &self,
        url: &str,
        output: &mut dyn Write,
        limit: u64,
        control: &OfficialRuntimeInstaller,
    ) -> PortResult<String> {
        if !allowed_url(url) {
            return Err(bundle::untrusted("运行时下载地址不在固定官方来源中"));
        }
        control.checkpoint()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| internal("无法创建运行时下载任务"))?;
        let result = runtime.block_on(async {
            let client = reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(600))
                .user_agent("Brows-runtime-installer/0.1")
                .build()
                .map_err(|_| unavailable())?;
            let mut response = cancellable(client.get(url).send(), control).await?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(unavailable());
            }
            let total = response.content_length();
            if total.is_some_and(|n| n > limit) {
                return Err(invalid_data("浏览器下载响应超过大小限制"));
            }
            control.reset_bytes(total);
            let mut received = 0u64;
            let mut hash = Sha256::new();
            while let Some(bytes) = cancellable(response.chunk(), control).await? {
                control.checkpoint()?;
                received = received
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| invalid_data("download size overflow"))?;
                if received > limit || total.is_some_and(|n| received > n) {
                    return Err(invalid_data("浏览器下载超过大小限制"));
                }
                output
                    .write_all(&bytes)
                    .map_err(|_| internal("无法保存浏览器下载内容"))?;
                hash.update(&bytes);
                control.bytes(bytes.len());
            }
            if received == 0 || total.is_some_and(|n| n != received) {
                return Err(invalid_data("浏览器下载内容不完整"));
            }
            Ok(format!("{:x}", hash.finalize()))
        });
        // Only DNS resolver work may remain in the runtime's blocking pool;
        // it never owns archive files, profiles or the installation directory.
        runtime.shutdown_timeout(Duration::from_secs(1));
        result
    }
}
async fn cancellable<T>(
    future: impl Future<Output = Result<T, reqwest::Error>>,
    control: &OfficialRuntimeInstaller,
) -> PortResult<T> {
    tokio::pin!(future);
    loop {
        control.checkpoint()?;
        tokio::select! {
            result = &mut future => { control.checkpoint()?; return result.map_err(|_| unavailable()); }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}
fn unavailable() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "无法下载官方浏览器，请检查网络并重试；仅接受固定 HTTPS 来源",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_network_reads_are_cancelled_without_waiting_for_the_request_timeout() {
        let (installer, root) = super::super::tests::fixture("http-cancel");
        let lease = installer.begin().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                let cancel = async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    installer.cancel().unwrap();
                };
                let read = cancellable(
                    std::future::pending::<Result<(), reqwest::Error>>(),
                    &installer,
                );
                let (result, ()) = tokio::join!(read, cancel);
                result.unwrap_err()
            })
            .await
            .unwrap()
        });
        assert_eq!(error.kind, PortErrorKind::Cancelled);
        drop(lease);
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
}
