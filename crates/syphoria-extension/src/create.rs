use dioxus::prelude::*;
use syphoria_api::{Addresses, Choice, Signup, site};

use crate::{check, client::Client, fill};

const SUFFIX: usize = 6;
const USERNAME: usize = 10;
const SLUG: usize = 24;
const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    New,
    Existing,
}

#[component]
pub fn Create(
    client: Client,
    tab: Option<i32>,
    url: String,
    on_locked: EventHandler<()>,
) -> Element {
    let listed = client.clone();
    let addresses = use_resource(move || {
        let client = listed.clone();
        async move { check(client.addresses().await, on_locked) }
    });
    let suffix = use_hook(|| random(SUFFIX));
    let page = !url.is_empty();
    let mut website = use_signal(|| url.clone());
    let mut description = use_signal(String::new);
    let mut username = use_signal(random_username);
    let mut local = use_signal(String::new);
    let mut edited = use_signal(|| false);
    let mut domain = use_signal(String::new);
    let mut existing = use_signal(String::new);
    let mut mode = use_signal(|| None::<Mode>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let generated = format!("{}.{suffix}", slug(&site_name(&website.read())));
    let shown_local = if edited() { local() } else { generated.clone() };

    let loaded: Option<Addresses> = match &*addresses.read() {
        Some(Ok(a)) => Some(a.clone()),
        _ => None,
    };
    let current_mode = mode().unwrap_or(match &loaded {
        Some(a) if a.domains.is_empty() => Mode::Existing,
        _ => Mode::New,
    });
    let current_domain = loaded
        .as_ref()
        .and_then(|a| {
            a.domains
                .iter()
                .find(|d| **d == domain())
                .or(a.domains.first())
                .cloned()
        })
        .unwrap_or_default();
    let current_existing = loaded
        .as_ref()
        .and_then(|a| {
            a.addresses
                .iter()
                .find(|d| **d == existing())
                .or(a.addresses.first())
                .cloned()
        })
        .unwrap_or_default();

    let submit = {
        let client = client.clone();
        let shown_local = shown_local.clone();
        let current_domain = current_domain.clone();
        let current_existing = current_existing.clone();
        move |event: FormEvent| {
            event.prevent_default();
            let address = match current_mode {
                Mode::New => Choice::New {
                    local: shown_local.clone(),
                    domain: current_domain.clone(),
                },
                Mode::Existing => Choice::Existing {
                    address: current_existing.clone(),
                },
            };
            let request = Signup {
                url: origin(&website.read()),
                description: description.read().trim().to_owned(),
                username: username.read().trim().to_owned(),
                address,
            };
            let client = client.clone();
            busy.set(true);
            error.set(None);
            spawn(async move {
                let result = match check(client.signup(&request).await, on_locked) {
                    Ok(login) => fill(tab, login).await,
                    Err(e) => Err(e),
                };
                error.set(result.err());
                busy.set(false);
            });
        }
    };

    rsx! {
        match &*addresses.read() {
            None => rsx! { p { class: "muted", "loading…" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
            Some(Ok(list)) => rsx! {
                form { class: "stack", onsubmit: submit,
                    if !page {
                        label { r#for: "website", "Website" }
                        input {
                            id: "website",
                            placeholder: "https://example.com",
                            value: "{website}",
                            oninput: move |e| website.set(e.value()),
                        }
                    }
                    label { r#for: "description", "Description" }
                    input {
                        id: "description",
                        autofocus: true,
                        placeholder: "what is this account for?",
                        value: "{description}",
                        oninput: move |e| description.set(e.value()),
                    }
                    label { r#for: "username", "Username" }
                    div { class: "inline",
                        input {
                            id: "username",
                            placeholder: "empty uses the email",
                            value: "{username}",
                            oninput: move |e| username.set(e.value()),
                        }
                        button {
                            r#type: "button",
                            class: "small",
                            title: "new random username",
                            onclick: move |_| username.set(random_username()),
                            "\u{21bb}"
                        }
                    }
                    label { "Email" }
                    if !list.domains.is_empty() && !list.addresses.is_empty() {
                        div { class: "tabs",
                            button {
                                r#type: "button",
                                class: if current_mode == Mode::New { "tab active" } else { "tab" },
                                onclick: move |_| mode.set(Some(Mode::New)),
                                "New address"
                            }
                            button {
                                r#type: "button",
                                class: if current_mode == Mode::Existing { "tab active" } else { "tab" },
                                onclick: move |_| mode.set(Some(Mode::Existing)),
                                "Existing"
                            }
                        }
                    }
                    if current_mode == Mode::New {
                        div { class: "address",
                            input {
                                value: "{shown_local}",
                                oninput: move |e| {
                                    edited.set(true);
                                    local.set(e.value());
                                },
                            }
                            span { "@" }
                            select {
                                value: "{current_domain}",
                                onchange: move |e| domain.set(e.value()),
                                for d in list.domains.clone() {
                                    option { value: "{d}", selected: d == current_domain, "{d}" }
                                }
                            }
                        }
                    } else if list.addresses.is_empty() {
                        p { class: "error", "You have no addresses yet." }
                    } else {
                        select {
                            value: "{current_existing}",
                            onchange: move |e| existing.set(e.value()),
                            for a in list.addresses.clone() {
                                option { value: "{a}", selected: a == current_existing, "{a}" }
                            }
                        }
                    }
                    button {
                        r#type: "submit",
                        disabled: busy() || site(&website.read()).is_none(),
                        if busy() { "Creating…" } else if tab.is_some() { "Create & fill" } else { "Create" }
                    }
                    p { class: "muted", "A strong password is generated and saved to your vault." }
                }
            },
        }
        if let Some(e) = error() {
            p { class: "error", "{e}" }
        }
    }
}

fn site_name(url: &str) -> String {
    site(url)
        .map(|s| s.split('.').next().unwrap_or(&s).to_owned())
        .unwrap_or_default()
}

fn origin(url: &str) -> String {
    let trimmed = url.trim();
    url::Url::parse(trimmed)
        .ok()
        .filter(|u| u.has_host())
        .or_else(|| url::Url::parse(&format!("https://{trimmed}")).ok())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|| trimmed.to_owned())
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(SLUG).collect();
    match slug.trim_end_matches('-') {
        "" => "account".to_owned(),
        s => s.to_owned(),
    }
}

fn random_username() -> String {
    let first = random(1).replace(|c: char| c.is_ascii_digit(), "x");
    format!("{first}{}", random(USERNAME - 1))
}

fn random(len: usize) -> String {
    (0..len)
        .map(|_| {
            let i = (js_sys::Math::random() * ALPHABET.len() as f64) as usize;
            char::from(ALPHABET[i.min(ALPHABET.len() - 1)])
        })
        .collect()
}
