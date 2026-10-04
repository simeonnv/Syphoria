use std::{collections::BTreeSet, fs, str::FromStr};

use email_address::EmailAddress;
use exn::{Exn, Result, ResultExt, bail, ensure};
use fqdn::FQDN;
use rand::{RngExt, distr::Alphanumeric};
use reqwest::Method;
use serde::{Deserialize, de::DeserializeOwned, de::IgnoredAny};
use serde_json::{Value, json};
use serde_with::{OneOrMany, serde_as};
use url::Url;

use crate::{
    config::StalwartConfig,
    mail::{Mail, MailError},
    state::User,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StalwartError(String);

#[derive(Debug, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "camelCase")]
pub enum ApiError {
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

impl<T> Reply<T> {
    fn data(self) -> Result<T, ApiError> {
        match self {
            Reply::Data { data } => Ok(data),
            Reply::Error(e) => bail!(e),
        }
    }
}

#[serde_as]
#[derive(Deserialize)]
struct Principal {
    #[serde_as(as = "OneOrMany<_>")]
    #[serde(default)]
    emails: Vec<String>,
}

#[derive(Debug)]
pub struct Stalwart {
    http: reqwest::Client,
    url: Url,
    admin_user: String,
    admin_password: String,
}

impl Stalwart {
    pub fn new(config: StalwartConfig) -> Result<Self, StalwartError> {
        let path = &config.admin_password_file;
        let password = fs::read_to_string(path).or_raise(|| {
            StalwartError(format!("failed to read admin password {}", path.display()))
        })?;
        Ok(Self {
            http: reqwest::Client::new(),
            url: config.url,
            admin_user: config.admin_user,
            admin_password: password.trim().to_owned(),
        })
    }

    pub async fn mail(&self, user: &User) -> Result<Mail, MailError> {
        let login = format!("{}%{}", user.name, self.admin_user);
        Mail::connect(&self.url, &login, &self.admin_password).await
    }

    pub async fn provision<'a>(
        &self,
        domains: &BTreeSet<FQDN>,
        users: impl IntoIterator<Item = &'a User>,
    ) -> Result<(), StalwartError> {
        for domain in domains {
            self.create(json!({ "type": "domain", "name": domain.to_string() }))
                .await
                .or_raise(|| StalwartError(format!("failed to create domain `{domain}`")))?;
        }
        for user in users {
            let err = || StalwartError(format!("failed to provision user `{}`", user.name));
            self.create(
                json!({ "type": "individual", "name": user.name.to_string(), "roles": ["user"] }),
            )
            .await
            .or_raise(err)?;
            for address in self.addresses(user).await.or_raise(err)? {
                if !user.owns(&address) {
                    println!(
                        "removing {address} from {}: domain no longer assigned",
                        user.name
                    );
                    self.update(user, "removeItem", &address)
                        .await
                        .or_raise(err)?;
                }
            }
        }
        Ok(())
    }

    pub async fn addresses(&self, user: &User) -> Result<Vec<String>, StalwartError> {
        let principal: Principal = self
            .call(Method::GET, &principal_path(user), None)
            .await
            .or_raise(|| StalwartError(format!("failed to list addresses of `{}`", user.name)))?;
        Ok(principal.emails)
    }

    pub async fn add_address(
        &self,
        user: &User,
        local: &str,
        domain: &FQDN,
    ) -> Result<Vec<String>, StalwartError> {
        let address = format!("{local}@{domain}");
        let err = || StalwartError(format!("failed to add `{address}`"));
        EmailAddress::from_str(&address).or_raise(err)?;
        ensure!(
            user.owns(&address),
            StalwartError(format!("`{address}` is not on one of your domains"))
        );
        self.update(user, "addItem", &address).await.or_raise(err)?;
        self.addresses(user).await
    }

    pub async fn remove_address(
        &self,
        user: &User,
        address: &str,
    ) -> Result<Vec<String>, StalwartError> {
        ensure!(
            user.owns(address),
            StalwartError(format!("`{address}` is not on one of your domains"))
        );
        self.update(user, "removeItem", address)
            .await
            .or_raise(|| StalwartError(format!("failed to remove `{address}`")))?;
        self.addresses(user).await
    }

    pub fn random_local_part() -> String {
        rand::rng()
            .sample_iter(Alphanumeric)
            .take(12)
            .map(|b| char::from(b).to_ascii_lowercase())
            .collect()
    }

    async fn create(&self, principal: Value) -> Result<(), StalwartError> {
        let path = "api/principal";
        match self
            .request::<IgnoredAny>(Method::POST, path, Some(principal))
            .await?
        {
            Reply::Data { .. } | Reply::Error(ApiError::FieldAlreadyExists { .. }) => Ok(()),
            Reply::Error(e) => Err(Exn::new(e).raise(rejected(&Method::POST, path))),
        }
    }

    async fn update(&self, user: &User, action: &str, address: &str) -> Result<(), StalwartError> {
        let body = json!([{ "action": action, "field": "emails", "value": address }]);
        self.call::<IgnoredAny>(Method::PATCH, &principal_path(user), Some(body))
            .await?;
        Ok(())
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, StalwartError> {
        self.request(method.clone(), path, body)
            .await?
            .data()
            .or_raise(|| rejected(&method, path))
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Reply<T>, StalwartError> {
        let err = || StalwartError(format!("request {method} /{path} failed"));
        let mut request = self
            .http
            .request(method.clone(), self.url.join(path).or_raise(err)?)
            .basic_auth(&self.admin_user, Some(&self.admin_password));
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .or_raise(err)?
            .json()
            .await
            .or_raise(err)
    }
}

fn rejected(method: &Method, path: &str) -> StalwartError {
    StalwartError(format!("stalwart rejected {method} /{path}"))
}

fn principal_path(user: &User) -> String {
    format!("api/principal/{}", user.name)
}
