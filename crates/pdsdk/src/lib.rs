//! Async Rust bindings to the Proton Drive SDK native library (`libproton_drive_sdk.so`).
//!
//! The SDK is the official C# implementation compiled with NativeAOT. Its C ABI is message based:
//! the host sends Protobuf `Request`s through `proton_drive_sdk_handle_request` and receives a
//! `Response` through a callback. In the other direction, the SDK calls back into the host for
//! HTTP transport and account data (addresses and unlocked keys), and the host answers through
//! `proton_drive_sdk_handle_response`.
//!
//! Every request carries a "bindings handle" that the SDK passes back to the callbacks. Here it is
//! a pointer to a [`Bindings`] obtained from `Arc::into_raw`.

pub mod proto;

use std::future::Future;
use std::io::{Cursor, Read};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use prost::Message;
use tokio::sync::{mpsc, oneshot};

use proto::{request::Payload, response, Any, ErrorDomain, Request, Response};

#[repr(C)]
#[derive(Clone, Copy)]
struct ByteArray {
    pointer: *const u8,
    length: usize,
}

impl ByteArray {
    fn new(bytes: &[u8]) -> Self {
        Self { pointer: bytes.as_ptr(), length: bytes.len() }
    }

    /// # Safety
    /// The pointer must be valid for `length` bytes for the duration of the borrow.
    unsafe fn as_slice<'a>(self) -> &'a [u8] {
        if self.pointer.is_null() || self.length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.pointer, self.length) }
        }
    }
}

type ArrayAction = extern "C" fn(handle: isize, array: ByteArray);

unsafe extern "C" {
    fn proton_drive_sdk_handle_request(request: ByteArray, bindings_handle: isize, response_action: ArrayAction);
    fn proton_drive_sdk_handle_response(sdk_handle: isize, response: ByteArray);
    fn override_native_library_name(library_name: ByteArray, overriding_library_name: ByteArray);
}

/// Tells the SDK where to load its native crypto library (`libproton_crypto.so`) from.
/// Must be called before the first client is created.
pub fn set_crypto_library_dir(dir: &std::path::Path) {
    let path = dir.join("libproton_crypto.so");
    let path = path.to_string_lossy();
    unsafe { override_native_library_name(ByteArray::new(b"proton_crypto"), ByteArray::new(path.as_bytes())) };
}

/// Directory the bindings were linked against (development builds).
pub const DEFAULT_LIB_DIR: &str = env!("PDSDK_DEFAULT_LIB_DIR");

// --- Errors -----------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("{domain:?} error from the SDK: {message}")]
    Sdk { domain: ErrorDomain, message: String, code: Option<i64> },
    #[error("unexpected SDK response: {0}")]
    Protocol(String),
}

impl From<proto::Error> for SdkError {
    fn from(e: proto::Error) -> Self {
        let mut message = e.message;
        let mut inner = e.inner_error;
        while let Some(i) = inner {
            message.push_str(" <- ");
            message.push_str(&i.message);
            inner = i.inner_error;
        }
        SdkError::Sdk {
            domain: ErrorDomain::try_from(e.domain).unwrap_or(ErrorDomain::Undefined),
            message,
            code: e.primary_code,
        }
    }
}

pub type Result<T> = std::result::Result<T, SdkError>;

fn unpack<M: Message + Default>(value: Option<Any>) -> Result<M> {
    let any = value.ok_or_else(|| SdkError::Protocol("missing response value".into()))?;
    M::decode(any.value.as_slice()).map_err(|e| SdkError::Protocol(format!("{}: {e}", any.type_url)))
}

// --- Host interface ---------------------------------------------------------------------------

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What the application must provide to the SDK.
pub trait Host: Send + Sync + 'static {
    /// Performs an HTTP request on behalf of the SDK. The host adds session headers
    /// (`x-pm-uid`, `Authorization`, `x-pm-appversion`) and refreshes the session when needed.
    fn http(&self, request: proto::HttpRequest, body: Option<Vec<u8>>) -> BoxFuture<'_, std::result::Result<HttpReply, String>>;

    /// Answers an account request. The returned `Any` must hold the type documented in the
    /// `.proto` for that request (`Address`, `BytesValue` or `RepeatedBytesValue`).
    fn account(&self, request: proto::AccountRequest) -> BoxFuture<'_, std::result::Result<Any, String>>;

    fn log(&self, _event: proto::LogEvent) {}
}

struct HostContext {
    host: Arc<dyn Host>,
    runtime: tokio::runtime::Handle,
}

