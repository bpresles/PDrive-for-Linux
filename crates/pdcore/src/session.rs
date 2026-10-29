// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Session persistence in the Secret Service (GNOME Keyring) through oo7.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

const ATTRIBUTE_APP: &str = "application";
const APP_ID: &str = "pdrive";
/// Identifier used before the project got its name: sessions stored under it are migrated.
const LEGACY_APP_ID: &str = "nautilus-drive-poc";

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Session {
    pub uid: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Salted password that unlocks the user keys.
    pub key_password: String,
}

fn attributes() -> HashMap<&'static str, &'static str> {
    attributes_for(APP_ID)
}

fn attributes_for(app_id: &'static str) -> HashMap<&'static str, &'static str> {
    HashMap::from([(ATTRIBUTE_APP, app_id)])
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("cannot open the Secret Service keyring")
}

pub async fn load() -> Result<Option<Session>> {
    let keyring = keyring().await?;
    keyring.unlock().await?;
    if let Some(item) = keyring.search_items(&attributes()).await?.into_iter().next() {
        let secret = item.secret().await?;
        return Ok(Some(serde_json::from_slice(secret.as_bytes()).context("corrupted session in the keyring")?));
    }
    let Some(item) = keyring.search_items(&attributes_for(LEGACY_APP_ID)).await?.into_iter().next() else {
        return Ok(None);
    };
    let session: Session = serde_json::from_slice(item.secret().await?.as_bytes()).context("corrupted session in the keyring")?;
    save(&session).await?;
    keyring.delete(&attributes_for(LEGACY_APP_ID)).await?;
    Ok(Some(session))
}

pub async fn save(session: &Session) -> Result<()> {
    let keyring = keyring().await?;
    keyring.unlock().await?;
    let secret = zeroize::Zeroizing::new(serde_json::to_vec(session)?);
    keyring.create_item("PDrive for Linux: Proton Drive session", &attributes(), secret.as_slice(), true).await?;
    Ok(())
}

pub async fn delete() -> Result<()> {
    keyring().await?.delete(&attributes()).await?;
    Ok(())
}
