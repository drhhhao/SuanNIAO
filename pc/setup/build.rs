include!("../assets/resource.rs");

fn main() {
    // 安装包的版本号跟着主界面走（这个 crate 自己的版本号不用管）
    let ui = std::fs::read_to_string("../ui/Cargo.toml").expect("读不到 ../ui/Cargo.toml");
    let version = ui
        .lines()
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("../ui/Cargo.toml 里没有版本号");
    std::env::set_var("CARGO_PKG_VERSION", version);
    println!("cargo:rerun-if-changed=../ui/Cargo.toml");
    println!("cargo:rerun-if-changed=setup.manifest");
    // 清单只给启动程序：按普通用户权限运行（不弹管理员确认），高分屏下出错的提示框不发糊
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("setup.manifest");
    let extra = format!("\n1 24 \"{}\"\n", manifest.display().to_string().replace('\\', "/"));
    let rc = write_app_rc("蒜鸟安装程序", "SuanNIAO-Setup.exe", &extra);
    let _ = embed_resource::compile_for(&rc, ["SuanNIAO-Setup"], embed_resource::NONE);
}
