//! Proof of concept: Rust host for the Proton Drive SDK.
//!
//!     pdpoc login          sign in through the browser, store the session in the keyring
//!     pdpoc ls [PATH]      list a folder of "My files" (e.g. `pdpoc ls Documents/Invoices`)
//!     pdpoc logout         revoke the session and remove it from the keyring
//!
//! Set PDPOC_VERBOSE=1 to log HTTP requests and SDK debug messages.

mod api;
mod auth;
mod config;
mod host;
mod keys;
mod session;

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use pdsdk::proto::{node::Kind, Node};
use pdsdk::{ClientOptions, DriveClient};
use reqwest::Method;

use api::ApiClient;
use host::AppHost;
use keys::AccountKeys;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("login") => login().await,
        Some("ls") => list(args.get(1).map(String::as_str).unwrap_or("")).await,
        Some("logout") => logout().await,
        Some("selftest") => selftest().await,
        _ => {
            eprintln!("usage: pdpoc login | ls [PATH] | logout | selftest");
            std::process::exit(2);
        }
    }
}

/// Offline check of the SDK bridge: client creation, a call that goes through the HTTP callback
/// (rejected by the host) with its error coming back, and client release.
async fn selftest() -> Result<()> {
    struct OfflineHost;
    impl pdsdk::Host for OfflineHost {
        fn http(&self, request: pdsdk::proto::HttpRequest, _body: Option<Vec<u8>>) -> pdsdk::BoxFuture<'_, Result<pdsdk::HttpReply, String>> {
            Box::pin(async move { Err(format!("offline: {} {}", request.method, request.url)) })
        }
        fn account(&self, _request: pdsdk::proto::AccountRequest) -> pdsdk::BoxFuture<'_, Result<pdsdk::proto::Any, String>> {
            Box::pin(async { Err("offline".to_owned()) })
        }
    }

    pdsdk::set_crypto_library_dir(Path::new(pdsdk::DEFAULT_LIB_DIR));
    let client = DriveClient::create(Arc::new(OfflineHost), &format!("{}/", config::API_BASE_URL), ClientOptions::default()).await?;
    println!("client created");
    match client.my_files_folder().await {
        Ok(_) => bail!("expected an error from the offline host"),
        Err(e) => println!("error propagated as expected: {e}"),
    }
    client.free().await?;
    println!("client freed: SDK bridge OK");
    Ok(())
}

async fn login() -> Result<()> {
    println!("{}\n", config::THIRD_PARTY_NOTICE);
    let api = ApiClient::new(None)?;
    let session = auth::login_via_browser(&api).await?;
    api.set_session(session).await?;
    println!("Signed in. The session is stored in the Secret Service keyring.");
    Ok(())
}

async fn logout() -> Result<()> {
    if let Some(session) = session::load().await? {
        let api = ApiClient::new(Some(session))?;
        let reply = api.send(Method::DELETE, &ApiClient::url("auth/v4"), &[], None).await?;
        if !reply.status.is_success() {
            eprintln!("Server-side revocation failed ({}); removing the local session anyway.", reply.status);
        }
    }
    session::delete().await?;
    println!("Signed out.");
    Ok(())
}

async fn list(path: &str) -> Result<()> {
    let verbose = std::env::var_os("PDPOC_VERBOSE").is_some();
    let session = session::load().await?.context("not signed in: run `pdpoc login` first")?;
    let key_password = session.key_password.clone();
    let api = Arc::new(ApiClient::new(Some(session))?);

    let keys = AccountKeys::load(&api, &key_password).await?;
    if verbose {
        for (email, count) in keys.summary() {
            eprintln!("[keys] {email}: {count} unlocked key(s)");
        }
    }

    pdsdk::set_crypto_library_dir(Path::new(pdsdk::DEFAULT_LIB_DIR));
    let host = Arc::new(AppHost { api, keys, verbose });
    let client = DriveClient::create(host, &format!("{}/", config::API_BASE_URL), ClientOptions::default()).await?;

    let result = list_with(&client, path).await;
    client.free().await?;
    result
}

async fn list_with(client: &DriveClient, path: &str) -> Result<()> {
    let root = client.my_files_folder().await?;
    let mut folder_uid = root.uid;
    for component in path.split('/').filter(|c| !c.is_empty()) {
        let children = children(client, &folder_uid).await?;
        let Some(uid) = children.iter().find_map(|node| match &node.kind {
            Some(Kind::Folder(f)) if f.name.as_ref().map(|n| n.display()).as_deref() == Some(component) => Some(f.uid.clone()),
            _ => None,
        }) else {
            bail!("folder not found: {component}");
        };
        folder_uid = uid;
    }

    let mut entries: Vec<(bool, String, String)> = children(client, &folder_uid)
        .await?
        .into_iter()
        .filter_map(|node| match node.kind? {
            Kind::Folder(f) => Some((true, f.name.map(|n| n.display()).unwrap_or_default(), String::new())),
            Kind::File(f) => {
                let size = f.active_revision.as_ref().and_then(|r| r.claimed_size).unwrap_or(f.total_storage_size);
                Some((false, f.name.map(|n| n.display()).unwrap_or_default(), human_size(size)))
            }
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));

    for (is_folder, name, size) in &entries {
        if *is_folder {
            println!("  {:>9}  {name}/", "");
        } else {
            println!("  {size:>9}  {name}");
        }
    }
    println!("{} item(s)", entries.len());
    Ok(())
}

async fn children(client: &DriveClient, folder_uid: &str) -> Result<Vec<Node>> {
    let uids = client.folder_children(folder_uid).await?;
    Ok(client.nodes(uids).await?)
}

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}
