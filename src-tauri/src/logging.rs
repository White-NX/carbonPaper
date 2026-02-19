//! 统一日志系统
//!
//! 提供按日期和大小轮换的日志文件写入，以及旧日志的 gzip 压缩和自动清理。
//! Rust 自身使用 tracing 宏输出日志，Python 子进程的 stderr 由 Rust 捕获后
//! 以 `monitor.stderr` target 写入同一日志文件。

use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;

/// 单个日志分片的最大字节数（30 MB）
const MAX_FILE_SIZE: u64 = 30 * 1024 * 1024;
/// 日志保留天数
const RETENTION_DAYS: i64 = 7;
/// 日志文件基础名称
const LOG_BASE_NAME: &str = "carbonpaper.log";

struct Inner {
    logs_root: PathBuf,
    current_date: String,
    file: Option<File>,
    written_bytes: u64,
}

impl Inner {
    fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    fn dir_for_date(&self, date: &str) -> PathBuf {
        self.logs_root.join(date)
    }

    fn log_path(&self, date: &str) -> PathBuf {
        self.dir_for_date(date).join(LOG_BASE_NAME)
    }

    /// 确保当前日期的目录和文件已就绪。日期变更时会自动切换。
    fn ensure_file(&mut self) -> io::Result<&mut File> {
        let today = Self::today();

        if self.file.is_none() || self.current_date != today {
            // 日期变更——切换到新目录
            self.current_date = today.clone();
            let dir = self.dir_for_date(&today);
            fs::create_dir_all(&dir)?;

            let path = self.log_path(&today);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            self.written_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(file);
        }

        Ok(self.file.as_mut().unwrap())
    }

    /// 大小超限时轮换文件
    fn rotate_if_needed(&mut self) -> io::Result<()> {
        if self.written_bytes < MAX_FILE_SIZE {
            return Ok(());
        }

        // 关闭当前文件
        self.file.take();

        let dir = self.dir_for_date(&self.current_date);
        // 找出已有的最大分片编号
        let mut max_index = 0u32;
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Some(suffix) = name_str.strip_prefix(&format!("{}.", LOG_BASE_NAME)) {
                    if let Ok(n) = suffix.parse::<u32>() {
                        max_index = max_index.max(n);
                    }
                }
            }
        }

        // 从高到低依次重命名：.N → .N+1
        for i in (1..=max_index).rev() {
            let from = dir.join(format!("{}.{}", LOG_BASE_NAME, i));
            let to = dir.join(format!("{}.{}", LOG_BASE_NAME, i + 1));
            let _ = fs::rename(&from, &to);
        }

        // carbonpaper.log → carbonpaper.log.1
        let current = dir.join(LOG_BASE_NAME);
        let rotated = dir.join(format!("{}.1", LOG_BASE_NAME));
        let _ = fs::rename(&current, &rotated);

        // 打开新文件
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)?;
        self.written_bytes = 0;
        self.file = Some(file);

        Ok(())
    }
}

/// 线程安全的按日期/大小轮换日志写入器。
/// 使用 `Arc<Mutex<Inner>>` 以支持 `Clone` 和 `'static` 生命周期。
#[derive(Clone)]
pub struct DailyRotatingWriter {
    inner: Arc<Mutex<Inner>>,
}

impl DailyRotatingWriter {
    fn new(logs_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                logs_root,
                current_date: String::new(),
                file: None,
                written_bytes: 0,
            })),
        }
    }
}

/// `tracing_subscriber` 需要的 Writer 包装。
/// 持有 `Arc` 引用，在 `Write::write` 时获取锁。
pub struct ArcWriter {
    inner: Arc<Mutex<Inner>>,
}

