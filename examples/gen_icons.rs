//! Génère les icônes PNG/ICO à partir de `assets/logo.svg`.
//! `cargo run --example gen_icons`

use std::io::Write;

fn render(tree: &resvg::usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    resvg::render(tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().unwrap()
}

fn main() {
    let svg = std::fs::read("assets/logo.svg").unwrap();
    let tree = resvg::usvg::Tree::from_data(&svg, &Default::default()).unwrap();

    for size in [256, 512, 1024] {
        std::fs::write(format!("assets/icon-{size}.png"), render(&tree, size)).unwrap();
    }

    // ICO contenant des PNG (pris en charge depuis Windows Vista).
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| render(&tree, s)).collect();
    let mut ico = Vec::new();
    ico.write_all(&[0, 0, 1, 0]).unwrap();
    ico.write_all(&(sizes.len() as u16).to_le_bytes()).unwrap();
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, img) in sizes.iter().zip(&images) {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        ico.write_all(&[dim, dim, 0, 0, 1, 0, 32, 0]).unwrap();
        ico.write_all(&(img.len() as u32).to_le_bytes()).unwrap();
        ico.write_all(&offset.to_le_bytes()).unwrap();
        offset += img.len() as u32;
    }
    for img in &images {
        ico.write_all(img).unwrap();
    }
    std::fs::write("assets/tourdetable.ico", ico).unwrap();
    println!("Icônes générées dans assets/");
}
