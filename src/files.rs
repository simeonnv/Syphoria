use std::{
    fs::Permissions,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use exn::{ResultExt, bail, ensure};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

use crate::{
    config::FilesConfig,
    error::{InputError, StorageError},
    state::User,
};

const MB: u64 = 1_000_000;
const MAX_NAME: usize = 255;
const MAX_COPIES: usize = 1000;

#[derive(Debug)]
pub struct Files {
    dir: PathBuf,
    pub quota: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub name: String,
    pub size: u64,
}

impl Files {
    pub fn new(config: FilesConfig) -> exn::Result<Self, StorageError> {
        std::fs::create_dir_all(&config.dir).or_raise(|| StorageError::FilesSetup)?;
        std::fs::set_permissions(&config.dir, Permissions::from_mode(0o700))
            .or_raise(|| StorageError::FilesSetup)?;
        Ok(Self {
            dir: config.dir,
            quota: config.quota_mb * MB,
        })
    }

    pub fn quota_mb(&self) -> u64 {
        self.quota / MB
    }

    pub async fn dir(&self, user: &User) -> exn::Result<PathBuf, StorageError> {
        let dir = self.dir.join(user.name.to_string());
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .await
            .or_raise(|| StorageError::FilesSetup)?;
        Ok(dir)
    }

    pub async fn list(&self, user: &User) -> exn::Result<Vec<StoredFile>, StorageError> {
        list(&self.dir(user).await?).await
    }

    pub async fn usage(&self, user: &User) -> exn::Result<u64, StorageError> {
        Ok(self.list(user).await?.iter().map(|f| f.size).sum())
    }

    pub async fn read(&self, user: &User, name: &str) -> exn::Result<Vec<u8>, StorageError> {
        check_name(name).or_raise(|| StorageError::ReadFile)?;
        fs::read(self.dir(user).await?.join(name))
            .await
            .or_raise(|| StorageError::ReadFile)
    }

    pub async fn save(
        &self,
        user: &User,
        name: &str,
        bytes: &[u8],
    ) -> exn::Result<String, StorageError> {
        let used = self.usage(user).await?;
        ensure!(
            used + bytes.len() as u64 <= self.quota,
            StorageError::Quota(self.quota_mb())
        );
        let dir = self.dir(user).await?;
        let name = sanitize(name);
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
            _ => (name.as_str(), String::new()),
        };
        for n in 0..MAX_COPIES {
            let candidate = match n {
                0 => name.clone(),
                n => format!("{stem} ({n}){ext}"),
            };
            if candidate.len() > MAX_NAME {
                break;
            }
            let opened = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(&candidate))
                .await;
            let mut file = match opened {
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                opened => opened.or_raise(|| StorageError::WriteFile)?,
            };
            file.write_all(bytes)
                .await
                .or_raise(|| StorageError::WriteFile)?;
            file.flush().await.or_raise(|| StorageError::WriteFile)?;
            return Ok(candidate);
        }
        bail!(StorageError::WriteFile)
    }

    pub async fn remove(&self, user: &User, names: &[String]) -> exn::Result<(), StorageError> {
        let dir = self.dir(user).await?;
        for name in names {
            check_name(name).or_raise(|| StorageError::RemoveFile)?;
            fs::remove_file(dir.join(name))
                .await
                .or_raise(|| StorageError::RemoveFile)?;
        }
        Ok(())
    }
}

pub async fn list(dir: &Path) -> exn::Result<Vec<StoredFile>, StorageError> {
    let mut entries = fs::read_dir(dir)
        .await
        .or_raise(|| StorageError::ListFiles)?;
    let mut files = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .or_raise(|| StorageError::ListFiles)?
    {
        let meta = entry
            .metadata()
            .await
            .or_raise(|| StorageError::ListFiles)?;
        if let (true, Some(name)) = (meta.is_file(), entry.file_name().to_str()) {
            files.push(StoredFile {
                name: name.to_owned(),
                size: meta.len(),
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

pub fn check_name(name: &str) -> exn::Result<(), InputError> {
    ensure!(
        !name.is_empty()
            && name.len() <= MAX_NAME
            && !name.starts_with('.')
            && !name.contains(['/', '\0']),
        InputError::InvalidFileName(name.to_owned())
    );
    Ok(())
}

fn sanitize(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\0') { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_start_matches('.');
    let mut end = clean.len().min(MAX_NAME / 2);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    match &clean[..end] {
        "" => "attachment".to_owned(),
        clean => clean.to_owned(),
    }
}
