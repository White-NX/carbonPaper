fn main() {
    for name in [
        "CARBONPAPER_APP_BOUND_DEV_INSTANCE",
        "CARBONPAPER_APP_BOUND_DEV_PUBLIC_KEY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if std::env::var_os("CARGO_FEATURE_DEVELOPMENT_RUNTIME").is_none() {
        return;
    }
    let instance = std::env::var("CARBONPAPER_APP_BOUND_DEV_INSTANCE")
        .expect("Use npm run debug to configure the development service");
    assert!(
        instance.len() == 16
            && instance
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "The development instance must be 16 lowercase hexadecimal characters"
    );
    let public_key = std::env::var("CARBONPAPER_APP_BOUND_DEV_PUBLIC_KEY")
        .expect("The development runtime requires its own build-time public key");
    assert!(
        public_key.len() == 44
            && public_key.ends_with('=')
            && public_key[..43]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/".contains(&c)),
        "Invalid development public key"
    );
    assert_ne!(
        public_key,
        std::fs::read_to_string("../update-public-key.txt")
            .unwrap()
            .trim(),
        "Development must not use the release signing identity"
    );
    println!("cargo:rustc-env=CARBONPAPER_APP_BOUND_DEV_INSTANCE={instance}");
    println!("cargo:rustc-env=CARBONPAPER_APP_BOUND_DEV_PUBLIC_KEY={public_key}");
}
