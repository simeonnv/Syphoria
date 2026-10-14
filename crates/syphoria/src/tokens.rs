use std::{
    fs::OpenOptions,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use exn::ResultExt;
use log::info;
use rand::{RngExt, distr::Alphanumeric};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
    config::Username,
    error::StorageError,
    state::{AppState, User},
};

const SECRET_LEN: usize = 43;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tokens (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    hash BLOB NOT NULL UNIQUE,
    created TEXT NOT NULL,
    used TEXT
) STRICT";

#[derive(Debug)]
pub struct Tokens {
    dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub id: i64,
    pub name: String,
    pub created: DateTime<Utc>,
    pub used: Option<DateTime<Utc>>,
}

impl Tokens {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub async fn list(&self, user: &User) -> exn::Result<Vec<Token>, StorageError> {
        let path = self.path(user);
        blocking(move || {
            let conn = open(&path)?;
            let mut statement = conn
                .prepare("SELECT id, name, created, used FROM tokens ORDER BY created")
                .or_raise(|| StorageError::Query)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(Token {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        created: row.get(2)?,
                        used: row.get(3)?,
                    })
                })
                .or_raise(|| StorageError::Query)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .or_raise(|| StorageError::Query)
        })
        .await
    }

    pub async fn create(
        &self,
        user: &User,
        name: String,
    ) -> exn::Result<Zeroizing<String>, StorageError> {
        let path = self.path(user);
        let secret = Zeroizing::new(random(SECRET_LEN));
        let token = Zeroizing::new(format!("{}.{}", user.name, *secret));
        let label = name.trim().to_owned();
        blocking(move || {
            let conn = open(&path)?;
            conn.execute(
                "INSERT INTO tokens (name, hash, created) VALUES (?1, ?2, ?3)",
                params![name.trim(), hash(&secret), Utc::now()],
            )
            .or_raise(|| StorageError::Query)?;
            Ok(())
        })
        .await?;
        info!("api token `{label}` created for {}", user.name);
        Ok(token)
    }

    pub async fn revoke(&self, user: &User, id: i64) -> exn::Result<(), StorageError> {
        let path = self.path(user);
        blocking(move || {
            open(&path)?
                .execute("DELETE FROM tokens WHERE id = ?1", [id])
                .or_raise(|| StorageError::Query)?;
            Ok(())
        })
        .await?;
        info!("api token {id} revoked for {}", user.name);
        Ok(())
    }

    pub async fn verify(
        &self,
        state: AppState,
        token: &str,
    ) -> exn::Result<Option<(&'static User, i64)>, StorageError> {
        let Some((name, secret)) = token.trim().rsplit_once('.') else {
            return Ok(None);
        };
        let Some(user) = Username::try_from(name.to_owned())
            .ok()
            .and_then(|name| state.user(&name))
        else {
            return Ok(None);
        };
        let path = self.path(user);
        if !path.exists() {
            return Ok(None);
        }
        let digest = hash(secret);
        let id = blocking(move || {
            let conn = open(&path)?;
            let id: Option<i64> = conn
                .query_row("SELECT id FROM tokens WHERE hash = ?1", [digest], |row| {
                    row.get(0)
                })
                .optional()
                .or_raise(|| StorageError::Query)?;
            if let Some(id) = id {
                conn.execute(
                    "UPDATE tokens SET used = ?1 WHERE id = ?2",
                    params![Utc::now(), id],
                )
                .or_raise(|| StorageError::Query)?;
            }
            Ok(id)
        })
        .await?;
        Ok(id.map(|id| (user, id)))
    }

    fn path(&self, user: &User) -> PathBuf {
        self.dir.join(format!("{}.tokens", user.name))
    }
}

pub fn random(len: usize) -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn open(path: &Path) -> exn::Result<Connection, StorageError> {
    if !path.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .or_raise(|| StorageError::Open)?;
    }
    let conn = Connection::open(path).or_raise(|| StorageError::Open)?;
    conn.execute_batch(SCHEMA).or_raise(|| StorageError::Open)?;
    Ok(conn)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> exn::Result<T, StorageError> + Send + 'static,
) -> exn::Result<T, StorageError> {
    tokio::task::spawn_blocking(f)
        .await
        .or_raise(|| StorageError::Task)?
}
