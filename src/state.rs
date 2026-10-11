use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Deref,
};

use exn::{ResultExt, bail};
use fqdn::FQDN;
use russh::keys::{HashAlg, PublicKey, ssh_key::public::KeyData};

use crate::{
    config::{Config, ServerConfig, Username},
    error::ConfigError,
    stalwart::Stalwart,
    vault::Vault,
};

#[derive(Debug)]
pub struct User {
    pub name: Username,
    pub domains: BTreeSet<FQDN>,
}

impl User {
    pub fn owns(&self, address: &str) -> bool {
        address
            .rsplit_once('@')
            .and_then(|(_, domain)| domain.parse::<FQDN>().ok())
            .is_some_and(|domain| self.domains.contains(&domain))
    }
}

#[derive(Debug)]
pub struct AppStateInner {
    pub server: ServerConfig,
    pub stalwart: Stalwart,
    pub vault: Vault,
    pub domains: BTreeSet<FQDN>,
    pub users: BTreeMap<Username, User>,
    keys: HashMap<KeyData, Username>,
}

#[derive(Debug, Clone, Copy)]
pub struct AppState(&'static AppStateInner);

impl Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl AppState {
    pub fn new(config: Config) -> exn::Result<Self, ConfigError> {
        let Config {
            domains,
            server,
            stalwart,
            vault,
            users: user_configs,
        } = config;

        let mut users = BTreeMap::new();
        let mut keys = HashMap::new();

        for (name, cfg) in user_configs {
            if let Some(domain) = cfg.domains.difference(&domains).next() {
                bail!(ConfigError::UnknownDomain(domain.to_string()));
            }
            for key in &cfg.ssh_keys {
                if keys.insert(key.key_data().clone(), name.clone()).is_some() {
                    bail!(ConfigError::DuplicateKey(
                        key.fingerprint(HashAlg::Sha256).to_string()
                    ));
                }
            }

            users.insert(
                name.clone(),
                User {
                    name,
                    domains: cfg.domains,
                },
            );
        }

        let inner = AppStateInner {
            server,
            stalwart: Stalwart::new(stalwart)?,
            vault: Vault::new(vault).or_raise(|| ConfigError::Vault)?,
            domains,
            users,
            keys,
        };
        Ok(Self(Box::leak(Box::new(inner))))
    }

    pub fn user_by_key(self, key: &PublicKey) -> Option<&'static User> {
        let inner = self.0;
        inner
            .keys
            .get(key.key_data())
            .and_then(|name| inner.users.get(name))
    }

    pub fn key_count(self) -> usize {
        self.0.keys.len()
    }
}
