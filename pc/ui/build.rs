include!("../assets/resource.rs");

fn main() {
    slint_build::compile("ui/app.slint").expect("编译界面失败");
    embed_app_resource("蒜鸟", "SuanNIAO.exe");
}
