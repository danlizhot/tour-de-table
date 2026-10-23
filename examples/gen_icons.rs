//! Génère les icônes PNG/ICO et les tuiles du Microsoft Store à partir de `assets/logo.svg`.
//! `cargo run --example gen_icons`

use std::io::Write;

fn render(tree: &resvg::usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    resvg::render(tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().unwrap()
}

/// Logo centré sur un canevas transparent (tuiles Windows : la couleur de fond
/// du manifeste remplit le reste).
fn render_on(tree: &resvg::usvg::Tree, w: u32, h: u32, logo: u32) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    let scale = logo as f32 / tree.size().width();
    let (dx, dy) = ((w - logo) as f32 / 2.0, (h - logo) as f32 / 2.0);
    resvg::render(tree, resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
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

    // Tuiles du paquet MSIX (Microsoft Store).
    std::fs::create_dir_all("packaging/windows/Assets").unwrap();
    let tiles = [
        ("StoreLogo", 50, 50, 50),
        ("Square44x44Logo", 44, 44, 44),
        ("Square150x150Logo", 150, 150, 96),
        ("Wide310x150Logo", 310, 150, 96),
        ("SplashScreen", 620, 300, 200),
    ];
    for (name, w, h, logo) in tiles {
        std::fs::write(format!("packaging/windows/Assets/{name}.png"), render_on(&tree, w, h, logo)).unwrap();
    }
    println!("Icônes générées dans assets/");
}
