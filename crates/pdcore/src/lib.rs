// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
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
    Ok(connect_with_api(verbose).await?.0)
}

/// Like [`connect`], also returning the API client (for account calls such as [`account_info`]).
pub async fn connect_with_api(verbose: bool) -> Result<(DriveClient, Arc<ApiClient>)> {
    let session = session::load().await?.context("not signed in: run `pdrive login` first")?;
    let key_password = session.key_password.clone();
    let api = Arc::new(ApiClient::new(Some(session))?);

    let keys = AccountKeys::load(&api, &key_password).await?;
    if verbose {
        for (email, count) in keys.summary() {
            eprintln!("[keys] {email}: {count} unlocked key(s)");
        }
    }

    pdsdk::set_crypto_library_dir(Path::new(pdsdk::DEFAULT_LIB_DIR));
    let host = Arc::new(AppHost { api: api.clone(), keys, verbose });
    let client = DriveClient::create(host, &format!("{}/", config::API_BASE_URL), ClientOptions::default()).await?;
    Ok((client, api))
}

/// Whether a session is stored in the keyring.
pub async fn is_signed_in() -> Result<bool> {
    Ok(session::load().await?.is_some())
}

/// Address and storage quota of the account.
pub struct AccountInfo {
    pub email: String,
    pub used_space: u64,
    pub max_space: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserResponse {
    user: User,
}

/// Fields of `GET core/v4/users` (see `UserDto` in the SDK's account project).
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct User {
    email: String,
    used_space: u64,
    max_space: u64,
}

/// One call to the user endpoint: the callers must not repeat it in a tight loop.
pub async fn account_info(api: &ApiClient) -> Result<AccountInfo> {
    let response: UserResponse = api.get_json("core/v4/users").await?;
    Ok(AccountInfo { email: response.user.email, used_space: response.user.used_space, max_space: response.user.max_space })
}
