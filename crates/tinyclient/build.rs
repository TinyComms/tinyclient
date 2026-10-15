// Puts packaging/windows/tinyclient.ico in tinyclient.exe as icon resource 1: the one Explorer and
// the taskbar show, and the one GPUI's Windows window loads. Nothing on other systems.
fn main() {
    #[cfg(windows)]
    {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let ico = manifest.join("../../packaging/windows/tinyclient.ico");
        println!("cargo:rerun-if-changed={}", ico.display());
        let rc = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("tinyclient.rc");
        let escaped = ico.display().to_string().replace('\\', "\\\\");
        std::fs::write(&rc, format!("1 ICON \"{escaped}\"\n")).unwrap();
        embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
    }
    println!("cargo:rerun-if-changed=build.rs");
}
