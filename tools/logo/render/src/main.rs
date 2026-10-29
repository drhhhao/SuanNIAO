// 用法：sn-render <in.svg> <out.png> <宽> [高]
// 把 SVG 按给定像素尺寸渲染成 PNG（透明背景），和 Slint 用的是同一个 resvg。
use resvg::{tiny_skia, usvg};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("用法：sn-render <in.svg> <out.png> <宽> [高]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("读不到 SVG");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("SVG 解析失败");
    let w: u32 = args[3].parse().unwrap();
    let h: u32 = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(w);
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
    let transform = tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.save_png(&args[2]).expect("写 PNG 失败");
}
