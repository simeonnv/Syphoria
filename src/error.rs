use exn::{Exn, Frame};
use reqwest::StatusCode;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config")]
    Read,
    #[error("failed to parse config")]
    Parse,
    #[error("domain `{0}` is not in the global `domains` list")]
    UnknownDomain(String),
    #[error("ssh key {0} is assigned to more than one user")]
    DuplicateKey(String),
    #[error("user `{0}` sets both `domains` and `addresses`, use one")]
    BothGrants(String),
    #[error("user `{0}` needs either `domains` or `addresses`")]
    NoGrant(String),
    #[error("address `{0}` is assigned to more than one user")]
    DuplicateAddress(String),
    #[error("address `{address}` is on `{domain}`, where user `{user}` can create addresses")]
    ClaimedDomain {
        address: String,
        domain: String,
        user: String,
    },
    #[error("failed to read stalwart admin password")]
    AdminPassword,
    #[error("failed to set up vault")]
    Vault,
    #[error("failed to set up file storage")]
    Files,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("vault does not exist")]
    NotFound,
    #[error("wrong password")]
    WrongPassword,
    #[error("vault locked after too many wrong passwords")]
    Locked,
    #[error("service may not be empty")]
    EmptyService,
    #[error("invalid account address")]
    InvalidAddress,
    #[error("failed to set up vault directory")]
    Setup,
    #[error("failed to create vault")]
    Create,
    #[error("failed to open vault")]
    Open,
    #[error("vault query failed")]
    Query,
    #[error("vault task failed")]
    Task,
    #[error("failed to read host key")]
    ReadHostKey,
    #[error("failed to create host key")]
    CreateHostKey,
    #[error("failed to set up file storage")]
    FilesSetup,
    #[error("failed to list files")]
    ListFiles,
    #[error("failed to read file")]
    ReadFile,
    #[error("failed to write file")]
    WriteFile,
    #[error("failed to remove file")]
    RemoveFile,
    #[error("file storage full, {0} MB max")]
    Quota(u64),
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("rejected by mail server")]
    Rejected,
    #[error("not authorized")]
    Unauthorized,
    #[error("mail server unavailable")]
    Unavailable,
    #[error("unexpected mail server response")]
    Internal,
    #[error("no {0} mailbox")]
    NoMailbox(String),
}

impl RemoteError {
    pub fn http(status: StatusCode) -> Self {
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Self::Unauthorized,
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::CONFLICT => Self::AlreadyExists,
            StatusCode::TOO_MANY_REQUESTS => Self::Unavailable,
            status if status.is_server_error() => Self::Unavailable,
            status if status.is_client_error() => Self::Rejected,
            _ => Self::Internal,
        }
    }

    #[track_caller]
    pub fn jmap(error: jmap_client::Error) -> Exn<Self> {
        let kind = match &error {
            jmap_client::Error::Transport(_) => Self::Unavailable,
            jmap_client::Error::Problem(problem) => match problem.status() {
                Some(401 | 403) => Self::Unauthorized,
                Some(404) => Self::NotFound,
                Some(429 | 500..) => Self::Unavailable,
                _ => Self::Internal,
            },
            _ => Self::Internal,
        };
        Exn::new(error).raise(kind)
    }
}

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

