// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
/// Honest client identification required by Proton for third-party applications:
/// `external-drive-{name}@{semver}-{channel}`. The version is the one of Cargo.toml
/// (`[workspace.package]`, the single source of the version, see scripts/check-version.sh).
/// The channel stays `alpha` while the project has not reached 1.0 (first public releases).
pub const APP_VERSION: &str = concat!("external-drive-pdrive_for_linux@", env!("CARGO_PKG_VERSION"), "-alpha");

/// Client ID used for the browser sign-in of third-party applications.
pub const AUTH_CLIENT_ID: &str = "external-drive";

pub const API_BASE_URL: &str = "https://drive-api.proton.me";
pub const ACCOUNT_URL: &str = "https://account.proton.me";

pub const THIRD_PARTY_NOTICE: &str = "This is a third-party application not officially supported by Proton.";

#[cfg(test)]
mod tests {
    use super::APP_VERSION;

    #[test]
    fn app_version_follows_cargo_version() {
        assert_eq!(APP_VERSION, format!("external-drive-pdrive_for_linux@{}-alpha", env!("CARGO_PKG_VERSION")));
    }
}
