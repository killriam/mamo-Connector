//! OS credential storage for the MaMo API tokens, so they stay out of `settings.json`.
//!
//! On Windows the tokens live in Credential Manager (generic credentials, service
//! "MaMo Connector"). Other platforms have no store yet: `set` fails there, which makes
//! `Settings` keep the token in the settings file exactly as before.

use anyhow::Result;

/// Credential account for `Settings::auth_token`.
pub const ACCOUNT_API_TOKEN: &str = "api-token";
/// Credential account for `Settings::gamelog_config.auth_token`.
pub const ACCOUNT_GAMELOG_TOKEN: &str = "gamelog-token";

pub trait SecretStore {
    /// `Ok(None)` when nothing is stored under `account`.
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> Result<()>;
    /// Deleting a secret that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<()>;
}

/// Credential service holding the Connector's own API tokens.
pub const CONNECTOR_SERVICE: &str = "MaMo Connector";
/// Credential service holding the tokens of the MaMo MCP server (accounts "read" and "write"); the
/// MCP server reads the same entries, so this name is a contract with mcp-servers/mamo.
pub const AGENT_SERVICE: &str = "MaMo MCP";

#[cfg(windows)]
pub struct CredentialManagerStore {
    service: String,
}

#[cfg(windows)]
impl CredentialManagerStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(&self.service, account)?)
    }
}

#[cfg(windows)]
impl SecretStore for CredentialManagerStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        Ok(self.entry(account)?.set_password(value)?)
    }

    fn delete(&self, account: &str) -> Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// Platforms without a credential store integration: nothing is ever stored, writes fail.
#[cfg(not(windows))]
pub struct NoStore;

#[cfg(not(windows))]
impl SecretStore for NoStore {
    fn get(&self, _account: &str) -> Result<Option<String>> {
        Ok(None)
    }

    fn set(&self, _account: &str, _value: &str) -> Result<()> {
        anyhow::bail!("no credential store on this platform")
    }

    fn delete(&self, _account: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Round-trips through the real Windows Credential Manager under a throwaway account.
    #[test]
    fn test_credential_manager_round_trip() {
        let store = CredentialManagerStore::new(CONNECTOR_SERVICE);
        let account = "unit-test-round-trip";

        store.delete(account).unwrap();
        assert_eq!(store.get(account).unwrap(), None);

        store.set(account, "pat_test_value").unwrap();
        assert_eq!(store.get(account).unwrap().as_deref(), Some("pat_test_value"));

        store.delete(account).unwrap();
        assert_eq!(store.get(account).unwrap(), None);
        store.delete(account).unwrap(); // deleting a missing secret is not an error
    }
}

/// The store for the Connector's own API tokens.
pub fn default_store() -> Box<dyn SecretStore> {
    store_for(CONNECTOR_SERVICE)
}

/// The store for the MaMo MCP server's tokens (service `AGENT_SERVICE` unless a test passes another).
pub fn agent_store(service: &str) -> Box<dyn SecretStore> {
    store_for(service)
}

fn store_for(service: &str) -> Box<dyn SecretStore> {
    #[cfg(windows)]
    {
        Box::new(CredentialManagerStore::new(service))
    }
    #[cfg(not(windows))]
    {
        let _ = service;
        Box::new(NoStore)
    }
}
