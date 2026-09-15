//! Where the OAuth token lives between runs.
//!
//! `keyring` 4's `v1` surface hardwires the platform credential store (macOS
//! Keychain, Windows Credential Manager, Secret Service) and offers no
//! in-memory backend, so tests would need a real OS keychain to run. This
//! trait is the seam that keeps them off it — [`KeyringStore`] is the only
//! implementation that ships.
use anyhow::Context;

/// Keychain service name every instance's entry is filed under.
const SERVICE: &str = "claudius-maximus";

pub(crate) trait TokenStore: Send + Sync {
    /// The stored token for `instance_name`, or `None` if there isn't one yet.
    fn load(&self, instance_name: &str) -> anyhow::Result<Option<String>>;
    fn store(&self, instance_name: &str, token: &str) -> anyhow::Result<()>;
}

/// The OS keychain, keyed on the instance name.
pub(crate) struct KeyringStore;

impl KeyringStore {
    fn entry(instance_name: &str) -> anyhow::Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, instance_name)
            .with_context(|| format!("opening keychain entry {SERVICE}/{instance_name}"))
    }
}

impl TokenStore for KeyringStore {
    fn load(&self, instance_name: &str) -> anyhow::Result<Option<String>> {
        match Self::entry(instance_name)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("reading token from keychain"),
        }
    }

    fn store(&self, instance_name: &str, token: &str) -> anyhow::Result<()> {
        Self::entry(instance_name)?
            .set_password(token)
            .context("writing token to keychain")
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::TokenStore;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory stand-in so tests never touch a real keychain.
    #[derive(Default)]
    pub(crate) struct MemoryStore(Mutex<HashMap<String, String>>);

    impl MemoryStore {
        pub(crate) fn with_token(instance_name: &str, token: &str) -> Self {
            let this = Self::default();
            this.store(instance_name, token).unwrap();
            this
        }

        pub(crate) fn get(&self, instance_name: &str) -> Option<String> {
            self.0.lock().unwrap().get(instance_name).cloned()
        }
    }

    impl TokenStore for MemoryStore {
        fn load(&self, instance_name: &str) -> anyhow::Result<Option<String>> {
            Ok(self.get(instance_name))
        }

        fn store(&self, instance_name: &str, token: &str) -> anyhow::Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(instance_name.to_owned(), token.to_owned());
            Ok(())
        }
    }
}
