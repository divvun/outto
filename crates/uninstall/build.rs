fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=OUTTO_BUILD_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = tauri_winres::WindowsResource::new();
        // CI's full build version (outto-core's build script checks it); the
        // FileVersion stays the crate's numeric version.
        if let Some(version) = std::env::var("OUTTO_BUILD_VERSION")
            .ok()
            .filter(|v| !v.is_empty())
        {
            res.set("ProductVersion", &version);
        }
        res.set_manifest(
            r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#,
        );
        res.compile().unwrap();
    }
}