impl Write for ArcWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = guard.rotate_if_needed();
        let file = guard.ensure_file()?;
        let n = file.write(buf)?;
        guard.written_bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut f) = guard.file {
            f.flush()?;
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for DailyRotatingWriter {
    type Writer = ArcWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ArcWriter {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// 初始化全局日志系统。返回的 guard 必须在 `run()` 中持有直到程序结束。
///
/// - Layer 1: 文件输出（无 ANSI 颜色）
/// - Layer 2: stderr 输出（调试用）
pub fn init_logging(data_dir: &Path) -> DailyRotatingWriter {
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::Layer;

    let logs_root = data_dir.join("logs");
    let _ = fs::create_dir_all(&logs_root);

    let writer = DailyRotatingWriter::new(logs_root);

    // 默认日志级别：release=INFO, debug=DEBUG
    let default_level = if cfg!(debug_assertions) {
        "debug"
    } else {
        "info"
    };

    let env_filter_file = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let env_filter_stderr = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    // 文件层：无 ANSI 颜色
    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer.clone())
        .with_filter(env_filter_file);

    // stderr 层：保留颜色供开发调试
    let stderr_layer = fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr)
        .with_filter(env_filter_stderr);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .init();

    writer
}

/// 启动日志维护异步任务：压缩旧日志、删除过期日志。
pub fn spawn_maintenance_task(data_dir: PathBuf) {
    tauri::async_runtime::spawn(async move {
        let logs_root = data_dir.join("logs");
        // 启动时立即执行一次
        run_maintenance(&logs_root);

        loop {
            // 计算距下次 00:05 本地时间的等待秒数
            let now = chrono::Local::now();
            let tomorrow_0005 = (now.date_naive() + chrono::Duration::days(1))
                .and_hms_opt(0, 5, 0)
                .unwrap();
            let tomorrow_0005 = tomorrow_0005.and_local_timezone(chrono::Local).unwrap();
            let wait_secs = (tomorrow_0005 - now).num_seconds().max(60) as u64;

            tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;
            run_maintenance(&logs_root);
        }
    });
}

fn run_maintenance(logs_root: &Path) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let cutoff = chrono::Local::now() - chrono::Duration::days(RETENTION_DAYS);

    let entries = match fs::read_dir(logs_root) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();

        // 仅处理日期格式目录
        if chrono::NaiveDate::parse_from_str(&name_str, "%Y-%m-%d").is_err() {
            continue;
        }

        // 跳过当天
        if name_str == today {
            continue;
        }

        let dir_path = entry.path();

        // 超过保留天数的目录直接删除
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&name_str, "%Y-%m-%d") {
            let dir_date = date.and_hms_opt(0, 0, 0).unwrap()
                .and_local_timezone(chrono::Local).unwrap();
            if dir_date < cutoff {
                tracing::info!("Removing old log directory: {}", name_str);
                let _ = fs::remove_dir_all(&dir_path);
                continue;
            }
        }

        // 非当天、未过期的目录：gzip 压缩 .log 文件
        if let Ok(files) = fs::read_dir(&dir_path) {
            for file_entry in files.flatten() {
                let fname = file_entry.file_name();
                let fname_str = fname.to_string_lossy();

                // 仅压缩 .log 和 .log.N 文件（跳过已压缩的 .gz）
                let is_log = fname_str == LOG_BASE_NAME
                    || fname_str.starts_with(&format!("{}.", LOG_BASE_NAME))
                        && !fname_str.ends_with(".gz");

                if !is_log {
                    continue;
                }

                let src = file_entry.path();
                let dst = src.with_extension(format!(
                    "{}.gz",
                    src.extension().unwrap_or_default().to_string_lossy()
                ));

                if let Err(e) = gzip_file(&src, &dst) {
                    tracing::warn!("Failed to gzip {}: {}", src.display(), e);
                } else {
                    let _ = fs::remove_file(&src);
                }
            }
        }
    }
}

fn gzip_file(src: &Path, dst: &Path) -> io::Result<()> {
    let input = fs::read(src)?;
    let output_file = File::create(dst)?;
    let mut encoder = GzEncoder::new(output_file, Compression::default());
    encoder.write_all(&input)?;
    encoder.finish()?;
    Ok(())
}
