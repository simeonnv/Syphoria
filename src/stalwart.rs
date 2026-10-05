use std::{collections::BTreeSet, fmt, fs, str::FromStr};

use email_address::EmailAddress;
use exn::{ResultExt, bail, ensure};
use fqdn::FQDN;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, de::DeserializeOwned, de::IgnoredAny};
use serde_json::{Value, json};
use serde_with::{OneOrMany, serde_as};
use url::Url;

use crate::{
    config::{StalwartConfig, Username},
    mail::{Mail, MailError},
    state::User,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StalwartError(String);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct AddressError(String);

#[derive(Debug, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "camelCase")]
enum ApiError {
    #[serde(skip)]
    #[error("invalid api path `{path}`")]
    Url {
        path: String,
        source: url::ParseError,
    },
    #[serde(skip)]
    #[error("failed to send request")]
    Send(#[source] reqwest::Error),
    #[serde(skip)]
    #[error("stalwart responded {status}, failed to read body")]
    Body {
        status: StatusCode,
        source: reqwest::Error,
    },
    #[serde(skip)]
    #[error("failed to decode response")]
    Decode(#[source] reqwest::Error),
    #[serde(skip)]
    #[error("stalwart responded {status}: {body}")]
    Status { status: StatusCode, body: String },
    #[error("{field} `{value}` already exists")]
    FieldAlreadyExists { field: String, value: String },
    #[error("missing field `{field}`")]
    FieldMissing { field: String },
    #[error("`{item}` not found")]
    NotFound { item: String },
    #[error("unsupported: {details}")]
    Unsupported { details: String },
    #[error("assertion failed")]
    AssertFailed,
    #[error("{details}")]
    Other {
        details: String,
        reason: Option<String>,
    },
}

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
    pub fn new(user: &User, address: &str) -> exn::Result<Self, AddressError> {
        let email = EmailAddress::from_str(address)
            .or_raise(|| AddressError(format!("`{address}` is not a valid address")))?;
        ensure!(
            user.owns(address),
            AddressError(format!("`{}` is not one of your domains", email.domain()))
        );
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
    pub fn new(config: StalwartConfig) -> exn::Result<Self, StalwartError> {
        let path = &config.admin_password_file;
        let password = fs::read_to_string(path).or_raise(|| {
            StalwartError(format!("failed to read admin password {}", path.display()))
        })?;

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

    pub async fn mail(&self, user: &User) -> exn::Result<Mail, MailError> {
        let login = format!("{}%{}", user.name, self.admin_user);
        Mail::connect(&self.url, &login, &self.admin_password).await
    }

    pub async fn provision<'a>(
        &self,
        domains: &BTreeSet<FQDN>,
        users: impl IntoIterator<Item = &'a User>,
    ) -> exn::Result<(), StalwartError> {
        for domain in domains {
            self.create(json!({ "type": "domain", "name": domain.to_string() }))
                .await
                .or_raise(|| StalwartError(format!("failed to create domain `{domain}`")))?;
        }

        for user in users {
            self.create(
                json!({ "type": "individual", "name": user.name.to_string(), "roles": ["user"] }),
            )
            .await
            .or_raise(|| StalwartError(format!("failed to create account `{}`", user.name)))?;

            for address in self.addresses(user).await? {
                if !user.owns(&address) {
                    println!(
                        "removing {address} from {}: domain no longer assigned",
                        user.name
                    );
                    self.edit_emails(&user.name, "removeItem", &address)
                        .await
                        .or_raise(|| {
                            StalwartError(format!(
                                "failed to remove stale address `{address}` from `{}`",
                                user.name
                            ))
                        })?;
                }
            }
        }
        Ok(())
    }

    pub async fn addresses(&self, user: &User) -> exn::Result<Vec<String>, StalwartError> {
        let principal: Principal = self
            .api(Method::GET, &format!("api/principal/{}", user.name), None)
            .await
            .or_raise(|| StalwartError(format!("failed to list addresses of `{}`", user.name)))?;
        Ok(principal.emails)
    }

    pub async fn add_address(&self, address: &Address) -> exn::Result<(), StalwartError> {
        self.edit_emails(&address.owner, "addItem", address.email.as_str())
            .await
            .or_raise(|| StalwartError(format!("failed to add `{address}` to `{}`", address.owner)))
    }

    async fn create(&self, principal: Value) -> exn::Result<(), ApiError> {
        match self
            .api::<IgnoredAny>(Method::POST, "api/principal", Some(principal))
            .await
        {
            Err(e) if matches!(*e, ApiError::FieldAlreadyExists { .. }) => Ok(()),
            result => result.map(drop),
        }
    }

    async fn edit_emails(
        &self,
        owner: &Username,
        action: &str,
        address: &str,
    ) -> exn::Result<(), ApiError> {
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
    ) -> exn::Result<T, ApiError> {
        let url = self.url.join(path).map_err(|source| ApiError::Url {
            path: path.to_owned(),
            source,
        })?;
        let mut request = self
            .http
            .request(method, url)
            .basic_auth(&self.admin_user, Some(&self.admin_password));
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.map_err(ApiError::Send)?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|source| ApiError::Body { status, source })?;
            bail!(ApiError::Status { status, body });
        }

        match response.json().await.map_err(ApiError::Decode)? {
            Reply::Data { data } => Ok(data),
            Reply::Error(e) => bail!(e),
        }
    }
}
