fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Vendored OpenSSL objects do not ship their debugging PDB. Match the
        // desktop's handling of this otherwise harmless MSVC linker message.
        println!("cargo:rustc-link-arg=/IGNORE:4099");
    }
}
