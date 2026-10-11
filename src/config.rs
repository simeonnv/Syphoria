use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use email_address::{EmailAddress, Options};
use exn::ResultExt;
use fqdn::FQDN;
use russh::keys::PublicKey;
use serde::Deserialize;
use serde_with::{DisplayFromStr, MapPreventDuplicates, SetPreventDuplicates, serde_as};
use url::Url;

use crate::error::ConfigError;

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde_as(as = "SetPreventDuplicates<_>")]
    pub domains: BTreeSet<FQDN>,
    pub server: ServerConfig,
    pub stalwart: StalwartConfig,
    pub vault: VaultConfig,
    #[serde_as(as = "MapPreventDuplicates<_, _>")]
    #[serde(default)]
    pub users: BTreeMap<Username, UserConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub host_key: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StalwartConfig {
    pub url: Url,
    pub admin_user: String,
    pub admin_password_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    pub dir: PathBuf,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub ssh_keys: Vec<PublicKey>,
    #[serde_as(as = "Option<SetPreventDuplicates<_>>")]
    #[serde(default)]
    pub domains: Option<BTreeSet<FQDN>>,
    #[serde_as(as = "Option<SetPreventDuplicates<_>>")]
    #[serde(default)]
    pub addresses: Option<BTreeSet<Email>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct Email {
    address: String,
    domain: FQDN,
}

impl Email {
    pub fn as_str(&self) -> &str {
        &self.address
    }

    pub fn domain(&self) -> &FQDN {
        &self.domain
    }
}

impl TryFrom<String> for Email {
    type Error = String;

    fn try_from(address: String) -> Result<Self, Self::Error> {
        let options = Options::default()
            .without_display_text()
            .without_domain_literal();
        let email = EmailAddress::parse_with_options(&address, options)
            .map_err(|e| format!("invalid address `{address}`: {e}"))?;
        let domain = email
            .domain()
            .parse()
            .map_err(|e| format!("invalid domain in `{address}`: {e}"))?;
        Ok(Self {
            address: address.to_lowercase(),
            domain,
        })
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.address)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct Username(String);

impl Username {
    const MAX_LEN: usize = 64;
}

impl TryFrom<String> for Username {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let Some(first) = name.chars().next() else {
            return Err("user name may not be empty".into());
        };
        if name.len() > Self::MAX_LEN {
            return Err(format!(
                "user name `{name}` is longer than {} characters",
                Self::MAX_LEN
            ));
        }
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(format!(
                "user name `{name}` must start with a lowercase letter or digit"
            ));
        }
        if let Some(c) = name.chars().find(|&c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        }) {
            return Err(format!(
                "user name `{name}` contains invalid character `{c}` (allowed: a-z, 0-9, `.`, `_`, `-`)"
            ));
        }
        Ok(Self(name))
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Config {
    pub fn load(path: &Path) -> exn::Result<Self, ConfigError> {
        let text = fs::read_to_string(path).or_raise(|| ConfigError::Read)?;
        let mut config: Config = toml::from_str(&text).or_raise(|| ConfigError::Parse)?;

        if let Some(dir) = path.parent() {
            for file in [
                &mut config.server.host_key,
                &mut config.stalwart.admin_password_file,
                &mut config.vault.dir,
            ] {
                if file.is_relative() {
                    *file = dir.join(&*file);
                }
            }
        }
        Ok(config)
    }
}
