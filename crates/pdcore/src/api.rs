// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Authenticated HTTP client for the Proton API, shared by the account code and the SDK host.

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode, header::HeaderMap};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, watch};

use crate::config::{API_BASE_URL, APP_VERSION};
use crate::session::{self, Session};

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// Only a new sign-in can help. Found in the error chain of the failed call (see
/// [`needs_sign_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInRequired {
    /// No session in the keyring.
    NotSignedIn,
    /// The server refused to renew the session: expired, revoked, or its refresh token was
    /// already used (a refresh token is valid once, so an older copy of the session is dead).
    SessionRejected,
    /// The stored key password no longer unlocks the account keys (password changed).
    KeysLocked,
}

impl std::fmt::Display for SignInRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SignInRequired::NotSignedIn => "not signed in: run `pdrive login` first",
            SignInRequired::SessionRejected => "the Proton session is no longer valid (expired or revoked): sign in again with `pdrive login`",
            SignInRequired::KeysLocked => "the account keys cannot be unlocked with the stored session: sign in again with `pdrive login`",
        })
    }
}

impl std::error::Error for SignInRequired {}

/// Whether the error comes from a session that only a new sign-in can replace.
pub fn needs_sign_in(error: &anyhow::Error) -> bool {
    error.chain().any(|e| e.is::<SignInRequired>())
}

/// What authenticates a request: the session it was sent with.
struct Credentials {
    uid: String,
    access_token: zeroize::Zeroizing<String>,
}

pub struct ApiClient {
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
    /// Held while the session is renewed: a refresh token is valid once, so concurrent requests
    /// that got a 401 must not each renew the session with it.
    refresh_lock: Mutex<()>,
    rejected: watch::Sender<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RefreshResponse {
    #[serde(rename = "UID")]
    uid: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
}

impl ApiClient {
    pub fn new(session: Option<Session>) -> Result<Self> {
        let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(30)).timeout(std::time::Duration::from_secs(300)).build()?;
        Ok(Self { http, session: Mutex::new(session), refresh_lock: Mutex::new(()), rejected: watch::Sender::new(false) })
    }

    pub fn url(path: &str) -> String {
        format!("{API_BASE_URL}/{}", path.trim_start_matches('/'))
    }

    pub async fn session(&self) -> Option<Session> {
        self.session.lock().await.clone()
    }

    pub async fn set_session(&self, session: Session) -> Result<()> {
        session::save(&session).await?;
        *self.session.lock().await = Some(session);
        Ok(())
    }

    /// The server refused to renew the session ([`SignInRequired::SessionRejected`]): every
    /// authenticated call fails from then on.
    pub fn session_rejected(&self) -> bool {
        *self.rejected.borrow()
    }

    /// Returns once [`ApiClient::session_rejected`] is true.
    pub async fn wait_session_rejected(&self) {
        let _ = self.rejected.subscribe().wait_for(|rejected| *rejected).await;
    }

    async fn credentials(&self) -> Option<Credentials> {
        let session = self.session.lock().await;
        session.as_ref().map(|s| Credentials { uid: s.uid.clone(), access_token: zeroize::Zeroizing::new(s.access_token.clone()) })
    }

    /// Sends a request with the session headers. On 401, renews the session once and retries;
    /// fails with [`SignInRequired::SessionRejected`] when the server refuses to renew it.
    pub async fn send(&self, method: Method, url: &str, headers: &[(String, String)], body: Option<Vec<u8>>) -> Result<Reply> {
        let credentials = self.credentials().await;
        let first = self.send_once(method.clone(), url, headers, body.clone(), credentials.as_ref()).await?;
        let Some(credentials) = credentials.filter(|_| first.status == StatusCode::UNAUTHORIZED) else {
            return Ok(first);
        };
        let renewed = self.refresh(&credentials).await?;
        self.send_once(method, url, headers, body, Some(&renewed)).await
    }

