use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::Request,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use exn::{ErrorExt, Exn, Frame, OptionExt, ResultExt, bail, ensure};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use log::{debug, error, info, warn};
use rustls::{
    ServerConfig,
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use syphoria_api::{
    ACCOUNTS, ADDRESSES, Addresses, Capture, Choice, Filter, KNOWN, LOCK, Login, SAVE, SIGNUP,
    Signup, Summary, UNLOCK, Unlock, Unlocked, site,
};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

use crate::{
    config::ApiConfig,
    error::{ConfigError, InputError, RemoteError, StorageError, WebError, chain},
    sessions::Session,
    stalwart::Address,
    state::AppState,
    vault::{Account, generate_password},
};

const FAIL_DELAY: Duration = Duration::from_secs(2);
const HANDSHAKE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Api {
    tls: TlsAcceptor,
    router: Router,
}

impl Api {
    pub fn new(state: AppState, config: &ApiConfig) -> exn::Result<Self, ConfigError> {
        let certs = CertificateDer::pem_file_iter(&config.cert)
            .or_raise(|| ConfigError::Tls)?
            .collect::<Result<Vec<_>, _>>()
            .or_raise(|| ConfigError::Tls)?;
        let key = PrivateKeyDer::from_pem_file(&config.key).or_raise(|| ConfigError::Tls)?;
        let mut tls = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .or_raise(|| ConfigError::Tls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .or_raise(|| ConfigError::Tls)?;
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let router = Router::new()
            .route(UNLOCK, post(unlock))
            .route(LOCK, post(lock))
            .route(ACCOUNTS, get(accounts))
            .route(&format!("{ACCOUNTS}/{{id}}"), get(account))
            .route(ADDRESSES, get(addresses))
            .route(SIGNUP, post(signup))
            .route(KNOWN, post(known))
            .route(SAVE, post(save))
            .layer(middleware::from_fn(access))
            .with_state(state);

        Ok(Self {
            tls: TlsAcceptor::from(Arc::new(tls)),
            router,
        })
    }

    pub async fn serve(self, stream: TcpStream, peer: SocketAddr) {
        if let Err(e) = self.connection(stream).await {
            debug!("https {peer}: {e:?}");
        }
    }

    async fn connection(self, stream: TcpStream) -> exn::Result<(), WebError> {
        let tls = tokio::time::timeout(HANDSHAKE, self.tls.accept(stream))
            .await
            .or_raise(|| WebError::Handshake)?
            .or_raise(|| WebError::Handshake)?;
        Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), TowerToHyperService::new(self.router))
            .await
            .map_err(std::io::Error::other)
            .or_raise(|| WebError::Connection)
    }
}

pub struct Failure(Exn<WebError>);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status = status(self.0.frame()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let error = if status.is_server_error() {
            error!("api error: {:?}", self.0);
            self.0.to_string()
        } else {
            chain(self.0.frame())
        };
        (status, Json(syphoria_api::Failure { error })).into_response()
    }
}

fn status(frame: &Frame) -> Option<StatusCode> {
    let error = frame.error();
    let own = if let Some(e) = error.downcast_ref::<WebError>() {
        match e {
            WebError::Session => Some(StatusCode::UNAUTHORIZED),
            WebError::Token => Some(StatusCode::FORBIDDEN),
            WebError::NotFound => Some(StatusCode::NOT_FOUND),
            WebError::UnknownAddress(_) => Some(StatusCode::BAD_REQUEST),
            _ => None,
        }
    } else if let Some(e) = error.downcast_ref::<StorageError>() {
        match e {
            StorageError::WrongPassword => Some(StatusCode::FORBIDDEN),
            StorageError::Locked => Some(StatusCode::TOO_MANY_REQUESTS),
            StorageError::NotFound => Some(StatusCode::NOT_FOUND),
            StorageError::EmptyService | StorageError::InvalidAddress => {
                Some(StatusCode::BAD_REQUEST)
            }
            _ => None,
        }
    } else if error.downcast_ref::<InputError>().is_some() {
        Some(StatusCode::BAD_REQUEST)
    } else if let Some(e) = error.downcast_ref::<RemoteError>() {
        match e {
            RemoteError::AlreadyExists => Some(StatusCode::CONFLICT),
            RemoteError::Unavailable => Some(StatusCode::SERVICE_UNAVAILABLE),
            _ => None,
        }
    } else {
        None
    };
    own.or_else(|| frame.children().iter().find_map(status))
}

type Reply<T> = Result<Json<T>, Failure>;

fn reply<T>(result: exn::Result<T, WebError>) -> Reply<T> {
    result.map(Json).map_err(Failure)
}

struct Authed {
    id: String,
    session: Session,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Failure> {
        parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|id| {
                state.sessions.get(id).map(|session| Authed {
                    id: id.to_owned(),
                    session,
                })
            })
            .ok_or_else(|| Failure(WebError::Session.raise()))
    }
}

async fn unlock(State(state): State<AppState>, Json(body): Json<Unlock>) -> Reply<Unlocked> {
    reply(open(state, body).await)
}

async fn open(state: AppState, body: Unlock) -> exn::Result<Unlocked, WebError> {
    let Unlock { token, password } = body;
    let password = Zeroizing::new(password);
    let verified = state
        .tokens
        .verify(state, &token)
        .await
        .or_raise(|| WebError::Unlock)?;
    let Some((user, token)) = verified else {
        warn!("api unlock with invalid token");
        tokio::time::sleep(FAIL_DELAY).await;
        bail!(WebError::Token);
    };
    let db = state
        .vault
        .unlock(user, password)
        .await
        .or_raise(|| WebError::Unlock)?;
    let session = state.sessions.open(user, token, db);
    info!("api unlocked: {}", user.name);
    Ok(Unlocked {
        session: session.to_string(),
        user: user.name.to_string(),
    })
}

