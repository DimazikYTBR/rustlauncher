//! Тема «Liquid Glass» для egui (бэкенд glow / OpenGL 3.3).
//! Всё рисуется примитивами egui::Painter (меши с вершинными цветами) — без шейдеров,
//! блюра и тяжёлых эффектов, поэтому работает быстро даже на старых видеокартах.

use eframe::egui::{
    self,
    epaint::{Mesh, Shadow},
    pos2, vec2, Align2, Color32, CursorIcon, FontFamily, FontId, Frame, Id, LayerId, Margin, Order, Painter, Pos2,
    Rect, Response, RichText, Rounding, Sense, Shape, Stroke, TextStyle, Ui, Visuals,
};
use std::f32::consts::TAU;

// ───────────────────────── Палитра ─────────────────────────

pub const CYAN: Color32 = Color32::from_rgb(0, 229, 255);
pub const INDIGO: Color32 = Color32::from_rgb(99, 102, 241);
pub const VIOLET: Color32 = Color32::from_rgb(168, 85, 247);
pub const ROSE: Color32 = Color32::from_rgb(244, 63, 94);
pub const NAVY: Color32 = Color32::from_rgb(6, 12, 30);
pub const TEXT: Color32 = Color32::from_rgb(226, 232, 248);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 160, 188);

/// Скругление углов окна (для безрамочного прозрачного окна).
pub const WINDOW_ROUNDING: f32 = 16.0;

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

// ───────────────────────── Глобальный стиль ─────────────────────────

