//! Offline test of the SDK bridge, the equivalent of `pdrive selftest`: it loads the NativeAOT
//! library, creates a client, makes a call that goes through the HTTP callback of the host (which
//! rejects it, so nothing reaches the network), checks that the error comes back, then frees the
//! client.
//!
//! The native libraries (`libproton_drive_sdk.so` to link, `libproton_crypto.so` to load) are
//! the ones of `native/linux-x64/` (see `scripts/build-native.sh`), or of `PDSDK_LIB_DIR`.
//! The test is ignored by default so that a plain `cargo test` does not depend on the SDK
//! runtime; run it with:
//!
//!     cargo test -p pdsdk --test bridge -- --ignored

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pdsdk::proto::{AccountRequest, Any, HttpRequest};
use pdsdk::{BoxFuture, ClientOptions, DriveClient, Host, HttpReply, SdkError};

/// A host that is offline: every request is rejected, and counted.
#[derive(Default)]
struct OfflineHost {
    http_calls: Arc<AtomicUsize>,
    last_url: std::sync::Mutex<String>,
}

impl Host for OfflineHost {
    fn http(&self, request: HttpRequest, _body: Option<Vec<u8>>) -> BoxFuture<'_, Result<HttpReply, String>> {
        self.http_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_url.lock().unwrap() = request.url.clone();
        Box::pin(async move { Err(format!("offline: {} {}", request.method, request.url)) })
    }

    fn account(&self, _request: AccountRequest) -> BoxFuture<'_, Result<Any, String>> {
        Box::pin(async { Err("offline".to_owned()) })
    }
}

#[tokio::test]
#[ignore = "needs the native libraries (native/linux-x64); run with --ignored"]
async fn client_lifecycle_and_error_propagation_with_an_offline_host() {
    assert!(Path::new(pdsdk::DEFAULT_LIB_DIR).join("libproton_crypto.so").is_file(), "libproton_crypto.so not found in {}", pdsdk::DEFAULT_LIB_DIR);
    pdsdk::set_crypto_library_dir(Path::new(pdsdk::DEFAULT_LIB_DIR));

    let host = Arc::new(OfflineHost::default());
    let calls = host.http_calls.clone();
    let client = DriveClient::create(host.clone(), "https://drive-api.invalid/", ClientOptions::default()).await.expect("client creation");

    // A call that needs the network: it goes through the host callback, which refuses.
    let error = client.my_files_folder().await.expect_err("the offline host must make the call fail");
    assert!(!matches!(error, SdkError::Protocol(_)), "unexpected protocol error: {error}");
    assert!(calls.load(Ordering::SeqCst) >= 1, "the request did not reach the host callback");
    assert!(host.last_url.lock().unwrap().starts_with("https://drive-api.invalid/"), "the SDK must use the configured base URL");

    client.free().await.expect("client release");
}