impl ApiError {
    pub fn kind(&self) -> RemoteError {
        match self {
            ApiError::FieldAlreadyExists { .. } => RemoteError::AlreadyExists,
            ApiError::FieldMissing { .. } | ApiError::Unsupported { .. } => RemoteError::Rejected,
            ApiError::NotFound { .. } => RemoteError::NotFound,
            ApiError::AssertFailed | ApiError::Other { .. } => RemoteError::Internal,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("not a valid address")]
    InvalidAddress,
    #[error("not one of your domains")]
    ForeignDomain,
    #[error("invalid recipient `{0}`")]
    InvalidRecipient(String),
    #[error("add at least one recipient")]
    NoRecipients,
    #[error("your addresses are fixed by the admin")]
    FixedAddresses,
    #[error("unknown folder `{0}`")]
    UnknownFolder(String),
    #[error("invalid file name `{0}`")]
    InvalidFileName(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("ssh server failed")]
    Serve,
    #[error("ssh session failed")]
    Session,
    #[error("auth succeeded without a public key")]
    NoPublicKey,
    #[error("failed to accept pty request")]
    Pty,
    #[error("failed to handle subsystem request")]
    Subsystem,
    #[error("failed to accept shell request")]
    Shell,
    #[error("failed to enter alternate screen")]
    AltScreen,
    #[error("failed to create terminal")]
    Terminal,
    #[error("failed to render app")]
    Render,
    #[error("channel closed with unsent data")]
    ChannelClosed,
}

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("failed to connect to mail server")]
    Connect,
    #[error("failed to draw")]
    Draw,
    #[error("failed to load folders")]
    LoadFolders,
    #[error("failed to load messages")]
    LoadMessages,
    #[error("failed to search messages")]
    Search,
    #[error("failed to load correspondents")]
    LoadCorrespondents,
    #[error("failed to load message")]
    LoadMessage,
    #[error("failed to send message")]
    SendMessage,
    #[error("failed to move message")]
    MoveMessage,
    #[error("failed to delete message")]
    DeleteMessage,
    #[error("failed to update read state")]
    UpdateSeen,
    #[error("failed to load addresses")]
    LoadAddresses,
    #[error("failed to add address")]
    AddAddress,
    #[error("you have no address yet, create one in Addresses")]
    NoAddress,
    #[error("sender is not on one of your domains")]
    ForeignSender,
    #[error("sending too fast, wait {0}s between messages")]
    SendCooldown(u64),
    #[error("invalid recipients")]
    InvalidRecipients,
    #[error("failed to attach `{0}`")]
    Attach(String),
    #[error("sent, but failed to remove attached files")]
    RemoveSent,
    #[error("failed to load files")]
    LoadFiles,
    #[error("failed to load attachment")]
    LoadAttachment,
    #[error("failed to save attachment")]
    SaveAttachment,
    #[error("failed to load accounts")]
    LoadAccounts,
    #[error("failed to save account")]
    SaveAccount,
    #[error("failed to delete account")]
    DeleteAccount,
    #[error("failed to unlock vault")]
    UnlockVault,
    #[error("failed to create vault")]
    CreateVault,
    #[error("vault task panicked")]
    VaultPanicked,
    #[error("passwords do not match, start over")]
    PasswordMismatch,
    #[error("use at least {0} characters")]
    PasswordTooShort(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum MainError {
    #[error("failed to load config")]
    LoadConfig,
    #[error("invalid config")]
    InvalidConfig,
    #[error("failed to provision stalwart")]
    Provision,
    #[error("failed to load host key")]
    HostKey,
    #[error("ssh server stopped")]
    Serve,
}

#[derive(Debug)]
pub struct HandlerError(pub Exn<SshError>);

impl From<Exn<SshError>> for HandlerError {
    fn from(exn: Exn<SshError>) -> Self {
        Self(exn)
    }
}

impl From<russh::Error> for HandlerError {
    #[track_caller]
    fn from(error: russh::Error) -> Self {
        Self(Exn::new(error).raise(SshError::Session))
    }
}

pub fn chain(frame: &Frame) -> String {
    let mut parts = vec![frame.to_string()];
    parts.extend(frame.children().iter().map(chain));
    parts.join(": ")
}

pub fn is_temporary(frame: &Frame) -> bool {
    let error = frame.error();
    matches!(
        error.downcast_ref::<RemoteError>(),
        Some(RemoteError::Unavailable)
    ) || matches!(
        error.downcast_ref::<StorageError>(),
        Some(StorageError::Locked)
    ) || frame.children().iter().any(is_temporary)
}
