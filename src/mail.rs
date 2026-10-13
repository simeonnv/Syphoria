use std::{collections::HashSet, fmt};

use chrono::{DateTime, Utc};
use exn::{OptionExt, Result};
use jmap_client::{
    client::Client,
    core::{query, response::EmailSetResponse, set::SetObject},
    email::{self, EmailAddress, EmailBodyPart, Property, query::Filter as Condition},
    mailbox::{self, Role},
};
use url::Url;

use crate::error::RemoteError;

const SEEN: &str = "$seen";
const JUNK: &str = "$junk";
const NOT_JUNK: &str = "$notjunk";
const PAGE: usize = 200;
const GET_LIMIT: usize = 500;
const CORRESPONDENTS: usize = 1000;

type Filter = query::Filter<Condition>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Me(String),
    With(String),
    From(String),
    To(String),
    Subject(String),
    In(String),
    Unread,
    Read,
    Text(String),
}

impl Term {
    fn filter(&self) -> Filter {
        match self {
            Term::Me(a) => Filter::or([
                Condition::from(a),
                Condition::to(a),
                Condition::cc(a),
                Condition::bcc(a),
            ]),
            Term::With(a) => Filter::or([Condition::from(a), Condition::to(a), Condition::cc(a)]),
            Term::From(a) => Condition::from(a).into(),
            Term::To(a) => Condition::to(a).into(),
            Term::Subject(s) => Condition::subject(s).into(),
            Term::In(mailbox) => Condition::in_mailbox(mailbox).into(),
            Term::Unread => Condition::not_keyword(SEEN).into(),
            Term::Read => Condition::has_keyword(SEEN).into(),
            Term::Text(t) => Condition::text(t).into(),
        }
    }
}

pub struct Found {
    pub total: usize,
    pub summaries: Vec<Summary>,
}

pub struct Mail {
    client: Client,
    pub inbox: String,
    sent: String,
    mailboxes: Vec<(Role, String)>,
}

#[derive(Debug, Clone)]
pub struct Addr {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub unread: usize,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub id: String,
    pub received: Option<DateTime<Utc>>,
    pub from: Vec<Addr>,
    pub to: Vec<Addr>,
    pub subject: String,
    pub seen: bool,
    pub mailbox: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub received: Option<DateTime<Utc>>,
    pub from: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub blob_id: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
}

impl Mail {
    pub async fn connect(url: &Url, login: &str, password: &str) -> Result<Self, RemoteError> {
        let client = Client::new()
            .credentials((login, password))
            .follow_redirects(url.host_str())
            .connect(url.as_str().trim_end_matches('/'))
            .await
            .map_err(RemoteError::jmap)?;

        let mut mail = Self {
            client,
            inbox: String::new(),
            sent: String::new(),
            mailboxes: Vec::new(),
        };

        let folders = mail.folders().await?;

        let find = |role: Role| {
            folders
                .iter()
                .find(|f| f.role == role)
                .map(|f| f.id.clone())
                .ok_or_raise(|| RemoteError::NoMailbox(format!("{role:?}")))
        };
        mail.inbox = find(Role::Inbox)?;
        mail.sent = find(Role::Sent)?;
        mail.mailboxes = folders
            .into_iter()
            .filter(|f| f.role != Role::None)
            .map(|f| (f.role, f.id))
            .collect();
        Ok(mail)
    }

    pub async fn move_to(&self, id: &str, from: &Role, to: &Role) -> Result<(), RemoteError> {
        let mailbox = self
            .mailboxes
            .iter()
            .find(|(r, _)| r == to)
            .map(|(_, mailbox)| mailbox.as_str())
            .ok_or_raise(|| RemoteError::NoMailbox(format!("{to:?}")))?;
        self.move_into(id, mailbox, from, to).await
    }

    pub async fn move_into(
        &self,
        id: &str,
        mailbox: &str,
        from: &Role,
        to: &Role,
    ) -> Result<(), RemoteError> {
        let mut request = self.client.build();
        let email = request.set_email().update(id).mailbox_ids([mailbox]);
        if let Some(junk) = junk(from, to) {
            email.keyword(JUNK, junk).keyword(NOT_JUNK, !junk);
        }
        request
            .send_single::<EmailSetResponse>()
            .await
            .map_err(RemoteError::jmap)?
            .updated(id)
            .map_err(RemoteError::jmap)?;
        Ok(())
    }

    pub async fn destroy(&self, id: &str) -> Result<(), RemoteError> {
        self.client
            .email_destroy(id)
            .await
            .map_err(RemoteError::jmap)
    }

    pub async fn set_seen(&self, id: &str, seen: bool) -> Result<(), RemoteError> {
        self.client
            .email_set_keyword(id, SEEN, seen)
            .await
            .map_err(RemoteError::jmap)?;
        Ok(())
    }

