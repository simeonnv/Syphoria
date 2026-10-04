use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Deref,
};

use anyhow::{Result, bail};
use exn_anyhow::into_anyhow;
use fqdn::FQDN;
use russh::keys::{HashAlg, PublicKey, ssh_key::public::KeyData};

use crate::{
    config::{Config, ServerConfig, Username},
    stalwart::Stalwart,
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
    pub fn new(config: Config) -> Result<Self> {
        let Config {
            domains,
            server,
            stalwart,
            users: user_configs,
        } = config;

        let mut users = BTreeMap::new();
        let mut keys = HashMap::new();

        for (name, cfg) in user_configs {
            if let Some(domain) = cfg.domains.difference(&domains).next() {
                bail!("user `{name}`: domain `{domain}` is not in the global `domains` list");
            }
            for key in &cfg.ssh_keys {
                if let Some(owner) = keys.insert(key.key_data().clone(), name.clone()) {
                    let fp = key.fingerprint(HashAlg::Sha256);
                    bail!("user `{name}`: ssh key {fp} is already assigned to user `{owner}`");
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
            stalwart: Stalwart::new(stalwart).map_err(into_anyhow)?,
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
