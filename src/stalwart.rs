use std::{collections::BTreeSet, fmt, fs, str::FromStr};

use email_address::EmailAddress;
use exn::{ResultExt, ensure};
use fqdn::FQDN;
use reqwest::Method;
use serde::{Deserialize, de::DeserializeOwned, de::IgnoredAny};
use serde_json::{Value, json};
use serde_with::{OneOrMany, serde_as};
use url::Url;

use crate::{
    config::{StalwartConfig, Username},
    error::{ApiError, ConfigError, InputError, RemoteError},
    mail::Mail,
    state::User,
};

#[derive(Deserialize)]
#[serde(untagged)]
enum Reply<T> {
    Data { data: T },
    Error(ApiError),
}

#[serde_as]
#[derive(Deserialize)]
struct Principal {
    #[serde_as(as = "OneOrMany<_>")]
    #[serde(default)]
    emails: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Address {
    owner: Username,
    email: EmailAddress,
}

impl Address {
    pub fn new(user: &User, address: &str) -> exn::Result<Self, InputError> {
        let email = EmailAddress::from_str(address).or_raise(|| InputError::InvalidAddress)?;
        ensure!(user.owns(address), InputError::ForeignDomain);
        Ok(Self {
            owner: user.name.clone(),
            email,
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.email.fmt(f)
    }
}

#[derive(Debug)]
pub struct Stalwart {
    http: reqwest::Client,
    url: Url,
    admin_user: String,
    admin_password: String,
}

impl Stalwart {
    pub fn new(config: StalwartConfig) -> exn::Result<Self, ConfigError> {
        let path = &config.admin_password_file;
        let password = fs::read_to_string(path).or_raise(|| ConfigError::AdminPassword)?;

        let mut url = config.url;
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }

        Ok(Self {
            http: reqwest::Client::new(),
            url,
            admin_user: config.admin_user,
            admin_password: password.trim().to_owned(),
        })
    }

    pub async fn mail(&self, user: &User) -> exn::Result<Mail, RemoteError> {
        let login = format!("{}%{}", user.name, self.admin_user);
        Mail::connect(&self.url, &login, &self.admin_password).await
    }

    pub async fn provision<'a>(
        &self,
        domains: &BTreeSet<FQDN>,
        users: impl IntoIterator<Item = &'a User>,
    ) -> exn::Result<(), RemoteError> {
        for domain in domains {
            self.create(json!({ "type": "domain", "name": domain.to_string() }))
                .await?;
        }

        for user in users {
            self.create(
                json!({ "type": "individual", "name": user.name.to_string(), "roles": ["user"] }),
            )
            .await?;

            for address in self.addresses(user).await? {
                if !user.owns(&address) {
                    println!(
                        "removing {address} from {}: domain no longer assigned",
                        user.name
                    );
                    self.edit_emails(&user.name, "removeItem", &address).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn addresses(&self, user: &User) -> exn::Result<Vec<String>, RemoteError> {
        let principal: Principal = self
            .api(Method::GET, &format!("api/principal/{}", user.name), None)
            .await?;
        Ok(principal.emails)
    }

    pub async fn add_address(&self, address: &Address) -> exn::Result<(), RemoteError> {
        self.edit_emails(&address.owner, "addItem", address.email.as_str())
            .await
    }

    async fn create(&self, principal: Value) -> exn::Result<(), RemoteError> {
        match self
            .api::<IgnoredAny>(Method::POST, "api/principal", Some(principal))
            .await
        {
            Err(e) if matches!(*e, RemoteError::AlreadyExists) => Ok(()),
            result => result.map(drop),
        }
    }

    async fn edit_emails(
        &self,
        owner: &Username,
        action: &str,
        address: &str,
    ) -> exn::Result<(), RemoteError> {
        let body = json!([{ "action": action, "field": "emails", "value": address }]);
        self.api::<IgnoredAny>(Method::PATCH, &format!("api/principal/{owner}"), Some(body))
            .await
            .map(drop)
    }

    async fn api<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> exn::Result<T, RemoteError> {
        let url = self.url.join(path).or_raise(|| RemoteError::Internal)?;
        let mut request = self
            .http
            .request(method, url)
            .basic_auth(&self.admin_user, Some(&self.admin_password));
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.or_raise(|| RemoteError::Unavailable)?;
        let status = response.status();
        let reply = response
            .error_for_status()
            .or_raise(|| RemoteError::http(status))?
            .json()
            .await
            .or_raise(|| RemoteError::Internal)?;
        match reply {
            Reply::Data { data } => Ok(data),
            Reply::Error(e) => {
                let kind = e.kind();
                Err(e).or_raise(|| kind)
            }
        }
    }
}
