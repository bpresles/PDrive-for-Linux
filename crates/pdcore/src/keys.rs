// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Account data the SDK asks for: addresses and their unlocked private keys.

use anyhow::{bail, Result};
use proton_crypto::crypto::{DataEncoding, PGPProviderSync};
use proton_crypto_account::keys::{AddressKeys, LockedKey, UserKeys};
use proton_crypto_account::salts::KeySecret;
use serde::Deserialize;
use zeroize::Zeroizing;

use pdsdk::proto;

use crate::api::ApiClient;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UsersResponse {
    user: ApiUser,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiUser {
    keys: Vec<LockedKey>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AddressesResponse {
    addresses: Vec<ApiAddress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiAddress {
    #[serde(rename = "ID")]
    id: String,
    email: String,
    order: i32,
    status: i32,
    keys: Vec<LockedKey>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PublicKeysResponse {
    address: PublicKeysAddress,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PublicKeysAddress {
    keys: Vec<ApiPublicKey>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiPublicKey {
    public_key: String,
}

struct UnlockedKey {
    primary: bool,
    private_key: Zeroizing<Vec<u8>>,
    public_key: Vec<u8>,
}

struct AccountAddress {
    message: proto::Address,
    keys: Vec<UnlockedKey>,
}

pub struct AccountKeys {
    addresses: Vec<AccountAddress>,
}

impl AccountKeys {
    /// Fetches the user and address keys and unlocks them with the session key password.
    pub async fn load(api: &ApiClient, key_password: &str) -> Result<Self> {
        let user: UsersResponse = api.get_json("core/v4/users").await?;
        let addresses: AddressesResponse = api.get_json("core/v4/addresses?Page=0&PageSize=50").await?;

        let pgp = proton_crypto::new_pgp_provider();
        let secret = KeySecret::new(key_password.as_bytes().to_vec());
        let user_keys = UserKeys::new(user.user.keys).unlock(&pgp, &secret);
        if user_keys.unlocked_keys.is_empty() {
            bail!("no user key could be unlocked");
        }

        let mut result = Vec::new();
        for address in addresses.addresses {
            let primary_key_index = address.keys.iter().position(|k| k.primary).unwrap_or(0) as i32;
            let key_messages = address
                .keys
                .iter()
                .map(|k| {
                    let flags = k.flags.unwrap_or_default();
                    proto::AddressKey {
                        address_id: address.id.clone(),
                        address_key_id: k.id.to_string(),
                        is_active: k.active,
                        is_allowed_for_encryption: !flags.is_obsolete(),
                        is_allowed_for_verification: !flags.is_compromised(),
                    }
                })
                .collect();

            let unlocked = AddressKeys::new(address.keys).unlock(&pgp, &user_keys.unlocked_keys, Some(&secret));
            let mut keys = Vec::new();
            for key in unlocked.unlocked_keys {
                keys.push(UnlockedKey {
                    primary: key.primary,
                    private_key: Zeroizing::new(pgp.private_key_export_unlocked(&key.private_key, DataEncoding::Bytes)?.as_ref().to_vec()),
                    public_key: pgp.public_key_export(&key.public_key, DataEncoding::Bytes)?.as_ref().to_vec(),
                });
            }

            result.push(AccountAddress {
                message: proto::Address {
                    address_id: address.id,
                    order: address.order,
                    email_address: address.email,
                    status: if address.status == 1 { proto::AddressStatus::Enabled } else { proto::AddressStatus::Disabled } as i32,
                    keys: key_messages,
                    primary_key_index,
                },
                keys,
            });
        }
        Ok(Self { addresses: result })
    }

    fn address(&self, address_id: &str) -> Result<&AccountAddress, String> {
        self.addresses
            .iter()
            .find(|a| a.message.address_id == address_id)
            .ok_or_else(|| format!("unknown address {address_id}"))
    }

    /// Answers an SDK account request.
    pub async fn handle(&self, api: &ApiClient, request: proto::AccountRequest) -> Result<proto::Any, String> {
        use proto::account_request::Payload;

        let address_message = |a: &AccountAddress| proto::pack("proton.drive.sdk.Address", &a.message);
        let repeated = |value: Vec<Vec<u8>>| proto::pack("proton.drive.sdk.RepeatedBytesValue", &proto::RepeatedBytesValue { value });

        match request.payload.ok_or("empty account request")? {
            Payload::GetAddress(r) => Ok(address_message(self.address(&r.address_id)?)),
            Payload::GetDefaultAddress(_) => {
                let default = self
                    .addresses
                    .iter()
                    .filter(|a| a.message.status == proto::AddressStatus::Enabled as i32)
                    .min_by_key(|a| a.message.order)
                    .ok_or("no enabled address")?;
                Ok(address_message(default))
            }
            Payload::GetAddressPrimaryPrivateKey(r) => {
                let address = self.address(&r.address_id)?;
                let key = address.keys.iter().find(|k| k.primary).or(address.keys.first()).ok_or("address has no unlocked key")?;
                Ok(proto::pack("google.protobuf.BytesValue", &*key.private_key))
            }
            Payload::GetAddressPrivateKeys(r) => {
                let address = self.address(&r.address_id)?;
                Ok(repeated(address.keys.iter().map(|k| k.private_key.to_vec()).collect()))
            }
            Payload::GetAddressPublicKeys(r) => {
                if let Some(own) = self.addresses.iter().find(|a| a.message.email_address.eq_ignore_ascii_case(&r.email_address)) {
                    return Ok(repeated(own.keys.iter().map(|k| k.public_key.clone()).collect()));
                }
                Ok(repeated(fetch_public_keys(api, &r.email_address).await.map_err(|e| e.to_string())?))
            }
        }
    }

    pub fn summary(&self) -> Vec<(String, usize)> {
        self.addresses.iter().map(|a| (a.message.email_address.clone(), a.keys.len())).collect()
    }
}

async fn fetch_public_keys(api: &ApiClient, email: &str) -> Result<Vec<Vec<u8>>> {
    let email = percent_encoding::utf8_percent_encode(email, percent_encoding::NON_ALPHANUMERIC);
    let response: PublicKeysResponse = api.get_json(&format!("core/v4/keys/all?Email={email}")).await?;
    let pgp = proton_crypto::new_pgp_provider();
    let mut keys = Vec::new();
    for key in response.address.keys {
        let public_key = pgp.public_key_import(key.public_key.as_bytes(), DataEncoding::Armor)?;
        keys.push(pgp.public_key_export(&public_key, DataEncoding::Bytes)?.as_ref().to_vec());
    }
    Ok(keys)
}
