use std::fmt;

use chrono::{DateTime, Utc};
use exn::{OptionExt, Result, ResultExt};
use jmap_client::{
    client::Client,
    core::{response::EmailSetResponse, set::SetObject},
    email::{self, EmailAddress, EmailBodyPart, Property},
    mailbox::{self, Role},
};
use url::Url;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MailError(String);

const SEEN: &str = "$seen";
const PAGE: usize = 200;

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
}

#[derive(Debug, Clone)]
pub struct Message {
    pub received: Option<DateTime<Utc>>,
    pub from: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub body: String,
}

impl Mail {
    pub async fn connect(url: &Url, login: &str, password: &str) -> Result<Self, MailError> {
        let err = || MailError(format!("failed to open mailbox of `{login}` at {url}"));
        let client = Client::new()
            .credentials((login, password))
            .follow_redirects(url.host_str())
            .connect(url.as_str().trim_end_matches('/'))
            .await
            .or_raise(err)?;

        let mut mail = Self {
            client,
            inbox: String::new(),
            sent: String::new(),
            mailboxes: Vec::new(),
        };

        let folders = mail.folders().await.or_raise(err)?;

        let find = |role: Role| {
            folders
                .iter()
                .find(|f| f.role == role)
                .map(|f| f.id.clone())
                .ok_or_raise(|| MailError(format!("account has no {role:?} mailbox")))
                .or_raise(err)
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

    pub async fn move_to(&self, id: &str, role: Role) -> Result<(), MailError> {
        let err = || MailError(format!("failed to move email `{id}` to {role:?}"));
        let mailbox = self
            .mailboxes
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, mailbox)| mailbox.as_str())
            .ok_or_raise(|| MailError(format!("account has no {role:?} mailbox")))
            .or_raise(err)?;
        self.client
            .email_set_mailboxes(id, [mailbox])
            .await
            .or_raise(err)?;
        Ok(())
    }

    pub async fn destroy(&self, id: &str) -> Result<(), MailError> {
        self.client
            .email_destroy(id)
            .await
            .or_raise(|| MailError(format!("failed to delete email `{id}`")))
    }

    pub async fn set_seen(&self, id: &str, seen: bool) -> Result<(), MailError> {
        let state = if seen { "read" } else { "unread" };
        self.client
            .email_set_keyword(id, SEEN, seen)
            .await
            .or_raise(|| MailError(format!("failed to mark email `{id}` as {state}")))?;
        Ok(())
    }

    pub async fn folders(&self) -> Result<Vec<Folder>, MailError> {
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
            .or_raise(|| MailError("failed to list mailboxes".into()))?
            .take_list();
        list.sort_by_key(|m| (rank(&m.role()), m.sort_order(), m.name().map(str::to_owned)));

        Ok(list
            .into_iter()
            .map(|m| Folder {
                id: m.id().unwrap_or_default().to_owned(),
                name: m.name().unwrap_or_default().to_owned(),
                role: m.role(),
                unread: m.unread_emails(),
            })
            .collect())
    }

    pub async fn summaries(&self, mailbox: &str) -> Result<Vec<Summary>, MailError> {
        let err = || MailError(format!("failed to list emails in mailbox `{mailbox}`"));
        let mut request = self.client.build();
        let query = request
            .query_email()
            .filter(email::query::Filter::in_mailbox(mailbox))
            .sort([email::query::Comparator::received_at().descending()])
            .limit(PAGE);

        let ids = query.result_reference();
        request.get_email().ids_ref(ids).properties([
            Property::Id,
            Property::ReceivedAt,
            Property::From,
            Property::To,
            Property::Subject,
            Property::Keywords,
        ]);

        let list = request
            .send()
            .await
            .or_raise(err)?
            .unwrap_method_responses()
            .pop()
            .ok_or_raise(|| MailError("server returned no Email/get response".into()))
            .or_raise(err)?
            .unwrap_get_email()
            .or_raise(err)?
            .take_list();

        Ok(list
            .into_iter()
            .map(|e| Summary {
                id: e.id().unwrap_or_default().to_owned(),
                received: received(e.received_at()),
                from: addrs(e.from()),
                to: addrs(e.to()),
                subject: e.subject().unwrap_or_default().to_owned(),
                seen: e.keywords().contains(&SEEN),
            })
            .collect())
    }

    pub async fn message(&self, id: &str) -> Result<Message, MailError> {
        let err = || MailError(format!("failed to load email `{id}`"));
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
            ])
            .arguments()
            .fetch_text_body_values(true);

        let email = request
            .send_get_email()
            .await
            .or_raise(err)?
            .take_list()
            .pop()
            .ok_or_raise(|| MailError("no such email".into()))
            .or_raise(err)?;

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
            .or_raise(|| MailError(format!("failed to mark email `{id}` as read")))?;

        Ok(Message {
            received: received(email.received_at()),
            from: addrs(email.from()),
            to: addrs(email.to()),
            cc: addrs(email.cc()),
            subject: email.subject().unwrap_or_default().to_owned(),
            body,
        })
    }

    pub async fn send(
        &self,
        name: &str,
        from: &str,
        to: &[String],
        subject: &str,
        body: &str,
    ) -> Result<(), MailError> {
        let err = || MailError(format!("failed to send email from `{from}`"));
        let identity = self.identity(name, from).await.or_raise(err)?;

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

        let create_id = email
            .create_id()
            .ok_or_raise(|| MailError("email has no create id".into()))
            .or_raise(err)?;

        let email_id = request
            .send_single::<EmailSetResponse>()
            .await
            .or_raise(err)?
            .created(&create_id)
            .or_raise(err)?
            .take_id();

        self.client
            .email_submission_create(email_id, identity)
            .await
            .or_raise(err)?;
        Ok(())
    }

    async fn identity(&self, name: &str, email: &str) -> Result<String, MailError> {
        let err = || MailError(format!("failed to find a sending identity for `{email}`"));
        let mut request = self.client.build();
        request.get_identity();

        let existing = request
            .send_get_identity()
            .await
            .or_raise(err)?
            .take_list()
            .into_iter()
            .find(|i| i.email() == Some(email));

        let mut identity = match existing {
            Some(identity) => identity,
            None => self
                .client
                .identity_create(name, email)
                .await
                .or_raise(err)?,
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

fn received(timestamp: Option<i64>) -> Option<DateTime<Utc>> {
    timestamp.and_then(|t| DateTime::from_timestamp(t, 0))
}
