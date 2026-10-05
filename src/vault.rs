use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions, Permissions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use exn::{Exn, ResultExt, bail, ensure};
use rusqlite::{Connection, ErrorCode, params};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    config::{Username, VaultConfig},
    stalwart::Address,
    state::User,
};

const ATTEMPTS: u32 = 3;
const LOCKOUT: Duration = Duration::from_secs(5 * 60);
const FAIL_DELAY: Duration = Duration::from_secs(2);

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    address TEXT NOT NULL,
    service TEXT NOT NULL,
    username TEXT NOT NULL,
    password TEXT NOT NULL,
    url TEXT NOT NULL,
    notes TEXT NOT NULL
) STRICT";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct VaultError(String);

#[derive(Debug, Default)]
struct Failures {
    count: u32,
    locked_until: Option<Instant>,
}

#[derive(Debug)]
pub struct Vault {
    dir: PathBuf,
    failures: Mutex<BTreeMap<Username, Failures>>,
}

#[derive(Clone, Default, Zeroize, ZeroizeOnDrop)]
pub struct Account {
    pub id: Option<i64>,
    pub address: String,
    pub service: String,
    pub username: String,
    pub password: String,
    pub url: String,
    pub notes: String,
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    user: &'static User,
}

impl Vault {
    pub fn new(config: VaultConfig) -> exn::Result<Self, VaultError> {
        let err = || {
            VaultError(format!(
                "failed to prepare vault directory {}",
                config.dir.display()
            ))
        };
        fs::create_dir_all(&config.dir).or_raise(err)?;
        fs::set_permissions(&config.dir, Permissions::from_mode(0o700)).or_raise(err)?;
        Ok(Self {
            dir: config.dir,
            failures: Mutex::default(),
        })
    }

    pub fn exists(&self, user: &User) -> bool {
        self.path(user).exists()
    }

    pub async fn create(
        &self,
        user: &'static User,
        password: Zeroizing<String>,
    ) -> exn::Result<Db, VaultError> {
        let err = || VaultError(format!("failed to create vault of `{}`", user.name));
        let path = self.path(user);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .or_raise(err)?;
        let target = path.clone();
        let conn = blocking(err, move || open(&target, &password))
            .await
            .inspect_err(|_| {
                let _ = fs::remove_file(&path);
            })?;
        Ok(Db::new(conn, user))
    }

    pub async fn unlock(
        &self,
        user: &'static User,
        password: Zeroizing<String>,
    ) -> exn::Result<Db, VaultError> {
        if let Some(until) = self.failures().get(&user.name).and_then(|f| f.locked_until)
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            bail!(VaultError(format!(
                "vault locked after {ATTEMPTS} incorrect password attempts, try again in {}m {}s",
                left.as_secs() / 60,
                left.as_secs() % 60
            )));
        }

        let path = self.path(user);
        ensure!(
            path.exists(),
            VaultError(format!("vault of `{}` does not exist", user.name))
        );
        let opened = blocking(
            || VaultError(format!("failed to unlock vault of `{}`", user.name)),
            move || match open(&path, &password) {
                Err(e) if e.sqlite_error_code() == Some(ErrorCode::NotADatabase) => Ok(None),
                result => result.map(Some),
            },
        )
        .await?;

        if let Some(conn) = opened {
            self.failures().remove(&user.name);
            return Ok(Db::new(conn, user));
        }

        tokio::time::sleep(FAIL_DELAY).await;
        let mut failures = self.failures();
        let failure = failures.entry(user.name.clone()).or_default();
        failure.count += 1;
        ensure!(
            failure.count >= ATTEMPTS,
            VaultError(format!(
                "sorry, try again ({} of {ATTEMPTS} attempts left)",
                ATTEMPTS - failure.count
            ))
        );
        *failure = Failures {
            count: 0,
            locked_until: Some(Instant::now() + LOCKOUT),
        };
        bail!(VaultError(format!(
            "{ATTEMPTS} incorrect password attempts, vault locked for {} minutes",
            LOCKOUT.as_secs() / 60
        )))
    }

    fn path(&self, user: &User) -> PathBuf {
        self.dir.join(format!("{}.db", user.name))
    }

    fn failures(&self) -> MutexGuard<'_, BTreeMap<Username, Failures>> {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Db {
    fn new(conn: Connection, user: &'static User) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            user,
        }
    }

    pub async fn accounts(&self) -> exn::Result<Vec<Account>, VaultError> {
        let conn = self.conn.clone();
        blocking(
            || VaultError("failed to list accounts".into()),
            move || {
                let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
                let mut statement = conn.prepare(
                    "SELECT id, address, service, username, password, url, notes
                     FROM accounts ORDER BY service, username",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(Account {
                            id: row.get(0)?,
                            address: row.get(1)?,
                            service: row.get(2)?,
                            username: row.get(3)?,
                            password: row.get(4)?,
                            url: row.get(5)?,
                            notes: row.get(6)?,
                        })
                    })?
                    .collect()
            },
        )
        .await
    }

    pub async fn save(&self, account: Account) -> exn::Result<(), VaultError> {
        let err = || VaultError(format!("failed to save account `{}`", account.service));
        ensure!(
            !account.service.trim().is_empty(),
            VaultError("service may not be empty".into())
        );
        Address::new(self.user, &account.address).or_raise(err)?;

        let conn = self.conn.clone();
        let saved = account.clone();
        blocking(err, move || {
            let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            let values = params![
                saved.address,
                saved.service,
                saved.username,
                saved.password,
                saved.url,
                saved.notes,
                saved.id
            ];
            match saved.id {
                Some(_) => conn.execute(
                    "UPDATE accounts SET address = ?1, service = ?2, username = ?3,
                     password = ?4, url = ?5, notes = ?6 WHERE id = ?7",
                    values,
                ),
                None => conn.execute(
                    "INSERT INTO accounts (address, service, username, password, url, notes)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    &values[..6],
                ),
            }
        })
        .await?;
        Ok(())
    }

    pub async fn delete(&self, account: &Account) -> exn::Result<(), VaultError> {
        let conn = self.conn.clone();
        let id = account.id;
        blocking(
            || VaultError(format!("failed to delete account `{}`", account.service)),
            move || {
                let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
                conn.execute("DELETE FROM accounts WHERE id = ?1", [id])
            },
        )
        .await?;
        Ok(())
    }
}

fn open(path: &Path, password: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "key", password)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    conn.pragma_update(None, "cipher_memory_security", "ON")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

async fn blocking<T: Send + 'static>(
    err: impl FnOnce() -> VaultError,
    f: impl FnOnce() -> rusqlite::Result<T> + Send + 'static,
) -> exn::Result<T, VaultError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result.or_raise(err),
        Err(e) => Err(Exn::new(e).raise(err())),
    }
}
