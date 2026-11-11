fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref()==Ok("windows") {
        println!("cargo:rerun-if-changed=lumen.rc");
        println!("cargo:rerun-if-changed=assets/lumen.ico");
        let _=embed_resource::compile("lumen.rc",embed_resource::NONE);
    }
}