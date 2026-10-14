use dioxus::prelude::*;
use syphoria_api::{Capture, site};

use crate::{
    check,
    client::Client,
    ext::{self, Message, Pending},
    report,
};

#[component]
pub fn Save(
    client: Client,
    tab: i32,
    pending: Pending,
    on_done: EventHandler<()>,
    on_locked: EventHandler<()>,
) -> Element {
    let request = Capture {
        url: pending.url.clone(),
        username: pending.username.clone(),
        password: pending.password.clone(),
    };
    let checked = client.clone();
    let probe = request.clone();
    let known = use_resource(move || {
        let client = checked.clone();
        let capture = probe.clone();
        async move { check(client.known(&capture).await, on_locked) }
    });
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let host = site(&pending.url).unwrap_or_else(|| pending.url.clone());

    let finish = move || async move {
        if let Err(e) = ext::notify(&Message::Discard { tab }).await {
            report(&e);
        }
        on_done.call(());
    };

    use_effect(move || {
        if let Some(Ok(true)) = &*known.read() {
            spawn(finish());
        }
    });

    rsx! {
        match &*known.read() {
            None | Some(Ok(true)) => rsx! {},
            Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
            Some(Ok(false)) => rsx! {
                section { class: "card",
                    p { "Save login for " strong { "{host}" } "?" }
                    p { class: "muted", "{pending.username}" }
                    div { class: "actions",
                        button {
                            disabled: busy(),
                            onclick: {
                                let client = client.clone();
                                let request = request.clone();
                                move |_| {
                                    let client = client.clone();
                                    let request = request.clone();
                                    busy.set(true);
                                    async move {
                                        match check(client.save(&request).await, on_locked) {
                                            Ok(()) => finish().await,
                                            Err(e) => error.set(Some(e)),
                                        }
                                        busy.set(false);
                                    }
                                }
                            },
                            "Save"
                        }
                        button { class: "secondary", onclick: move |_| finish(), "Not now" }
                    }
                    if let Some(e) = error() {
                        p { class: "error", "{e}" }
                    }
                }
            },
        }
    }
}
