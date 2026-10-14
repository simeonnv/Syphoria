use wasm_bindgen::JsValue;

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error("set up the server and token first")]
    Setup,
    #[error("vault locked")]
    Locked,
    #[error("{0}")]
    Server(String),
    #[error("server unreachable")]
    Network,
    #[error("unexpected server response")]
    Decode,
    #[error("invalid server url")]
    Url,
    #[error("browser storage failed")]
    Storage,
    #[error("extension messaging failed")]
    Message,
    #[error("clipboard unavailable")]
    Clipboard,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct JsFailure(String);

impl From<JsValue> for JsFailure {
    fn from(value: JsValue) -> Self {
        let text = js_sys::Reflect::get(&value, &JsValue::from_str("message"))
            .ok()
            .and_then(|m| m.as_string())
            .or_else(|| value.as_string())
            .unwrap_or_else(|| format!("{value:?}"));
        Self(text)
    }
}