async fn access(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = Instant::now();
    let response = next.run(request).await;
    debug!(
        "{method} {path} {} {:?}",
        response.status().as_u16(),
        start.elapsed()
    );
    response
}

async fn lock(State(state): State<AppState>, authed: Authed) -> Reply<()> {
    state.sessions.close(&authed.id);
    info!("api locked: {}", authed.session.user.name);
    Ok(Json(()))
}

async fn accounts(authed: Authed, Query(filter): Query<Filter>) -> Reply<Vec<Summary>> {
    reply(list(&authed.session, filter.url.as_deref()).await)
}

async fn list(session: &Session, url: Option<&str>) -> exn::Result<Vec<Summary>, WebError> {
    let wanted = url.map(site);
    let accounts = session
        .db
        .accounts()
        .await
        .or_raise(|| WebError::LoadAccounts)?;
    Ok(accounts
        .iter()
        .filter(|a| {
            wanted
                .as_ref()
                .is_none_or(|w| w.is_some() && site(&a.url) == *w)
        })
        .map(|a| Summary {
            id: a.id.unwrap_or_default(),
            service: a.service.clone(),
            username: a.username.clone(),
            address: a.address.clone(),
            url: a.url.clone(),
            notes: a.notes.clone(),
        })
        .collect())
}

async fn account(authed: Authed, Path(id): Path<i64>) -> Reply<Login> {
    reply(find(&authed.session, id).await)
}

async fn find(session: &Session, id: i64) -> exn::Result<Login, WebError> {
    let accounts = session
        .db
        .accounts()
        .await
        .or_raise(|| WebError::LoadAccounts)?;
    let account = accounts
        .iter()
        .find(|a| a.id == Some(id))
        .ok_or_raise(|| WebError::NotFound)?;
    Ok(Login {
        username: account.username.clone(),
        email: account.address.clone(),
        password: account.password.clone(),
    })
}

async fn addresses(State(state): State<AppState>, authed: Authed) -> Reply<Addresses> {
    reply(owned(state, &authed.session).await)
}

async fn owned(state: AppState, session: &Session) -> exn::Result<Addresses, WebError> {
    let addresses = state
        .stalwart
        .addresses(session.user)
        .await
        .or_raise(|| WebError::LoadAddresses)?;
    Ok(Addresses {
        addresses,
        domains: session
            .user
            .domains()
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    })
}

async fn signup(
    State(state): State<AppState>,
    authed: Authed,
    Json(body): Json<Signup>,
) -> Reply<Login> {
    reply(register(state, &authed.session, body).await)
}

async fn register(
    state: AppState,
    session: &Session,
    body: Signup,
) -> exn::Result<Login, WebError> {
    let user = session.user;
    let address = match body.address {
        Choice::Existing { address } => {
            let owned = owned(state, session).await?;
            ensure!(
                owned
                    .addresses
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&address)),
                WebError::UnknownAddress(address)
            );
            address
        }
        Choice::New { local, domain } => {
            let address = Address::new(user, &format!("{}@{domain}", local.trim()))
                .or_raise(|| WebError::AddAddress)?;
            state
                .stalwart
                .add_address(&address)
                .await
                .or_raise(|| WebError::AddAddress)?;
            info!("api created {address} for {}", user.name);
            address.to_string()
        }
    };
    let password = generate_password();
    let username = match body.username.trim() {
        "" => address.clone(),
        name => name.to_owned(),
    };
    session
        .db
        .save(Account {
            id: None,
            address: address.clone(),
            service: site(&body.url).unwrap_or_else(|| body.url.clone()),
            username: username.clone(),
            password: password.to_string(),
            url: body.url,
            notes: body.description.trim().to_owned(),
        })
        .await
        .or_raise(|| WebError::SaveAccount)?;
    Ok(Login {
        username,
        email: address,
        password: password.to_string(),
    })
}

async fn known(authed: Authed, Json(body): Json<Capture>) -> Reply<bool> {
    reply(
        matching(&authed.session, &body)
            .await
            .map(|found| found.is_some_and(|a| a.password == body.password)),
    )
}

async fn matching(session: &Session, capture: &Capture) -> exn::Result<Option<Account>, WebError> {
    let wanted = site(&capture.url).ok_or_raise(|| WebError::NotFound)?;
    let accounts = session
        .db
        .accounts()
        .await
        .or_raise(|| WebError::LoadAccounts)?;
    Ok(accounts.into_iter().find(|a| {
        a.username.eq_ignore_ascii_case(&capture.username)
            && site(&a.url).as_deref() == Some(wanted.as_str())
    }))
}

async fn save(authed: Authed, Json(body): Json<Capture>) -> Reply<()> {
    reply(store(&authed.session, body).await)
}

async fn store(session: &Session, capture: Capture) -> exn::Result<(), WebError> {
    let account = match matching(session, &capture).await? {
        Some(mut account) => {
            account.password = capture.password;
            account
        }
        None => Account {
            id: None,
            address: if session.user.owns(&capture.username) {
                capture.username.to_lowercase()
            } else {
                String::new()
            },
            service: site(&capture.url).unwrap_or_else(|| capture.url.clone()),
            username: capture.username,
            password: capture.password,
            url: capture.url,
            notes: String::new(),
        },
    };
    session
        .db
        .save(account)
        .await
        .or_raise(|| WebError::SaveAccount)
}
