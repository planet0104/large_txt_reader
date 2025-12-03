use leptos::prelude::*;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use js_sys::Promise;
use serde_wasm_bindgen::to_value;
// use web_sys::console; // not used directly

/// Usage examples:
///
/// Async/await style (inside an async context):
///
/// let ok = dialog::ask("Are you sure?", dialog::MessageOptions { title: Some("Tauri"), kind: None }).await?;
///
/// Callback style (useful in Leptos component handlers):
///
/// dialog::open_with_callback(dialog::OpenOptions { multiple: Some(false), directory: Some(false) }, move |paths| {
///     // paths: Option<Vec<String>>
/// });
///
/// To check locally in PowerShell (from project root):
///
/// cargo +nightly wasm-pack build --target web
///
/// Or for cargo-leptos frontend workflow, run your normal wasm build used by the project.

// 说明: 绑定到 `window.__TAURI__.plugins.dialog` 下的函数。
// 我们将绑定为返回 `Promise` 的 JS 函数，然后在 Rust 端通过 `JsFuture` await。

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = ask)]
    fn ask_raw(message: &str, options: JsValue) -> Promise;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = confirm)]
    fn confirm_raw(message: &str, options: JsValue) -> Promise;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = message)]
    fn message_raw(message: &str, options: JsValue) -> Promise;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = open)]
    fn open_raw(options: JsValue) -> Promise;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = save)]
    fn save_raw(options: JsValue) -> Promise;
}

// 辅助序列化选项的结构体
#[derive(Serialize, Default)]
pub struct MessageOptions<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'a str>,
}

#[derive(Serialize, Default)]
pub struct OpenOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiple: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<bool>,
}

#[derive(Serialize)]
pub struct SaveFilter<'a> {
    pub name: &'a str,
    pub extensions: &'a [&'a str],
}

#[derive(Serialize, Default)]
pub struct SaveOptions<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<&'a [SaveFilter<'a>]>,
}

// ----------------- 高层安全封装（Leptos 友好） -----------------

pub async fn ask(message: &str, options: MessageOptions<'_>) -> Result<bool, JsValue> {
    let js_opts = to_value(&options).map_err(|e| JsValue::from_str(&format!("serde-wasm error: {}", e)))?;
    let p = ask_raw(message, js_opts);
    let v = JsFuture::from(p).await?;
    Ok(v.as_bool().unwrap_or(false))
}

pub async fn confirm(message: &str, options: MessageOptions<'_>) -> Result<bool, JsValue> {
    let js_opts = to_value(&options).map_err(|e| JsValue::from_str(&format!("serde-wasm error: {}", e)))?;
    let p = confirm_raw(message, js_opts);
    let v = JsFuture::from(p).await?;
    Ok(v.as_bool().unwrap_or(false))
}

pub async fn message(message: &str, options: MessageOptions<'_>) -> Result<bool, JsValue> {
    let js_opts = to_value(&options).map_err(|e| JsValue::from_str(&format!("serde-wasm error: {}", e)))?;
    let p = message_raw(message, js_opts);
    let v = JsFuture::from(p).await?;
    Ok(v.as_bool().unwrap_or(false))
}

pub fn ask_with_callback<F>(message: &str, options: MessageOptions<'_>, cb: F)
where
    F: 'static + FnOnce(bool),
{
    let message_owned = message.to_string();
    let js_opts = match to_value(&options) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(&JsValue::from_str(&format!("serde-wasm error: {}", e)));
            cb(false);
            return;
        }
    };

    wasm_bindgen_futures::spawn_local(async move {
        // call ask via raw binding using JsValue options
        let p = ask_raw(&message_owned, js_opts);
        match JsFuture::from(p).await {
            Ok(v) => cb(v.as_bool().unwrap_or(false)),
            Err(e) => {
                web_sys::console::error_1(&e);
                cb(false)
            }
        }
    });
}

pub fn confirm_with_callback<F>(message: &str, options: MessageOptions<'_>, cb: F)
where
    F: 'static + FnOnce(bool),
{
    let message_owned = message.to_string();
    let js_opts = match to_value(&options) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(&JsValue::from_str(&format!("serde-wasm error: {}", e)));
            cb(false);
            return;
        }
    };

    wasm_bindgen_futures::spawn_local(async move {
        let p = confirm_raw(&message_owned, js_opts);
        match JsFuture::from(p).await {
            Ok(v) => cb(v.as_bool().unwrap_or(false)),
            Err(e) => {
                web_sys::console::error_1(&e);
                cb(false)
            }
        }
    });
}

pub fn message_with_callback<F>(message: &str, options: MessageOptions<'_>, cb: F)
where
    F: 'static + FnOnce(bool),
{
    let message_owned = message.to_string();
    let js_opts = match to_value(&options) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(&JsValue::from_str(&format!("serde-wasm error: {}", e)));
            cb(false);
            return;
        }
    };

    wasm_bindgen_futures::spawn_local(async move {
        let p = message_raw(&message_owned, js_opts);
        match JsFuture::from(p).await {
            Ok(v) => cb(v.as_bool().unwrap_or(false)),
            Err(e) => {
                web_sys::console::error_1(&e);
                cb(false)
            }
        }
    });
}

pub async fn open(options: OpenOptions) -> Result<Option<Vec<String>>, JsValue> {
    let js_opts = to_value(&options).map_err(|e| JsValue::from_str(&format!("serde-wasm error: {}", e)))?;
    let p = open_raw(js_opts);
    let v = JsFuture::from(p).await?;

    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }

    // 返回可能是单个字符串或字符串数组
    if let Some(s) = v.as_string() {
        return Ok(Some(vec![s]));
    }

    // 尝试将其作为数组解析
    let arr = js_sys::Array::from(&v);
    let mut out = Vec::new();
    for i in 0..arr.length() {
        let item = arr.get(i);
        if let Some(s) = item.as_string() {
            out.push(s);
        }
    }

    Ok(Some(out))
}

pub async fn save<'a>(options: SaveOptions<'a>) -> Result<Option<String>, JsValue> {
    let js_opts = to_value(&options).map_err(|e| JsValue::from_str(&format!("serde-wasm error: {}", e)))?;
    let p = save_raw(js_opts);
    let v = JsFuture::from(p).await?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    Ok(v.as_string())
}

// 回调风格封装，便于在 Leptos 组件中直接传入 closure
pub fn open_with_callback<F>(options: OpenOptions, cb: F)
where
    F: 'static + FnOnce(Option<Vec<String>>),
{
    wasm_bindgen_futures::spawn_local(async move {
        match open(options).await {
            Ok(paths) => cb(paths),
            Err(err) => {
                web_sys::console::error_1(&err);
                cb(None)
            }
        }
    });
}

pub fn save_with_callback<'a, F>(options: SaveOptions<'a>, cb: F)
where
    F: 'static + FnOnce(Option<String>),
{
    // serialize options to JsValue now so we don't capture borrowed data into the 'static async
    let js_opts = match to_value(&options) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(&JsValue::from_str(&format!("serde-wasm error: {}", e)));
            cb(None);
            return;
        }
    };

    wasm_bindgen_futures::spawn_local(async move {
        let p = save_raw(js_opts);
        match JsFuture::from(p).await {
            Ok(v) => cb(v.as_string()),
            Err(err) => {
                web_sys::console::error_1(&err);
                cb(None)
            }
        }
    });
}