/// Применяет тёмную стеклянную тему: скругления 14/10, увеличенные отступы, контрастная типографика.
pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = rgba(20, 24, 38, 238);
    v.window_stroke = Stroke::new(1.0, rgba(255, 255, 255, 40));
    v.window_rounding = Rounding::same(14.0);
    v.menu_rounding = Rounding::same(12.0);
    v.window_shadow = Shadow { offset: vec2(0.0, 10.0), blur: 30.0, spread: 0.0, color: Color32::from_black_alpha(120) };
    v.popup_shadow = v.window_shadow;
    v.extreme_bg_color = rgba(6, 9, 18, 150);
    v.faint_bg_color = rgba(255, 255, 255, 8);
    v.code_bg_color = rgba(255, 255, 255, 14);
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = with_alpha(CYAN, 70);
    v.selection.stroke = Stroke::new(1.0, CYAN);

    let r = Rounding::same(10.0);
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = rgba(255, 255, 255, 10);
    w.noninteractive.weak_bg_fill = rgba(255, 255, 255, 10);
    w.noninteractive.bg_stroke = Stroke::new(1.0, rgba(255, 255, 255, 26));
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.rounding = r;

    w.inactive.bg_fill = rgba(255, 255, 255, 22);
    w.inactive.weak_bg_fill = rgba(255, 255, 255, 18);
    w.inactive.bg_stroke = Stroke::new(1.0, rgba(255, 255, 255, 34));
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.rounding = r;

    w.hovered.bg_fill = with_alpha(CYAN, 34);
    w.hovered.weak_bg_fill = with_alpha(CYAN, 30);
    w.hovered.bg_stroke = Stroke::new(1.0, with_alpha(CYAN, 170));
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.hovered.rounding = r;
    w.hovered.expansion = 1.0;

    w.active.bg_fill = with_alpha(CYAN, 70);
    w.active.weak_bg_fill = with_alpha(CYAN, 60);
    w.active.bg_stroke = Stroke::new(1.5, CYAN);
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.rounding = r;

    w.open.bg_fill = with_alpha(CYAN, 26);
    w.open.weak_bg_fill = with_alpha(CYAN, 26);
    w.open.bg_stroke = Stroke::new(1.0, with_alpha(CYAN, 120));
    w.open.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.open.rounding = r;

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(16.0, 9.0);
    style.spacing.interact_size.y = 34.0;
    style.spacing.window_margin = Margin::same(16.0);
    style.spacing.slider_width = 240.0;
    style.spacing.combo_width = 220.0;
    style.text_styles.insert(TextStyle::Heading, FontId::new(26.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(12.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
    ctx.set_style(style);
}

// ───────────────────────── Геометрические хелперы ─────────────────────────

/// Контур скруглённого прямоугольника по часовой стрелке.
fn rounded_points(rect: Rect, radius: f32, seg: usize) -> Vec<Pos2> {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let corners = [
        (pos2(rect.min.x + r, rect.min.y + r), std::f32::consts::PI),
        (pos2(rect.max.x - r, rect.min.y + r), std::f32::consts::PI * 1.5),
        (pos2(rect.max.x - r, rect.max.y - r), 0.0),
        (pos2(rect.min.x + r, rect.max.y - r), std::f32::consts::FRAC_PI_2),
    ];
    let mut pts = Vec::with_capacity(4 * (seg + 1));
    for (center, start) in corners {
        for i in 0..=seg {
            let a = start + std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
            pts.push(center + vec2(a.cos(), a.sin()) * r);
        }
    }
    pts
}

/// Скруглённый прямоугольник с горизонтальным градиентом left -> right (веер из треугольников).
pub fn rounded_gradient(p: &Painter, rect: Rect, radius: f32, left: Color32, right: Color32) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let pts = rounded_points(rect, radius, 8);
    let n = pts.len() as u32;
    let mut m = Mesh::default();
    m.colored_vertex(rect.center(), lerp_color(left, right, 0.5));
    for pt in &pts {
        let t = ((pt.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        m.colored_vertex(*pt, lerp_color(left, right, t));
    }
    for i in 1..n {
        m.add_triangle(0, i, i + 1);
    }
    m.add_triangle(0, n, 1);
    p.add(Shape::mesh(m));
}

fn vertical_gradient(p: &Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut m = Mesh::default();
    m.colored_vertex(rect.left_top(), top);
    m.colored_vertex(rect.right_top(), top);
    m.colored_vertex(rect.right_bottom(), bottom);
    m.colored_vertex(rect.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(Shape::mesh(m));
}

/// Мягкая светящаяся сфера: радиальный градиент с плавным затуханием.
fn radial_glow(p: &Painter, center: Pos2, radius: f32, col: Color32, alpha: u8) {
    const N: usize = 48;
    let mut m = Mesh::default();
    m.colored_vertex(center, with_alpha(col, alpha));
    for i in 0..N {
        let a = i as f32 / N as f32 * TAU;
        m.colored_vertex(center + vec2(a.cos(), a.sin()) * radius * 0.5, with_alpha(col, (alpha as f32 * 0.38) as u8));
    }
    for i in 0..N {
        let a = i as f32 / N as f32 * TAU;
        m.colored_vertex(center + vec2(a.cos(), a.sin()) * radius, Color32::TRANSPARENT);
    }
    let n = N as u32;
    for i in 0..n {
        let a = 1 + i;
        let b = 1 + (i + 1) % n;
        m.add_triangle(0, a, b);
        m.add_triangle(a, a + n, b + n);
        m.add_triangle(a, b + n, b);
    }
    p.add(Shape::mesh(m));
}

// ───────────────────────── Фон ─────────────────────────

/// Глубокий сине-графитовый градиент, три светящиеся сферы (cyan / indigo / violet), сетка и блик.
pub fn draw_liquid_background(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    let p = ctx.layer_painter(LayerId::background());
    let r = WINDOW_ROUNDING;
    let (w, h) = (rect.width(), rect.height());
    let top = Color32::from_rgb(7, 12, 30);
    let bottom = Color32::from_rgb(17, 14, 38);

    // Основа со скруглёнными углами + вертикальный градиент между ними.
    p.rect_filled(rect, Rounding::same(r), top);
    p.rect_filled(
        Rect::from_min_max(pos2(rect.min.x, rect.max.y - 2.0 * r), rect.max),
        Rounding::same(r),
        bottom,
    );
    vertical_gradient(
        &p,
        Rect::from_min_max(pos2(rect.min.x, rect.min.y + r), pos2(rect.max.x, rect.max.y - r)),
        top,
        bottom,
    );

    // Сферы (расположены так, чтобы не задевать скруглённые углы окна).
    let q = p.with_clip_rect(rect);
    let u = h.min(w * 0.7);
    radial_glow(&q, pos2(rect.min.x + w * 0.30, rect.min.y + h * 0.36), u * 0.50, CYAN, 66);
    radial_glow(&q, pos2(rect.min.x + w * 0.64, rect.min.y + h * 0.22), u * 0.48, INDIGO, 78);
    radial_glow(&q, pos2(rect.min.x + w * 0.72, rect.min.y + h * 0.84), u * 0.44, VIOLET, 66);

    // Тонкая сетка (внутри скруглённых углов).
    let grid = Stroke::new(1.0, rgba(255, 255, 255, 6));
    let step = 48.0;
    let mut x = rect.min.x + r + step;
    while x < rect.max.x - r {
        q.line_segment([pos2(x, rect.min.y + r), pos2(x, rect.max.y - r)], grid);
        x += step;
    }
    let mut y = rect.min.y + r + step;
    while y < rect.max.y - r {
        q.line_segment([pos2(rect.min.x + r, y), pos2(rect.max.x - r, y)], grid);
        y += step;
    }

    // Диагональный блик «стекла».
    q.add(Shape::convex_polygon(
        vec![
            pos2(rect.min.x + r, rect.min.y + r),
            pos2(rect.min.x + w * 0.58, rect.min.y + r),
            pos2(rect.min.x + w * 0.22, rect.min.y + h * 0.42),
            pos2(rect.min.x + r, rect.min.y + h * 0.42),
        ],
        rgba(255, 255, 255, 5),
        Stroke::NONE,
    ));
}

/// Тонкая стеклянная обводка всего окна (рисуется поверх фона).
pub fn draw_window_border(ctx: &egui::Context) {
    let p = ctx.layer_painter(LayerId::new(Order::Middle, Id::new("window_border")));
    p.rect_stroke(
        ctx.screen_rect().shrink(0.5),
        Rounding::same(WINDOW_ROUNDING),
        Stroke::new(1.0, rgba(255, 255, 255, 46)),
    );
}

// ───────────────────────── Контейнеры ─────────────────────────

fn card_impl(ui: &mut Ui, full_height: bool, add_contents: impl FnOnce(&mut Ui)) {
    let resp = Frame::none()
        .fill(rgba(24, 28, 42, 175))
        .stroke(Stroke::new(1.0, rgba(255, 255, 255, 38)))
        .rounding(Rounding::same(14.0))
        .shadow(Shadow { offset: vec2(0.0, 8.0), blur: 24.0, spread: 0.0, color: Color32::from_black_alpha(90) })
        .inner_margin(Margin::same(18.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if full_height {
                ui.set_min_height(ui.available_height());
            }
            add_contents(ui);
        });
    let rect = resp.response.rect;
    let p = ui.painter();
    // Верхний внутренний блик: 1px светлая линия + лёгкая вуаль.
    vertical_gradient(
        p,
        Rect::from_min_max(pos2(rect.min.x + 14.0, rect.min.y + 1.0), pos2(rect.max.x - 14.0, rect.min.y + 40.0)),
        rgba(255, 255, 255, 12),
        rgba(255, 255, 255, 0),
    );
    p.line_segment(
        [pos2(rect.min.x + 14.0, rect.min.y + 1.0), pos2(rect.max.x - 14.0, rect.min.y + 1.0)],
        Stroke::new(1.0, rgba(255, 255, 255, 78)),
    );
}

/// Стеклянная карточка по высоте содержимого.
pub fn glass_card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    card_impl(ui, false, add_contents);
}

/// Стеклянная карточка, занимающая всю доступную высоту (для списков и консоли).
pub fn glass_card_full(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    card_impl(ui, true, add_contents);
}

pub fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(12.0).strong().color(with_alpha(CYAN, 215)));
}

// ───────────────────────── Виджеты ─────────────────────────

/// Логотип RustLauncher: картинка из assets/logo.png, а если она не загрузилась — градиентный квадрат с буквой R.
pub fn logo(ui: &mut Ui, tex: Option<&egui::TextureHandle>) {
    if let Some(t) = tex {
        let sz = t.size_vec2();
        let h = 26.0;
        let size = vec2(sz.x * h / sz.y, h);
        ui.add(egui::Image::new(egui::load::SizedTexture::new(t.id(), size)));
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect.expand(3.0), Rounding::same(12.0), with_alpha(CYAN, 28));
    rounded_gradient(p, rect, 9.0, CYAN, INDIGO);
    p.rect_stroke(rect, Rounding::same(9.0), Stroke::new(1.0, rgba(255, 255, 255, 90)));
    p.text(rect.center(), Align2::CENTER_CENTER, "R", FontId::proportional(18.0), NAVY);
}

#[derive(Clone, Copy)]
pub enum WindowButton {
    Minimize,
    Close,
}

/// Круглая кнопка управления окном, глиф рисуется линиями (без зависимости от шрифта).
pub fn window_button(ui: &mut Ui, kind: WindowButton) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let base = match kind {
        WindowButton::Minimize => CYAN,
        WindowButton::Close => ROSE,
    };
    let p = ui.painter();
    p.circle_filled(rect.center(), 14.0, with_alpha(base, (16.0 + hover * 90.0) as u8));
    p.circle_stroke(rect.center(), 14.0, Stroke::new(1.0, with_alpha(base, (40.0 + hover * 150.0) as u8)));
    let col = lerp_color(TEXT_DIM, Color32::WHITE, hover);
    let st = Stroke::new(1.7, col);
    let c = rect.center();
    match kind {
        WindowButton::Minimize => p.line_segment([c + vec2(-5.0, 3.0), c + vec2(5.0, 3.0)], st),
        WindowButton::Close => {
            p.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], st);
            p.line_segment([c + vec2(-4.5, 4.5), c + vec2(4.5, -4.5)], st)
        }
    }
    resp
}