/// Receives the bytes the SDK writes to a host stream.
pub type StreamSink = Box<dyn FnMut(&[u8]) -> std::io::Result<()> + Send>;

/// Target of a bindings handle.
struct Bindings {
    response: Mutex<Option<oneshot::Sender<Vec<u8>>>>,
    items: Option<mpsc::UnboundedSender<Vec<u8>>>,
    host: Option<HostContext>,
    sink: Option<Mutex<StreamSink>>,
}

impl Bindings {
    fn new(items: Option<mpsc::UnboundedSender<Vec<u8>>>, host: Option<HostContext>) -> (Arc<Self>, oneshot::Receiver<Vec<u8>>) {
        Self::with_sink(items, host, None)
    }

    fn with_sink(
        items: Option<mpsc::UnboundedSender<Vec<u8>>>,
        host: Option<HostContext>,
        sink: Option<StreamSink>,
    ) -> (Arc<Self>, oneshot::Receiver<Vec<u8>>) {
        let (tx, rx) = oneshot::channel();
        (Arc::new(Self { response: Mutex::new(Some(tx)), items, host, sink: sink.map(Mutex::new) }), rx)
    }

    /// # Safety
    /// `handle` must come from `Arc::into_raw` on a `Bindings` that is still alive.
    unsafe fn from_handle<'a>(handle: isize) -> &'a Self {
        unsafe { &*(handle as *const Self) }
    }
}

/// Sends a request to the SDK and waits for its response.
async fn invoke(request: Request, bindings: Arc<Bindings>, response: oneshot::Receiver<Vec<u8>>) -> Result<Option<Any>> {
    // One strong count is lent to the SDK for the duration of the call.
    let handle = Arc::into_raw(bindings) as isize;
    let bytes = request.encode_to_vec();
    unsafe { proton_drive_sdk_handle_request(ByteArray::new(&bytes), handle, on_response) };

    let reply = response.await;
    // The SDK answers once, after the last yielded item: the lent count can be returned.
    // If this future is dropped before that, the count leaks rather than dangling.
    drop(unsafe { Arc::from_raw(handle as *const Bindings) });

    let reply = reply.map_err(|_| SdkError::Protocol("response channel closed".into()))?;
    match Response::decode(reply.as_slice()).map_err(|e| SdkError::Protocol(e.to_string()))?.result {
        None => Ok(None),
        Some(response::Result::Value(any)) => Ok(Some(any)),
        Some(response::Result::Error(e)) => Err(e.into()),
    }
}

async fn call(payload: Payload) -> Result<Option<Any>> {
    let (bindings, rx) = Bindings::new(None, None);
    invoke(Request { payload: Some(payload) }, bindings, rx).await
}

/// Calls an enumeration request and collects what the SDK yields.
async fn call_collect<M: Message + Default>(payload: Payload) -> Result<Vec<M>> {
    let (items_tx, mut items_rx) = mpsc::unbounded_channel();
    let (bindings, rx) = Bindings::new(Some(items_tx), None);
    invoke(Request { payload: Some(payload) }, bindings, rx).await?;
    let mut items = Vec::new();
    while let Ok(bytes) = items_rx.try_recv() {
        items.push(M::decode(bytes.as_slice()).map_err(|e| SdkError::Protocol(e.to_string()))?);
    }
    Ok(items)
}

fn respond(sdk_handle: isize, response: Response) {
    let bytes = response.encode_to_vec();
    unsafe { proton_drive_sdk_handle_response(sdk_handle, ByteArray::new(&bytes)) };
}

fn respond_value(sdk_handle: isize, value: Any) {
    respond(sdk_handle, Response { result: Some(response::Result::Value(value)) });
}

fn respond_error(sdk_handle: isize, domain: ErrorDomain, message: String) {
    let error = proto::Error { r#type: "HostError".into(), message, domain: domain as i32, ..Default::default() };
    respond(sdk_handle, Response { result: Some(response::Result::Error(error)) });
}

// --- Callbacks invoked by the SDK -------------------------------------------------------------

extern "C" fn on_response(handle: isize, array: ByteArray) {
    let bindings = unsafe { Bindings::from_handle(handle) };
    if let Some(tx) = bindings.response.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(unsafe { array.as_slice() }.to_vec());
    }
}

extern "C" fn on_item(handle: isize, array: ByteArray) {
    let bindings = unsafe { Bindings::from_handle(handle) };
    if let Some(items) = &bindings.items {
        let _ = items.send(unsafe { array.as_slice() }.to_vec());
    }
}

