//! Session persistence in the Secret Service (GNOME Keyring) through oo7.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

const ATTRIBUTE_APP: &str = "application";
const APP_ID: &str = "nautilus-drive-poc";

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Session {
    pub uid: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Salted password that unlocks the user keys.
    pub key_password: String,
}

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([(ATTRIBUTE_APP, APP_ID)])
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("cannot open the Secret Service keyring")
}

pub async fn load() -> Result<Option<Session>> {
    let keyring = keyring().await?;
    keyring.unlock().await?;
    let Some(item) = keyring.search_items(&attributes()).await?.into_iter().next() else {
        return Ok(None);
    };
    let secret = item.secret().await?;
    Ok(Some(serde_json::from_slice(secret.as_bytes()).context("corrupted session in the keyring")?))
}

pub async fn save(session: &Session) -> Result<()> {
    let keyring = keyring().await?;
    keyring.unlock().await?;
    let secret = zeroize::Zeroizing::new(serde_json::to_vec(session)?);
    keyring
        .create_item("Proton Drive session (third-party Nautilus PoC)", &attributes(), secret.as_slice(), true)
        .await?;
    Ok(())
}

pub async fn delete() -> Result<()> {
    keyring().await?.delete(&attributes()).await?;
    Ok(())
}
