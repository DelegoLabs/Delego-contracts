fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    
    if let Ok(version) = std::env::var("CARGO_PKG_VERSION") {
        let formatted = version.replace('.', "_");
        println!("cargo:rustc-env=CARGO_PKG_VERSION_SYM={}", formatted);
    }
}
