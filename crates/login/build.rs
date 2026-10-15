// Puts packaging/windows/tinyclient.ico in tinyclient-login.exe as icon resource 1, so the sign-in
// window's taskbar button and Explorer show tinyclient's icon. Nothing on other systems.
fn main() {
    #[cfg(windows)]
    {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let ico = manifest.join("../../packaging/windows/tinyclient.ico");
        println!("cargo:rerun-if-changed={}", ico.display());
        let rc = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("login.rc");
        let escaped = ico.display().to_string().replace('\\', "\\\\");
        std::fs::write(&rc, format!("1 ICON \"{escaped}\"\n")).unwrap();
        embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
    }
    println!("cargo:rerun-if-changed=build.rs");
}
