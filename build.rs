//! On macOS, embed an Info.plist in the binary. It gives irie its own identity
//! for privacy prompts: the web player's helper asks for "System Audio
//! Recording" (to drive the visualizer) as irie, not as your terminal app.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.frandika.irie</string>
  <key>CFBundleName</key><string>irie</string>
  <key>CFBundleDisplayName</key><string>irie</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>NSAudioCaptureUsageDescription</key><string>irie listens to its own music player's sound to draw the audio visualizer. Nothing is recorded or sent anywhere.</string>
</dict>
</plist>
"#
    );
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("Info.plist");
    std::fs::write(&out, plist).unwrap();
    println!(
        "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        out.display()
    );
}
