mod client;
mod create;
mod error;
mod ext;
mod popup;
mod save;

use dioxus::prelude::*;
use exn::Exn;
use syphoria_api::Login;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::{
    client::Client,
    error::ExtError,
    ext::{Message, close_popup},
};

#[wasm_bindgen]
pub fn start() {
    if let Err(e) = console_log::init_with_level(log::Level::Info) {
        web_sys::console::error_1(&e.to_string().into());
    }
    dioxus::web::launch::launch_cfg(popup::Popup, dioxus::web::Config::new());
}

async fn fill(tab: Option<i32>, login: Login) -> Result<(), String> {
    let Some(tab) = tab else {
        return Ok(());
    };
    ext::notify(&Message::Fill {
        tab,
        username: login.username,
        email: login.email,
        password: login.password,
    })
    .await
    .map_err(|e| report(&e))?;
    close_popup();
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
enum Access {
    Loading,
    Setup,
    Locked(Client),
    Ready(Client),
    Failed(String),
}

async fn resolve() -> Access {
    match Client::load().await {
        Ok(client) if client.session.is_some() => Access::Ready(client),
        Ok(client) => Access::Locked(client),
        Err(e) if matches!(*e, ExtError::Setup) => Access::Setup,
        Err(e) => Access::Failed(report(&e)),
    }
}

fn use_access() -> (Signal<Access>, EventHandler<()>) {
    let mut access = use_signal(|| Access::Loading);
    let refresh = use_callback(move |()| {
        spawn(async move {
            access.set(resolve().await);
        });
    });
    use_hook(move || refresh.call(()));
    (access, EventHandler::new(move |()| refresh.call(())))
}

fn report(error: &Exn<ExtError>) -> String {
    log::error!("{error:?}");
    error.to_string()
}

fn check<T>(result: exn::Result<T, ExtError>, on_locked: EventHandler<()>) -> Result<T, String> {
    result.map_err(|e| {
        if matches!(*e, ExtError::Locked) {
            on_locked.call(());
        }
        report(&e)
    })
}

#[component]
fn Unlock(client: Client, on_done: EventHandler<()>) -> Element {
    let mut password = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let server = client.server().to_owned();

    rsx! {
        form {
            class: "stack",
            onsubmit: move |event: FormEvent| {
                event.prevent_default();
                let client = client.clone();
                let typed = password.take();
                busy.set(true);
                error.set(None);
                spawn(async move {
                    match client.unlock(typed).await {
                        Ok(()) => on_done.call(()),
                        Err(e) => error.set(Some(report(&e))),
                    }
                    busy.set(false);
                });
            },
            label { r#for: "password", "Vault password" }
            input {
                id: "password",
                r#type: "password",
                autofocus: true,
                autocomplete: "off",
                value: "{password}",
                oninput: move |e| password.set(e.value()),
            }
            button { r#type: "submit", disabled: busy() || password.read().is_empty(),
                if busy() { "Unlocking…" } else { "Unlock" }
            }
            if let Some(error) = error() {
                p { class: "error", "{error}" }
            }
            p { class: "muted", "{server}" }
        }
    }
}

#[component]
fn Setup(on_done: EventHandler<()>) -> Element {
    let mut server = use_signal(String::new);
    let mut token = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);

    rsx! {
        form {
            class: "stack",
            onsubmit: move |event: FormEvent| {
                event.prevent_default();
                error.set(None);
                spawn(async move {
                    match Client::setup(&server.read(), &token.read()).await {
                        Ok(()) => on_done.call(()),
                        Err(e) => error.set(Some(report(&e))),
                    }
                });
            },
            p { class: "muted", "Create a token in the vault (Accounts → t) over ssh." }
            label { r#for: "server", "Server" }
            input {
                id: "server",
                placeholder: "https://mail.example.com:2222",
                value: "{server}",
                oninput: move |e| server.set(e.value()),
            }
            label { r#for: "token", "API token" }
            input {
                id: "token",
                r#type: "password",
                autocomplete: "off",
                value: "{token}",
                oninput: move |e| token.set(e.value()),
            }
            button { r#type: "submit", disabled: server.read().is_empty() || token.read().is_empty(), "Connect" }
            if let Some(error) = error() {
                p { class: "error", "{error}" }
            }
        }
    }
}
