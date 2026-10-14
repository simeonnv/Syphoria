use dioxus::prelude::*;
use exn::ResultExt;
use syphoria_api::{Summary, site};
use wasm_bindgen_futures::JsFuture;

use crate::{
    Access, Setup, Unlock, check,
    client::Client,
    create::Create,
    error::{ExtError, JsFailure},
    ext::{self, Context, Form},
    fill, report,
    save::Save,
    use_access,
};

#[component]
pub fn Popup() -> Element {
    let (access, refresh) = use_access();

    rsx! {
        main { class: "popup",
            header { "syphoria" }
            match access() {
                Access::Loading => rsx! { p { class: "muted", "loading…" } },
                Access::Setup => rsx! { Setup { on_done: refresh } },
                Access::Failed(error) => rsx! {
                    p { class: "error", "{error}" }
                    Setup { on_done: refresh }
                },
                Access::Locked(client) => rsx! {
                    Unlock { client, on_done: refresh }
                    button {
                        class: "link",
                        onclick: move |_| async move {
                            if let Err(e) = Client::forget().await {
                                report(&e);
                            }
                            refresh.call(());
                        },
                        "Change server or token"
                    }
                },
                Access::Ready(client) => rsx! { Main { client, on_locked: refresh } },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Vault,
    Create,
}

#[component]
fn Main(client: Client, on_locked: EventHandler<()>) -> Element {
    let mut loaded = use_resource(ext::context);
    let mut view = use_signal(|| None::<View>);

    let context: Option<Context> = match &*loaded.read() {
        None => return rsx! { p { class: "muted", "loading…" } },
        Some(Ok(context)) => context.clone(),
        Some(Err(e)) => {
            report(e);
            None
        }
    };
    let tab = context.as_ref().map(|c| c.tab);
    let url = context.as_ref().map(|c| c.url.clone()).unwrap_or_default();
    let pending = context.as_ref().and_then(|c| c.pending.clone());
    let shown = view().unwrap_or(match context.as_ref().and_then(|c| c.form) {
        Some(Form::Signup) => View::Create,
        _ => View::Vault,
    });

    rsx! {
        if let (Some(tab), Some(pending)) = (tab, pending) {
            Save {
                client: client.clone(),
                tab,
                pending,
                on_done: move |()| loaded.restart(),
                on_locked,
            }
        }
        div { class: "tabs",
            button {
                class: if shown == View::Vault { "tab active" } else { "tab" },
                onclick: move |_| view.set(Some(View::Vault)),
                "Vault"
            }
            button {
                class: if shown == View::Create { "tab active" } else { "tab" },
                onclick: move |_| view.set(Some(View::Create)),
                "New account"
            }
        }
        match shown {
            View::Vault => rsx! { Vault { client, tab, url, on_locked } },
            View::Create => rsx! { Create { client, tab, url, on_locked } },
        }
    }
}

#[component]
fn Vault(client: Client, tab: Option<i32>, url: String, on_locked: EventHandler<()>) -> Element {
    let listed = client.clone();
    let accounts = use_resource(move || {
        let client = listed.clone();
        async move { check(client.accounts(None).await, on_locked) }
    });
    let mut search = use_signal(String::new);
    let mut notice = use_signal(|| None::<Result<String, String>>);
    let here = site(&url);

    let copy = {
        let client = client.clone();
        move |account: Summary, password: bool| {
            let client = client.clone();
            spawn(async move {
                let text = if password {
                    check(client.login(account.id).await, on_locked).map(|l| l.password)
                } else {
                    Ok(account.username.clone())
                };
                let result = match text {
                    Ok(text) => clipboard(&text).await.map_err(|e| report(&e)),
                    Err(e) => Err(e),
                };
                let what = if password { "password" } else { "username" };
                notice.set(Some(result.map(|()| format!("{what} copied"))));
            });
        }
    };

    let use_login = {
        let client = client.clone();
        move |account: Summary| {
            let client = client.clone();
            spawn(async move {
                let result = match check(client.login(account.id).await, on_locked) {
                    Ok(login) => fill(tab, login).await,
                    Err(e) => Err(e),
                };
                if let Err(e) = result {
                    notice.set(Some(Err(e)));
                }
            });
        }
    };

    let lock = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            async move {
                if let Err(e) = client.lock().await {
                    report(&e);
                }
                on_locked.call(());
            }
        }
    };

    rsx! {
        input {
            class: "search",
            placeholder: "Search",
            autofocus: true,
            value: "{search}",
            oninput: move |e| search.set(e.value()),
        }
        match &*accounts.read() {
            None => rsx! { p { class: "muted", "loading…" } },
            Some(Err(error)) => rsx! { p { class: "error", "{error}" } },
            Some(Ok(list)) => {
                let query = search.read().to_lowercase();
                let mut shown: Vec<(bool, Summary)> = list
                    .iter()
                    .filter(|a| {
                        query.is_empty()
                            || [&a.service, &a.username, &a.url, &a.address, &a.notes]
                                .iter()
                                .any(|f| f.to_lowercase().contains(&query))
                    })
                    .map(|a| (here.is_some() && site(&a.url) == here, a.clone()))
                    .collect();
                shown.sort_by_key(|(matched, _)| !matched);
                rsx! {
                    if shown.is_empty() {
                        p { class: "muted", "no accounts" }
                    }
                    ul { class: "list",
                        for (matched, account) in shown {
                            li { key: "{account.id}", class: if matched { "row here" } else { "row" },
                                div { class: "grow",
                                    strong { "{account.service}" }
                                    span { class: "muted", "{account.username}" }
                                    if let Some(note) = account.notes.lines().next().filter(|n| !n.trim().is_empty()) {
                                        span { class: "muted note", "{note}" }
                                    }
                                }
                                if matched && tab.is_some() {
                                    button {
                                        class: "small primary",
                                        onclick: {
                                            let account = account.clone();
                                            let use_login = use_login.clone();
                                            move |_| use_login(account.clone())
                                        },
                                        "fill"
                                    }
                                }
                                button {
                                    class: "small",
                                    onclick: {
                                        let account = account.clone();
                                        let copy = copy.clone();
                                        move |_| copy(account.clone(), false)
                                    },
                                    "user"
                                }
                                button {
                                    class: "small",
                                    onclick: {
                                        let account = account.clone();
                                        let copy = copy.clone();
                                        move |_| copy(account.clone(), true)
                                    },
                                    "pass"
                                }
                            }
                        }
                    }
                }
            }
        }
        footer {
            match notice() {
                Some(Ok(text)) => rsx! { span { class: "ok", "{text}" } },
                Some(Err(text)) => rsx! { span { class: "error", "{text}" } },
                None => rsx! { span {} },
            }
            button { class: "small", onclick: lock, "Lock" }
        }
    }
}

async fn clipboard(text: &str) -> exn::Result<(), ExtError> {
    let window = web_sys::window().ok_or_else(|| exn::Exn::new(ExtError::Clipboard))?;
    JsFuture::from(window.navigator().clipboard().write_text(text))
        .await
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Clipboard)?;
    Ok(())
}