    pub async fn folders(&self) -> Result<Vec<Folder>, RemoteError> {
        let mut request = self.client.build();
        request.get_mailbox().properties([
            mailbox::Property::Id,
            mailbox::Property::Name,
            mailbox::Property::Role,
            mailbox::Property::SortOrder,
            mailbox::Property::UnreadEmails,
        ]);

        let mut list = request
            .send_get_mailbox()
            .await
            .map_err(RemoteError::jmap)?
            .take_list();
        list.sort_by_key(|m| (rank(&m.role()), m.sort_order(), m.name().map(str::to_owned)));

        Ok(list
            .into_iter()
            .map(|m| Folder {
                id: m.id().unwrap_or_default().to_owned(),
                name: match m.role() {
                    Role::Junk => "Spam".to_owned(),
                    _ => m.name().unwrap_or_default().to_owned(),
                },
                role: m.role(),
                unread: m.unread_emails(),
            })
            .collect())
    }

    pub async fn summaries(&self, mailbox: &str) -> Result<Vec<Summary>, RemoteError> {
        let found = self.query(&[Term::In(mailbox.to_owned())], 0, PAGE).await?;
        Ok(found.summaries)
    }

    pub async fn query(
        &self,
        terms: &[Term],
        position: usize,
        limit: usize,
    ) -> Result<Found, RemoteError> {
        let mut request = self.client.build();
        let query = request.query_email();
        if !terms.is_empty() {
            query.filter(Filter::and(terms.iter().map(Term::filter)));
        }
        query
            .sort([email::query::Comparator::received_at().descending()])
            .position(position as i32)
            .limit(limit.min(GET_LIMIT))
            .calculate_total(true);

        let ids = query.result_reference();
        request.get_email().ids_ref(ids).properties([
            Property::Id,
            Property::ReceivedAt,
            Property::From,
            Property::To,
            Property::Subject,
            Property::Keywords,
            Property::MailboxIds,
        ]);

        let mut responses = request
            .send()
            .await
            .map_err(RemoteError::jmap)?
            .unwrap_method_responses();
        let list = responses
            .pop()
            .ok_or_raise(|| RemoteError::Internal)?
            .unwrap_get_email()
            .map_err(RemoteError::jmap)?
            .take_list();
        let total = responses
            .pop()
            .ok_or_raise(|| RemoteError::Internal)?
            .unwrap_query_email()
            .map_err(RemoteError::jmap)?
            .total()
            .unwrap_or(position + list.len());

        let summaries = list
            .into_iter()
            .map(|e| Summary {
                id: e.id().unwrap_or_default().to_owned(),
                received: received(e.received_at()),
                from: addrs(e.from()),
                to: addrs(e.to()),
                subject: e.subject().unwrap_or_default().to_owned(),
                seen: e.keywords().contains(&SEEN),
                mailbox: e.mailbox_ids().first().map(|id| (*id).to_owned()),
            })
            .collect();
        Ok(Found { total, summaries })
    }

    pub async fn correspondents(&self) -> Result<Vec<String>, RemoteError> {
        self.addresses(None, &[Property::From, Property::To, Property::Cc])
            .await
    }

    pub async fn recipients(&self) -> Result<Vec<String>, RemoteError> {
        self.addresses(
            Some(&self.sent),
            &[Property::To, Property::Cc, Property::Bcc],
        )
        .await
    }

