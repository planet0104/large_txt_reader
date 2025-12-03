use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use memmap2::{Mmap, MmapOptions};
use log::{info, warn, error};
use tauri::{Runtime};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use once_cell::sync::Lazy;
#[cfg(target_os = "android")]
use tauri_plugin_android_fs::{AndroidFsExt, FileUri};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use smol::lock::Mutex as AsyncMutex;
use anyhow::Result;
use std::io::Read;
use std::path::Path;
// memchr may be useful later for fast byte searches; not required here currently

// 最大单行字节数（6MB）——超过该长度的单行在读取时将被截断
const MAX_LINE_BYTES: usize = 6 * 1024 * 1024;

#[cfg(not(target_os = "android"))]
use rfd::AsyncFileDialog;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingRequest {
  pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResponse {
  pub value: Option<String>,
}


#[derive(Clone)]
/// 大文件预览辅助结构，用于高效读取文件特定行段和基于 mmap 的快速搜索。
pub struct LargeFilePreview {
    /// 打开的文件路径
    pub path: PathBuf,
    /// 文件的总行数（打开时统计）
    pub total_lines: usize,
    /// 每隔 `index_interval` 行记录一次字节偏移，便于快速跳转
    pub index: Vec<u64>,
    /// 索引间隔（行数）
    pub index_interval: usize,
    /// 缓存最近创建的 mmap 窗口：(`aligned_offset`, `len`, `mmap`)
    pub cached_window: Arc<StdMutex<Option<(u64, usize, Mmap)>>>,
    /// 复用的已打开文件句柄（用于 mmap 和 BufReader）
    pub file_handle: Arc<std::fs::File>,
}

impl LargeFilePreview {
    pub fn open(path: PathBuf) -> Result<Self> {
        info!("LargeFilePreview::open - attempting to open file: {:?}", path);
        let mut opts = OpenOptions::new();
        opts.read(true);
        #[cfg(windows)]
        {
            opts.share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004);
        }
        let file = opts.open(&path)?;
        info!("LargeFilePreview::open - opened file handle OK");
        let file_arc = Arc::new(file);
        // 使用分块读取以避免在遇到极长单行时分配过大缓冲区
        let mut reader = file_arc.as_ref().try_clone()?;
        let mut total = 0usize;
        let mut index: Vec<u64> = Vec::new();
        // 默认每 1000 行记录一次索引，减少内存占用并提高随机访问效率
        let index_interval = 1000usize;
        let mut buf = vec![0u8; 64 * 1024]; // 64KB 缓冲
        let mut rem: Vec<u8> = Vec::new();
        let mut pos = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                if !rem.is_empty() {
                    total += 1;
                    pos += rem.len() as u64;
                    if total % index_interval == 0 {
                        index.push(pos);
                    }
                }
                break;
            }
            let mut start = 0usize;
            for i in 0..n {
                if buf[i] == b'\n' {
                    // 收集行数据长度
                    let part_len = i + 1 - start;
                    let line_len = rem.len() + part_len;
                    // 如果单行超过 MAX_LINE_BYTES，则按限制计算位置并丢弃多余字节
                    if line_len > MAX_LINE_BYTES {
                        // 将 pos 增加到截断后的位置（只计算 MAX_LINE_BYTES）
                        pos += MAX_LINE_BYTES as u64;
                    } else {
                        pos += line_len as u64;
                    }
                    total += 1;
                    if total % index_interval == 0 {
                        index.push(pos);
                    }
                    rem.clear();
                    start = i + 1;
                }
            }
            // 处理未结束的行残余
            if start < n {
                rem.extend_from_slice(&buf[start..n]);
                // 防止 rem 无限增长（单行超长），当超过阈值时丢弃超过部分
                if rem.len() > MAX_LINE_BYTES {
                    // 我们只保留 MAX_LINE_BYTES 的计数信息，不保留全部内容
                    pos += (rem.len() - MAX_LINE_BYTES) as u64;
                    rem.truncate(MAX_LINE_BYTES);
                }
            }
        }
        info!("LargeFilePreview::open - finished scanning file. total_lines={}, index.len()={} ", total, index.len());
        Ok(Self {
            path,
            total_lines: total,
            index,
            index_interval,
            cached_window: Arc::new(StdMutex::new(None)),
            file_handle: file_arc,
        })
    }

    #[cfg(unix)]
    /// Create a LargeFilePreview from a native file descriptor (Android case).
    pub fn open_from_fd(fd: i32, path_hint: PathBuf) -> Result<Self> {
        use std::os::unix::io::FromRawFd;
        // Safety: take ownership of fd; caller must ensure fd was detached and not used elsewhere
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let file_arc = Arc::new(file);
        let mut reader = file_arc.as_ref().try_clone()?;
        let mut total = 0usize;
        let mut index: Vec<u64> = Vec::new();
        let index_interval = 1000usize;
        let mut buf = vec![0u8; 64 * 1024];
        let mut rem: Vec<u8> = Vec::new();
        let mut pos = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                if !rem.is_empty() {
                    total += 1;
                    pos += rem.len() as u64;
                    if total % index_interval == 0 {
                        index.push(pos);
                    }
                }
                break;
            }
            let mut start = 0usize;
            for i in 0..n {
                if buf[i] == b'\n' {
                    let part_len = i + 1 - start;
                    let line_len = rem.len() + part_len;
                    if line_len > MAX_LINE_BYTES {
                        pos += MAX_LINE_BYTES as u64;
                    } else {
                        pos += line_len as u64;
                    }
                    total += 1;
                    if total % index_interval == 0 {
                        index.push(pos);
                    }
                    rem.clear();
                    start = i + 1;
                }
            }
            if start < n {
                rem.extend_from_slice(&buf[start..n]);
                if rem.len() > MAX_LINE_BYTES {
                    pos += (rem.len() - MAX_LINE_BYTES) as u64;
                    rem.truncate(MAX_LINE_BYTES);
                }
            }
        }
        Ok(Self {
            path: path_hint,
            total_lines: total,
            index,
            index_interval,
            cached_window: Arc::new(StdMutex::new(None)),
            file_handle: file_arc,
        })
    }

    /// 返回已统计的总行数（open 时计算）
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// 异步读取从 `start` 行开始的 `count` 行文本。
    ///
    /// 实现要点：优先尝试使用 mmap 窗口进行切片读取以提升性能；失败时回退到 `BufReader` 顺序读取。
    /// - `start`: 起始行（0 基准）
    /// - `count`: 要读取的行数
    /// 返回读取到的多行字符串，每行以 `\n` 结尾（如果文件末尾不足则返回实际行数）。
    pub async fn read_lines(&self, start: usize, count: usize) -> Result<String> {
        let index = self.index.clone();
        let index_interval = self.index_interval;
        let cache = self.cached_window.clone();
        let file_handle = self.file_handle.clone();
        smol::unblock(move || -> Result<String> {
            let file = file_handle.as_ref().try_clone()?;
            let pos_idx = start / index_interval;
            let (base_offset, base_line) = if pos_idx == 0 {
                (0u64, 0usize)
            } else {
                let idx = pos_idx.saturating_sub(1);
                if idx < index.len() {
                    (index[idx], pos_idx * index_interval)
                } else {
                    (0u64, 0usize)
                }
            };

            // 计算 mmap 映射窗口（以页对齐）以尝试零拷贝读取
            let page_size = 4096usize;
            let estimated_line_len = 120usize;
            let desired_lines = count + index_interval;
            let desired_bytes = desired_lines.saturating_mul(estimated_line_len);
            let aligned = (base_offset / page_size as u64) * page_size as u64;
            let delta = (base_offset.saturating_sub(aligned)) as usize;
            let mut map_len = delta.saturating_add(desired_bytes);
            let cap = 8 * 1024 * 1024usize;
            if map_len > cap {
                map_len = cap;
            }

            // 尝试复用缓存的 mmap 窗口以减少系统调用和重新映射
            if map_len > 0 {
                if let Ok(guard) = cache.lock() {
                    if let Some((cached_aligned, cached_len, mmap)) = &*guard {
                        let cached_start = *cached_aligned;
                        let cached_end = cached_start + (*cached_len as u64);
                        if base_offset >= cached_start && (base_offset + map_len as u64) <= cached_end {
                            let delta2 = (base_offset - cached_start) as usize;
                            let slice = &mmap[delta2..];
                            let text = String::from_utf8_lossy(slice);
                            let mut iter = text.lines();
                            let skip = start.saturating_sub(base_line);
                            let mut ok = true;
                            for _ in 0..skip {
                                if iter.next().is_none() {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                let mut out = String::new();
                                for _ in 0..count {
                                    if let Some(l) = iter.next() {
                                        if l.as_bytes().len() > MAX_LINE_BYTES {
                                            out.push_str(&String::from_utf8_lossy(&l.as_bytes()[..MAX_LINE_BYTES]));
                                            out.push('\n');
                                            break;
                                        } else {
                                            out.push_str(l);
                                            out.push('\n');
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                return Ok(out);
                            }
                        }
                    }
                }

                
                // 在尝试 mmap 前，基于文件真实长度裁剪 map_len，避免映射越界引发 SIGBUS
                let file_len = match file.metadata() {
                    Ok(m) => m.len(),
                    Err(e) => {
                        warn!("read_lines - failed to read file metadata for mmap clipping: {}", e);
                        0u64
                    }
                };
                

                if aligned >= file_len {
                } else {
                    let max_map = (file_len - aligned) as usize;
                    if map_len > max_map {
                        map_len = max_map;
                    }
                    if map_len > 0 {
                        // 创建新的 mmap 窗口并缓存，随后尝试用它读取需要的行
                        let mmap_res = unsafe { MmapOptions::new().offset(aligned).len(map_len).map(&file) };
                        match mmap_res {
                            Ok(mmap) => {
                                if let Ok(mut guard) = cache.lock() {
                                    *guard = Some((aligned, map_len, mmap));
                                }
                                if let Ok(guard2) = cache.lock() {
                                    if let Some((cached_aligned, _cached_len, mmap2)) = &*guard2 {
                                        let delta2 = (base_offset.saturating_sub(*cached_aligned)) as usize;
                                        let slice = &mmap2[delta2..];
                                        let text = String::from_utf8_lossy(slice);
                                        let mut iter = text.lines();
                                        let skip = start.saturating_sub(base_line);
                                        let mut ok = true;
                                        for _ in 0..skip {
                                            if iter.next().is_none() {
                                                ok = false;
                                                break;
                                            }
                                        }
                                        if ok {
                                            let mut out = String::new();
                                            for _ in 0..count {
                                                if let Some(l) = iter.next() {
                                                    if l.as_bytes().len() > MAX_LINE_BYTES {
                                                        out.push_str(&String::from_utf8_lossy(&l.as_bytes()[..MAX_LINE_BYTES]));
                                                        out.push('\n');
                                                        break;
                                                    } else {
                                                        out.push_str(l);
                                                        out.push('\n');
                                                    }
                                                } else {
                                                    break;
                                                }
                                            }
                                            return Ok(out);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("read_lines - mmap failed after clipping: offset={}, len={}, file_len={}, err={}", aligned, map_len, file_len, e);
                            }
                        }
                    }
                }
            }

            // 回退：使用 BufReader 顺序读取，保证在任意情况下都能返回结果
            let mut reader = BufReader::new(file);
            if base_offset > 0 {
                reader.seek(std::io::SeekFrom::Start(base_offset))?;
            }
            let mut cur = base_line;
            while cur < start {
                let mut tmp: Vec<u8> = Vec::new();
                if reader.read_until(b'\n', &mut tmp)? == 0 {
                    break;
                }
                cur += 1;
            }
            let mut out = String::new();
            for _ in 0..count {
                let mut tmp: Vec<u8> = Vec::new();
                if reader.read_until(b'\n', &mut tmp)? == 0 {
                    break;
                }
                // 截断过长的单行，防止内存溢出
                if tmp.len() > MAX_LINE_BYTES {
                    out.push_str(&String::from_utf8_lossy(&tmp[..MAX_LINE_BYTES]));
                    out.push('\n');
                    break;
                } else {
                    let s = String::from_utf8_lossy(&tmp);
                    out.push_str(&s);
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            Ok(out)
        })
        .await
    }

    /// 在整个文件上使用 mmap 执行字节级别的快速搜索。
    ///
    /// - `needle`: 要搜索的字节序列（通常为 UTF-8 字符串的 `.as_bytes()`）。
    /// - `ignore_case`: 是否忽略大小写（会为整个文件分配额外缓冲区）。
    /// 返回 `(match_count, samples, duration, extra_alloc_bytes, first_match)`，其中 `first_match` 为可选的 `(line, col_chars, match_len_chars)`。
    pub fn mmap_search(
        &self,
        needle: &[u8],
        ignore_case: bool,
    ) -> std::io::Result<(
        usize,
        Vec<String>,
        std::time::Duration,
        usize,
        Option<(usize, usize, usize)>,
        Vec<serde_json::Value>,
    )> {
        use memchr::memmem;
        use memmap2::Mmap;
        use std::time::Instant;

        let f = self.file_handle.as_ref().try_clone()?;
        // report file metadata for debugging and guard zero-length files
        let file_len = match f.metadata() {
            Ok(m) => {
                info!("mmap_search - file metadata: len={}, is_file={}", m.len(), m.is_file());
                m.len()
            }
            Err(e) => {
                warn!("mmap_search - failed to read metadata: {}", e);
                0u64
            }
        };
        let start_time = Instant::now();
        info!("mmap_search - needle_len={}, ignore_case={}, file_len={}", needle.len(), ignore_case, file_len);

        if file_len == 0 {
            return Ok((0usize, Vec::new(), start_time.elapsed(), 0usize, None, Vec::new()));
        }

        let mmap = unsafe { Mmap::map(&f)? };
        let hay_orig = &mmap[..];

        let mut extra_alloc = 0usize;
        let (hay, needle_used): (std::borrow::Cow<[u8]>, Vec<u8>) = if ignore_case {
            let lowered: Vec<u8> = hay_orig.iter().map(|b| b.to_ascii_lowercase()).collect();
            extra_alloc = lowered.len();
            let n = needle
                .iter()
                .map(|b| b.to_ascii_lowercase())
                .collect::<Vec<u8>>();
            (std::borrow::Cow::Owned(lowered), n)
        } else {
            (std::borrow::Cow::Borrowed(hay_orig), needle.to_vec())
        };

        let mut count = 0usize;
        let mut samples = Vec::new();
        let mut matches_pos: Vec<serde_json::Value> = Vec::new();
        let max_matches_return = 1000usize;
        let mut start = 0usize;
        let mut first_match: Option<(usize, usize, usize)> = None;
        // 遍历所有匹配位置，收集样例行并记录第一次匹配的行/列信息
        while let Some(pos) = memmem::find(&hay[start..], &needle_used) {
            let abs = start + pos;
            if first_match.is_none() {
                let ln = hay[..abs].iter().filter(|&&b| b == b'\n').count();
                let line_start = hay[..abs]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let col_chars = std::str::from_utf8(&hay_orig[line_start..abs])
                    .map(|s| s.chars().count())
                    .unwrap_or(0usize);
                let match_len_chars = std::str::from_utf8(&needle_used)
                    .map(|s| s.chars().count())
                    .unwrap_or(needle_used.len());
                first_match = Some((ln, col_chars, match_len_chars));
            }
            // record this match's position (line, column, length) up to the configured cap
            if matches_pos.len() < max_matches_return {
                let ln = hay[..abs].iter().filter(|&&b| b == b'\n').count();
                let line_start = hay[..abs]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let col_chars = std::str::from_utf8(&hay_orig[line_start..abs])
                    .map(|s| s.chars().count())
                    .unwrap_or(0usize);
                let match_len_chars = std::str::from_utf8(&needle_used)
                    .map(|s| s.chars().count())
                    .unwrap_or(needle_used.len());
                matches_pos.push(json!({"line": ln, "column": col_chars, "length": match_len_chars}));
            }
            let line_start = hay[..abs]
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let line_end = hay[abs..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| abs + p)
                .unwrap_or(hay.len());
            if let Ok(s) = std::str::from_utf8(&hay_orig[line_start..line_end]) {
                if samples.len() < 5 {
                    samples.push(s.to_string());
                }
            }
            count += 1;
            start = abs + needle_used.len();
        }

        let dur = start_time.elapsed();
        Ok((count, samples, dur, extra_alloc, first_match, matches_pos))
    }
}

// 定义返回给前端的结果结构体
#[derive(Serialize)]
pub struct FileInfo {
    pub uri: String,
    // 或者其他元数据，如文件大小等
}

// 全局缓存用于存储打开的 LargeFilePreview
static LARGE_FILE_PREVIEW: Lazy<Arc<AsyncMutex<Option<LargeFilePreview>>>> = 
    Lazy::new(|| Arc::new(AsyncMutex::new(None)));

// 插件状态管理结构（如果需要）
// PluginState removed — not currently used

pub async fn get_total_lines() -> Result<usize, String> {
    // debug!("get_total_lines command invoked");
    let preview_guard = LARGE_FILE_PREVIEW.lock()
        .await;
    let preview = preview_guard.as_ref()
        .ok_or("No file is currently opened")?;
    let lines = preview.total_lines();
    info!("Total lines: {}", lines);
    Ok(lines)
}

/// 返回当前打开文件的字节大小（若没有打开文件，返回 0）
pub async fn get_file_size() -> Result<usize, String> {
    // debug!("get_file_size command invoked");
    let preview_guard = LARGE_FILE_PREVIEW.lock().await;
    if let Some(preview) = preview_guard.as_ref() {
        // 尝试通过 file handle 获取元数据
        match preview.file_handle.as_ref().metadata() {
            Ok(meta) => Ok(meta.len() as usize),
            Err(e) => Err(format!("Failed to read file metadata: {}", e)),
        }
    } else {
        // 如果没有打开文件，按要求返回 0（作为 Ok）
        Ok(0usize)
    }
}

pub async fn read_lines(start: usize, count: usize) -> Result<String, String> {
    let preview = {
        let preview_guard = LARGE_FILE_PREVIEW.lock().await;
        preview_guard.as_ref()
            .ok_or("No file is currently opened")?
            .clone()
    };
    preview.read_lines(start, count).await
        .map_err(|e| format!("Failed to read lines: {}", e))
}

pub async fn mmap_search(needle: String, ignore_case: bool) -> Result<serde_json::Value, String> {
    let preview_guard = LARGE_FILE_PREVIEW.lock().await;
    let preview = preview_guard.as_ref()
        .ok_or("No file is currently opened")?;
    
    let (count, samples, duration, extra_alloc, first_match, matches_pos) = preview
        .mmap_search(needle.as_bytes(), ignore_case)
        .map_err(|e| format!("Search failed: {}", e))?;
    
    let duration_ms = duration.as_millis();
    let first_match_json = if let Some((line, col, len)) = first_match {
        Some(json!({"line": line, "column": col, "length": len}))
    } else {
        None
    };

    Ok(json!({
        "count": count,
        "samples": samples,
        "matches": matches_pos,
        "duration_ms": duration_ms,
        "extra_alloc_bytes": extra_alloc,
        "first_match": first_match_json
    }))
}

pub async fn close_file() -> Result<(), String> {
    // debug!("close_file command invoked");
    let mut preview_guard = LARGE_FILE_PREVIEW.lock().await;
    
    if preview_guard.is_some() {
        *preview_guard = None;
        info!("File closed successfully");
        Ok(())
    } else {
        warn!("Attempted to close file but no file is open");
        Err("No file is currently opened".to_string())
    }
}

pub async fn open_file<R: Runtime>(app: tauri::AppHandle<R>, extensions: Option<Vec<String>>) -> Result<serde_json::Value, String> {
    // debug!("open_file command invoked");
    info!("Opening file via large-file-preview plugin");
    // Android: use tauri_plugin_android_fs
    #[cfg(target_os = "android")]
    {
        info!("open_file (Android) - using file picker");

        let api = app.android_fs_async();

        // Map extensions to mime types if provided; fall back to */*
        let mime_types: Vec<&str> = vec!["*/*"];
        info!("open_file (Android) - extensions param: {:?}", extensions);
        info!("open_file (Android) - computed mime_types: {:?}", mime_types);

        // For Android, force a broad picker filter to increase chance of seeing .log files
        let mut selected_files: Vec<FileUri> = Vec::new();
        let broad = vec!["*/*"];
        info!("open_file (Android) - forcing pick_files with broad filter {:?}", broad);
        match api.file_picker().pick_files(None, &broad, false).await {
            Ok(v) => {
                info!("open_file (Android) - pick_files returned {} entries for broad filter", v.len());
                selected_files = v;
            }
            Err(e) => {
                warn!("open_file (Android) - pick_files error for broad filter: {}", e);
            }
        }

        if selected_files.is_empty() {
            return Err("No file selected".to_string());
        }

        let uri = &selected_files[0];
        info!("open_file (Android) - selected uri: {:?}", uri);

        // 尝试从 content URI 中提取文件名/扩展名（percent-encoded）并在用户提供了 `extensions` 白名单时进行验证。
        fn percent_decode(s: &str) -> String {
            fn hex_val(b: u8) -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            }
            let bytes = s.as_bytes();
            let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
            let mut i = 0usize;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                        out.push((h << 4) | l);
                        i += 3;
                        continue;
                    }
                }
                if bytes[i] == b'+' {
                    out.push(b' ');
                    i += 1;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).into_owned()
        }

        // 从 URI 提取文件名并推断扩展名（如果有）
        // `FileUri` doesn't expose `uri` field publicly; format to string and extract the inner URI.
        let uri_formatted = format!("{:?}", uri);
        // formatted example: `FileUri { uri: "content://...", document_top_tree_uri: None }`
        // try to extract the first quoted substring as the URI
        let uri_str = uri_formatted
            .split('"')
            .nth(1)
            .unwrap_or_else(|| "")
            .to_string();
        let filename_suspect = uri_str.rsplit('/').next().map(|s| percent_decode(s));
        let selected_ext_opt = filename_suspect
            .as_deref()
            .and_then(|f| Path::new(f).extension())
            .map(|os| os.to_string_lossy().to_string().to_lowercase());

        if let Some(ref exts) = extensions {
            if !exts.is_empty() {
                let allowed = selected_ext_opt.as_ref().map(|ext| {
                    exts.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
                });
                match allowed {
                    Some(true) => {
                        info!("open_file (Android) - selected extension allowed: {:?}", selected_ext_opt);
                    }
                    Some(false) => {
                        error!("open_file (Android) - selected extension not allowed: {:?}, allowed={:?}", selected_ext_opt, exts);
                        return Err(format!("Selected file extension {:?} is not allowed", selected_ext_opt));
                    }
                    None => {
                        error!("open_file (Android) - could not determine selected file extension from uri: {}", uri_str);
                        return Err("Could not determine selected file extension".to_string());
                    }
                }
            }
        }

        match api.open_file_readable(uri).await {
            Ok(mut reader) => {
                match reader.metadata() {
                    Ok(md) => info!("open_file (Android) - reader opened, file_type: {:?}, len: {:?}", md.file_type(), md.len()),
                    Err(e) => warn!("open_file (Android) - reader.metadata() failed: {}", e),
                }

                let mut tmp = std::env::temp_dir();
                let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
                tmp.push(format!("tauri_tmp_{}.tmp", nanos));
                info!("open_file (Android) - creating temp file at: {:?}", tmp);

                match std::fs::File::create(&tmp) {
                    Ok(mut out) => {
                        use std::io::copy;
                        match copy(&mut reader, &mut out) {
                            Ok(bytes_copied) => {
                                info!("open_file (Android) - copied {} bytes to temp file", bytes_copied);
                                // 使用 LargeFilePreview 打开并缓存
                                match LargeFilePreview::open(tmp.clone()) {
                                    Ok(preview) => {
                                        info!("open_file (Android) - LargeFilePreview::open succeeded");
                                        // 尝试读取文件大小（字节）
                                        let size = match preview.file_handle.as_ref().metadata() {
                                            Ok(meta) => meta.len() as usize,
                                            Err(e) => {
                                                warn!("open_file (Android) - failed to get metadata from preview.file_handle: {}", e);
                                                0usize
                                            }
                                        };
                                        let mut preview_guard = LARGE_FILE_PREVIEW.lock().await;
                                        *preview_guard = Some(preview);
                                        info!("open_file (Android) - preview cached (size={} bytes)", size);
                                        Ok(json!({"path": tmp.to_string_lossy(), "status": "success", "size": size, "truncation_policy": "lines_longer_than_6MB_are_truncated"}))
                                    }
                                    Err(e) => {
                                        error!("open_file (Android) - LargeFilePreview::open failed: {}", e);
                                        Err(format!("Failed to open file preview: {}", e))
                                    }
                                }
                            }
                            Err(e) => {
                                error!("open_file (Android) - copy to temp file failed: {}", e);
                                Err(format!("Failed to copy file: {}", e))
                            }
                        }
                    }
                    Err(e) => {
                        error!("open_file (Android) - failed to create temp file {:?}: {}", tmp, e);
                        Err(format!("Failed to create temp file: {}", e))
                    }
                }
            }
            Err(e) => {
                error!("open_file (Android) - api.open_file_readable failed for uri {:?}: {}", uri, e);
                Err(format!("Failed to open file readable: {}", e))
            }
        }
    }

    // Non-Android (PC): use rfd async dialog
    #[cfg(not(target_os = "android"))]
    {
        info!("open_file (PC) - using rfd AsyncFileDialog");

        // Prepare extension filters for rfd if provided, otherwise default to txt/log
        let filters: Vec<String> = if let Some(exts) = &extensions {
            exts.iter().map(|s| s.trim_start_matches('.').to_string()).collect()
        } else {
            vec!["txt".to_string(), "log".to_string()]
        };

        if let Some(file_handle) = AsyncFileDialog::new()
            .add_filter("Text", &filters.iter().map(|s| s.as_str()).collect::<Vec<&str>>())
            .pick_file()
            .await
        {
            let path = file_handle.path().to_path_buf();

            // 使用 LargeFilePreview 打开并缓存
                match LargeFilePreview::open(path.clone()) {
                Ok(preview) => {
                    let size = match preview.file_handle.as_ref().metadata() {
                        Ok(meta) => meta.len() as usize,
                        Err(_) => 0usize,
                    };
                    let mut preview_guard = LARGE_FILE_PREVIEW.lock().await;
                    *preview_guard = Some(preview);
                    Ok(json!({"path": path.to_string_lossy(), "status": "success", "size": size, "truncation_policy": "lines_longer_than_6MB_are_truncated"}))
                }
                Err(e) => {
                    Err(format!("Failed to open file preview: {}", e))
                }
            }
        } else {
            Err("No file selected".to_string())
        }
    }
}