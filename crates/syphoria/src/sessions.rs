use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use zeroize::Zeroizing;

use crate::{state::User, tokens::random, vault::Db};

const IDLE: Duration = Duration::from_secs(24 * 60 * 60);
const ID_LEN: usize = 43;

#[derive(Clone)]
pub struct Session {
    pub user: &'static User,
    pub token: i64,
    pub db: Db,
    used: Instant,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sessions")
    }
}

impl Sessions {
    pub fn open(&self, user: &'static User, token: i64, db: Db) -> Zeroizing<String> {
        let id = Zeroizing::new(random(ID_LEN));
        let session = Session {
            user,
            token,
            db,
            used: Instant::now(),
        };
        self.lock().insert(id.to_string(), session);
        id
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        let mut sessions = self.lock();
        let now = Instant::now();
        sessions.retain(|_, s| now.duration_since(s.used) < IDLE);
        let session = sessions.get_mut(id)?;
        session.used = now;
        Some(session.clone())
    }

    pub fn close(&self, id: &str) {
        self.lock().remove(id);
    }

    pub fn revoke(&self, user: &User, token: i64) {
        self.lock()
            .retain(|_, s| !(s.user.name == user.name && s.token == token));
    }

    pub fn close_user(&self, user: &User) {
        self.lock().retain(|_, s| s.user.name != user.name);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
