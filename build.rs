fn main() {
    // Встраиваем иконку в lumen.exe (нужен rc.exe — он есть на Windows с Visual Studio Build Tools).
    println!("cargo:rerun-if-changed=lumen.rc");
    println!("cargo:rerun-if-changed=assets/lumen.ico");
    let _ = embed_resource::compile("lumen.rc", embed_resource::NONE);
}
