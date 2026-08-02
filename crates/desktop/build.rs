fn main() {
    // Keep the first milestone self-contained while a final branded icon is
    // still pending. This is a valid 1x1 32-bit ICO used only by winres.
    const ICON: &[u8] = &[
        0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 48, 0, 0, 0, 22, 0, 0, 0, 40, 0, 0, 0, 1, 0, 0,
        0, 2, 0, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0x6f, 0x46, 0xd9, 0xff, 0, 0, 0, 0,
    ];
    let icon = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("brows.ico");
    std::fs::write(&icon, ICON).expect("write generated development icon");
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon));
    tauri_build::try_build(attributes).expect("tauri build failed");
}