extern "C" fn on_log(handle: isize, array: ByteArray) {
    let bindings = unsafe { Bindings::from_handle(handle) };
    if let (Some(ctx), Ok(event)) = (&bindings.host, proto::LogEvent::decode(unsafe { array.as_slice() })) {
        ctx.host.log(event);
    }
}

/// The SDK writes downloaded content. The buffer is only valid during this call.
extern "C" fn on_stream_write(handle: isize, buffer: ByteArray, sdk_handle: isize) -> isize {
    let bindings = unsafe { Bindings::from_handle(handle) };
    let result = match &bindings.sink {
        Some(sink) => (sink.lock().unwrap_or_else(|e| e.into_inner()))(unsafe { buffer.as_slice() }),
        None => Err(std::io::Error::other("no stream sink for this request")),
    };
    match result {
        Ok(()) => respond(sdk_handle, Response { result: None }),
        Err(e) => respond_error(sdk_handle, ErrorDomain::FileSystem, e.to_string()),
    }
    0
}

/// Reads a request body from an SDK-owned stream.
async fn read_sdk_stream(stream_handle: i64) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = proto::StreamReadRequest {
            stream_handle,
            buffer_pointer: buffer.as_mut_ptr() as i64,
            buffer_length: buffer.len() as i32,
        };
        let n: i32 = unpack(call(Payload::StreamRead(read)).await?)?;
        if n <= 0 {
            return Ok(body);
        }
        body.extend_from_slice(&buffer[..n as usize]);
    }
}

type ResponseContent = Mutex<Cursor<Vec<u8>>>;

extern "C" fn on_http_request(handle: isize, request: ByteArray, sdk_handle: isize) -> isize {
    let bindings = unsafe { Bindings::from_handle(handle) };
    let Some(ctx) = &bindings.host else {
        respond_error(sdk_handle, ErrorDomain::Transport, "no HTTP host for this client".into());
        return 0;
    };
    let request = match proto::HttpRequest::decode(unsafe { request.as_slice() }) {
        Ok(r) => r,
        Err(e) => {
            respond_error(sdk_handle, ErrorDomain::Serialization, e.to_string());
            return 0;
        }
    };
    let host = ctx.host.clone();
    ctx.runtime.spawn(async move {
        // The SDK frees its body stream as soon as it gets the response, so read it first.
        let body = match request.sdk_content_handle {
            Some(stream) => match read_sdk_stream(stream).await {
                Ok(body) => Some(body),
                Err(e) => return respond_error(sdk_handle, ErrorDomain::Transport, e.to_string()),
            },
            None => None,
        };
        match host.http(request, body).await {
            Ok(reply) => {
                let content: Box<ResponseContent> = Box::new(Mutex::new(Cursor::new(reply.body)));
                let response = proto::HttpResponse {
                    status_code: reply.status.into(),
                    headers: reply.headers.into_iter().map(|(name, value)| proto::HttpHeader { name, values: vec![value] }).collect(),
                    bindings_content_handle: Some(Box::into_raw(content) as i64),
                };
                respond_value(sdk_handle, proto::pack("proton.drive.sdk.HttpResponse", &response));
            }
            Err(message) => respond_error(sdk_handle, ErrorDomain::Network, message),
        }
    });
    // No cancellation support yet: the cancellation action ignores this handle.
    0
}

extern "C" fn on_http_content_read(content_handle: isize, buffer: ByteArray, sdk_handle: isize) -> isize {
    let content = unsafe { &*(content_handle as *const ResponseContent) };
    let destination = unsafe { std::slice::from_raw_parts_mut(buffer.pointer as *mut u8, buffer.length) };
    let n = content.lock().unwrap_or_else(|e| e.into_inner()).read(destination).unwrap_or(0);
    respond_value(sdk_handle, proto::pack("google.protobuf.Int32Value", &(n as i32)));
    0
}

extern "C" fn on_http_content_dispose(content_handle: isize) {
    drop(unsafe { Box::from_raw(content_handle as *mut ResponseContent) });
}

extern "C" fn on_http_cancel(_operation_handle: isize) {}

