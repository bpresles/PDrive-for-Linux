// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Authenticated HTTP client for the Proton API, shared by the account code and the SDK host.

use anyhow::{bail, Context, Result};
use reqwest::{header::HeaderMap, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::{API_BASE_URL, APP_VERSION};
use crate::session::{self, Session};

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub struct ApiClient {
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
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
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self { http, session: Mutex::new(session) })
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

    /// Sends a request with the session headers. On 401, refreshes the session once and retries.
    pub async fn send(&self, method: Method, url: &str, headers: &[(String, String)], body: Option<Vec<u8>>) -> Result<Reply> {
        let first = self.send_once(method.clone(), url, headers, body.clone()).await?;
        if first.status != StatusCode::UNAUTHORIZED || !self.refresh().await? {
            return Ok(first);
        }
        self.send_once(method, url, headers, body).await
    }

    async fn send_once(&self, method: Method, url: &str, headers: &[(String, String)], body: Option<Vec<u8>>) -> Result<Reply> {
        let mut request = self.http.request(method, url).header("x-pm-appversion", APP_VERSION);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(session) = self.session.lock().await.as_ref() {
            request = request
                .header("x-pm-uid", &session.uid)
                .header("Authorization", format!("Bearer {}", session.access_token));
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

    async fn refresh(&self) -> Result<bool> {
        let Some(mut session) = self.session().await else {
            return Ok(false);
        };
        let body = serde_json::json!({
            "ResponseType": "token",
            "GrantType": "refresh_token",
            "RefreshToken": session.refresh_token,
        });
        let reply = self
            .send_once(Method::POST, &Self::url("auth/v4/refresh"), &json_headers(), Some(serde_json::to_vec(&body)?))
            .await?;
        if !reply.status.is_success() {
            return Ok(false);
        }
        let refreshed: RefreshResponse = serde_json::from_slice(&reply.body)?;
        if let Some(uid) = refreshed.uid {
            session.uid = uid;
        }
        session.access_token = refreshed.access_token;
        if let Some(refresh_token) = refreshed.refresh_token {
            session.refresh_token = refresh_token;
        }
        self.set_session(session).await?;
        Ok(true)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let reply = self.send(Method::GET, &Self::url(path), &[], None).await?;
        parse_json(reply)
    }
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
