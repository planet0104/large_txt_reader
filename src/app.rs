use leptos::task::spawn_local;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::console;
use crate::dialog;
use wasm_bindgen_futures::JsFuture;
use js_sys::Promise;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke)]
    fn invoke_promise(cmd: &str, args: JsValue) -> Promise;
}

// 状态管理
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchArgs {
    needle: String,
    ignore_case: bool,
}

#[derive(Serialize, Deserialize)]
struct ReadLinesArgs {
    start: usize,
    count: usize,
}

#[component]
pub fn App() -> impl IntoView {
    // 文件状态
    let (file_path, set_file_path) = signal(String::new());
    let (file_size, set_file_size) = signal(0usize);
    let (total_lines, set_total_lines) = signal(0usize);
    let (file_content, set_file_content) = signal(String::new());
    let (current_line, set_current_line) = signal(0usize);
    // 当前可视内容起始行（用于行号显示）
    let (visible_start, set_visible_start) = signal(0usize);
    
    // 搜索状态
    let (search_query, set_search_query) = signal(String::new());
    // matches: list of match JSON strings returned from backend (each should have line/column/length)
    let (matches_list, set_matches_list) = signal(Vec::<String>::new());
    // simplified per-match line numbers (usize) for quick navigation
    let (matches_lines, set_matches_lines) = signal(Vec::<usize>::new());
    let (current_match_idx, set_current_match_idx) = signal(0usize);
    let (search_info, set_search_info) = signal(String::new());
    let (show_dropdown, set_show_dropdown) = signal(false);

    // Helper: construct a selection callback that will run after content is loaded.
    // Returns `Some(Closure)` when matches_list[idx] contains column/length, otherwise None.
    let make_select_cb = move |matches_snapshot: Vec<String>, idx: usize, start_local: usize, target_line: usize| {
        if let Some(mjs) = matches_snapshot.get(idx).cloned() {
            if !mjs.is_empty() {
                if let Ok(jv) = js_sys::JSON::parse(&mjs) {
                    let column = js_sys::Reflect::get(&jv, &wasm_bindgen::JsValue::from_str("column")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                    let length = js_sys::Reflect::get(&jv, &wasm_bindgen::JsValue::from_str("length")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                    let cb = Closure::wrap(Box::new(move || {
                        if let Some(window) = web_sys::window() {
                            if let Some(doc) = window.document() {
                                if let Some(el) = doc.get_element_by_id("editor-textarea") {
                                    if let Some(textarea) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                                        let content = textarea.value();
                                        let rel_line = if target_line >= start_local { target_line - start_local } else { 0 };
                                        let mut off = 0usize;
                                        let mut cur_line = 0usize;
                                        for l in content.lines() {
                                            if cur_line < rel_line {
                                                off = off.saturating_add(l.chars().count()).saturating_add(1);
                                            } else {
                                                break;
                                            }
                                            cur_line += 1;
                                        }
                                        off = off.saturating_add(column);
                                        let start_sel = off;
                                        let end_sel = off.saturating_add(length);
                                        let _ = textarea.set_selection_start(Some(start_sel as u32));
                                        let _ = textarea.set_selection_end(Some(end_sel as u32));
                                        let _ = textarea.focus();
                                        let line_px = compute_line_pixel("editor-textarea").unwrap_or(18.0);
                                        let scroll_top = (rel_line.saturating_sub(0) as f64 * line_px) as i32;
                                        let he: web_sys::HtmlElement = textarea.clone().unchecked_into();
                                        he.set_scroll_top(scroll_top);
                                        console::log_1(&wasm_bindgen::JsValue::from_str(&format!("select_cb applied (factory): rel_line={}, start={}, end={}", rel_line, start_sel, end_sel)));
                                    }
                                }
                            }
                        }
                    }) as Box<dyn Fn()>);
                    return Some(cb);
                }
            }
        }
        None
    };
    
    // UI 状态
    let (loading, set_loading) = signal(false);
    // 搜索专用 loading 状态：区分 “打开文件” 与 “正在搜索” 两种不同的 loading 文案
    let (searching, set_searching) = signal(false);
    const LINES_PER_PAGE: usize = 30; // 每次加载的行数 (改为以行号为单位)

    // 如果无法测量，可回退到这个值
    const DEFAULT_VISIBLE_LINES: usize = 20;
    // 为避免边界处出现竖向滚动条，保留一个安全行数的余量
    const VISIBLE_SAFETY_MARGIN: usize = 2;

    // 弹窗错误提示的辅助函数
    async fn show_error(message: &str) {
        let _ = dialog::message(message, dialog::MessageOptions { title: Some("错误"), kind: None }).await;
        console::error_1(&wasm_bindgen::JsValue::from_str(message));
    }

    // 格式化字节为 KB/MB 字符串
    fn format_bytes(bytes: usize) -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = KB * 1024.0;
        let b = bytes as f64;
        if b >= MB {
            format!("{:.2} MB", b / MB)
        } else if b >= KB {
            format!("{:.2} KB", b / KB)
        } else {
            format!("{} B", bytes)
        }
    }

    // 安全调用 invoke 的辅助函数：返回 Result 而不是直接 panic
    async fn call_invoke(cmd: &str, args: JsValue) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue> {
        let p = invoke_promise(cmd, args);
        match JsFuture::from(p).await {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        }
    }

    // 打开文件
    let open_file = move |ev| {
        // synchronous debug log to ensure click handler runs
        // removed perf log
        let _ = ev; // keep signature compatible
        spawn_local(async move {
            // removed perf log
            set_loading.set(true);
            // pass extension filters to plugin (allow .txt and .log)
            let args = js_sys::Object::new();
            let ex = serde_wasm_bindgen::to_value(&vec![".txt", ".log"]).unwrap();
            let _ = js_sys::Reflect::set(&args, &wasm_bindgen::JsValue::from_str("extensions"), &ex);
            let res = match call_invoke("plugin:large-file-preview|open_file", wasm_bindgen::JsValue::from(args)).await {
                Ok(v) => v,
                Err(e) => {
                    let em = e.as_string().unwrap_or_else(|| format!("{:?}", e));
                    show_error(&format!("打开文件调用失败：{}", em)).await;
                    set_loading.set(false);
                    return;
                }
            };
                        // removed perf log

            if let Ok(path_val) = js_sys::Reflect::get(&res, &wasm_bindgen::JsValue::from_str("path")) {
                if !path_val.is_undefined() && !path_val.is_null() {
                    if let Some(path) = path_val.as_string() {
                        set_file_path.set(path);
                        // 如果 open_file 返回中带有 size 字段，则直接使用它设置 file_size
                        if let Ok(size_val) = js_sys::Reflect::get(&res, &wasm_bindgen::JsValue::from_str("size")) {
                            if !size_val.is_undefined() && !size_val.is_null() {
                                if let Some(n) = size_val.as_f64() {
                                    set_file_size.set(n as usize);
                                } else if let Some(s) = size_val.as_string() {
                                    if let Ok(parsed) = s.parse::<f64>() {
                                        set_file_size.set(parsed as usize);
                                    }
                                }
                            }
                        }
                        // 初始化可视起始行为 0
                        set_visible_start.set(0);
                        // schedule auto-scroll for filename display after DOM updates
                        schedule_auto_scroll("file-path");
                        // removed perf log
                        
                        // 获取总行数
                        let lines_res = match call_invoke("plugin:large-file-preview|get_total_lines", JsValue::NULL).await {
                            Ok(v) => v,
                            Err(e) => {
                                let em = e.as_string().unwrap_or_else(|| format!("{:?}", e));
                                show_error(&format!("获取总行数失败：{}", em)).await;
                                set_loading.set(false);
                                return;
                            }
                        };
                        // removed perf log

                        if lines_res.is_undefined() || lines_res.is_null() {
                            show_error("获取总行数失败：调用返回空结果").await;
                        } else if let Some(lines) = lines_res.as_f64() {
                            set_total_lines.set(lines as usize);
                            set_current_line.set(0);
                            
                                // 在 DOM 更新后测量编辑框可见行数并加载对应行数，避免出现垂直滚动
                                // 延迟一点时间以等待 textarea 渲染并计算高度
                                {
                                    let set_file_content = set_file_content.clone();
                                    let set_loading = set_loading.clone();
                                    let _ = web_sys::window().map(|w| {
                                        let closure = Closure::wrap(Box::new(move || {
                                            let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                                            // 留出安全边距，避免载入过满导致竖向滚动
                                            let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                                            let to_load = safe.min(LINES_PER_PAGE);
                                            load_content(0, to_load, set_file_content.clone(), set_loading.clone(), None);
                                        }) as Box<dyn Fn()>);
                                        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(closure.as_ref().unchecked_ref(), 120);
                                        closure.forget();
                                    });
                                }

                                // 注册窗口 resize 的防抖处理：在 resize 结束后重新测量并加载可见行数
                                {
                                    if let Some(win) = web_sys::window() {
                                        // 创建防抖 closure（存放在 window.__txt_reader_resize_closure）
                                        let set_file_content = set_file_content.clone();
                                        let set_loading = set_loading.clone();
                                        let resize_closure = Closure::wrap(Box::new(move || {
                                            // 在 resize 事件被触发后延迟 180ms 再测量
                                            if let Some(w2) = web_sys::window() {
                                                let inner = Closure::wrap(Box::new(move || {
                                                    let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                                                    let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                                                    let to_load = safe.min(LINES_PER_PAGE);
                                                    load_content(0, to_load, set_file_content.clone(), set_loading.clone(), None);
                                                }) as Box<dyn Fn()>);
                                                let _ = w2.set_timeout_with_callback_and_timeout_and_arguments_0(inner.as_ref().unchecked_ref(), 180);
                                                inner.forget();
                                            }
                                        }) as Box<dyn Fn()>);

                                        // 将该 closure 赋给 window.__txt_reader_resize_closure 以便 later removal
                                        let _ = js_sys::Reflect::set(&win, &wasm_bindgen::JsValue::from_str("__txt_reader_resize_closure"), resize_closure.as_ref());
                                        // attach to onresize
                                        let _ = win.set_onresize(Some(resize_closure.as_ref().unchecked_ref()));
                                        // leak the closure intentionally (we will remove it reference on close)
                                        resize_closure.forget();
                                    }
                                }
                        } else {
                            show_error(&format!("获取总行数失败：无法解析返回值 {:?}", lines_res.as_string())).await;
                        }
                    } else {
                        // removed perf log
                        show_error("打开文件失败：无法解析文件路径").await;
                    }
                } else {
                    // removed perf log
                    show_error("打开文件失败：返回的 path 字段为空").await;
                }
            } else {
                // removed perf log
                show_error("打开文件失败：未找到 path 字段").await;
            }
            set_loading.set(false);
        });
    };

    // 关闭文件
    let close_file = move |_| {
        spawn_local(async move {
            // removed perf log
            match call_invoke("plugin:large-file-preview|close_file", JsValue::NULL).await {
                Ok(_res) => {
                    // removed perf log
                    // if res.is_undefined() || res.is_null() {
                    //     let _ = dialog::message("关闭文件调用可能失败：返回空结果", dialog::MessageOptions { title: Some("警告"), ..Default::default() }).await;
                    // }
                }
                Err(e) => {
                    let _em = e.as_string().unwrap_or_else(|| format!("{:?}", e));
                    // removed perf log
                    // let _ = dialog::message(&format!("关闭文件调用失败：{}", em), dialog::MessageOptions { title: Some("警告"), ..Default::default() }).await;
                }
            }
            set_file_path.set(String::new());
            set_file_size.set(0);
            // clear auto-scroll when closing
            clear_auto_scroll("file-path");
            // 尝试移除之前注册的 resize handler
            if let Some(win) = web_sys::window() {
                if let Ok(val) = js_sys::Reflect::get(&win, &wasm_bindgen::JsValue::from_str("__txt_reader_resize_closure")) {
                    if val.is_function() {
                        // clear onresize
                        let _ = win.set_onresize(None);
                    }
                    let _ = js_sys::Reflect::delete_property(&win, &wasm_bindgen::JsValue::from_str("__txt_reader_resize_closure"));
                }
            }
            set_file_content.set(String::new());
            set_total_lines.set(0);
            set_current_line.set(0);
            set_search_query.set(String::new());
            set_search_info.set(String::new());
            // removed perf log
        });
    };

    // We no longer perform character-offset selection here. Navigation will jump by line number
    // using `matches_lines` and reusing `load_content` to refresh the editor and scrollbar.

    // previous/next match handlers
    let go_prev_match = move |_: leptos::ev::MouseEvent| {
        // removed perf log
            let matches_list = matches_list.clone();
            let matches_lines = matches_lines.clone();
        let set_idx = set_current_match_idx.clone();
        let set_file_content_clone = set_file_content.clone();
        let set_loading_clone = set_loading.clone();
        spawn_local(async move {
                let len = matches_lines.get_untracked().len();
                if len == 0 {
                    // removed perf log
                    return;
                }
                let mut idx = current_match_idx.get_untracked();
                if idx == 0 { idx = len - 1; } else { idx = idx - 1; }
                set_idx.set(idx);
                let target_line = matches_lines.get_untracked().get(idx).cloned().unwrap_or(0usize);
                // removed perf log
                // set visible start and current_line, then load content for that page
                let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                let context_before: usize = 3;
                let start = if target_line >= context_before { target_line - context_before } else { 0 };
                set_visible_start.set(start);
                set_current_line.set(start);
                let snapshot = matches_list.get_untracked().clone();
                let select_cb_opt = make_select_cb(snapshot, idx, start, target_line);
                load_content(start, safe.min(LINES_PER_PAGE), set_file_content_clone.clone(), set_loading_clone.clone(), select_cb_opt);
        });
    };

    let go_next_match = move |_: leptos::ev::MouseEvent| {
        // removed perf log
            let matches_list = matches_list.clone();
            let matches_lines = matches_lines.clone();
        let set_idx = set_current_match_idx.clone();
        let set_file_content_clone = set_file_content.clone();
        let set_loading_clone = set_loading.clone();
        spawn_local(async move {
                let len = matches_lines.get_untracked().len();
                if len == 0 {
                    // removed perf log
                    return;
                }
                let mut idx = current_match_idx.get_untracked();
                idx = (idx + 1) % len;
                set_idx.set(idx);
                let target_line = matches_lines.get_untracked().get(idx).cloned().unwrap_or(0usize);
                // removed perf log
                let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                let context_before: usize = 3;
                let start = if target_line >= context_before { target_line - context_before } else { 0 };
                set_visible_start.set(start);
                set_current_line.set(start);
                let snapshot = matches_list.get_untracked().clone();
                let select_cb_opt = make_select_cb(snapshot, idx, start, target_line);
                load_content(start, safe.min(LINES_PER_PAGE), set_file_content_clone.clone(), set_loading_clone.clone(), select_cb_opt);
        });
    };

    // 搜索功能
    let search = move |_: leptos::ev::MouseEvent| {
        let query = search_query.get();
        if query.is_empty() {
            return;
        }

        spawn_local(async move {
            set_searching.set(true);
            let args = serde_wasm_bindgen::to_value(&SearchArgs {
                needle: query.clone(),
                ignore_case: true,
            }).unwrap();

            let parsed = match call_invoke("plugin:large-file-preview|mmap_search", args).await {
                Ok(v) => v,
                Err(e) => {
                    let em = e.as_string().unwrap_or_else(|| format!("{:?}", e));
                    show_error(&format!("搜索调用失败：{}", em)).await;
                    set_searching.set(false);
                    return;
                }
            };
            // removed perf log
            // 直接从 JsValue 读取字段
            if parsed.is_undefined() || parsed.is_null() {
                show_error("搜索失败：调用返回空结果").await;
                set_searching.set(false);
                return;
            }
                let count = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("count"))
                    .ok().and_then(|c| c.as_f64()).unwrap_or(0.0) as usize;
            let duration_ms = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("duration_ms"))
                .ok().and_then(|d| d.as_f64()).unwrap_or(0.0) as u128;
            let extra_alloc_bytes = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("extra_alloc_bytes"))
                .ok().and_then(|a| a.as_f64()).unwrap_or(0.0) as usize;

                // parse matches array if present
                // We'll store raw JsValue objects in a Vec<JsValue> via serde_wasm_bindgen::to_value/from_value helpers
                let mut parsed_matches: Vec<wasm_bindgen::JsValue> = Vec::new();
                if let Ok(mv) = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("matches")) {
                    if !mv.is_undefined() && !mv.is_null() {
                        if let Some(arr) = mv.dyn_ref::<js_sys::Array>() {
                            for i in 0..arr.length() {
                                parsed_matches.push(arr.get(i));
                            }
                        }
                    }
                }
                // update matches state (store as JSON strings for simplicity)
                let mut mm_strs: Vec<String> = Vec::new();
                for v in &parsed_matches {
                    let s = js_sys::JSON::stringify(v).ok().and_then(|j| j.as_string()).unwrap_or_default();
                    mm_strs.push(s);
                }
                // If backend didn't return per-match positions but reported a positive count,
                // fall back to repeating the first_match (if available) so navigation buttons work.
                if mm_strs.is_empty() && count > 0 {
                    if let Some(first_match_val) = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("first_match")).ok() {
                        if !first_match_val.is_undefined() && !first_match_val.is_null() {
                            if let Ok(s) = js_sys::JSON::stringify(&first_match_val) {
                                let json_str = s.as_string().unwrap_or_default();
                                let max_dup = count.min(100usize);
                                for _ in 0..max_dup { mm_strs.push(json_str.clone()); }
                                // removed perf log
                            }
                        }
                    }
                }
                set_matches_list.set(mm_strs.clone());
                set_current_match_idx.set(0usize);
                // removed perf log
                
                // 如果有 samples 字段则忽略在 UI 上展示（我们直接在编辑器中定位）
                // Build matches_lines (line numbers) from parsed matches if available
                let mut lines_vec: Vec<usize> = Vec::new();
                if !parsed_matches.is_empty() {
                    for v in &parsed_matches {
                        // v is already a JsValue representing the match object
                        let ln = js_sys::Reflect::get(v, &wasm_bindgen::JsValue::from_str("line")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                        lines_vec.push(ln);
                    }
                }
                // If backend didn't provide per-match positions but has first_match, use it to populate lines
                if lines_vec.is_empty() && count > 0 {
                    if let Some(first_match_val) = js_sys::Reflect::get(&parsed, &wasm_bindgen::JsValue::from_str("first_match")).ok() {
                        if !first_match_val.is_undefined() && !first_match_val.is_null() {
                            if let Some(ln) = js_sys::Reflect::get(&first_match_val, &wasm_bindgen::JsValue::from_str("line")).ok().and_then(|v| v.as_f64()) {
                                let ln_us = ln as usize;
                                let max_dup = count.min(100usize);
                                for _ in 0..max_dup { lines_vec.push(ln_us); }
                                // removed perf log
                            }
                        }
                    }
                }
                // set lines signal
                set_matches_lines.set(lines_vec.clone());
                // if we have at least one line, jump to the first match by line
                if let Some(&first_line) = lines_vec.get(0) {
                    let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                    let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                    let context_before: usize = 3;
                    let start = if first_line >= context_before { first_line - context_before } else { 0 };
                    set_visible_start.set(start);
                    set_current_line.set(start);
                    let snapshot = matches_list.get_untracked().clone();
                    let select_cb_opt = make_select_cb(snapshot, 0usize, start, first_line);
                    load_content(start, safe.min(LINES_PER_PAGE), set_file_content.clone(), set_loading.clone(), select_cb_opt);
                }

                // format duration as seconds with 3 decimals, and extra_alloc in MB with 2 decimals
                let duration_s = (duration_ms as f64) / 1000.0;
                let extra_mb = (extra_alloc_bytes as f64) / 1024.0 / 1024.0;
                set_search_info.set(format!(
                    "{} 个匹配，{:.3} s，额外分配 {:.2} MB",
                    count,
                    duration_s,
                    extra_mb
                ));
                // removed perf log
            // removed perf log
            set_searching.set(false);
        });
    };

    // 加载内容的辅助函数
    fn load_content(
        start_line: usize,
        count: usize,
        set_file_content: WriteSignal<String>,
        set_loading: WriteSignal<bool>,
        on_loaded: Option<wasm_bindgen::prelude::Closure<dyn Fn()>>,
    ) {
        spawn_local(async move {
            let args = serde_wasm_bindgen::to_value(&ReadLinesArgs {
                start: start_line,
                count,
            }).unwrap();
            // removed perf log

            let res = match call_invoke("plugin:large-file-preview|read_lines", args).await {
                Ok(v) => v,
                Err(e) => {
                    let em = e.as_string().unwrap_or_else(|| format!("{:?}", e));
                    show_error(&format!("读取文件内容调用失败：{}", em)).await;
                    set_loading.set(false);
                    return;
                }
            };
            // removed perf log

            // 优先尝试把返回值作为字符串读取并记录长度/预览
                if let Some(content) = res.as_string() {
                set_file_content.set(content);
                // 如果有回调，安排在下一个事件循环 tick 调用（确保 DOM 渲染后执行）
                if let Some(cb) = on_loaded {
                    if let Some(win) = web_sys::window() {
                        // removed perf log
                        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 0);
                        // 将 closure 泄漏以保持其在回调执行前存活；回调内部或浏览器卸载时可自行清理
                        cb.forget();
                    } else {
                        // removed perf warn
                    }
                }
            } else if res.is_undefined() || res.is_null() {
                // removed perf log
                // 保证前端显示为空内容，而不是保留旧内容
                set_file_content.set(String::new());
                // 也提示但不打断流程
                console::warn_1(&wasm_bindgen::JsValue::from_str("读取文件内容失败：调用返回空结果"));
            } else {
                // removed perf log
                // 尝试将其序列化为字符串再展示
                let s = js_sys::JSON::stringify(&res).ok().and_then(|j| j.as_string()).unwrap_or_default();
                // removed perf log
                set_file_content.set(s);
                if let Some(cb) = on_loaded {
                    if let Some(win) = web_sys::window() {
                            // removed perf log
                        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 0);
                        cb.forget();
                    } else {
                            // removed perf warn
                    }
                }
            }
            set_loading.set(false);
        });
    }

    // 如果文件名宽度超出容器宽度，则为其添加自动滚动（marquee）类并设置滚动距离/时长
    fn schedule_auto_scroll(element_id: &str) {
        let id = element_id.to_string();
        let closure = Closure::wrap(Box::new(move || {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    if let Some(el) = document.get_element_by_id(&id) {
                        if let Some(html) = el.dyn_ref::<web_sys::HtmlElement>() {
                            // Find the clipping parent (the immediate parent that hides overflow)
                            let parent = html.parent_element();
                            let scroll_w = html.scroll_width();
                            let client_w = parent.map(|p| p.client_width()).unwrap_or(html.client_width());
                            if scroll_w > client_w {
                                let distance = (scroll_w - client_w) as f64 + 8.0;
                                let duration = (distance / 30.0).max(6.0);
                                let _ = html.style().set_property("--scroll-distance", &format!("{}px", distance));
                                let _ = html.style().set_property("--scroll-duration", &format!("{}s", duration));
                                let _ = el.class_list().add_1("auto-scroll");
                            } else {
                                let _ = el.class_list().remove_1("auto-scroll");
                            }
                        }
                    }
                }
            }
        }) as Box<dyn Fn()>);

        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(closure.as_ref().unchecked_ref(), 150);
        }
        closure.forget();
    }

    // 立即移除自动滚动样式并清理变量
    fn clear_auto_scroll(element_id: &str) {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.get_element_by_id(element_id) {
                    let _ = el.class_list().remove_1("auto-scroll");
                    if let Some(html) = el.dyn_ref::<web_sys::HtmlElement>() {
                        let _ = html.style().remove_property("--scroll-distance");
                        let _ = html.style().remove_property("--scroll-duration");
                    }
                }
            }
        }
    }
    
    view! {
        <div class="app-container">
            // 增加顶部边距
            <div style="height: 12px; display:block;"></div>
            <header class="header" style="display:flex; align-items:center; justify-content:space-between;">
                <h1 class="title">"超大文本查看器"</h1>
                <div class="menu-container" style="margin-left:auto; position:relative;">
                    <button 
                        class="menu-button" 
                        on:click=move |_| set_show_dropdown.set(!show_dropdown.get())
                        aria-label="menu"
                        title="菜单"
                    >
                        <img src="public/menu.svg" alt="menu" width="20" height="20" style="display:block;"/>
                    </button>
                    <Show when=move || show_dropdown.get()>
                        <div class="dropdown-menu" style="position:absolute; right:0; top:100%; margin-top:8px; min-width:220px; background:Canvas; color:CanvasText; border:1px solid ButtonText; box-shadow:0 6px 18px rgba(0,0,0,0.12); padding:8px; border-radius:6px; z-index:1000; color-scheme:light dark;">
                            <button class="menu-item" on:click=move |ev| { open_file(ev); set_show_dropdown.set(false); } style="display:block; width:100%; text-align:left; padding:8px 10px;">
                                "打开"
                            </button>
                            <button class="menu-item" on:click=move |ev| { close_file(ev); set_show_dropdown.set(false); } style="display:block; width:100%; text-align:left; padding:8px 10px; margin-top:6px;">
                                "关闭"
                            </button>
                        </div>
                    </Show>
                </div>
            </header>

            <div class="search-container" style="display:flex; gap:8px; padding:8px;">
                <input
                    type="text"
                    class="search-input"
                    placeholder="输入搜索内容..."
                    prop:value=search_query
                    on:input=move |ev| set_search_query.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            search(leptos::ev::MouseEvent::new("click").unwrap());
                        }
                    }
                    style="flex:1; min-width:0;"
                />
                <button class="search-button" on:click=search disabled=move || loading.get() || searching.get() aria-label="搜索" title="搜索">
                    { move || {
                        // choose icon based on state: loading(opening file) -> loading icon; searching -> loading icon; if matches found -> found icon; otherwise default search icon
                        let src = if loading.get() || searching.get() {
                            "public/search-loading.svg"
                        } else if !matches_list.get().is_empty() {
                            "public/search-found.svg"
                        } else {
                            "public/search.svg"
                        };
                        let blinking = loading.get() || searching.get();
                        let class_str = if blinking { "icon-blink" } else { "" };
                        view! { <img src=src alt="search" width="20" height="20" class=class_str style="display:block;"/> }
                    } }
                </button>
            </div>

            <Show when=move || !search_info.get().is_empty()>
                <div class="search-info" style="font-size:12px; opacity:0.7; display:flex; align-items:center; gap:8px; padding:4px 8px;">
                    <div style="flex:1; min-width:0;">{ move || {
                        let info = search_info.get();
                        let total = matches_list.get().len();
                        let idx = if total==0 { 0 } else { current_match_idx.get() + 1 };
                        if total == 0 {
                            info
                        } else {
                            format!("{} （第 {} / {} 项）", info, idx, total)
                        }
                    } }</div>

                    <div style="display:flex; gap:6px; align-items:center;">
                        <button class="match-nav" on:click=go_prev_match aria-label="prev" style="background:transparent;border:1px solid transparent;padding:6px 8px;border-radius:4px;cursor:pointer;">{ move || "<" }</button>
                        <button class="match-nav" on:click=go_next_match aria-label="next" style="background:transparent;border:1px solid transparent;padding:6px 8px;border-radius:4px;cursor:pointer;">{ move || ">" }</button>
                    </div>
                </div>
            </Show>

            

            <main class="main-content" style="flex:1; display:flex; overflow:hidden;">
                <div class="content-area" style="flex:1; display:flex; flex-direction:column; overflow:hidden;">
                        <div class="file-info">
                            <div style="display:flex; align-items:center; gap:12px; min-width:0;">
                                    <div style="flex:1; min-width:0; overflow:hidden;">
                                        <span id="file-path" style="display:inline-block; white-space:nowrap;">{ move || if file_path.get().is_empty() { "请使用顶部菜单打开一个文本文件".to_string() } else { file_path.get() } }</span>
                                    </div>
                                    <Show when=move || file_size.get() != 0>
                                        <span style="font-weight:700; opacity:0.65; flex:0 0 auto; margin-left:6px;">{ move || format_bytes(file_size.get()) }</span>
                                    </Show>
                                </div>
                        </div>
                            <div style="flex:1; display:flex; align-items:stretch; overflow:hidden;">
                                    <div class="line-numbers" aria-hidden="true">
                                        <pre class="line-numbers-pre">{ move || {
                                            // 根据 visible_start 与当前文件内容行数生成行号
                                            let start = visible_start.get();
                                            let content = file_content.get();
                                            let mut out = String::new();
                                            if file_path.get().is_empty() {
                                                // 未打开文件时显示空白行号区域，行数为可见行数的估计
                                                let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                                                for _ in 0..visible {
                                                    out.push_str("\n");
                                                }
                                            } else {
                                                let lines = content.lines().count().max(1);
                                                for i in 0..lines {
                                                    out.push_str(&format!("{}\n", start + i + 1));
                                                }
                                            }
                                            out
                                        } }</pre>
                                    </div>

                                <textarea
                                    class="content-textarea"
                                    id="editor-textarea"
                                    readonly=true
                                    wrap="off"
                                    prop:value=file_content
                                    on:wheel=move |ev| {
                                        ev.prevent_default();
                                        let dy = ev.delta_y();
                                        let px_per_line = compute_line_pixel("editor-textarea").unwrap_or(18.0);
                                        let lines = (dy / px_per_line).round() as isize;
                                        if lines != 0 {
                                            let cur = current_line.get();
                                            let mut new = if lines > 0 {
                                                cur.saturating_add(lines as usize)
                                            } else {
                                                cur.saturating_sub((-lines) as usize)
                                            };
                                            let max_start = total_lines.get();
                                            if new > max_start { new = max_start; }
                                            set_current_line.set(new);
                                            let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                                            let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                                            set_visible_start.set(new);
                                            load_content(new, safe.min(LINES_PER_PAGE), set_file_content.clone(), set_loading.clone(), None);
                                        }
                                    }
                                    style="flex:1; width:100%; resize:none; white-space:pre; overflow:auto;"
                                ></textarea>

                                <div class="editor-scrollbar" style="width:40px; display:flex; align-items:stretch; justify-content:center; padding:4px;">
                                    <input
                                        type="range"
                                        class="scrollbar"
                                        min=0
                                        max=move || total_lines.get() as i32
                                        // Slider maps directly: 0 (top) -> first line, max -> last line
                                        prop:value=move || current_line.get() as i32
                                        disabled=move || file_path.get().is_empty() || total_lines.get() == 0
                                        on:input=move |ev| {
                                            if let Ok(v) = event_target_value(&ev).parse::<f64>() {
                                                let mut raw = v as isize;
                                                if raw < 0 { raw = 0; }
                                                let raw = raw as usize;
                                                // raw is the new current_line (page top)
                                                let line = raw.min(total_lines.get());
                                                set_current_line.set(line);
                                                // 计算当前编辑器可见行数，并加载以 line 为顶部的内容
                                                let visible = compute_visible_lines("editor-textarea").unwrap_or(DEFAULT_VISIBLE_LINES);
                                                let safe = visible.saturating_sub(VISIBLE_SAFETY_MARGIN).max(1);
                                                let start = line;
                                                set_visible_start.set(start);
                                                load_content(start, safe.min(LINES_PER_PAGE), set_file_content.clone(), set_loading.clone(), None);
                                            }
                                        }
                                        aria-orientation="vertical"
                                        style="writing-mode:vertical-rl; -webkit-appearance: slider-vertical; -webkit-transform-origin:center; transform-origin:center;"
                                    />
                                </div>
                            </div>
                </div>
                
            </main>
        </div>
    }
}

    // 计算可见行数：读取 textarea 的高度和计算的 line-height
    fn compute_visible_lines(element_id: &str) -> Option<usize> {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.get_element_by_id(element_id) {
                    if let Some(html) = el.dyn_ref::<web_sys::HtmlElement>() {
                        // 获取高度（clientHeight 包含 padding）
                        let mut height = html.client_height() as f64;
                        // 尝试读取计算样式的 line-height 与 padding
                        if let Ok(style) = window.get_computed_style(&el) {
                            if let Some(style) = style {
                                // 读取 padding-top / padding-bottom 并从高度中剔除
                                if let Ok(pad_top) = style.get_property_value("padding-top") {
                                    if pad_top.ends_with("px") {
                                        if let Ok(v) = pad_top[..pad_top.len()-2].trim().parse::<f64>() {
                                            height = (height - v).max(0.0);
                                        }
                                    }
                                }
                                if let Ok(pad_bot) = style.get_property_value("padding-bottom") {
                                    if pad_bot.ends_with("px") {
                                        if let Ok(v) = pad_bot[..pad_bot.len()-2].trim().parse::<f64>() {
                                            height = (height - v).max(0.0);
                                        }
                                    }
                                }

                                if let Ok(line_height_val) = style.get_property_value("line-height") {
                                    // line-height 可能为 "20px" 或 "normal"
                                    if line_height_val.ends_with("px") {
                                        if let Ok(v) = line_height_val[..line_height_val.len()-2].trim().parse::<f64>() {
                                            if v > 0.0 {
                                                let count = (height / v).floor() as usize;
                                                return Some(count.max(1));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // 如果无法读取计算样式，尝试从内联样式获取 font-size 与 line-height 估算
                        // 最后回退为一个粗略估计：每行 18px
                        let estimated = (height / 18.0).floor() as usize;
                        return Some(estimated.max(1));
                    }
                }
            }
        }
        None
    }

    // 计算每行大约占用的像素高度（用于将滚轮/触摸位移转换为行数）
    fn compute_line_pixel(element_id: &str) -> Option<f64> {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.get_element_by_id(element_id) {
                    if let Some(_html) = el.dyn_ref::<web_sys::HtmlElement>() {
                        let mut line_px = 18.0f64; // 默认估计
                        if let Ok(style) = window.get_computed_style(&el) {
                            if let Some(style) = style {
                                if let Ok(line_height_val) = style.get_property_value("line-height") {
                                    if line_height_val.ends_with("px") {
                                        if let Ok(v) = line_height_val[..line_height_val.len()-2].trim().parse::<f64>() {
                                            if v > 0.0 { line_px = v; }
                                        }
                                    }
                                }
                            }
                        }
                        return Some(line_px);
                    }
                }
            }
        }
        None
    }