extern "C" fn on_account_request(handle: isize, request: ByteArray, sdk_handle: isize) {
    let bindings = unsafe { Bindings::from_handle(handle) };
    let Some(ctx) = &bindings.host else {
        return respond_error(sdk_handle, ErrorDomain::BusinessLogic, "no account host for this client".into());
    };
    let request = match proto::AccountRequest::decode(unsafe { request.as_slice() }) {
        Ok(r) => r,
        Err(e) => return respond_error(sdk_handle, ErrorDomain::Serialization, e.to_string()),
    };
    let host = ctx.host.clone();
    ctx.runtime.spawn(async move {
        match host.account(request).await {
            Ok(value) => respond_value(sdk_handle, value),
            Err(message) => respond_error(sdk_handle, ErrorDomain::BusinessLogic, message),
        }
    });
}

fn fn_ptr(f: *const ()) -> i64 {
    f as i64
}

/// Result of a batch operation on a single node.
fn single_result(results: Vec<proto::NodeResultPair>) -> Result<()> {
    match results.into_iter().next() {
        Some(proto::NodeResultPair { error: Some(error), .. }) => Err(error.into()),
        Some(_) => Ok(()),
        None => Err(SdkError::Protocol("no result for the node".into())),
    }
}

// --- Cancellation -----------------------------------------------------------------------------

/// Cancels SDK operations it is passed to. Must be released with [`CancellationToken::free`].
pub struct CancellationToken {
    handle: i64,
}

impl CancellationToken {
    pub async fn new() -> Result<Self> {
        Ok(Self { handle: unpack(call(Payload::CancellationTokenSourceCreate(proto::Empty {})).await?)? })
    }

    pub async fn cancel(&self) -> Result<()> {
        let request = proto::CancellationTokenSourceRequest { cancellation_token_source_handle: self.handle };
        call(Payload::CancellationTokenSourceCancel(request)).await.map(|_| ())
    }

    pub async fn free(self) -> Result<()> {
        let request = proto::CancellationTokenSourceRequest { cancellation_token_source_handle: self.handle };
        call(Payload::CancellationTokenSourceFree(request)).await.map(|_| ())
    }
}

// --- Drive client -----------------------------------------------------------------------------

#[derive(Default)]
pub struct ClientOptions {
    /// Stable client identifier; the SDK generates one when absent.
    pub uid: Option<String>,
    /// SQLite entity cache; in-memory when absent.
    pub cache_path: Option<String>,
    /// 32-byte key to encrypt the cache.
    pub cache_encryption_key: Option<Vec<u8>>,
}

pub struct DriveClient {
    handle: i64,
    // The SDK keeps this bindings handle for HTTP, account and log callbacks.
    bindings: Option<Arc<Bindings>>,
}

impl Drop for DriveClient {
    fn drop(&mut self) {
        // Not freed through `free`: the SDK may still call back with this handle, so leak it.
        if let Some(bindings) = self.bindings.take() {
            std::mem::forget(bindings);
        }
    }
}

impl DriveClient {
    /// Creates a client. Must be called from within a Tokio runtime.
    pub async fn create(host: Arc<dyn Host>, base_url: &str, options: ClientOptions) -> Result<Self> {
        let ctx = HostContext { host, runtime: tokio::runtime::Handle::current() };
        let (bindings, rx) = Bindings::new(None, Some(ctx));
        let request = proto::DriveClientCreateRequest {
            base_url: base_url.to_owned(),
            http_client: Some(proto::HttpClient {
                request_function: fn_ptr(on_http_request as *const ()),
                response_content_read_action: fn_ptr(on_http_content_read as *const ()),
                response_content_dispose_action: fn_ptr(on_http_content_dispose as *const ()),
                cancellation_action: fn_ptr(on_http_cancel as *const ()),
            }),
            client_options: Some(proto::ProtonDriveClientOptions { uid: options.uid, bindings_language: None }),
            account_request_action: fn_ptr(on_account_request as *const ()),
            cache_path: options.cache_path,
            telemetry: Some(proto::Telemetry { log_action: Some(fn_ptr(on_log as *const ())) }),
            cache_encryption_key: options.cache_encryption_key,
        };
        let value = invoke(Request { payload: Some(Payload::DriveClientCreate(request)) }, bindings.clone(), rx).await?;
        let handle: i64 = unpack(value)?;
        Ok(Self { handle, bindings: Some(bindings) })
    }

    pub async fn my_files_folder(&self) -> Result<proto::FolderNode> {
        let request = proto::DriveClientGetMyFilesFolderRequest { client_handle: self.handle, cancellation_token_source_handle: 0 };
        // Despite the .proto comment, the SDK answers with a `Node` wrapping the folder.
        let node: proto::Node = unpack(call(Payload::DriveClientGetMyFilesFolder(request)).await?)?;
        match node.kind {
            Some(proto::node::Kind::Folder(folder)) => Ok(folder),
            _ => Err(SdkError::Protocol("\"My files\" is not a folder".into())),
        }
    }

