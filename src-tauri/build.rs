// src-tauri/build.rs

use std::fs;
use std::path::Path;
use walkdir::WalkDir;

fn main() {
    // --- 1. 定义路径 ---
    // 源文件夹
    let source_dir = Path::new("../monitor");
    // 构建脚本将处理好的文件临时存放在这里
    let prebundle_dir = Path::new("pre-bundle/monitor");

    // --- 2. 确保临时目录存在（避免每次清空导致热重载循环） ---
    fs::create_dir_all(prebundle_dir).expect("Failed to create pre-bundle directory");

    // 只在内容变更时复制，避免无谓的文件时间戳抖动触发重建
    fn copy_file_if_needed(src: &Path, dst: &Path) {
        let needs_copy = match (fs::metadata(src), fs::metadata(dst)) {
            (Ok(src_meta), Ok(dst_meta)) => {
                let same_size = src_meta.len() == dst_meta.len();
                let src_newer = src_meta
                    .modified()
                    .ok()
                    .zip(dst_meta.modified().ok())
                    .map(|(s, d)| s > d)
                    .unwrap_or(true);
                !(same_size && !src_newer)
            }
            (Ok(_), Err(_)) => true,
            _ => true,
        };

        if needs_copy {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).expect("Failed to create destination directory");
            }
            fs::copy(src, dst).expect("Failed to copy file");
        }
    }

    // --- 3. 遍历和过滤文件 ---
    // `walkdir` 可以让我们轻松地遍历所有文件和文件夹
    let walker = WalkDir::new(source_dir).into_iter();

    // `filter_entry` 是一个强大的功能，可以提前排除整个目录
    for entry in walker.filter_entry(|e| {
        let path = e.path();
        // 检查是否是需要被完全排除的文件夹
        if path.is_dir() {
            let file_name = path.file_name().unwrap_or_default();
            if file_name == ".venv" || file_name == "__pycache__" {
                // 如果是，返回 false，`walkdir` 将不会进入这个目录
                return false;
            }
        }
        // 否则，保留此条目以进行下一步处理
        true
    }) {
        let entry = entry.expect("Failed to read directory entry");
        let src_path = entry.path();

        // 计算文件在目标目录中的对应路径
        let relative_path = src_path
            .strip_prefix(source_dir)
            .expect("Failed to get relative path");
        let dest_path = prebundle_dir.join(relative_path);

        // 如果是文件夹，则在目标位置创建它
        if src_path.is_dir() {
            fs::create_dir_all(&dest_path).expect("Failed to create directory in pre-bundle");
        }
        // 如果是文件，则根据规则决定是否复制
        else if src_path.is_file() {
            let mut should_copy = false;

            // 规则 1: 是否在 `model` 或 `models` 文件夹内？（打包该目录下全部文件，不筛选）
            let model_dir_singular = source_dir.join("model");
            let model_dir_plural = source_dir.join("models");
            if src_path.starts_with(&model_dir_singular) || src_path.starts_with(&model_dir_plural)
            {
                should_copy = true;
            }

            // 规则 2: 文件扩展名是否符合要求？
            if !should_copy {
                // 如果上面的规则不满足，才检查这个
                if let Some(ext) = src_path.extension().and_then(|s| s.to_str()) {
                    if ["py", "txt", "md", "json"].contains(&ext) {
                        should_copy = true;
                    }
                }
            }

            // 规则 3: 排除 `chroma_db` 文件夹内的所有文件
            if src_path.starts_with(source_dir.join("chroma_db")) {
                should_copy = false;
            }

            // 如果满足复制条件，就执行复制
            if should_copy {
                copy_file_if_needed(src_path, &dest_path);
            }
        }
    }

    // --- 4. 特殊处理：创建空的 chroma_db 文件夹 ---
    fs::create_dir_all(prebundle_dir.join("chroma_db"))
        .expect("Failed to create empty chroma_db directory");

    // --- 5. 包含项目根目录下的 python 可执行文件（如果存在） ---
    // 目标：把 ../python-3.12.10-amd64.exe 复制为 pre-bundle/python-3.12.10-amd64.exe
    // 泛用文件复制函数
    fn copy_if_exists(src: &Path, dst: &Path) {
        if src.exists() && src.is_file() {
            copy_file_if_needed(src, dst);
            println!("cargo:warning=Included file: {:?} -> {:?}", src, dst);
        } else {
            println!("cargo:warning=File not found: {:?}; skipping copy", src);
        }
    }

    // 复制 python 可执行文件
    copy_if_exists(Path::new("../python-3.12.10-amd64.exe"), Path::new("pre-bundle/python-3.12.10-amd64.exe"));
    // 复制 aria2c
    copy_if_exists(Path::new("../aria2c.exe"), Path::new("pre-bundle/aria2c.exe"));

    // --- 7. 复制 browser-extension 目录到 pre-bundle ---
    let ext_source = Path::new("../browser-extension");
    let ext_dest = Path::new("pre-bundle/browser-extension");
    if ext_source.exists() && ext_source.is_dir() {
        fs::create_dir_all(ext_dest).expect("Failed to create browser-extension dir in pre-bundle");
        for entry in WalkDir::new(ext_source).into_iter().filter_map(|e| e.ok()) {
            let src_path = entry.path();
            let relative = src_path.strip_prefix(ext_source).expect("strip_prefix failed");
            let dest_path = ext_dest.join(relative);
            if src_path.is_dir() {
                fs::create_dir_all(&dest_path).expect("Failed to create dir");
            } else if src_path.is_file() {
                copy_file_if_needed(src_path, &dest_path);
            }
        }
        println!("cargo:warning=Included browser-extension directory");
    }

    // --- 8. 告诉 Tauri 需要重新运行此脚本 ---
    println!("cargo:rerun-if-changed=../monitor");
    println!("cargo:rerun-if-changed=../python-3.12.10-amd64.exe");
    println!("cargo:rerun-if-changed=../browser-extension");

    // 最后，调用 tauri_build
    tauri_build::build();
}
