// 两个 exe 的 build.rs 都 include! 这个文件：把 app.rc.in 填好（图标路径、文件说明、版本号），编进 exe
fn embed_app_resource(description: &str, exe: &str) {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let assets = std::path::Path::new(&env("CARGO_MANIFEST_DIR")).join("../assets");
    let version = env("CARGO_PKG_VERSION");
    let mut numbers = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let mut rc = std::fs::read_to_string(assets.join("app.rc.in")).expect("读不到 assets/app.rc.in");
    for (key, value) in [
        ("{icon}", assets.join("icon.ico").display().to_string().replace('\\', "/")),
        ("{description}", description.to_string()),
        ("{exe}", exe.to_string()),
        ("{version}", version.clone()),
        ("{v1}", numbers.next().unwrap_or(0).to_string()),
        ("{v2}", numbers.next().unwrap_or(0).to_string()),
        ("{v3}", numbers.next().unwrap_or(0).to_string()),
    ] {
        rc = rc.replace(key, &value);
    }
    let out = std::path::Path::new(&env("OUT_DIR")).join("app.rc");
    std::fs::write(&out, rc).expect("写不了 app.rc");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../assets/app.rc.in");
    println!("cargo:rerun-if-changed=../assets/resource.rs");
    println!("cargo:rerun-if-changed=../assets/icon.ico");
    let _ = embed_resource::compile(&out, embed_resource::NONE);
}