    async fn addresses(
        &self,
        mailbox: Option<&str>,
        properties: &[Property],
    ) -> Result<Vec<String>, RemoteError> {
        let mut request = self.client.build();
        for position in (0..CORRESPONDENTS).step_by(GET_LIMIT) {
            let query = request.query_email();
            if let Some(mailbox) = mailbox {
                query.filter(Condition::in_mailbox(mailbox));
            }
            query
                .sort([email::query::Comparator::received_at().descending()])
                .position(position as i32)
                .limit(GET_LIMIT);
            let ids = query.result_reference();
            request
                .get_email()
                .ids_ref(ids)
                .properties(properties.iter().cloned());
        }

        let responses = request
            .send()
            .await
            .map_err(RemoteError::jmap)?
            .unwrap_method_responses();
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for response in responses.into_iter().skip(1).step_by(2) {
            for email in response
                .unwrap_get_email()
                .map_err(RemoteError::jmap)?
                .take_list()
            {
                for addr in [email.from(), email.to(), email.cc(), email.bcc()]
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let address = addr.email().to_lowercase();
                    if seen.insert(address.clone()) {
                        list.push(address);
                    }
                }
            }
        }
        Ok(list)
    }

    pub async fn message(&self, id: &str) -> Result<Message, RemoteError> {
        let mut request = self.client.build();
        request
            .get_email()
            .ids([id])
            .properties([
                Property::Id,
                Property::ReceivedAt,
                Property::From,
                Property::To,
                Property::Cc,
                Property::Subject,
                Property::TextBody,
                Property::BodyValues,
                Property::Attachments,
            ])
            .arguments()
            .fetch_text_body_values(true);

        let email = request
            .send_get_email()
            .await
            .map_err(RemoteError::jmap)?
            .take_list()
            .pop()
            .ok_or_raise(|| RemoteError::NotFound)?;

        let body = email
            .text_body()
            .unwrap_or_default()
            .iter()
            .filter_map(|part| email.body_value(part.part_id()?))
            .map(|value| value.value())
            .collect::<Vec<_>>()
            .join("\n");

        self.client
            .email_set_keyword(id, SEEN, true)
            .await
            .map_err(RemoteError::jmap)?;

        Ok(Message {
            received: received(email.received_at()),
            from: addrs(email.from()),
            to: addrs(email.to()),
            cc: addrs(email.cc()),
            subject: email.subject().unwrap_or_default().to_owned(),
            body,
            attachments: email
                .attachments()
                .unwrap_or_default()
                .iter()
                .filter_map(|part| {
                    Some(Attachment {
                        blob_id: part.blob_id()?.to_owned(),
                        name: part.name().unwrap_or("attachment").to_owned(),
                        kind: part
                            .content_type()
                            .unwrap_or("application/octet-stream")
                            .to_owned(),
                        size: part.size() as u64,
                    })
                })
                .collect(),
        })
    }

    pub async fn download(&self, blob_id: &str) -> Result<Vec<u8>, RemoteError> {
        self.client
            .download(blob_id)
            .await
            .map_err(RemoteError::jmap)
    }

    pub async fn upload(
        &self,
        name: &str,
        kind: &str,
        bytes: Vec<u8>,
    ) -> Result<Attachment, RemoteError> {
        let size = bytes.len() as u64;
        let blob_id = self
            .client
            .upload(None, bytes, Some(kind))
            .await
            .map_err(RemoteError::jmap)?
            .take_blob_id();
        Ok(Attachment {
            blob_id,
            name: name.to_owned(),
            kind: kind.to_owned(),
            size,
        })
    }

    pub async fn send(
        &self,
        name: &str,
        from: &str,
        to: &[String],
        subject: &str,
        body: &str,
        attachments: &[Attachment],
    ) -> Result<(), RemoteError> {
        let identity = self.identity(name, from).await?;

        let mut request = self.client.build();
        let email = request
            .set_email()
            .create()
            .mailbox_ids([self.sent.as_str()])
            .keyword(SEEN, true)
            .from([(name, from)])
            .to(to.iter().map(String::as_str))
            .subject(subject)
            .text_body(
                EmailBodyPart::new()
                    .part_id("body")
                    .content_type("text/plain"),
            )
            .body_value("body".into(), body);
        for attachment in attachments {
            email.attachment(
                EmailBodyPart::new()
                    .blob_id(&attachment.blob_id)
                    .name(&attachment.name)
                    .content_type(&attachment.kind),
            );
        }

        let create_id = email.create_id().ok_or_raise(|| RemoteError::Internal)?;

        let email_id = request
            .send_single::<EmailSetResponse>()
            .await
            .map_err(RemoteError::jmap)?
            .created(&create_id)
            .map_err(RemoteError::jmap)?
            .take_id();

        self.client
            .email_submission_create(email_id.as_str(), identity)
            .await
            .map_err(RemoteError::jmap)?;
        Ok(())
    }

    async fn identity(&self, name: &str, email: &str) -> Result<String, RemoteError> {
        let mut request = self.client.build();
        request.get_identity();

        let existing = request
            .send_get_identity()
            .await
            .map_err(RemoteError::jmap)?
            .take_list()
            .into_iter()
            .find(|i| i.email() == Some(email));

        let mut identity = match existing {
            Some(identity) => identity,
            None => self
                .client
                .identity_create(name, email)
                .await
                .map_err(RemoteError::jmap)?,
        };
        Ok(identity.take_id())
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name} <{}>", self.email),
            None => f.write_str(&self.email),
        }
    }
}

fn addrs(list: Option<&[EmailAddress]>) -> Vec<Addr> {
    list.unwrap_or_default()
        .iter()
        .map(|a| Addr {
            name: a.name().map(str::to_owned),
            email: a.email().to_owned(),
        })
        .collect()
}

fn rank(role: &Role) -> u8 {
    match role {
        Role::Inbox => 0,
        Role::Drafts => 1,
        Role::Sent => 2,
        Role::Archive => 3,
        Role::Junk => 4,
        Role::Trash => 5,
        _ => 6,
    }
}

fn junk(from: &Role, to: &Role) -> Option<bool> {
    match (from, to) {
        (Role::Junk, Role::Junk) => None,
        (_, Role::Junk) => Some(true),
        (Role::Junk, Role::Trash) => None,
        (Role::Junk, _) => Some(false),
        _ => None,
    }
}

fn received(timestamp: Option<i64>) -> Option<DateTime<Utc>> {
    timestamp.and_then(|t| DateTime::from_timestamp(t, 0))
}
