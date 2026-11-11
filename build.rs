fn main() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    println!("cargo:rerun-if-changed=ui/ui.html");
    let html = std::fs::read_to_string("ui/ui.html")
        .expect("UI source")
        .replace("\r\n", "\n");
    let mut hashes = Vec::new();
    for block in html.split("<script>").skip(1) {
        let script = block.split_once("</script>").expect("closed UI script").0;
        hashes.push(format!(
            "'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script.as_bytes()))
        ));
    }
    assert!(!hashes.is_empty());
    let html = html.replacen(
        "script-src 'unsafe-inline'",
        &format!("script-src {}", hashes.join(" ")),
        1,
    );
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("ui.html"),
        html,
    )
    .unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=lumen.rc");
        println!("cargo:rerun-if-changed=assets/lumen.ico");
        let _ = embed_resource::compile("lumen.rc", embed_resource::NONE);
    }
}
