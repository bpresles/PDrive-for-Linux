// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Implementation of what the SDK asks the application for.

use std::sync::Arc;

use pdsdk::{BoxFuture, HttpReply, proto};
use reqwest::Method;

use crate::api::ApiClient;
use crate::keys::AccountKeys;

pub struct AppHost {
    pub api: Arc<ApiClient>,
    pub keys: AccountKeys,
    pub verbose: bool,
}

impl pdsdk::Host for AppHost {
    fn http(&self, request: proto::HttpRequest, body: Option<Vec<u8>>) -> BoxFuture<'_, Result<HttpReply, String>> {
        Box::pin(async move {
            let method = Method::from_bytes(request.method.as_bytes()).map_err(|e| e.to_string())?;
            let headers: Vec<(String, String)> = request.headers.iter().flat_map(|h| h.values.iter().map(|v| (h.name.clone(), v.clone()))).collect();
            let reply = self.api.send(method, &request.url, &headers, body).await.map_err(|e| format!("{e:#}"))?;
            if self.verbose {
                eprintln!("[http] {} {} -> {}", request.method, request.url, reply.status);
            }
            Ok(HttpReply {
                status: reply.status.as_u16(),
                headers: reply.headers.iter().filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned()))).collect(),
                body: reply.body,
            })
        })
    }

    fn account(&self, request: proto::AccountRequest) -> BoxFuture<'_, Result<proto::Any, String>> {
        Box::pin(self.keys.handle(&self.api, request))
    }

    fn log(&self, event: proto::LogEvent) {
        // Microsoft.Extensions.Logging levels: 0 trace .. 5 critical.
        if event.level >= 3 || (self.verbose && event.level >= 2) {
            eprintln!("[sdk:{}] {}: {}", event.level, event.category_name, event.message);
        }
    }
}
