fn main() {
    println!("cargo:rerun-if-changed=assets/icon-on.ico");
    println!("cargo:rerun-if-changed=assets/icon-off.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".into());
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon_with_id("assets/icon-on.ico", "1")
            .set_icon_with_id("assets/icon-off.ico", "2")
            .set("CompanyName", "Dycool")
            .set("ProductName", "Corsair Elite Display")
            .set("FileDescription", "Corsair Elite LCD second display utility")
            .set("InternalName", "corsair-elite-display")
            .set("OriginalFilename", "corsair-elite-display.exe")
            .set("ProductVersion", &version)
            .set("FileVersion", &version)
            .set(
                "Comments",
                "Windows utility for Corsair Elite LCD display control and second-screen streaming",
            )
            .set("LegalCopyright", "Copyright (c) 2026 Diogo");
        resource
            .compile()
            .expect("failed to embed Windows resources");
    }
}
