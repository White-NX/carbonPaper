// src-tauri/build.rs

use base64::Engine;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

const UPDATE_PUBLIC_KEY_FILE: &str = "update-public-key.txt";

fn generate_native_locales() {
    let locales_dir = Path::new("../src/i18n/locales");
    println!("cargo:rerun-if-changed={}", locales_dir.display());
    let mut entries = fs::read_dir(locales_dir)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", locales_dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    entries.sort();

    let mut generated = String::from("pub const NATIVE_LOCALES: &[(&str, &str)] = &[\n");
    for path in entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let locale = path
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("locale filename must be valid UTF-8");
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("Failed to read {}: {error}", path.display()));
        generated.push_str(&format!("    ({locale:?}, {content:?}),\n"));
    }
    generated.push_str("];\n");

    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR must be set by Cargo");
    fs::write(Path::new(&out_dir).join("native_locales.rs"), generated)
        .expect("Failed to generate native locale catalog");
}

/// Resources that earlier builds generated into `pre-bundle/` for the retired
/// Python monitor. Tauri bundles the whole directory, so a stale copy left by
/// an older build would otherwise ship with the application.
const RETIRED_PREBUNDLE_ENTRIES: &[&str] = &[
    "monitor",
    "monitor.pyz",
    "monitor.pyz.tmp",
    "python-3.12.10-amd64.exe",
    "carbonpaper-python.exe",
];

fn remove_retired_prebundle_entries() {
    for name in RETIRED_PREBUNDLE_ENTRIES {
        let path = Path::new("pre-bundle").join(name);
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else if path.exists() {
            fs::remove_file(&path)
        } else {
            continue;
        };
        result.unwrap_or_else(|error| {
            panic!(
                "Failed to remove retired resource {}: {error}",
                path.display()
            )
        });
    }
}

fn configure_update_public_key() {
    let path = Path::new(UPDATE_PUBLIC_KEY_FILE);
    println!("cargo:rerun-if-changed={}", UPDATE_PUBLIC_KEY_FILE);

    let public_key = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "Build failed: cannot read checked-in update public key {}: {}",
            path.display(),
            e
        )
    });
    let public_key = public_key.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(public_key)
        .unwrap_or_else(|e| panic!("Build failed: update public key is not valid base64: {}", e));
    if decoded.len() != 32 {
        panic!(
            "Build failed: update public key must decode to 32 bytes, got {}",
            decoded.len()
        );
    }

    if let Ok(configured) = std::env::var("CARBONPAPER_UPDATE_PUBLIC_KEY") {
        if configured.trim() != public_key {
            panic!(
                "Build failed: CARBONPAPER_UPDATE_PUBLIC_KEY does not match {}",
                UPDATE_PUBLIC_KEY_FILE
            );
        }
    }

    println!(
        "cargo:rustc-env=CARBONPAPER_UPDATE_PUBLIC_KEY={}",
        public_key
    );
}

/// 屏蔽从源码编译的 OpenSSL 静态库引发的成片 LNK4099 警告。
///
/// rusqlite 的 `bundled-sqlcipher-vendored-openssl` 会在构建时从源码编译一份
/// 静态 OpenSSL，其目标文件被打包进 `libopenssl_sys-*.rlib`，但配套的调试符号
/// `ossl_static.pdb` 留在 openssl-sys 自己的构建输出目录里，不会跟到 rlib 旁边。
/// MSVC 链接器于是对 libcrypto 的每个目标文件各报一条 LNK4099，单次构建刷出
/// 上千行噪声，而且 Cargo 会缓存这些诊断并在后续构建里原样重放。
///
/// 缺失的只是 OpenSSL 那部分 C 代码的调试符号，Rust 侧的调试信息与程序行为都
/// 不受影响，因此让链接器不再报告这一类警告。
fn silence_vendored_openssl_pdb_warnings() {
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" {
        return;
    }
    // 只作用于本包的可执行文件与 cdylib，也就是真正执行链接步骤的那些目标。
    println!("cargo:rustc-link-arg=/IGNORE:4099");
}

fn main() {
    generate_native_locales();
    configure_update_public_key();
    silence_vendored_openssl_pdb_warnings();
    remove_retired_prebundle_entries();
    fs::create_dir_all("pre-bundle").expect("Failed to create pre-bundle directory");

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

    // --- 1. 复制项目根目录下的可选可执行文件 ---
    fn copy_if_exists(src: &Path, dst: &Path) {
        if src.exists() && src.is_file() {
            copy_file_if_needed(src, dst);
            eprintln!("Included file: {:?} -> {:?}", src, dst);
        } else {
            eprintln!("Optional file not found: {:?}; skipping copy", src);
        }
    }

    // 复制 aria2c
    copy_if_exists(
        Path::new("../aria2c.exe"),
        Path::new("pre-bundle/aria2c.exe"),
    );

    // --- 2. 复制 compliance_process 到 pre-bundle ---
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

    // --- 3. 复制 browser-extension 目录到 pre-bundle ---
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

    // --- 4. 告诉 Tauri 需要重新运行此脚本 ---
    println!("cargo:rerun-if-changed=../compliance_process");
    println!("cargo:rerun-if-changed=../aria2c.exe");
    println!("cargo:rerun-if-changed=../browser-extension");

    // 最后，调用 tauri_build
    tauri_build::build();
}
