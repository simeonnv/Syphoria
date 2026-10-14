use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use url::Url;

pub const UNLOCK: &str = "/api/unlock";
pub const LOCK: &str = "/api/lock";
pub const ACCOUNTS: &str = "/api/accounts";
pub const ADDRESSES: &str = "/api/addresses";
pub const SIGNUP: &str = "/api/signup";
pub const KNOWN: &str = "/api/known";
pub const SAVE: &str = "/api/save";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unlock {
    pub token: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unlocked {
    pub session: String,
    pub user: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: i64,
    pub service: String,
    pub username: String,
    pub address: String,
    pub url: String,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Login {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Addresses {
    pub addresses: Vec<String>,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Choice {
    New { local: String, domain: String },
    Existing { address: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signup {
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub username: String,
    pub address: Choice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub error: String,
}

pub fn site(url: &str) -> Option<String> {
    let parsed = Url::parse(url)
        .ok()
        .filter(|u| u.has_host())
        .or_else(|| Url::parse(&format!("https://{url}")).ok())?;
    let host = parsed.host_str()?.trim_end_matches('.').to_lowercase();
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return Some(host);
    }
    Some(psl::domain_str(&host).map_or(host.clone(), str::to_owned))
}
