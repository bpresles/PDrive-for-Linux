//! Hand-written subset of `proton.drive.sdk.proto` (package `proton.drive.sdk`).
//!
//! The upstream file uses Protobuf editions (2023), which prost-build does not handle yet, so the
//! messages needed by the bindings are declared here with the same field numbers. Fields with
//! explicit presence on the SDK side (`HasXxx` in C#) are `Option`s so that they are only sent
//! when set.

pub use prost_types::{Any, Timestamp};

pub const TYPE_URL_PREFIX: &str = "type.googleapis.com/";

/// Wraps a message into an `Any` with the fully-qualified Protobuf name the SDK expects.
pub fn pack(full_name: &str, message: &impl prost::Message) -> Any {
    Any {
        type_url: format!("{TYPE_URL_PREFIX}{full_name}"),
        value: message.encode_to_vec(),
    }
}

// --- Envelope ---------------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct Request {
    #[prost(oneof = "request::Payload", tags = "100, 101, 102, 200, 1000, 1001, 1002, 1003, 1004, 1007, 1008, 1009, 1010, 1015, 1019, 1021, 1022, 1101, 1102, 1104, 1108, 1200, 1202, 1205, 1208")]
    pub payload: Option<request::Payload>,
}

pub mod request {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "100")]
        CancellationTokenSourceCreate(super::Empty),
        #[prost(message, tag = "101")]
        CancellationTokenSourceCancel(super::CancellationTokenSourceRequest),
        #[prost(message, tag = "102")]
        CancellationTokenSourceFree(super::CancellationTokenSourceRequest),
        #[prost(message, tag = "200")]
        StreamRead(super::StreamReadRequest),
        #[prost(message, tag = "1000")]
        DriveClientCreate(super::DriveClientCreateRequest),
        #[prost(message, tag = "1001")]
        DriveClientFree(super::DriveClientFreeRequest),
        #[prost(message, tag = "1002")]
        DriveClientGetFileUploader(super::DriveClientGetFileUploaderRequest),
        #[prost(message, tag = "1003")]
        DriveClientGetFileRevisionUploader(super::DriveClientGetFileRevisionUploaderRequest),
        #[prost(message, tag = "1004")]
        DriveClientGetFileDownloader(super::DriveClientGetFileDownloaderRequest),
        #[prost(message, tag = "1007")]
        DriveClientCreateFolder(super::DriveClientCreateFolderRequest),
        #[prost(message, tag = "1008")]
        DriveClientTrashNodes(super::DriveClientTrashNodesRequest),
        #[prost(message, tag = "1009")]
        DriveClientEnumerateFolderChildren(super::DriveClientEnumerateFolderChildrenRequest),
        #[prost(message, tag = "1010")]
        DriveClientGetMyFilesFolder(super::DriveClientGetMyFilesFolderRequest),
        #[prost(message, tag = "1015")]
        DriveClientEnumerateThumbnails(super::DriveClientEnumerateThumbnailsRequest),
        #[prost(message, tag = "1019")]
        DriveClientMoveNodes(super::DriveClientMoveNodesRequest),
        #[prost(message, tag = "1021")]
        DriveClientEnumerateEvents(super::DriveClientEnumerateEventsRequest),
        #[prost(message, tag = "1022")]
        DriveClientEnumerateNodes(super::DriveClientEnumerateNodesRequest),
        #[prost(message, tag = "1101")]
        UploadFromFile(super::UploadFromFileRequest),
        #[prost(message, tag = "1102")]
        FileUploaderFree(super::FileUploaderFreeRequest),
        #[prost(message, tag = "1104")]
        UploadControllerAwaitCompletion(super::UploadControllerAwaitCompletionRequest),
        #[prost(message, tag = "1108")]
        UploadControllerFree(super::UploadControllerFreeRequest),
        #[prost(message, tag = "1200")]
        DownloadToStream(super::DownloadToStreamRequest),
        #[prost(message, tag = "1202")]
        FileDownloaderFree(super::FileDownloaderFreeRequest),
        #[prost(message, tag = "1205")]
        DownloadControllerAwaitCompletion(super::DownloadControllerAwaitCompletionRequest),
        #[prost(message, tag = "1208")]
        DownloadControllerFree(super::DownloadControllerFreeRequest),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Response {
    #[prost(oneof = "response::Result", tags = "1, 2")]
    pub result: Option<response::Result>,
}

