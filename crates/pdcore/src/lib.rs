//! Account, session and SDK host shared by the client programs.

pub mod api;
pub mod auth;
pub mod config;
pub mod host;
pub mod keys;
pub mod session;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use pdsdk::{ClientOptions, DriveClient};
use reqwest::Method;

use api::ApiClient;
use host::AppHost;
use keys::AccountKeys;

/// Signs in through the browser and stores the session in the keyring.
pub async fn login() -> Result<()> {
    println!("{}\n", config::THIRD_PARTY_NOTICE);
    let api = ApiClient::new(None)?;
    let session = auth::login_via_browser(&api).await?;
    api.set_session(session).await?;
    Ok(())
}

/// Revokes the session on the server and removes it from the keyring.
pub async fn logout() -> Result<()> {
    if let Some(session) = session::load().await? {
        let api = ApiClient::new(Some(session))?;
        let reply = api.send(Method::DELETE, &ApiClient::url("auth/v4"), &[], None).await?;
        if !reply.status.is_success() {
            eprintln!("Server-side revocation failed ({}); removing the local session anyway.", reply.status);
        }
    }
    session::delete().await
}

/// Loads the stored session, unlocks the account keys and creates an SDK client.
pub async fn connect(verbose: bool) -> Result<DriveClient> {
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
    Ok(DriveClient::create(host, &format!("{}/", config::API_BASE_URL), ClientOptions::default()).await?)
}
