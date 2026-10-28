// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Browser sign-in through a session fork: the user signs in on account.proton.me, which hands a
//! child session and the encrypted key password back to this client. The client never sees the
//! account password, and CAPTCHA and 2FA are handled by Proton's web page.

use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::api::{parse_json, ApiClient};
use crate::config::{ACCOUNT_URL, AUTH_CLIENT_ID};
use crate::session::Session;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_WAIT: Duration = Duration::from_secs(10 * 60);
const NONCE_LENGTH: usize = 12;
const TAG_LENGTH: usize = 16;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ForkInit {
    selector: String,
    user_code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ForkStatus {
    #[serde(rename = "UID")]
    uid: String,
    access_token: String,
    refresh_token: String,
    payload: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForkPayload {
    key_password: String,
}

pub async fn login_via_browser(api: &ApiClient) -> Result<Session> {
    let init: ForkInit = api.get_json("auth/v4/sessions/forks").await?;

    let mut key = Zeroizing::new([0u8; 32]);
    rand::rng().fill_bytes(&mut key[..]);
    let payload = format!("0:{}:{}:{}", init.user_code, BASE64.encode(&key[..]), AUTH_CLIENT_ID);
    let payload = percent_encoding::utf8_percent_encode(&payload, percent_encoding::NON_ALPHANUMERIC);
    let url = format!("{ACCOUNT_URL}/desktop/login?app=drive&pv=3#payload={payload}");

    println!("Sign in with your browser (it can be on another device):\n{url}\n");
    if std::process::Command::new("xdg-open").arg(&url).spawn().is_err() {
        println!("Could not open a browser automatically: open the link above manually.");
    }
    println!("Waiting for the sign-in to complete...");

    let started = Instant::now();
    let path = format!("auth/v4/sessions/forks/{}", init.selector);
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if started.elapsed() > MAX_WAIT {
            bail!("sign-in timed out");
        }
        let reply = api.send(Method::GET, &ApiClient::url(&path), &[], None).await?;
        // 422 means the user has not completed the sign-in yet.
        if reply.status == StatusCode::UNPROCESSABLE_ENTITY {
            continue;
        }
        let status: ForkStatus = parse_json(reply)?;
        let key_password = decrypt_key_password(&key, &status.payload)?;
        return Ok(Session {
            uid: status.uid,
            access_token: status.access_token,
            refresh_token: status.refresh_token,
            key_password,
        });
    }
}

/// Fork payload: base64(nonce || ciphertext || tag), AES-256-GCM with AAD "fork".
fn decrypt_key_password(key: &[u8; 32], payload: &str) -> Result<String> {
    let blob = BASE64.decode(payload).context("fork payload is not base64")?;
    if blob.len() < NONCE_LENGTH + TAG_LENGTH {
        bail!("fork payload too short");
    }
    let (nonce, ciphertext) = blob.split_at(NONCE_LENGTH);
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad: b"fork" })
            .map_err(|_| anyhow!("cannot decrypt the fork payload"))?,
    );
    let payload: ForkPayload = serde_json::from_slice(&plaintext).context("invalid fork payload")?;
    Ok(payload.key_password)
}