pub mod response {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Result {
        #[prost(message, tag = "1")]
        Value(super::Any),
        #[prost(message, tag = "2")]
        Error(super::Error),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ErrorDomain {
    Undefined = 0,
    SuccessfulCancellation = 1,
    Api = 2,
    Network = 3,
    Transport = 4,
    Serialization = 5,
    Cryptography = 6,
    DataIntegrity = 7,
    BusinessLogic = 8,
    UnknownIo = 9,
    FileSystem = 10,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Error {
    #[prost(string, tag = "1")]
    pub r#type: String,
    #[prost(string, tag = "2")]
    pub message: String,
    #[prost(enumeration = "ErrorDomain", tag = "3")]
    pub domain: i32,
    #[prost(int64, optional, tag = "4")]
    pub primary_code: Option<i64>,
    #[prost(int64, optional, tag = "5")]
    pub secondary_code: Option<i64>,
    #[prost(string, optional, tag = "6")]
    pub context: Option<String>,
    #[prost(message, optional, boxed, tag = "7")]
    pub inner_error: Option<Box<Error>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RepeatedBytesValue {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub value: Vec<Vec<u8>>,
}

// --- Cancellation ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct CancellationTokenSourceRequest {
    #[prost(int64, tag = "1")]
    pub cancellation_token_source_handle: i64,
}

// --- Streams and logging ----------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamReadRequest {
    #[prost(int64, tag = "1")]
    pub stream_handle: i64,
    #[prost(int64, tag = "2")]
    pub buffer_pointer: i64,
    #[prost(int32, tag = "3")]
    pub buffer_length: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Telemetry {
    #[prost(int64, optional, tag = "1")]
    pub log_action: Option<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct LogEvent {
    #[prost(int32, tag = "1")]
    pub level: i32,
    #[prost(string, tag = "2")]
    pub message: String,
    #[prost(string, tag = "3")]
    pub category_name: String,
}

// --- HTTP -------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum HttpRequestType {
    RegularApi = 0,
    StorageDownload = 1,
    StorageUpload = 2,
    SmallUpload = 3,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HttpHeader {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, repeated, tag = "2")]
    pub values: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HttpRequest {
    #[prost(enumeration = "HttpRequestType", tag = "1")]
    pub r#type: i32,
    #[prost(string, tag = "2")]
    pub url: String,
    #[prost(string, tag = "3")]
    pub method: String,
    #[prost(message, repeated, tag = "4")]
    pub headers: Vec<HttpHeader>,
    #[prost(int64, optional, tag = "5")]
    pub sdk_content_handle: Option<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HttpResponse {
    #[prost(int32, tag = "1")]
    pub status_code: i32,
    #[prost(message, repeated, tag = "2")]
    pub headers: Vec<HttpHeader>,
    #[prost(int64, optional, tag = "3")]
    pub bindings_content_handle: Option<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HttpClient {
    #[prost(int64, tag = "1")]
    pub request_function: i64,
    #[prost(int64, tag = "2")]
    pub response_content_read_action: i64,
    #[prost(int64, tag = "3")]
    pub response_content_dispose_action: i64,
    #[prost(int64, tag = "4")]
    pub cancellation_action: i64,
}

// --- Account ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum AddressStatus {
    Disabled = 0,
    Enabled = 1,
    Deleting = 2,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Address {
    #[prost(string, tag = "1")]
    pub address_id: String,
    #[prost(int32, tag = "2")]
    pub order: i32,
    #[prost(string, tag = "3")]
    pub email_address: String,
    #[prost(enumeration = "AddressStatus", tag = "4")]
    pub status: i32,
    #[prost(message, repeated, tag = "5")]
    pub keys: Vec<AddressKey>,
    #[prost(int32, tag = "6")]
    pub primary_key_index: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AddressKey {
    #[prost(string, tag = "1")]
    pub address_id: String,
    #[prost(string, tag = "2")]
    pub address_key_id: String,
    #[prost(bool, tag = "3")]
    pub is_active: bool,
    #[prost(bool, tag = "4")]
    pub is_allowed_for_encryption: bool,
    #[prost(bool, tag = "5")]
    pub is_allowed_for_verification: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AccountRequest {
    #[prost(oneof = "account_request::Payload", tags = "1, 2, 3, 4, 5")]
    pub payload: Option<account_request::Payload>,
}

pub mod account_request {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "1")]
        GetAddress(super::GetAddressRequest),
        #[prost(message, tag = "2")]
        GetDefaultAddress(super::GetDefaultAddressRequest),
        #[prost(message, tag = "3")]
        GetAddressPrimaryPrivateKey(super::GetAddressPrimaryPrivateKeyRequest),
        #[prost(message, tag = "4")]
        GetAddressPrivateKeys(super::GetAddressPrivateKeysRequest),
        #[prost(message, tag = "5")]
        GetAddressPublicKeys(super::GetAddressPublicKeysRequest),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetAddressRequest {
    #[prost(string, tag = "1")]
    pub address_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetDefaultAddressRequest {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetAddressPrimaryPrivateKeyRequest {
    #[prost(string, tag = "1")]
    pub address_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetAddressPrivateKeysRequest {
    #[prost(string, tag = "1")]
    pub address_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetAddressPublicKeysRequest {
    #[prost(string, tag = "1")]
    pub email_address: String,
}

// --- Drive client -----------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct ProtonDriveClientOptions {
    #[prost(string, optional, tag = "1")]
    pub uid: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub bindings_language: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientCreateRequest {
    #[prost(string, tag = "1")]
    pub base_url: String,
    #[prost(message, optional, tag = "2")]
    pub http_client: Option<HttpClient>,
    /// Must always be present: the SDK dereferences it unconditionally.
    #[prost(message, optional, tag = "3")]
    pub client_options: Option<ProtonDriveClientOptions>,
    #[prost(int64, tag = "4")]
    pub account_request_action: i64,
    #[prost(string, optional, tag = "5")]
    pub cache_path: Option<String>,
    #[prost(message, optional, tag = "6")]
    pub telemetry: Option<Telemetry>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub cache_encryption_key: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientFreeRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientGetMyFilesFolderRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(int64, tag = "2")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientEnumerateFolderChildrenRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub folder_uid: String,
    #[prost(int64, tag = "3")]
    pub yield_action: i64,
    #[prost(int64, tag = "4")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientEnumerateNodesRequest {
    #[prost(string, repeated, tag = "1")]
    pub node_uids: Vec<String>,
    #[prost(int64, tag = "2")]
    pub client_handle: i64,
    #[prost(int64, tag = "3")]
    pub yield_action: i64,
    #[prost(int64, tag = "4")]
    pub cancellation_token_source_handle: i64,
}

// --- Writes ---------------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientCreateFolderRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub parent_folder_uid: String,
    #[prost(string, tag = "3")]
    pub folder_name: String,
    #[prost(message, optional, tag = "4")]
    pub last_modification_time: Option<Timestamp>,
    #[prost(int64, tag = "5")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientTrashNodesRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, repeated, tag = "2")]
    pub node_uids: Vec<String>,
    #[prost(int64, tag = "3")]
    pub cancellation_token_source_handle: i64,
    #[prost(int64, tag = "4")]
    pub yield_action: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeMoveItem {
    #[prost(string, tag = "1")]
    pub node_uid: String,
    #[prost(string, tag = "2")]
    pub current_parent_uid: String,
    #[prost(string, tag = "3")]
    pub current_name: String,
    #[prost(string, tag = "4")]
    pub target_name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientMoveNodesRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(message, repeated, tag = "2")]
    pub items: Vec<NodeMoveItem>,
    #[prost(string, tag = "3")]
    pub target_parent_folder_uid: String,
    #[prost(int64, tag = "4")]
    pub cancellation_token_source_handle: i64,
    #[prost(int64, tag = "5")]
    pub yield_action: i64,
}

/// Outcome of a batch operation for one node.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeResultPair {
    #[prost(string, tag = "1")]
    pub node_uid: String,
    #[prost(message, optional, tag = "2")]
    pub error: Option<Error>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientGetFileUploaderRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub parent_folder_uid: String,
    #[prost(string, tag = "3")]
    pub name: String,
    #[prost(string, tag = "4")]
    pub media_type: String,
    #[prost(int64, tag = "5")]
    pub size: i64,
    #[prost(message, optional, tag = "6")]
    pub last_modification_time: Option<Timestamp>,
    #[prost(bool, tag = "8")]
    pub override_existing_draft_by_other_client: bool,
    #[prost(bool, tag = "9")]
    pub no_waiting: bool,
    #[prost(int64, tag = "10")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientGetFileRevisionUploaderRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub current_active_revision_uid: String,
    #[prost(int64, tag = "3")]
    pub size: i64,
    #[prost(message, optional, tag = "4")]
    pub last_modification_time: Option<Timestamp>,
    #[prost(bool, tag = "6")]
    pub no_waiting: bool,
    #[prost(int64, tag = "7")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UploadFromFileRequest {
    #[prost(int64, tag = "1")]
    pub uploader_handle: i64,
    #[prost(string, tag = "3")]
    pub file_path: String,
    #[prost(int64, tag = "4")]
    pub progress_action: i64,
    #[prost(int64, tag = "5")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileUploaderFreeRequest {
    #[prost(int64, tag = "1")]
    pub file_uploader_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UploadControllerAwaitCompletionRequest {
    #[prost(int64, tag = "1")]
    pub upload_controller_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UploadControllerFreeRequest {
    #[prost(int64, tag = "1")]
    pub upload_controller_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UploadResult {
    #[prost(string, tag = "1")]
    pub node_uid: String,
    #[prost(string, tag = "2")]
    pub revision_uid: String,
}

// --- Downloads ------------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientGetFileDownloaderRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub revision_uid: String,
    #[prost(bool, tag = "3")]
    pub no_waiting: bool,
    #[prost(int64, tag = "4")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DownloadToStreamRequest {
    #[prost(int64, tag = "1")]
    pub downloader_handle: i64,
    /// `intptr_t write(intptr_t bindings_handle, ByteArray buffer, intptr_t sdk_handle)`
    #[prost(int64, tag = "2")]
    pub write_action: i64,
    #[prost(int64, tag = "3")]
    pub progress_action: i64,
    #[prost(int64, tag = "4")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileDownloaderFreeRequest {
    #[prost(int64, tag = "1")]
    pub file_downloader_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DownloadControllerAwaitCompletionRequest {
    #[prost(int64, tag = "1")]
    pub download_controller_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DownloadControllerFreeRequest {
    #[prost(int64, tag = "1")]
    pub download_controller_handle: i64,
}

// --- Thumbnails -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ThumbnailType {
    Unspecified = 0,
    /// Small thumbnail (up to 512 px).
    Thumbnail = 1,
    /// Larger preview.
    Preview = 2,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ThumbnailHeader {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(enumeration = "ThumbnailType", tag = "2")]
    pub r#type: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientEnumerateThumbnailsRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, repeated, tag = "2")]
    pub file_uids: Vec<String>,
    #[prost(enumeration = "ThumbnailType", tag = "3")]
    pub r#type: i32,
    #[prost(int64, tag = "4")]
    pub yield_action: i64,
    #[prost(int64, tag = "5")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileThumbnail {
    #[prost(string, tag = "1")]
    pub file_uid: String,
    #[prost(oneof = "file_thumbnail::Result", tags = "2, 3")]
    pub result: Option<file_thumbnail::Result>,
}

pub mod file_thumbnail {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Result {
        #[prost(bytes, tag = "2")]
        Data(Vec<u8>),
        #[prost(message, tag = "3")]
        Error(super::DriveError),
    }
}

// --- Events -----------------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveClientEnumerateEventsRequest {
    #[prost(int64, tag = "1")]
    pub client_handle: i64,
    #[prost(string, tag = "2")]
    pub tree_event_scope_id: String,
    #[prost(string, optional, tag = "3")]
    pub cursor_event_id: Option<String>,
    #[prost(int64, tag = "4")]
    pub yield_action: i64,
    #[prost(int64, tag = "5")]
    pub cancellation_token_source_handle: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveEvent {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(oneof = "drive_event::Event", tags = "2, 3, 4, 5, 6, 7")]
    pub event: Option<drive_event::Event>,
}

pub mod drive_event {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Event {
        #[prost(message, tag = "2")]
        NodeUpdated(super::NodeUpdatedEvent),
        #[prost(message, tag = "3")]
        NodeDeleted(super::NodeDeletedEvent),
        #[prost(message, tag = "4")]
        SharedWithMeUpdated(super::Empty),
        #[prost(message, tag = "5")]
        CursorAdvanced(super::Empty),
        #[prost(message, tag = "6")]
        ContinuityLost(super::Empty),
        #[prost(message, tag = "7")]
        ScopeAccessLost(super::Empty),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeUpdatedEvent {
    #[prost(string, tag = "1")]
    pub node_uid: String,
    #[prost(string, optional, tag = "2")]
    pub parent_node_uid: Option<String>,
    #[prost(bool, tag = "3")]
    pub is_trashed: bool,
    #[prost(bool, tag = "4")]
    pub is_shared: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeDeletedEvent {
    #[prost(string, tag = "1")]
    pub node_uid: String,
    #[prost(string, optional, tag = "2")]
    pub parent_node_uid: Option<String>,
}

// --- Nodes ------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
pub struct DriveError {
    #[prost(string, optional, tag = "1")]
    pub message: Option<String>,
    #[prost(message, optional, boxed, tag = "2")]
    pub inner_error: Option<Box<DriveError>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct StringResult {
    #[prost(oneof = "string_result::Result", tags = "1, 2")]
    pub result: Option<string_result::Result>,
}

pub mod string_result {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Result {
        #[prost(string, tag = "1")]
        Value(String),
        #[prost(message, tag = "2")]
        Error(super::DriveError),
    }
}

impl StringResult {
    /// The decrypted value, or a placeholder describing why it is unavailable.
    pub fn display(&self) -> String {
        match &self.result {
            Some(string_result::Result::Value(v)) => v.clone(),
            Some(string_result::Result::Error(e)) => {
                format!("<error: {}>", e.message.as_deref().unwrap_or("unknown"))
            }
            None => "<missing>".to_owned(),
        }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileRevision {
    #[prost(string, tag = "1")]
    pub uid: String,
    #[prost(message, optional, tag = "2")]
    pub creation_time: Option<Timestamp>,
    #[prost(int64, tag = "3")]
    pub storage_size: i64,
    #[prost(int64, optional, tag = "4")]
    pub claimed_size: Option<i64>,
    #[prost(message, optional, tag = "6")]
    pub claimed_modification_time: Option<Timestamp>,
    #[prost(message, repeated, tag = "7")]
    pub thumbnails: Vec<ThumbnailHeader>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileNode {
    #[prost(string, tag = "1")]
    pub uid: String,
    #[prost(string, tag = "2")]
    pub parent_uid: String,
    #[prost(message, optional, tag = "4")]
    pub name: Option<StringResult>,
    #[prost(string, tag = "5")]
    pub media_type: String,
    #[prost(message, optional, tag = "6")]
    pub creation_time: Option<Timestamp>,
    #[prost(message, optional, tag = "7")]
    pub trash_time: Option<Timestamp>,
    #[prost(message, optional, tag = "10")]
    pub active_revision: Option<FileRevision>,
    #[prost(int64, tag = "11")]
    pub total_storage_size: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FolderNode {
    #[prost(string, tag = "1")]
    pub uid: String,
    #[prost(string, optional, tag = "2")]
    pub parent_uid: Option<String>,
    /// Scope to pass to the events API (the volume of the node).
    #[prost(string, tag = "3")]
    pub tree_event_scope_id: String,
    #[prost(message, optional, tag = "4")]
    pub name: Option<StringResult>,
    #[prost(message, optional, tag = "5")]
    pub creation_time: Option<Timestamp>,
    #[prost(message, optional, tag = "6")]
    pub trash_time: Option<Timestamp>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Node {
    #[prost(oneof = "node::Kind", tags = "1, 2")]
    pub kind: Option<node::Kind>,
}

pub mod node {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        Folder(super::FolderNode),
        #[prost(message, tag = "2")]
        File(super::FileNode),
    }
}
