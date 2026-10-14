use exn::ResultExt;
use js_sys::{Object, Reflect};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_wasm_bindgen::Serializer;
use wasm_bindgen::JsValue;
use web_extensions_sys::{StorageArea, chrome};

use crate::error::{ExtError, JsFailure};

pub const SERVER: &str = "server";
pub const TOKEN: &str = "token";

fn local() -> StorageArea {
    chrome().storage().local()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Message {
    Context,
    Fill {
        tab: i32,
        username: String,
        email: String,
        password: String,
    },
    Discard {
        tab: i32,
    },
    Session,
    SetSession {
        session: String,
    },
    ClearSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    Login,
    Signup,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Context {
    pub tab: i32,
    pub url: String,
    pub form: Option<Form>,
    pub pending: Option<Pending>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pending {
    pub url: String,
    pub username: String,
    pub password: String,
}

pub async fn load(key: &str) -> exn::Result<Option<String>, ExtError> {
    let values = local()
        .get(&JsValue::from_str(key))
        .await
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Storage)?;
    let value = Reflect::get(&values, &JsValue::from_str(key))
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Storage)?;
    Ok(value.as_string().filter(|v| !v.is_empty()))
}

pub async fn store(key: &str, value: &str) -> exn::Result<(), ExtError> {
    let entry = Object::new();
    Reflect::set(&entry, &JsValue::from_str(key), &JsValue::from_str(value))
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Storage)?;
    local()
        .set(&entry)
        .await
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Storage)?;
    Ok(())
}

pub async fn remove(keys: &[&str]) -> exn::Result<(), ExtError> {
    let list: js_sys::Array = keys.iter().map(|k| JsValue::from_str(k)).collect();
    local()
        .remove(&list)
        .await
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Storage)?;
    Ok(())
}

pub async fn send<T: DeserializeOwned>(message: &Message) -> exn::Result<T, ExtError> {
    let value = message
        .serialize(&Serializer::json_compatible())
        .or_raise(|| ExtError::Message)?;
    let reply = chrome()
        .runtime()
        .send_message(None, &value, None)
        .await
        .map_err(JsFailure::from)
        .or_raise(|| ExtError::Message)?;
    serde_wasm_bindgen::from_value(reply).or_raise(|| ExtError::Message)
}

pub async fn notify(message: &Message) -> exn::Result<(), ExtError> {
    send::<Option<()>>(message).await.map(drop)
}

pub async fn context() -> exn::Result<Option<Context>, ExtError> {
    send(&Message::Context).await
}

pub fn close_popup() {
    if let Some(window) = web_sys::window()
        && let Err(e) = window.close()
    {
        log::error!("failed to close popup: {e:?}");
    }
}