    async fn send_once(
        &self,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
        credentials: Option<&Credentials>,
    ) -> Result<Reply> {
        let mut request = self.http.request(method, url).header("x-pm-appversion", APP_VERSION);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(credentials) = credentials {
            request = request.header("x-pm-uid", &credentials.uid).header("Authorization", format!("Bearer {}", credentials.access_token.as_str()));
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.with_context(|| format!("request to {url} failed"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await?.to_vec();
        Ok(Reply { status, headers, body })
    }

    /// Renews the session after `failed` got a 401, and returns the credentials to retry with.
    /// Network errors and temporary refusals (429, 5xx) are returned as such: the session may
    /// still be valid.
    async fn refresh(&self, failed: &Credentials) -> Result<Credentials> {
        let _renewing = self.refresh_lock.lock().await;
        let Some(mut session) = self.session().await else { bail!(SignInRequired::NotSignedIn) };
        if session.access_token != *failed.access_token {
            // Renewed by another request in the meantime.
            return self.credentials().await.context("session removed");
        }
        if self.session_rejected() {
            bail!(SignInRequired::SessionRejected);
        }
        let body = serde_json::json!({
            "ResponseType": "token",
            "GrantType": "refresh_token",
            "RefreshToken": session.refresh_token,
        });
        let reply = self.send_once(Method::POST, &Self::url("auth/v4/refresh"), &json_headers(), Some(serde_json::to_vec(&body)?), Some(failed)).await?;
        if !reply.status.is_success() {
            let error = format!("session refresh refused: API error {}: {}", reply.status, String::from_utf8_lossy(&reply.body));
            if !is_definitive_refusal(reply.status) {
                bail!(error);
            }
            return self.rejected_session(&session).await.map_err(|e| e.context(error));
        }
        let refreshed: RefreshResponse = serde_json::from_slice(&reply.body)?;
        if let Some(uid) = refreshed.uid {
            session.uid = uid;
        }
        session.access_token = refreshed.access_token;
        if let Some(refresh_token) = refreshed.refresh_token {
            session.refresh_token = refresh_token;
        }
        // The previous refresh token is spent: keep the new session even if it cannot be stored.
        *self.session.lock().await = Some(session.clone());
        if let Err(e) = session::save(&session).await {
            eprintln!("cannot store the renewed session in the keyring: {e:#}");
        }
        self.credentials().await.context("session removed")
    }

    /// The server refused to renew `session`. If another program stored a newer session in the
    /// meantime (it renewed it, or signed in again), returns it to retry with. Otherwise the
    /// session is dead: it is removed from the keyring, and every call fails from now on.
    async fn rejected_session(&self, session: &Session) -> Result<Credentials> {
        match session::load().await {
            Ok(Some(stored)) if stored.refresh_token != session.refresh_token => {
                *self.session.lock().await = Some(stored);
                return self.credentials().await.context("session removed");
            }
            Ok(Some(_)) => {
                if let Err(e) = session::delete().await {
                    eprintln!("cannot remove the rejected session from the keyring: {e:#}");
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("cannot read the keyring: {e:#}"),
        }
        self.rejected.send_replace(true);
        Err(SignInRequired::SessionRejected.into())
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let reply = self.send(Method::GET, &Self::url(path), &[], None).await?;
        parse_json(reply)
    }
}

/// A refusal of the refresh endpoint that retrying cannot change.
fn is_definitive_refusal(status: StatusCode) -> bool {
    status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::REQUEST_TIMEOUT
}

pub fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".into(), "application/json".into())]
}

pub fn parse_json<T: DeserializeOwned>(reply: Reply) -> Result<T> {
    if !reply.status.is_success() {
        bail!("API error {}: {}", reply.status, String::from_utf8_lossy(&reply.body));
    }
    Ok(serde_json::from_slice(&reply.body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_required_is_found_under_context() {
        let error = anyhow::Error::new(SignInRequired::SessionRejected).context("session refresh refused: API error 400").context("cannot list the folder");
        assert!(needs_sign_in(&error));
        assert!(!needs_sign_in(&anyhow::anyhow!("API error 401 Unauthorized")));
    }

    #[test]
    fn only_client_errors_reject_the_session() {
        for status in [StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN, StatusCode::UNPROCESSABLE_ENTITY] {
            assert!(is_definitive_refusal(status), "{status}");
        }
        for status in [StatusCode::TOO_MANY_REQUESTS, StatusCode::REQUEST_TIMEOUT, StatusCode::INTERNAL_SERVER_ERROR, StatusCode::SERVICE_UNAVAILABLE] {
            assert!(!is_definitive_refusal(status), "{status}");
        }
    }
}
