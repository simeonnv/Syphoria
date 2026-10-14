use exn::{OptionExt, ResultExt, bail};
use reqwest::{RequestBuilder, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use syphoria_api::{
    ACCOUNTS, ADDRESSES, Addresses, Capture, Failure, KNOWN, LOCK, Login, SAVE, SIGNUP, Signup,
    Summary, UNLOCK, Unlock, Unlocked,
};
use url::Url;

use crate::{
    error::ExtError,
    ext::{self, Message, SERVER, TOKEN},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Client {
    server: Url,
    token: String,
    pub session: Option<String>,
}

impl Client {
    pub async fn load() -> exn::Result<Self, ExtError> {
        let server = ext::load(SERVER).await?;
        let token = ext::load(TOKEN).await?;
        let (Some(server), Some(token)) = (server, token) else {
            bail!(ExtError::Setup);
        };
        Ok(Self {
            server: Url::parse(&server).or_raise(|| ExtError::Url)?,
            token,
            session: ext::send(&Message::Session).await?,
        })
    }

    pub async fn setup(server: &str, token: &str) -> exn::Result<(), ExtError> {
        let mut url = Url::parse(server.trim()).or_raise(|| ExtError::Url)?;
        if url.scheme() != "https" || !url.has_host() {
            bail!(ExtError::Url);
        }
        url.set_path("/");
        ext::store(SERVER, url.as_str()).await?;
        ext::store(TOKEN, token.trim()).await?;
        ext::notify(&Message::ClearSession).await
    }

    pub async fn forget() -> exn::Result<(), ExtError> {
        ext::remove(&[SERVER, TOKEN]).await?;
        ext::notify(&Message::ClearSession).await
    }

    pub fn server(&self) -> &str {
        self.server.host_str().unwrap_or_default()
    }

    pub async fn unlock(&self, password: String) -> exn::Result<(), ExtError> {
        let body = Unlock {
            token: self.token.clone(),
            password,
        };
        let unlocked: Unlocked =
            send(self.request(reqwest::Method::POST, UNLOCK)?.json(&body)).await?;
        ext::notify(&Message::SetSession {
            session: unlocked.session,
        })
        .await
    }

    pub async fn lock(&self) -> exn::Result<(), ExtError> {
        let result = self.post::<_, ()>(LOCK, &()).await;
        ext::notify(&Message::ClearSession).await?;
        match result {
            Err(e) if matches!(*e, ExtError::Locked) => Ok(()),
            other => other,
        }
    }

    pub async fn accounts(&self, url: Option<&str>) -> exn::Result<Vec<Summary>, ExtError> {
        let mut path = self.server.join(ACCOUNTS).or_raise(|| ExtError::Url)?;
        if let Some(url) = url {
            path.query_pairs_mut().append_pair("url", url);
        }
        self.authed(reqwest::Client::new().get(path)).await
    }

    pub async fn login(&self, id: i64) -> exn::Result<Login, ExtError> {
        self.get(&format!("{ACCOUNTS}/{id}")).await
    }

    pub async fn addresses(&self) -> exn::Result<Addresses, ExtError> {
        self.get(ADDRESSES).await
    }

    pub async fn signup(&self, signup: &Signup) -> exn::Result<Login, ExtError> {
        self.post(SIGNUP, signup).await
    }

    pub async fn known(&self, capture: &Capture) -> exn::Result<bool, ExtError> {
        self.post(KNOWN, capture).await
    }

    pub async fn save(&self, capture: &Capture) -> exn::Result<(), ExtError> {
        self.post(SAVE, capture).await
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> exn::Result<RequestBuilder, ExtError> {
        let url = self.server.join(path).or_raise(|| ExtError::Url)?;
        Ok(reqwest::Client::new().request(method, url))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> exn::Result<T, ExtError> {
        self.authed(self.request(reqwest::Method::GET, path)?).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> exn::Result<T, ExtError> {
        self.authed(self.request(reqwest::Method::POST, path)?.json(body))
            .await
    }

    async fn authed<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> exn::Result<T, ExtError> {
        let session = self.session.as_deref().ok_or_raise(|| ExtError::Locked)?;
        let result = send(request.bearer_auth(session)).await;
        if let Err(e) = &result
            && matches!(**e, ExtError::Locked)
        {
            ext::notify(&Message::ClearSession).await?;
        }
        result
    }
}

async fn send<T: DeserializeOwned>(request: RequestBuilder) -> exn::Result<T, ExtError> {
    let response = request.send().await.or_raise(|| ExtError::Network)?;
    let status = response.status();
    if status.is_success() {
        return response.json().await.or_raise(|| ExtError::Decode);
    }
    if status == StatusCode::UNAUTHORIZED {
        bail!(ExtError::Locked);
    }
    match response.json::<Failure>().await {
        Ok(failure) => bail!(ExtError::Server(failure.error)),
        Err(e) => Err(e).or_raise(|| ExtError::Server(status.to_string())),
    }
}
