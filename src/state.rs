use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Deref,
};

use exn::{ResultExt, bail};
use fqdn::FQDN;
use russh::keys::{HashAlg, PublicKey, ssh_key::public::KeyData};

use crate::{
    config::{Config, Email, ServerConfig, Username},
    error::ConfigError,
    files::Files,
    stalwart::Stalwart,
    vault::Vault,
};

#[derive(Debug)]
pub enum Grant {
    Domains(BTreeSet<FQDN>),
    Addresses(BTreeSet<Email>),
}

#[derive(Debug)]
pub struct User {
    pub name: Username,
    pub grant: Grant,
}

impl User {
    pub fn owns(&self, address: &str) -> bool {
        match &self.grant {
            Grant::Domains(domains) => address
                .rsplit_once('@')
                .and_then(|(_, domain)| domain.parse::<FQDN>().ok())
                .is_some_and(|domain| domains.contains(&domain)),
            Grant::Addresses(addresses) => addresses
                .iter()
                .any(|a| a.as_str().eq_ignore_ascii_case(address)),
        }
    }

    pub fn domains(&self) -> Vec<&FQDN> {
        match &self.grant {
            Grant::Domains(domains) => domains.iter().collect(),
            Grant::Addresses(_) => Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct AppStateInner {
    pub server: ServerConfig,
    pub stalwart: Stalwart,
    pub vault: Vault,
    pub files: Files,
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
            files,
            users: user_configs,
        } = config;

        let mut users = BTreeMap::new();
        let mut keys = HashMap::new();

        let mut claimed = BTreeSet::new();
        for (name, cfg) in user_configs {
            let grant = match (cfg.domains, cfg.addresses) {
                (Some(granted), None) => {
                    if let Some(domain) = granted.difference(&domains).next() {
                        bail!(ConfigError::UnknownDomain(domain.to_string()));
                    }
                    Grant::Domains(granted)
                }
                (None, Some(addresses)) => {
                    for address in &addresses {
                        if !domains.contains(address.domain()) {
                            bail!(ConfigError::UnknownDomain(address.domain().to_string()));
                        }
                        if !claimed.insert(address.clone()) {
                            bail!(ConfigError::DuplicateAddress(address.to_string()));
                        }
                    }
                    Grant::Addresses(addresses)
                }
                (Some(_), Some(_)) => bail!(ConfigError::BothGrants(name.to_string())),
                (None, None) => bail!(ConfigError::NoGrant(name.to_string())),
            };
            for key in &cfg.ssh_keys {
                if keys.insert(key.key_data().clone(), name.clone()).is_some() {
                    bail!(ConfigError::DuplicateKey(
                        key.fingerprint(HashAlg::Sha256).to_string()
                    ));
                }
            }

            users.insert(name.clone(), User { name, grant });
        }

        for address in &claimed {
            let free = users.values().find(|user| match &user.grant {
                Grant::Domains(granted) => granted.contains(address.domain()),
                Grant::Addresses(_) => false,
            });
            if let Some(user) = free {
                bail!(ConfigError::ClaimedDomain {
                    address: address.to_string(),
                    domain: address.domain().to_string(),
                    user: user.name.to_string(),
                });
            }
        }

        let inner = AppStateInner {
            server,
            stalwart: Stalwart::new(stalwart)?,
            vault: Vault::new(vault).or_raise(|| ConfigError::Vault)?,
            files: Files::new(files).or_raise(|| ConfigError::Files)?,
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