    /// UIDs of the children of a folder.
    pub async fn folder_children(&self, folder_uid: &str) -> Result<Vec<String>> {
        let request = proto::DriveClientEnumerateFolderChildrenRequest {
            client_handle: self.handle,
            folder_uid: folder_uid.to_owned(),
            yield_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle: 0,
        };
        call_collect(Payload::DriveClientEnumerateFolderChildren(request)).await
    }

    /// Loads and decrypts nodes.
    pub async fn nodes(&self, node_uids: Vec<String>) -> Result<Vec<proto::Node>> {
        if node_uids.is_empty() {
            return Ok(Vec::new());
        }
        let request = proto::DriveClientEnumerateNodesRequest {
            node_uids,
            client_handle: self.handle,
            yield_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle: 0,
        };
        call_collect(Payload::DriveClientEnumerateNodes(request)).await
    }

    /// Downloads and decrypts a file revision, handing the content to `sink` in order.
    pub async fn download(&self, revision_uid: &str, sink: StreamSink, cancellation: Option<&CancellationToken>) -> Result<()> {
        let cancellation_token_source_handle = cancellation.map_or(0, |c| c.handle);
        let request = proto::DriveClientGetFileDownloaderRequest {
            client_handle: self.handle,
            revision_uid: revision_uid.to_owned(),
            no_waiting: false,
            cancellation_token_source_handle,
        };
        let downloader: i64 = unpack(call(Payload::DriveClientGetFileDownloader(request)).await?)?;
        let result = Self::run_download(downloader, sink, cancellation_token_source_handle).await;
        call(Payload::FileDownloaderFree(proto::FileDownloaderFreeRequest { file_downloader_handle: downloader })).await?;
        result
    }

    async fn run_download(downloader: i64, sink: StreamSink, cancellation_token_source_handle: i64) -> Result<()> {
        // Writes and progress use this bindings handle until the download completes, so it must
        // outlive the request that starts the download: keep a clone until the end.
        let (bindings, rx) = Bindings::with_sink(None, None, Some(sink));
        let request = proto::DownloadToStreamRequest {
            downloader_handle: downloader,
            write_action: fn_ptr(on_stream_write as *const ()),
            progress_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle,
        };
        let controller: i64 = unpack(invoke(Request { payload: Some(Payload::DownloadToStream(request)) }, bindings.clone(), rx).await?)?;
        let completion = call(Payload::DownloadControllerAwaitCompletion(proto::DownloadControllerAwaitCompletionRequest {
            download_controller_handle: controller,
        }))
        .await;
        call(Payload::DownloadControllerFree(proto::DownloadControllerFreeRequest { download_controller_handle: controller })).await?;
        drop(bindings);
        completion.map(|_| ())
    }

    /// Creates a folder and returns it.
    pub async fn create_folder(&self, parent_folder_uid: &str, name: &str) -> Result<proto::FolderNode> {
        let request = proto::DriveClientCreateFolderRequest {
            client_handle: self.handle,
            parent_folder_uid: parent_folder_uid.to_owned(),
            folder_name: name.to_owned(),
            last_modification_time: None,
            cancellation_token_source_handle: 0,
        };
        // As for "My files", the folder comes wrapped in a `Node`.
        let node: proto::Node = unpack(call(Payload::DriveClientCreateFolder(request)).await?)?;
        match node.kind {
            Some(proto::node::Kind::Folder(folder)) => Ok(folder),
            _ => Err(SdkError::Protocol("created node is not a folder".into())),
        }
    }

    /// Moves a node to the trash.
    pub async fn trash(&self, node_uid: &str) -> Result<()> {
        let request = proto::DriveClientTrashNodesRequest {
            client_handle: self.handle,
            node_uids: vec![node_uid.to_owned()],
            cancellation_token_source_handle: 0,
            yield_action: fn_ptr(on_item as *const ()),
        };
        single_result(call_collect(Payload::DriveClientTrashNodes(request)).await?)
    }

    /// Moves and/or renames a node (a rename is a move within the same folder).
    pub async fn move_node(&self, item: proto::NodeMoveItem, target_parent_folder_uid: &str) -> Result<()> {
        let request = proto::DriveClientMoveNodesRequest {
            client_handle: self.handle,
            items: vec![item],
            target_parent_folder_uid: target_parent_folder_uid.to_owned(),
            cancellation_token_source_handle: 0,
            yield_action: fn_ptr(on_item as *const ()),
        };
        single_result(call_collect(Payload::DriveClientMoveNodes(request)).await?)
    }

