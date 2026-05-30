// src-tauri/build.rs

use std::fs;
use std::path::Path;
use std::process::Command;
use sha2::{Digest, Sha256};
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
                !same_size || src_newer
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
            if file_name == ".venv" || file_name == ".pytest_cache" || file_name == "__pycache__" || file_name == "tests" {
                // 如果是，返回 false，`walkdir` 将不会进入这个目录
                // 排除 tests/ 是为了不把测试代码打进生产 monitor.pyz
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

            // 规则 1: 打包特定的文件夹
            let model_dir_singular = source_dir.join("model");
            let model_dir_plural = source_dir.join("models");
            if src_path.starts_with(&model_dir_singular)
                || src_path.starts_with(&model_dir_plural)
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

    // --- 4.5 生成 monitor.pyz（zipapp 打包）并计算 SHA-256 嵌入二进制 ---
    // 把源 monitor/ 下的运行时 Python 代码打成单个 zipapp 归档，
    // 让 Rust 端启动时可以校验完整性，防止散落 .py 文件被篡改。
    // 注意：我们从源目录重建一个干净的 staging 目录（不依赖 pre-bundle/monitor/，
    //       后者可能含有源中已删除的陈旧文件）。
    let pyz_path = Path::new("pre-bundle/monitor.pyz");
    let pyz_hash = build_monitor_pyz(source_dir, pyz_path);
    println!("cargo:rustc-env=MONITOR_PYZ_SHA256={}", pyz_hash);
    eprintln!("monitor.pyz built; sha256={}", pyz_hash);

    // --- 5. 包含项目根目录下的 python 可执行文件（如果存在） ---
    // 目标：把 ../python-3.12.10-amd64.exe 复制为 pre-bundle/python-3.12.10-amd64.exe
    // 泛用文件复制函数
    fn copy_if_exists(src: &Path, dst: &Path) {
        if src.exists() && src.is_file() {
            copy_file_if_needed(src, dst);
            eprintln!("Included file: {:?} -> {:?}", src, dst);
        } else {
            eprintln!("Optional file not found: {:?}; skipping copy", src);
        }
    }

    // 复制 python 可执行文件
    copy_if_exists(
        Path::new("../python-3.12.10-amd64.exe"),
        Path::new("pre-bundle/python-3.12.10-amd64.exe"),
    );
    // 复制 aria2c
    copy_if_exists(
        Path::new("../aria2c.exe"),
        Path::new("pre-bundle/aria2c.exe"),
    );

    // --- 7. 复制 compliance_process 到 pre-bundle ---
    let cp_source = Path::new("../compliance_process");
    let cp_dest = Path::new("pre-bundle/compliance_process");
    if cp_source.exists() && cp_source.is_dir() {
        fs::create_dir_all(cp_dest).expect("Failed to create compliance_process dir in pre-bundle");
        for entry in WalkDir::new(cp_source).into_iter().filter_map(|e| e.ok()) {
            let src_path = entry.path();
            let relative = src_path
                .strip_prefix(cp_source)
                .expect("strip_prefix failed");
            let dest_path = cp_dest.join(relative);
            if src_path.is_dir() {
                fs::create_dir_all(&dest_path).expect("Failed to create dir");
            } else if src_path.is_file() {
                copy_file_if_needed(src_path, &dest_path);
            }
        }
        eprintln!("Included compliance_process directory");
    }

    // --- 8. 复制 browser-extension 目录到 pre-bundle ---
    let ext_source = Path::new("../browser-extension");
    let ext_dest = Path::new("pre-bundle/browser-extension");
    if ext_source.exists() && ext_source.is_dir() {
        fs::create_dir_all(ext_dest).expect("Failed to create browser-extension dir in pre-bundle");
        for entry in WalkDir::new(ext_source).into_iter().filter_map(|e| e.ok()) {
            let src_path = entry.path();
            let relative = src_path
                .strip_prefix(ext_source)
                .expect("strip_prefix failed");
            let dest_path = ext_dest.join(relative);
            if src_path.is_dir() {
                fs::create_dir_all(&dest_path).expect("Failed to create dir");
            } else if src_path.is_file() {
                copy_file_if_needed(src_path, &dest_path);
            }
        }
        eprintln!("Included browser-extension directory");
    }

    // --- 9. 告诉 Tauri 需要重新运行此脚本 ---
    println!("cargo:rerun-if-changed=../monitor");
    println!("cargo:rerun-if-changed=../compliance_process");
    println!("cargo:rerun-if-changed=../python-3.12.10-amd64.exe");
    println!("cargo:rerun-if-changed=../browser-extension");
    println!("cargo:rerun-if-changed=build_pyz.py");

    // 最后，调用 tauri_build
    tauri_build::build();
}

