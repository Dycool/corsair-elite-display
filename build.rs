const WINDOWS_MANIFEST: &str = r#"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
      version="1.0.0.0"
      processorArchitecture="amd64"
      name="Dycool.CorsairEliteDisplay"
      type="win32" />
  <description>Corsair Elite Display</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon-on.ico");
    println!("cargo:rerun-if-changed=assets/icon-off.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".into());
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_manifest(WINDOWS_MANIFEST)
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
