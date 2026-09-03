fn main() {
    println!("cargo:rerun-if-changed=assets/icon-on.ico");
    println!("cargo:rerun-if-changed=assets/icon-off.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon_with_id("assets/icon-on.ico", "1")
            .set_icon_with_id("assets/icon-off.ico", "2")
            .set("ProductName", "Corsair Elite Display")
            .set("FileDescription", "Corsair Elite LCD second display")
            .set("LegalCopyright", "Copyright (c) 2026 Diogo");
        resource
            .compile()
            .expect("failed to embed Windows resources");
    }
}