/// Сегментные «пилюли»-вкладки с неоново-голубой подсветкой активной. Возвращает true при смене вкладки.
pub fn pill_tabs<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> bool {
    let font = FontId::proportional(15.0);
    let (pad, h, gap, inset) = (18.0, 38.0, 4.0, 4.0);
    let widths: Vec<f32> = items
        .iter()
        .map(|(_, t)| ui.painter().layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x + pad * 2.0)
        .collect();
    let total = widths.iter().sum::<f32>() + gap * items.len().saturating_sub(1) as f32 + inset * 2.0;
    let (outer, _) = ui.allocate_exact_size(vec2(total, h + inset * 2.0), Sense::hover());
    let p = ui.painter().clone();
    let round = Rounding::same(outer.height() / 2.0);
    p.rect_filled(outer, round, rgba(255, 255, 255, 13));
    p.rect_stroke(outer, round, Stroke::new(1.0, rgba(255, 255, 255, 34)));

    let mut changed = false;
    let mut x = outer.min.x + inset;
    for (i, (val, label)) in items.iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, outer.min.y + inset), vec2(widths[i], h));
        x += widths[i] + gap;
        let id = ui.id().with(("pill", i));
        let resp = ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() && *current != *val {
            *current = *val;
            changed = true;
        }
        let active = *current == *val;
        let t = ui.ctx().animate_bool_with_time(id.with("active"), active, 0.18);
        let hover = ui.ctx().animate_bool_with_time(id.with("hover"), resp.hovered(), 0.12);
        if t > 0.01 {
            for k in 1..=3 {
                let e = k as f32 * 2.5;
                p.rect_filled(rect.expand(e), Rounding::same(h / 2.0 + e), with_alpha(CYAN, ((14 - k * 3) as f32 * t) as u8));
            }
            rounded_gradient(&p, rect, h / 2.0, with_alpha(CYAN, (74.0 * t) as u8), with_alpha(INDIGO, (64.0 * t) as u8));
            p.rect_stroke(rect, Rounding::same(h / 2.0), Stroke::new(1.2, with_alpha(CYAN, (200.0 * t) as u8)));
        } else if hover > 0.01 {
            p.rect_filled(rect, Rounding::same(h / 2.0), rgba(255, 255, 255, (16.0 * hover) as u8));
        }
        let color = lerp_color(lerp_color(TEXT_DIM, TEXT, hover), Color32::WHITE, t);
        p.text(rect.center(), Align2::CENTER_CENTER, label, font.clone(), color);
    }
    changed
}

