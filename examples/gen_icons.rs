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

/// Logo centré sur un fond dégradé bleu Québec (mêmes couleurs que le logo).
fn render_on_blue(tree: &resvg::usvg::Tree, w: u32, h: u32, logo: u32) -> Vec<u8> {
    use resvg::tiny_skia::{Color, GradientStop, LinearGradient, Paint, Point, Rect, SpreadMode, Transform};
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    let shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(w as f32, h as f32),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x1A, 0x5B, 0xD6, 0xFF)),
            GradientStop::new(1.0, Color::from_rgba8(0x00, 0x3D, 0xA5, 0xFF)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    let paint = Paint { shader, ..Default::default() };
    pixmap.fill_rect(Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(), &paint, Transform::identity(), None);
    let scale = logo as f32 / tree.size().width();
    let (dx, dy) = ((w - logo) as f32 / 2.0, (h - logo) as f32 / 2.0);
    resvg::render(tree, Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
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

    // Logos de la fiche du Store (Partner Center → Store logos). Fond bleu plein :
    // le Store affiche ces images telles quelles, sans couleur de fond.
    std::fs::create_dir_all("packaging/windows/store-logos").unwrap();
    // Sans le carré arrondi du logo : la table flotte directement sur le fond bleu.
    let sans_fond = String::from_utf8(svg.clone()).unwrap().replace(r#"<rect width="100" height="100" rx="22" fill="url(#fond)"/>"#, "");
    let tree_sans_fond = resvg::usvg::Tree::from_data(sans_fond.as_bytes(), &Default::default()).unwrap();
    let logos = [
        ("icone-1x1-300", 300, 300, 300, false),
        ("boite-1x1-1080", 1080, 1080, 760, true),
        ("affiche-9x16-720x1080", 720, 1080, 480, true),
    ];
    for (name, w, h, logo, fond) in logos {
        let png = if fond { render_on_blue(&tree_sans_fond, w, h, logo) } else { render_on(&tree, w, h, logo) };
        std::fs::write(format!("packaging/windows/store-logos/{name}.png"), png).unwrap();
    }
    println!("Icônes générées dans assets/");
}