    /// Uploads a local file as a new file of a folder.
    pub async fn upload_new_file(
        &self,
        parent_folder_uid: &str,
        name: &str,
        media_type: &str,
        path: &std::path::Path,
        size: u64,
        modification_time: Option<proto::Timestamp>,
    ) -> Result<proto::UploadResult> {
        let request = proto::DriveClientGetFileUploaderRequest {
            client_handle: self.handle,
            parent_folder_uid: parent_folder_uid.to_owned(),
            name: name.to_owned(),
            media_type: media_type.to_owned(),
            size: size as i64,
            last_modification_time: modification_time,
            override_existing_draft_by_other_client: false,
            no_waiting: false,
            cancellation_token_source_handle: 0,
        };
        let uploader: i64 = unpack(call(Payload::DriveClientGetFileUploader(request)).await?)?;
        Self::run_upload(uploader, path).await
    }

    /// Uploads a local file as a new revision of an existing file.
    pub async fn upload_revision(
        &self,
        current_revision_uid: &str,
        path: &std::path::Path,
        size: u64,
        modification_time: Option<proto::Timestamp>,
    ) -> Result<proto::UploadResult> {
        let request = proto::DriveClientGetFileRevisionUploaderRequest {
            client_handle: self.handle,
            current_active_revision_uid: current_revision_uid.to_owned(),
            size: size as i64,
            last_modification_time: modification_time,
            no_waiting: false,
            cancellation_token_source_handle: 0,
        };
        let uploader: i64 = unpack(call(Payload::DriveClientGetFileRevisionUploader(request)).await?)?;
        Self::run_upload(uploader, path).await
    }

    async fn run_upload(uploader: i64, path: &std::path::Path) -> Result<proto::UploadResult> {
        if uploader == 0 {
            return Err(SdkError::Protocol("no uploader available".into()));
        }
        // Progress uses this bindings handle until completion: keep it alive until the end.
        let (bindings, rx) = Bindings::new(None, None);
        let request = proto::UploadFromFileRequest {
            uploader_handle: uploader,
            file_path: path.to_string_lossy().into_owned(),
            progress_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle: 0,
        };
        let result = async {
            let controller: i64 = unpack(invoke(Request { payload: Some(Payload::UploadFromFile(request)) }, bindings.clone(), rx).await?)?;
            let completion = call(Payload::UploadControllerAwaitCompletion(proto::UploadControllerAwaitCompletionRequest {
                upload_controller_handle: controller,
            }))
            .await;
            call(Payload::UploadControllerFree(proto::UploadControllerFreeRequest { upload_controller_handle: controller })).await?;
            unpack::<proto::UploadResult>(completion?)
        }
        .await;
        drop(bindings);
        call(Payload::FileUploaderFree(proto::FileUploaderFreeRequest { file_uploader_handle: uploader })).await?;
        result
    }

    /// Decrypted thumbnails of files (JPEG data), for those that have one of this type.
    pub async fn thumbnails(&self, file_uids: Vec<String>, kind: proto::ThumbnailType) -> Result<Vec<proto::FileThumbnail>> {
        if file_uids.is_empty() {
            return Ok(Vec::new());
        }
        let request = proto::DriveClientEnumerateThumbnailsRequest {
            client_handle: self.handle,
            file_uids,
            r#type: kind as i32,
            yield_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle: 0,
        };
        call_collect(Payload::DriveClientEnumerateThumbnails(request)).await
    }

    /// Events of a tree since `cursor`. Without a cursor, yields a single `CursorAdvanced` event
    /// carrying the latest event ID, to start from.
    pub async fn events(&self, tree_event_scope_id: &str, cursor: Option<String>) -> Result<Vec<proto::DriveEvent>> {
        let request = proto::DriveClientEnumerateEventsRequest {
            client_handle: self.handle,
            tree_event_scope_id: tree_event_scope_id.to_owned(),
            cursor_event_id: cursor,
            yield_action: fn_ptr(on_item as *const ()),
            cancellation_token_source_handle: 0,
        };
        call_collect(Payload::DriveClientEnumerateEvents(request)).await
    }

    /// Releases the SDK client. Callbacks may still run until this returns.
    pub async fn free(mut self) -> Result<()> {
        call(Payload::DriveClientFree(proto::DriveClientFreeRequest { client_handle: self.handle })).await?;
        self.bindings = None;
        Ok(())
    }
}
