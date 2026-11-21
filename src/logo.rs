//! Логотип RustLauncher: иконка окна и картинка для шапки.
//! Файл логотипа должен лежать в assets/logo.png (рядом с Cargo.toml) — он вшивается в exe при сборке.

use eframe::egui;
use image::{
    imageops::{self, FilterType},
    Rgba, RgbaImage,
};

const LOGO_BYTES: &[u8] = include_bytes!("../assets/logo.png");

fn decode() -> Option<RgbaImage> {
    image::load_from_memory(LOGO_BYTES).ok().map(|i| i.to_rgba8())
}

/// Иконка окна и панели задач: центрированный квадрат 256×256 с оригинальным тёмным фоном.
pub fn window_icon() -> Option<egui::IconData> {
    let img = decode()?;
    let s = img.width().min(img.height());
    let (x, y) = ((img.width() - s) / 2, (img.height() - s) / 2);
    let square = imageops::crop_imm(&img, x, y, s, s).to_image();
    let small = imageops::resize(&square, 256, 256, FilterType::Triangle);
    Some(egui::IconData { rgba: small.into_raw(), width: 256, height: 256 })
}

/// Картинка для шапки: обрезана по буквам, тёмный фон превращён в прозрачность,
/// уменьшена до высоты 72 px (маленькая текстура — минимум нагрузки на видеокарту).
pub fn titlebar_image() -> Option<egui::ColorImage> {
    let img = decode()?;
    let bg = *img.get_pixel(0, 0);
    let diff = |p: &Rgba<u8>| -> i32 {
        (0..3).map(|i| (p[i] as i32 - bg[i] as i32).abs()).max().unwrap_or(0)
    };

    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, p) in img.enumerate_pixels() {
        if diff(p) > 40 {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
    }
    if minx > maxx || miny > maxy {
        return None;
    }
    let pad = (((maxx - minx).max(maxy - miny)) as f32 * 0.03) as u32;
    let x0 = minx.saturating_sub(pad);
    let y0 = miny.saturating_sub(pad);
    let x1 = (maxx + pad).min(img.width() - 1);
    let y1 = (maxy + pad).min(img.height() - 1);
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);

    let crop = imageops::crop_imm(&img, x0, y0, w, h).to_image();
    let target_h = 72u32;
    let target_w = ((w as f32 * target_h as f32 / h as f32).round() as u32).max(1);
    let small = imageops::resize(&crop, target_w, target_h, FilterType::Lanczos3);

    let mut rgba = small.into_raw();
    for px in rgba.chunks_exact_mut(4) {
        let d = (0..3).map(|i| (px[i] as i32 - bg[i] as i32).abs()).max().unwrap_or(0);
        px[3] = (((d - 14).max(0) * 255) / 50).min(255) as u8;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([target_w as usize, target_h as usize], &rgba))
}