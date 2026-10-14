use std::{
    collections::HashMap,
    io::{self, ErrorKind, SeekFrom},
    path::PathBuf,
};

use exn::Exn;
use russh_sftp::{
    protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode},
    server::{Handler, StatusReply},
};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

use crate::{
    error::{StorageError, chain},
    files::{self, check_name},
    state::{AppState, User},
};

enum Open {
    File(fs::File),
    Dir { listed: bool },
}

pub struct Sftp {
    state: AppState,
    user: &'static User,
    dir: PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
}

impl Sftp {
    pub fn new(state: AppState, user: &'static User) -> Self {
        Self {
            state,
            user,
            dir: PathBuf::new(),
            handles: HashMap::new(),
            next: 0,
        }
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let handle = self.next.to_string();
        self.handles.insert(handle.clone(), open);
        handle
    }

    fn file(&mut self, handle: &str) -> Result<&mut fs::File, StatusReply> {
        match self.handles.get_mut(handle) {
            Some(Open::File(file)) => Ok(file),
            _ => Err(StatusCode::Failure.with_message("invalid handle")),
        }
    }

    fn path(&self, raw: &str) -> Result<PathBuf, StatusReply> {
        match resolve(raw)? {
            Some(name) => Ok(self.dir.join(name)),
            None => Err(StatusCode::Failure.with_message("not a file")),
        }
    }

    async fn attrs(&self, raw: &str) -> Result<FileAttributes, StatusReply> {
        let path = match resolve(raw)? {
            Some(name) => self.dir.join(name),
            None => self.dir.clone(),
        };
        let meta = fs::metadata(path).await.map_err(io_status)?;
        Ok((&meta).into())
    }
}

impl Handler for Sftp {
    type Error = StatusReply;

    fn unimplemented(&self) -> StatusReply {
        StatusCode::OpUnsupported.into()
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<russh_sftp::protocol::Version, StatusReply> {
        self.dir = self
            .state
            .files
            .dir(self.user)
            .await
            .map_err(storage_status)?;
        Ok(russh_sftp::protocol::Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusReply> {
        let path = match resolve(&path)? {
            Some(name) => format!("/{name}"),
            None => "/".to_owned(),
        };
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        Ok(Attrs {
            id,
            attrs: self.attrs(&path).await?,
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusReply> {
        let meta = self.file(&handle)?.metadata().await.map_err(io_status)?;
        Ok(Attrs {
            id,
            attrs: (&meta).into(),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        self.attrs(&path).await?;
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        self.file(&handle)?;
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusReply> {
        if resolve(&path)?.is_some() {
            return Err(StatusCode::NoSuchFile.into());
        }
        Ok(Handle {
            id,
            handle: self.handle(Open::Dir { listed: false }),
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusReply> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir { listed: false }) => {}
            Some(Open::Dir { listed: true }) => return Err(StatusCode::Eof.into()),
            _ => return Err(StatusCode::Failure.with_message("invalid handle")),
        }
        let mut entries = Vec::new();
        for file in files::list(&self.dir).await.map_err(storage_status)? {
            let meta = fs::metadata(self.dir.join(&file.name))
                .await
                .map_err(io_status)?;
            entries.push(File::new(file.name, (&meta).into()));
        }
        self.handles.insert(handle, Open::Dir { listed: true });
        Ok(Name { id, files: entries })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, StatusReply> {
        let path = self.path(&filename)?;
        let write = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND);
        let file = OpenOptions::new()
            .read(pflags.contains(OpenFlags::READ) || !write)
            .write(write)
            .create(write && pflags.contains(OpenFlags::CREATE))
            .truncate(write && pflags.contains(OpenFlags::TRUNCATE))
            .create_new(write && pflags.contains(OpenFlags::EXCLUDE))
            .mode(0o600)
            .open(path)
            .await
            .map_err(io_status)?;
        Ok(Handle {
            id,
            handle: self.handle(Open::File(file)),
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusReply> {
        let file = self.file(&handle)?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(io_status)?;
        let mut data = Vec::with_capacity(len as usize);
        file.take(len as u64)
            .read_to_end(&mut data)
            .await
            .map_err(io_status)?;
        if data.is_empty() {
            return Err(StatusCode::Eof.into());
        }
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusReply> {
        let (state, user) = (self.state, self.user);
        let file = self.file(&handle)?;
        let current = file.metadata().await.map_err(io_status)?.len();
        let end = offset + data.len() as u64;
        if end > current {
            let used = state.files.usage(user).await.map_err(storage_status)?;
            if used - current.min(used) + end > state.files.quota {
                return Err(storage_status(Exn::new(StorageError::Quota(
                    state.files.quota_mb(),
                ))));
            }
        }
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(io_status)?;
        file.write_all(&data).await.map_err(io_status)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusReply> {
        match self.handles.remove(&handle) {
            Some(Open::File(mut file)) => {
                file.flush().await.map_err(io_status)?;
                Ok(ok(id))
            }
            Some(Open::Dir { .. }) => Ok(ok(id)),
            None => Err(StatusCode::Failure.with_message("invalid handle")),
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusReply> {
        fs::remove_file(self.path(&filename)?)
            .await
            .map_err(io_status)?;
        Ok(ok(id))
    }
}

fn resolve(raw: &str) -> Result<Option<String>, StatusReply> {
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    match parts.as_slice() {
        [] => Ok(None),
        [name] => {
            check_name(name)
                .map_err(|e| StatusCode::PermissionDenied.with_message(chain(e.frame())))?;
            Ok(Some((*name).to_owned()))
        }
        _ => Err(StatusCode::NoSuchFile.into()),
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}

fn io_status(error: io::Error) -> StatusReply {
    let code = match error.kind() {
        ErrorKind::NotFound => StatusCode::NoSuchFile,
        ErrorKind::PermissionDenied | ErrorKind::AlreadyExists => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    };
    code.with_message(error.to_string())
}

fn storage_status(error: Exn<StorageError>) -> StatusReply {
    StatusCode::Failure.with_message(chain(error.frame()))
}