/// 在构建机上定位可用的 Python 解释器（3.x）。
/// 依次尝试 `python`、`py -3`、本地 venv 路径。返回 (program, prefix_args)
/// 第一个能执行 `--version` 的就用。
fn locate_python() -> (String, Vec<String>) {
    // 候选：(命令, 前置参数)
    let candidates: Vec<(&str, Vec<&str>)> = vec![
        ("python", vec![]),
        ("py", vec!["-3"]),
        ("../carbonPaper/.venv/Scripts/python.exe", vec![]),
        ("../.venv/Scripts/python.exe", vec![]),
    ];

    for (cmd, args) in &candidates {
        let mut probe = Command::new(cmd);
        for a in args {
            probe.arg(a);
        }
        probe.arg("--version");
        if let Ok(output) = probe.output() {
            if output.status.success() {
                let owned_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                return (cmd.to_string(), owned_args);
            }
        }
    }

    panic!(
        "Build failed: no usable Python interpreter found. \
         carbonPaper's build.rs needs Python 3.x on PATH to run `build_pyz.py` for \
         the monitor integrity-check package. Install Python 3.12 or ensure `python` / `py -3` \
         resolves on your PATH."
    );
}

/// 把 `source_dir`（典型为 ../monitor，**源**目录）里的运行时 Python 代码
/// 打成 zipapp 归档写入 `out_pyz`，并返回归档内容的 SHA-256（十六进制小写）。
///
/// 这里**重新走一遍源目录**而不是用 pre-bundle/monitor/，
/// 因为 pre-bundle 可能积累了源中已删除的陈旧文件（旧 build.rs 只复制不清理）。
/// 我们把通过过滤的文件复制到 `target/pyz-staging-monitor/`（每次清空），
/// 然后用 `build_pyz.py`（项目自带的确定性打包器）把 staging 目录打成 zipapp。
///
/// 为什么不直接 `python -m zipapp`：标准 zipapp 把每个 entry 的文件系统 mtime
/// 写进 zip header，于是同一份源代码两次 build 产出字节不同的 .pyz，hash 漂移。
/// `build_pyz.py` 固定 entry timestamp / external_attr / 排序 / compresslevel，
/// 保证字节稳定，从而 `MONITOR_PYZ_SHA256` 可重现。
///
/// 过滤规则与 main() 中的复制循环保持一致：
///   - 排除 `.venv` / `__pycache__` / `tests` 子目录
///   - 排除 `chroma_db/` 内容
///   - 仅收录扩展名为 `.py` / `.txt` / `.md` / `.json` 的文件
///   - `model/` 和 `models/` 全部收录（含权重等二进制）
///
/// 入口点通过 `"main:main"` 生成（脚本自动写一个 __main__.py 调用 main.main()，
/// 与 zipapp 的 MAIN_TEMPLATE 一致）。
fn build_monitor_pyz(source_dir: &Path, out_pyz: &Path) -> String {
    // 先确认源目录确实存在主入口
    let main_py = source_dir.join("main.py");
    if !main_py.is_file() {
        panic!(
            "Build failed: expected {} to exist before building monitor.pyz",
            main_py.display()
        );
    }

    // 1. 准备干净的 staging 目录
    let staging = Path::new("target/pyz-staging-monitor");
    if staging.exists() {
        fs::remove_dir_all(staging)
            .unwrap_or_else(|e| panic!("Failed to clean pyz staging dir: {}", e));
    }
    fs::create_dir_all(staging)
        .unwrap_or_else(|e| panic!("Failed to create pyz staging dir: {}", e));

    // 2. 应用与 main() 同样的过滤规则，把源 monitor/ 文件复制到 staging
    //    比 main() 更严：在 walker 阶段就排除 chroma_db/ 和 screenshots/，
    //    避免它们以空目录 entry 形式进 zipapp（语义上它们是运行时数据，不该入包）。
    let walker = WalkDir::new(source_dir).into_iter().filter_entry(|e| {
        let path = e.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if matches!(
                &*name,
                ".venv" | ".pytest_cache" | "__pycache__" | "tests" | "chroma_db" | "screenshots"
            ) {
                return false;
            }
        }
        true
    });

    let model_dir_singular = source_dir.join("model");
    let model_dir_plural = source_dir.join("models");
    let chroma_dir = source_dir.join("chroma_db");

    for entry in walker {
        let entry = entry.unwrap_or_else(|e| panic!("Failed to walk source dir: {}", e));
        let src_path = entry.path();
        let relative = match src_path.strip_prefix(source_dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let dst_path = staging.join(relative);

        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)
                .unwrap_or_else(|e| panic!("Failed to mkdir in staging: {}", e));
            continue;
        }
        if !src_path.is_file() {
            continue;
        }

        // 排除 chroma_db 下文件
        if src_path.starts_with(&chroma_dir) {
            continue;
        }

        let in_models = src_path.starts_with(&model_dir_singular)
            || src_path.starts_with(&model_dir_plural);
        let ok_ext = src_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| ["py", "txt", "md", "json"].contains(&e))
            .unwrap_or(false);

        if in_models || ok_ext {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)
                    .unwrap_or_else(|e| panic!("Failed to mkdir for staging file: {}", e));
            }
            fs::copy(src_path, &dst_path)
                .unwrap_or_else(|e| panic!("Failed to copy to staging: {}", e));
        }
    }

    // 3. 跑 build_pyz.py <staging> <out_pyz> "main:main"
    //    自定义打包脚本：固定 entry mtime / create_system / external_attr，
    //    排序写入 + 显式 compresslevel，保证 .pyz 字节稳定（同源代码 → 同 hash），
    //    这样 build.rs 嵌入到 Rust 二进制里的 SHA-256 可以重现。
    //    `python -m zipapp` 会把每个成员的文件系统 mtime 写进 zip header，
    //    导致两次 build 产出字节不同的 .pyz，hash 永远漂移——所以这里弃用。
    let helper_script = Path::new("build_pyz.py");
    if !helper_script.is_file() {
        panic!(
            "Build failed: missing helper script {}; expected at src-tauri/build_pyz.py",
            helper_script.display()
        );
    }

    let (program, prefix_args) = locate_python();
    let mut zipapp_cmd = Command::new(&program);
    for a in &prefix_args {
        zipapp_cmd.arg(a);
    }
    zipapp_cmd
        .arg(helper_script)
        .arg(staging)
        .arg(out_pyz)
        .arg("main:main");

    let output = zipapp_cmd
        .output()
        .unwrap_or_else(|e| panic!("Failed to spawn build_pyz.py: {}", e));

    if !output.status.success() {
        panic!(
            "build_pyz.py failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // 4. 计算 SHA-256
    let pyz_bytes = fs::read(out_pyz).unwrap_or_else(|e| {
        panic!("Failed to read freshly built {}: {}", out_pyz.display(), e)
    });
    let mut hasher = Sha256::new();
    hasher.update(&pyz_bytes);
    let hash = hasher.finalize();
    // 转为小写十六进制（与 Rust 端 verify 的 format!("{:x}") 一致）
    hash.iter().map(|b| format!("{:02x}", b)).collect::<String>()
}