/// Крупная акцентная кнопка с градиентом, ореолом и стеклянным бликом.
pub fn accent_button(ui: &mut Ui, text: &str, size: egui::Vec2, from: Color32, to: Color32, text_color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let pressed = resp.is_pointer_button_down_on();
    let body = if pressed { rect.shrink(1.5) } else { rect };
    let r = (body.height() * 0.28).min(20.0);
    let p = ui.painter();

    for k in 1..=5 {
        let e = k as f32 * (2.5 + 2.0 * hover);
        let a = ((17 - k * 3).max(2) as f32 * (1.0 + hover)).min(60.0) as u8;
        p.rect_filled(body.expand(e), Rounding::same(r + e), with_alpha(from, a));
    }
    let lift = |c: Color32| lerp_color(c, Color32::WHITE, 0.14 * hover);
    rounded_gradient(p, body, r, lift(from), lift(to));
    // Стеклянный блик по верхней половине + обводка.
    p.rect_filled(
        Rect::from_min_max(body.min + vec2(3.0, 3.0), pos2(body.max.x - 3.0, body.center().y)),
        Rounding { nw: r - 2.0, ne: r - 2.0, sw: 6.0, se: 6.0 },
        rgba(255, 255, 255, 36),
    );
    p.rect_stroke(body, Rounding::same(r), Stroke::new(1.2, rgba(255, 255, 255, 90)));
    p.text(
        body.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional((body.height() * 0.38).clamp(14.0, 28.0)),
        text_color,
    );
    resp
}

/// Плавный стеклянный прогресс-бар на всю доступную ширину.
pub fn glass_progress(ui: &mut Ui, fraction: f32) {
    let h = 20.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let f = ui.ctx().animate_value_with_time(resp.id, fraction.clamp(0.0, 1.0), 0.25);
    let p = ui.painter();
    let round = Rounding::same(h / 2.0);
    p.rect_filled(rect, round, rgba(255, 255, 255, 16));
    p.rect_stroke(rect, round, Stroke::new(1.0, rgba(255, 255, 255, 36)));
    if f > 0.001 {
        let fill = Rect::from_min_size(rect.min, vec2((rect.width() * f).max(h), h)).shrink(2.0);
        p.rect_filled(fill.expand(3.0), Rounding::same(h / 2.0 + 1.0), with_alpha(CYAN, 22));
        rounded_gradient(p, fill, (h - 4.0) / 2.0, CYAN, INDIGO);
        p.rect_filled(
            Rect::from_min_max(fill.min + vec2(4.0, 1.0), pos2(fill.max.x - 4.0, fill.center().y)),
            Rounding::same(4.0),
            rgba(255, 255, 255, 40),
        );
    }
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
        FontId::proportional(12.0),
        Color32::WHITE,
    );
}