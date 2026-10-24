//! Pure-white Liquid Glass design tokens, painting and vector icons.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Mesh, Painter, Pos2, Rect, Sense, Shape,
    Stroke, StrokeKind, TextStyle, pos2, vec2,
};

use crate::theme::geometry::rounded_outline;

/// The launcher background is intentionally pure white.
pub(crate) const BG: Color32 = Color32::WHITE;
/// Primary text on glass.
pub(crate) const INK: Color32 = Color32::from_rgb(27, 31, 41);
/// Secondary text.
pub(crate) const MUTED: Color32 = Color32::from_rgb(108, 116, 132);
/// Tertiary text and metadata.
pub(crate) const FAINT: Color32 = Color32::from_rgb(151, 158, 171);
/// Hairline glass perimeter.
pub(crate) const BORDER: Color32 = Color32::from_rgba_unmultiplied_const(219, 225, 235, 170);
/// A very light cool tint used for neutral chips.
pub(crate) const NEUTRAL_SOFT: Color32 = Color32::from_rgb(245, 247, 251);
/// Selected navigation and primary accent.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(76, 122, 219);
/// Darker accent text that remains readable on pale blue glass.
pub(crate) const ACCENT_INK: Color32 = Color32::from_rgb(46, 84, 166);
/// Pale blue selected surface.
pub(crate) const ACCENT_SOFT: Color32 = Color32::from_rgb(232, 241, 255);
/// Pale blue hairline.
pub(crate) const ACCENT_LINE: Color32 = Color32::from_rgb(194, 214, 248);
/// Success green.
pub(crate) const GREEN: Color32 = Color32::from_rgb(52, 139, 102);
/// Pale success surface.
pub(crate) const GREEN_SOFT: Color32 = Color32::from_rgb(233, 247, 240);
/// Warning orange.
pub(crate) const ORANGE: Color32 = Color32::from_rgb(170, 106, 36);
/// Pale warning surface.
pub(crate) const ORANGE_SOFT: Color32 = Color32::from_rgb(255, 246, 231);
/// Loader purple.
pub(crate) const PURPLE: Color32 = Color32::from_rgb(110, 82, 177);
/// Pale loader surface.
pub(crate) const PURPLE_SOFT: Color32 = Color32::from_rgb(244, 240, 255);
/// Error red.
pub(crate) const DANGER: Color32 = Color32::from_rgb(178, 62, 76);
/// Pale error surface.
pub(crate) const DANGER_SOFT: Color32 = Color32::from_rgb(255, 240, 242);
/// Logs use a slightly recessed, cool-tinted well so the shell stays white.
pub(crate) const LOG_BG: Color32 = Color32::from_rgba_unmultiplied_const(245, 248, 253, 235);
/// Log text.
pub(crate) const LOG_INK: Color32 = Color32::from_rgb(46, 56, 76);

pub(crate) const RADIUS_SHELL: u8 = 24;
pub(crate) const RADIUS_RAIL: u8 = 26;
pub(crate) const RADIUS_SECTION: u8 = 22;
pub(crate) const RADIUS_CARD: u8 = 24;
pub(crate) const RADIUS_BUTTON: u8 = 14;

/// Installs the light Liquid Glass theme across every launcher surface.
pub(crate) fn configure(context: &egui::Context) {
    context.set_theme(egui::Theme::Light);
    context.all_styles_mut(|style| {
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = BG;
        visuals.window_fill = Color32::from_rgb(252, 253, 255);
        visuals.extreme_bg_color = Color32::from_rgb(244, 247, 251);
        visuals.faint_bg_color = Color32::from_rgb(249, 250, 253);
        visuals.override_text_color = Some(INK);
        visuals.hyperlink_color = ACCENT;
        visuals.selection.bg_fill = ACCENT_SOFT;
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(247, 249, 253);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(250, 251, 254);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, INK);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(238, 244, 255);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(244, 248, 255);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_LINE);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, INK);
        visuals.widgets.active.bg_fill = ACCENT_SOFT;
        visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, INK);
        visuals.widgets.open = visuals.widgets.hovered;
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(10);
        }
        style.visuals = visuals;
        style.spacing.item_spacing = vec2(9.0, 9.0);
        style.spacing.button_padding = vec2(12.0, 7.0);
        style.spacing.interact_size = vec2(40.0, 32.0);
        style.spacing.scroll.bar_width = 7.0;
        style.animation_time = 0.18;
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(28.0, FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        );
    });
}

/// A plain glass frame used for content sections.
pub(crate) fn section_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 246))
        .corner_radius(RADIUS_SECTION)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(20, 18))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 22,
            spread: 0,
            color: Color32::from_rgba_unmultiplied(57, 66, 91, 7),
        })
}

/// Returns a color with a replacement alpha channel.
pub(crate) fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    // `Color32` stores premultiplied channels. Reading `r()`/`g()`/`b()` and
    // rebuilding through the unmultiplied constructor would premultiply twice
    // and darken every translucent surface.
    let [red, green, blue, _] = color.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

/// Blends two colors. `t` is clamped to the unit interval.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn blend(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

/// Interpolates including alpha; used for hover-only glass tints.
///
/// `Color32` is premultiplied, so interpolation happens directly on its stored
/// channels and the result is rebuilt as premultiplied. Reading the channels
/// and passing them to `from_rgba_unmultiplied` would multiply by alpha a
/// second time and turn a white fade-to-transparent into a grey band.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn mix_rgba(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let from = from.to_array();
    let to = to.to_array();
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
        channel(from[3], to[3]),
    )
}

/// Paints a smooth linear gradient clipped to a rounded rectangle.
///
/// A vertex-colored mesh keeps corners crisp and avoids stacking strips on
/// cards that animate every frame.
pub(crate) fn gradient_rect(
    painter: &Painter,
    rect: Rect,
    corners: CornerRadius,
    from: Color32,
    to: Color32,
    horizontal: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let outline = rounded_outline(rect, corners, 10);
    if outline.len() < 3 {
        return;
    }
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), mix_rgba(from, to, 0.5));
    for point in &outline {
        let t = if horizontal {
            (point.x - rect.left()) / rect.width()
        } else {
            (point.y - rect.top()) / rect.height()
        };
        mesh.colored_vertex(*point, mix_rgba(from, to, t));
    }
    let count = u32::try_from(outline.len()).unwrap_or(0);
    for index in 0..count {
        mesh.add_triangle(0, 1 + index, 1 + (index + 1) % count);
    }
    painter.add(Shape::mesh(mesh));
}

/// Rounded-rectangle outline geometry, ordered clockwise.
pub(crate) mod geometry {
    use super::{CornerRadius, Pos2, Rect, pos2};

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn rounded_outline(
        rect: Rect,
        corners: CornerRadius,
        segments_per_corner: usize,
    ) -> Vec<Pos2> {
        let segments = segments_per_corner.max(1);
        let radii = [
            (
                corners.nw,
                pos2(
                    rect.left() + f32::from(corners.nw),
                    rect.top() + f32::from(corners.nw),
                ),
                std::f32::consts::PI,
                std::f32::consts::FRAC_PI_2,
            ),
            (
                corners.ne,
                pos2(
                    rect.right() - f32::from(corners.ne),
                    rect.top() + f32::from(corners.ne),
                ),
                std::f32::consts::FRAC_PI_2 * 3.0,
                std::f32::consts::FRAC_PI_2,
            ),
            (
                corners.se,
                pos2(
                    rect.right() - f32::from(corners.se),
                    rect.bottom() - f32::from(corners.se),
                ),
                0.0,
                std::f32::consts::FRAC_PI_2,
            ),
            (
                corners.sw,
                pos2(
                    rect.left() + f32::from(corners.sw),
                    rect.bottom() - f32::from(corners.sw),
                ),
                std::f32::consts::FRAC_PI_2,
                std::f32::consts::FRAC_PI_2,
            ),
        ];
        let mut points = Vec::with_capacity(segments * 4 + 4);
        for (radius, center, start, sweep) in radii {
            let radius = f32::from(radius);
            if radius <= 0.5 {
                points.push(center);
                continue;
            }
            for step in 0..=segments {
                let angle = start + sweep * (step as f32 / segments as f32);
                points.push(pos2(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                ));
            }
        }
        points
    }
}

/// Paints the outer white window shell with a soft edge and corner rounding.
///
/// The radius becomes zero while maximized so transparent window corners do
/// not reveal the desktop.
pub(crate) fn paint_shell(painter: &Painter, rect: Rect, radius: u8) {
    let corners = CornerRadius::same(radius);
    // A barely visible halo separates the frameless shell from any desktop
    // background while keeping the interior pure white.
    painter.rect_filled(
        rect.shrink(1.0).translate(vec2(0.0, 6.0)),
        corners,
        Color32::from_rgba_unmultiplied(51, 60, 84, 12),
    );
    painter.rect_filled(rect, corners, BG);
    if radius > 0 {
        painter.rect_stroke(
            rect.shrink(0.5),
            corners,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(203, 211, 224, 120)),
            StrokeKind::Inside,
        );
    }
}

/// Paints a floating translucent glass surface.
///
/// `hover` is an animated 0–1 value. `accent` optionally tints the glass with
/// a loader or semantic color; colors stay pale rather than opaque.
#[allow(
    clippy::too_many_arguments,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn paint_glass(
    painter: &Painter,
    rect: Rect,
    radius: u8,
    hover: f32,
    accent: Option<Color32>,
    strong_shadow: bool,
) {
    if rect.width() <= 2.0 || rect.height() <= 2.0 {
        return;
    }
    let hover = hover.clamp(0.0, 1.0);
    let corners = CornerRadius::same(radius);
    // Three low-alpha layers approximate a soft diffuse shadow without the
    // cost of a blurred backdrop. A single hard rectangle reads as a grey band
    // under the glass, especially on the pure-white shell.
    let shadow_alpha = if strong_shadow { 9.0 } else { 6.0 } + hover * 3.0;
    for (offset, expand, weight) in [
        (3.0, 1.0, 0.55_f32),
        (6.0, 3.0, 0.75),
        (10.0 + hover * 4.0, 5.0 + hover * 2.0, 1.0),
    ] {
        painter.rect_filled(
            rect.translate(vec2(0.0, offset)).expand(expand),
            CornerRadius::same(radius.saturating_add(u8::try_from(expand as u32).unwrap_or(0))),
            Color32::from_rgba_unmultiplied(74, 88, 116, (shadow_alpha * weight).round() as u8),
        );
    }

    let top = Color32::from_rgba_unmultiplied(255, 255, 255, 248);
    let bottom =
        Color32::from_rgba_unmultiplied(245 + u8::from(accent.is_some()) * 2, 248, 253, 238);
    gradient_rect(painter, rect, corners, top, bottom, false);

    if let Some(accent) = accent {
        let tint = mix_rgba(
            with_alpha(accent, 0),
            with_alpha(accent, 38),
            0.55 + hover * 0.2,
        );
        painter.rect_filled(rect.shrink(0.5), corners, tint);
    }

    // Top-edge gloss and a faint lower refraction band make the surface read
    // as a thick pane without expensive background blur.
    let gloss = Rect::from_min_size(
        pos2(rect.left() + 1.0, rect.top() + 1.0),
        vec2(
            (rect.width() - 2.0).max(0.0),
            (rect.height() * 0.42).min(46.0),
        ),
    );
    gradient_rect(
        painter,
        gloss,
        CornerRadius {
            nw: radius.saturating_sub(1),
            ne: radius.saturating_sub(1),
            sw: 0,
            se: 0,
        },
        Color32::from_rgba_unmultiplied(255, 255, 255, if strong_shadow { 120 } else { 82 }),
        Color32::from_rgba_unmultiplied(255, 255, 255, 0),
        false,
    );
    let low_edge = Rect::from_min_size(
        pos2(rect.left() + f32::from(radius) * 0.4, rect.bottom() - 1.5),
        vec2((rect.width() - f32::from(radius) * 0.8).max(0.0), 1.0),
    );
    painter.rect_filled(
        low_edge,
        0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 175),
    );

    let border = if hover > 0.02 {
        mix_rgba(
            Color32::from_rgba_unmultiplied(199, 208, 223, 150),
            with_alpha(accent.unwrap_or(ACCENT), 170),
            hover * 0.7,
        )
    } else {
        Color32::from_rgba_unmultiplied(199, 208, 223, 150)
    };
    painter.rect_stroke(
        rect.shrink(0.5),
        corners,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    painter.rect_stroke(
        rect.shrink(1.5),
        CornerRadius::same(radius.saturating_sub(1)),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 185)),
        StrokeKind::Inside,
    );
}

/// Glass highlight for the selected top-navigation item.
pub(crate) fn paint_selection_glass(painter: &Painter, rect: Rect, radius: u8) {
    let corners = CornerRadius::same(radius);
    gradient_rect(
        painter,
        rect,
        corners,
        Color32::from_rgba_unmultiplied(226, 239, 255, 235),
        Color32::from_rgba_unmultiplied(243, 248, 255, 225),
        true,
    );
    painter.rect_stroke(
        rect.shrink(0.5),
        corners,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(174, 202, 246, 190)),
        StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_min_size(
            pos2(rect.left() + 8.0, rect.top() + 1.0),
            vec2((rect.width() - 16.0).max(0.0), 1.0),
        ),
        0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 220),
    );
}

/// Paints a glass action button and returns its response.
#[allow(clippy::too_many_arguments)]
pub(crate) fn glass_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    enabled: bool,
    accent: Option<Color32>,
    icon: Option<Icon>,
    height: f32,
) -> egui::Response {
    let width = width.max(1.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, height.max(1.0)), sense);
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("glass_hover"),
        enabled && response.hovered(),
        0.16,
    );
    let pressed = enabled && response.is_pointer_button_down_on();
    let painter = ui.painter().with_clip_rect(rect);
    if enabled {
        paint_glass(
            &painter,
            rect,
            RADIUS_BUTTON,
            hover,
            accent,
            accent.is_some(),
        );
        if pressed {
            painter.rect_filled(
                rect.shrink(0.5),
                CornerRadius::same(RADIUS_BUTTON),
                Color32::from_rgba_unmultiplied(63, 77, 108, 18),
            );
        }
    } else {
        painter.rect_filled(
            rect,
            CornerRadius::same(RADIUS_BUTTON),
            Color32::from_rgba_unmultiplied(247, 248, 251, 230),
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(RADIUS_BUTTON),
            Stroke::new(1.0, with_alpha(BORDER, 150)),
            StrokeKind::Inside,
        );
    }
    let ink = if !enabled {
        FAINT
    } else if let Some(accent) = accent {
        blend(accent, INK, 0.42)
    } else {
        INK
    };
    let font = FontId::proportional(13.0);
    let galley = painter.layout_no_wrap(label.to_owned(), font, ink);
    let icon_size = 15.0;
    let gap = if icon.is_some() { 7.0 } else { 0.0 };
    let icon_space = if icon.is_some() { icon_size + gap } else { 0.0 };
    let content_width = galley.size().x + icon_space;
    let start = rect.center().x - content_width / 2.0;
    if let Some(icon) = icon {
        paint_icon(
            &painter,
            Rect::from_center_size(
                pos2(start + icon_size / 2.0, rect.center().y),
                vec2(icon_size, icon_size),
            ),
            icon,
            ink,
        );
    }
    painter.galley(
        pos2(start + icon_space, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Paints a circular monogram used by the account entries.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn paint_avatar(painter: &Painter, rect: Rect, name: &str, tint: Color32) {
    let rect = Rect::from_center_size(
        rect.center(),
        vec2(
            rect.width().min(rect.height()),
            rect.height().min(rect.width()),
        ),
    );
    let radius = rect.width() / 2.0;
    painter.circle_filled(
        rect.center(),
        radius + 1.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 210),
    );
    gradient_rect(
        painter,
        rect,
        CornerRadius::same(radius.round() as u8),
        blend(Color32::WHITE, tint, 0.22),
        blend(Color32::WHITE, tint, 0.48),
        true,
    );
    painter.circle_stroke(
        rect.center(),
        radius - 0.5,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 190)),
    );
    let initial = name
        .chars()
        .next()
        .map_or_else(|| "?".to_owned(), |value| value.to_uppercase().to_string());
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        FontId::proportional((radius * 0.92).clamp(10.0, 22.0)),
        blend(tint, INK, 0.35),
    );
}

/// Every glyph is drawn from primitives; no icon font is required.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    Gamepad,
    Mods,
    ResourcePack,
    World,
    Download,
    Package,
    User,
    Gear,
    Logs,
    More,
    Plus,
    Play,
    Check,
    Back,
    ChevronDown,
    ChevronRight,
    Shield,
    Cube,
    Spark,
    Folder,
    Refresh,
    Copy,
    External,
    Microsoft,
    Logout,
    Image,
    Alert,
    Minus,
    Maximize,
    Restore,
    Close,
}

/// Paints a small vector icon centered in `rect`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
pub(crate) fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let center = rect.center();
    let size = rect.width().min(rect.height());
    if size <= 0.0 {
        return;
    }
    let stroke = Stroke::new((size * 0.085).clamp(1.25, 2.0), color);
    let point = |x: f32, y: f32| pos2(center.x + size * x, center.y + size * y);
    match icon {
        Icon::Gamepad => {
            let body = Rect::from_center_size(center, vec2(size * 0.8, size * 0.52));
            painter.rect_stroke(
                body,
                CornerRadius::same((size * 0.2) as u8),
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([point(-0.26, -0.09), point(-0.26, 0.09)], stroke);
            painter.line_segment([point(-0.35, 0.0), point(-0.17, 0.0)], stroke);
            painter.circle_filled(point(0.24, -0.08), size * 0.05, color);
            painter.circle_filled(point(0.32, 0.02), size * 0.05, color);
        }
        Icon::Mods => {
            let body = Rect::from_center_size(center, vec2(size * 0.52, size * 0.52));
            painter.rect_stroke(
                body,
                CornerRadius::same((size * 0.09) as u8),
                stroke,
                StrokeKind::Inside,
            );
            painter.circle_stroke(point(0.0, -0.26), size * 0.11, stroke);
            painter.line_segment([point(0.0, -0.15), point(0.0, -0.07)], stroke);
        }
        Icon::ResourcePack => {
            for offset in [0.15, 0.0, -0.15] {
                let layer =
                    Rect::from_center_size(point(0.0, offset), vec2(size * 0.58, size * 0.28));
                painter.rect_stroke(
                    layer,
                    CornerRadius::same((size * 0.06) as u8),
                    stroke,
                    StrokeKind::Inside,
                );
            }
        }
        Icon::World => {
            painter.circle_stroke(center, size * 0.32, stroke);
            painter.add(Shape::line(
                (0..=16)
                    .map(|step| {
                        let angle = std::f32::consts::TAU * (step as f32 / 16.0);
                        point(0.13 * angle.cos(), 0.32 * angle.sin())
                    })
                    .collect(),
                stroke,
            ));
            painter.line_segment([point(-0.32, 0.0), point(0.32, 0.0)], stroke);
        }
        Icon::Download => {
            painter.line_segment([point(0.0, -0.3), point(0.0, 0.1)], stroke);
            painter.line_segment([point(-0.15, -0.04), point(0.0, 0.11)], stroke);
            painter.line_segment([point(0.15, -0.04), point(0.0, 0.11)], stroke);
            painter.line_segment([point(-0.25, 0.28), point(0.25, 0.28)], stroke);
        }
        Icon::Package => {
            let frame = Rect::from_center_size(center, vec2(size * 0.62, size * 0.52));
            painter.rect_stroke(frame, CornerRadius::same(3), stroke, StrokeKind::Inside);
            painter.line_segment([point(-0.31, -0.08), point(0.0, 0.04)], stroke);
            painter.line_segment([point(0.31, -0.08), point(0.0, 0.04)], stroke);
            painter.line_segment([point(0.0, 0.04), point(0.0, 0.26)], stroke);
        }
        Icon::User => {
            painter.circle_stroke(point(0.0, -0.13), size * 0.14, stroke);
            painter.add(Shape::line(
                (0..=8)
                    .map(|step| {
                        let angle = std::f32::consts::PI * (step as f32 / 8.0);
                        point(0.26 * angle.cos(), 0.08 + 0.2 * angle.sin())
                    })
                    .collect(),
                stroke,
            ));
        }
        Icon::Gear => {
            painter.circle_stroke(center, size * 0.16, stroke);
            for step in 0..8 {
                let angle = std::f32::consts::TAU * (step as f32 / 8.0);
                painter.line_segment(
                    [
                        point(0.25 * angle.cos(), 0.25 * angle.sin()),
                        point(0.34 * angle.cos(), 0.34 * angle.sin()),
                    ],
                    stroke,
                );
            }
        }
        Icon::Logs => {
            for y in [-0.2, 0.0, 0.2] {
                painter.circle_filled(point(-0.27, y), size * 0.035, color);
                painter.line_segment([point(-0.15, y), point(0.28, y)], stroke);
            }
        }
        Icon::More => {
            for offset in [-0.22, 0.0, 0.22] {
                painter.circle_filled(point(offset, 0.0), size * 0.055, color);
            }
        }
        Icon::Plus => {
            painter.line_segment([point(-0.26, 0.0), point(0.26, 0.0)], stroke);
            painter.line_segment([point(0.0, -0.26), point(0.0, 0.26)], stroke);
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![point(-0.18, -0.26), point(0.26, 0.0), point(-0.18, 0.26)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Check => {
            painter.line_segment([point(-0.28, 0.0), point(-0.06, 0.2)], stroke);
            painter.line_segment([point(-0.06, 0.2), point(0.3, -0.2)], stroke);
        }
        Icon::Back => {
            painter.line_segment([point(0.14, -0.24), point(-0.12, 0.0)], stroke);
            painter.line_segment([point(-0.12, 0.0), point(0.14, 0.24)], stroke);
        }
        Icon::ChevronDown => {
            painter.line_segment([point(-0.18, -0.08), point(0.0, 0.1)], stroke);
            painter.line_segment([point(0.0, 0.1), point(0.18, -0.08)], stroke);
        }
        Icon::ChevronRight => {
            painter.line_segment([point(-0.08, -0.18), point(0.1, 0.0)], stroke);
            painter.line_segment([point(0.1, 0.0), point(-0.08, 0.18)], stroke);
        }
        Icon::Shield => {
            painter.add(Shape::closed_line(
                vec![
                    point(0.0, -0.32),
                    point(0.25, -0.2),
                    point(0.2, 0.1),
                    point(0.0, 0.32),
                    point(-0.2, 0.1),
                    point(-0.25, -0.2),
                ],
                stroke,
            ));
        }
        Icon::Cube => {
            painter.add(Shape::closed_line(
                vec![
                    point(0.0, -0.3),
                    point(0.28, -0.14),
                    point(0.0, 0.02),
                    point(-0.28, -0.14),
                ],
                stroke,
            ));
            painter.line_segment([point(-0.28, -0.14), point(-0.28, 0.16)], stroke);
            painter.line_segment([point(0.28, -0.14), point(0.28, 0.16)], stroke);
            painter.line_segment([point(0.0, 0.02), point(0.0, 0.32)], stroke);
            painter.line_segment([point(-0.28, 0.16), point(0.0, 0.32)], stroke);
            painter.line_segment([point(0.28, 0.16), point(0.0, 0.32)], stroke);
        }
        Icon::Spark => {
            painter.line_segment([point(0.0, -0.3), point(0.0, 0.3)], stroke);
            painter.line_segment([point(-0.3, 0.0), point(0.3, 0.0)], stroke);
            painter.line_segment([point(-0.18, -0.18), point(0.18, 0.18)], stroke);
            painter.line_segment([point(0.18, -0.18), point(-0.18, 0.18)], stroke);
        }
        Icon::Folder => {
            let body = Rect::from_center_size(point(0.0, 0.06), vec2(size * 0.66, size * 0.46));
            painter.rect_stroke(
                body,
                CornerRadius::same((size * 0.06) as u8),
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([point(-0.33, -0.12), point(-0.08, -0.12)], stroke);
            painter.line_segment([point(-0.08, -0.12), point(-0.02, -0.2)], stroke);
            painter.line_segment([point(-0.02, -0.2), point(0.2, -0.2)], stroke);
            painter.line_segment([point(0.2, -0.2), point(0.24, -0.12)], stroke);
        }
        Icon::Refresh => {
            let points: Vec<Pos2> = (0..=12)
                .map(|step| {
                    let angle = -0.4 + std::f32::consts::TAU * 0.82 * (step as f32 / 12.0);
                    point(0.3 * angle.cos(), 0.3 * angle.sin())
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            painter.line_segment([point(0.14, -0.34), point(0.28, -0.3)], stroke);
            painter.line_segment([point(0.28, -0.3), point(0.26, -0.15)], stroke);
        }
        Icon::Copy => {
            let back = Rect::from_min_size(point(-0.28, -0.3), vec2(size * 0.42, size * 0.48));
            let front = Rect::from_min_size(point(-0.12, -0.14), vec2(size * 0.42, size * 0.48));
            painter.rect_stroke(back, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.rect_stroke(front, CornerRadius::same(2), stroke, StrokeKind::Inside);
        }
        Icon::External => {
            painter.line_segment([point(-0.25, -0.22), point(-0.25, 0.26)], stroke);
            painter.line_segment([point(-0.25, 0.26), point(0.23, 0.26)], stroke);
            painter.line_segment([point(0.23, 0.26), point(0.23, -0.22)], stroke);
            painter.line_segment([point(0.23, -0.22), point(-0.25, -0.22)], stroke);
            painter.line_segment([point(-0.02, 0.03), point(0.27, -0.26)], stroke);
            painter.line_segment([point(0.09, -0.26), point(0.27, -0.26)], stroke);
            painter.line_segment([point(0.27, -0.26), point(0.27, -0.08)], stroke);
        }
        Icon::Microsoft => {
            let square = size * 0.23;
            for (x, y) in [(-0.26, -0.26), (0.03, -0.26), (-0.26, 0.03), (0.03, 0.03)] {
                painter.rect_filled(
                    Rect::from_min_size(point(x, y), vec2(square, square)),
                    1,
                    color,
                );
            }
        }
        Icon::Logout => {
            painter.line_segment([point(-0.3, -0.26), point(-0.3, 0.26)], stroke);
            painter.line_segment([point(-0.3, -0.26), point(0.02, -0.26)], stroke);
            painter.line_segment([point(-0.3, 0.26), point(0.02, 0.26)], stroke);
            painter.line_segment([point(-0.06, 0.0), point(0.3, 0.0)], stroke);
            painter.line_segment([point(0.16, -0.14), point(0.3, 0.0)], stroke);
            painter.line_segment([point(0.16, 0.14), point(0.3, 0.0)], stroke);
        }
        Icon::Image => {
            let frame = Rect::from_center_size(center, vec2(size * 0.64, size * 0.54));
            painter.rect_stroke(
                frame,
                CornerRadius::same((size * 0.06) as u8),
                stroke,
                StrokeKind::Inside,
            );
            painter.circle_filled(point(-0.14, -0.12), size * 0.055, color);
            painter.line_segment([point(-0.28, 0.2), point(-0.04, -0.03)], stroke);
            painter.line_segment([point(-0.04, -0.03), point(0.12, 0.13)], stroke);
            painter.line_segment([point(0.12, 0.13), point(0.22, 0.04)], stroke);
            painter.line_segment([point(0.22, 0.04), point(0.3, 0.12)], stroke);
        }
        Icon::Alert => {
            painter.add(Shape::closed_line(
                vec![point(0.0, -0.3), point(0.3, 0.26), point(-0.3, 0.26)],
                stroke,
            ));
            painter.line_segment([point(0.0, -0.12), point(0.0, 0.08)], stroke);
            painter.circle_filled(point(0.0, 0.18), size * 0.035, color);
        }
        Icon::Minus => {
            painter.line_segment([point(-0.28, 0.0), point(0.28, 0.0)], stroke);
        }
        Icon::Maximize => {
            let rect = Rect::from_center_size(center, vec2(size * 0.5, size * 0.5));
            painter.rect_stroke(rect, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        Icon::Restore => {
            let back = Rect::from_min_size(point(-0.28, -0.28), vec2(size * 0.42, size * 0.42));
            let front = Rect::from_min_size(point(-0.04, -0.08), vec2(size * 0.42, size * 0.42));
            painter.rect_stroke(back, CornerRadius::same(1), stroke, StrokeKind::Inside);
            painter.rect_filled(front, 0, BG);
            painter.rect_stroke(front, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        Icon::Close => {
            painter.line_segment([point(-0.24, -0.24), point(0.24, 0.24)], stroke);
            painter.line_segment([point(0.24, -0.24), point(-0.24, 0.24)], stroke);
        }
    }
}

/// Purely decorative cover artwork. `zoom` is used for the subtle hover scale.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
pub(crate) fn paint_scenery(painter: &Painter, rect: Rect, seed: u64, variant: u8, zoom: f32) {
    if rect.width() < 4.0 || rect.height() < 4.0 {
        return;
    }
    let painter = painter.with_clip_rect(rect);
    let index = usize::try_from(seed % 4).unwrap_or(0);
    let scene = SCENES[index];
    gradient_rect(
        &painter,
        rect,
        CornerRadius {
            nw: RADIUS_CARD,
            ne: RADIUS_CARD,
            sw: 0,
            se: 0,
        },
        scene.sky_top,
        scene.sky_bottom,
        false,
    );

    let content = Rect::from_center_size(rect.center(), rect.size() * zoom.clamp(1.0, 1.08));
    let width = content.width();
    let height = content.height();
    let left = content.left();
    let top = content.top();
    let horizon = top + height * 0.68;

    let sun_center = pos2(left + width * 0.74, top + height * 0.27);
    painter.circle_filled(sun_center, height * 0.24, with_alpha(scene.sun, 30));
    painter.circle_filled(sun_center, height * 0.105, with_alpha(scene.sun, 225));

    for (x, y, scale) in [(0.14, 0.2, 1.0), (0.46, 0.13, 0.8), (0.82, 0.35, 0.65)] {
        paint_cloud(
            &painter,
            pos2(left + width * x, top + height * y),
            height * 0.075 * scale,
            255,
        );
    }

    painter.add(Shape::convex_polygon(
        vec![
            pos2(left, horizon),
            pos2(left + width * 0.14, horizon - height * 0.2),
            pos2(left + width * 0.28, horizon - height * 0.06),
            pos2(left + width * 0.48, horizon - height * 0.27),
            pos2(left + width * 0.64, horizon - height * 0.1),
            pos2(left + width * 0.82, horizon - height * 0.2),
            pos2(left + width, horizon - height * 0.08),
            pos2(content.right(), horizon),
        ],
        scene.far,
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        vec![
            pos2(left, horizon + height * 0.04),
            pos2(left + width * 0.24, horizon - height * 0.12),
            pos2(left + width * 0.5, horizon + height * 0.03),
            pos2(left + width * 0.76, horizon - height * 0.08),
            pos2(content.right(), horizon + height * 0.04),
            pos2(content.right(), content.bottom()),
            pos2(left, content.bottom()),
        ],
        scene.near,
        Stroke::NONE,
    ));
    painter.rect_filled(
        Rect::from_min_max(pos2(left, horizon + height * 0.06), content.max),
        0,
        scene.ground,
    );

    if variant.is_multiple_of(4) {
        let water = Rect::from_min_max(
            pos2(left, horizon + height * 0.13),
            pos2(content.right(), content.bottom()),
        );
        painter.rect_filled(water, 0, with_alpha(scene.water, 210));
        for step in 0..3 {
            let y = water.top() + water.height() * (0.3 + 0.2 * step as f32);
            painter.line_segment(
                [pos2(left + width * 0.1, y), pos2(left + width * 0.52, y)],
                Stroke::new(1.0, with_alpha(Color32::WHITE, 75)),
            );
        }
    }

    match variant % 4 {
        1 => {
            paint_tree(
                &painter,
                pos2(left + width * 0.18, horizon + height * 0.06),
                height,
                scene,
                true,
            );
            paint_tree(
                &painter,
                pos2(left + width * 0.86, horizon - height * 0.02),
                height * 0.8,
                scene,
                true,
            );
        }
        2 => {
            paint_castle(&painter, pos2(left + width * 0.55, horizon), height, scene);
        }
        3 => {
            paint_tree(
                &painter,
                pos2(left + width * 0.15, horizon + height * 0.1),
                height * 0.9,
                scene,
                false,
            );
            paint_tree(
                &painter,
                pos2(left + width * 0.8, horizon + height * 0.04),
                height * 0.75,
                scene,
                false,
            );
        }
        _ => {
            paint_tree(
                &painter,
                pos2(left + width * 0.86, horizon + height * 0.03),
                height * 0.8,
                scene,
                false,
            );
        }
    }

    // A faint glass-like sheen lifts the banner without a flat gradient look.
    painter.add(Shape::convex_polygon(
        vec![
            pos2(rect.left(), rect.top()),
            pos2(rect.left() + rect.width() * 0.55, rect.top()),
            pos2(rect.left() + rect.width() * 0.34, rect.bottom()),
            pos2(rect.left(), rect.bottom()),
        ],
        with_alpha(Color32::WHITE, 16),
        Stroke::NONE,
    ));
}

fn paint_cloud(painter: &Painter, center: Pos2, radius: f32, alpha: u8) {
    let color = with_alpha(Color32::WHITE, alpha);
    painter.circle_filled(
        pos2(center.x - radius * 0.8, center.y + radius * 0.15),
        radius * 0.62,
        color,
    );
    painter.circle_filled(pos2(center.x, center.y), radius, color);
    painter.circle_filled(
        pos2(center.x + radius * 0.9, center.y + radius * 0.2),
        radius * 0.7,
        color,
    );
    painter.rect_filled(
        Rect::from_min_max(
            pos2(center.x - radius * 0.8, center.y + radius * 0.2),
            pos2(center.x + radius * 0.9, center.y + radius * 0.85),
        ),
        0,
        color,
    );
}

fn paint_tree(painter: &Painter, base: Pos2, height: f32, scene: Scene, blossom: bool) {
    let trunk = Rect::from_min_max(
        pos2(base.x - height * 0.025, base.y - height * 0.28),
        pos2(base.x + height * 0.025, base.y),
    );
    painter.rect_filled(trunk, 2, scene.trunk);
    let leaf = if blossom { scene.blossom } else { scene.leaf };
    for (x, y, scale) in [
        (-0.11, -0.38, 0.13),
        (0.0, -0.46, 0.16),
        (0.12, -0.36, 0.12),
        (0.04, -0.3, 0.1),
    ] {
        painter.circle_filled(
            pos2(base.x + height * x, base.y + height * y),
            height * scale,
            leaf,
        );
    }
}

fn paint_castle(painter: &Painter, base: Pos2, height: f32, scene: Scene) {
    let tower = |x: f32, width: f32, tower_height: f32| {
        let rect = Rect::from_min_max(
            pos2(base.x + height * x, base.y - height * tower_height),
            pos2(base.x + height * (x + width), base.y),
        );
        painter.rect_filled(rect, 1.5, scene.castle);
        painter.add(Shape::convex_polygon(
            vec![
                pos2(rect.left() - height * 0.02, rect.top() + 1.0),
                pos2(rect.center().x, rect.top() - height * 0.11),
                pos2(rect.right() + height * 0.02, rect.top() + 1.0),
            ],
            scene.castle_dark,
            Stroke::NONE,
        ));
        painter.rect_filled(
            Rect::from_center_size(
                pos2(rect.center().x, rect.bottom() - height * 0.06),
                vec2(height * 0.045, height * 0.075),
            ),
            2,
            with_alpha(scene.window, 220),
        );
    };
    tower(-0.2, 0.12, 0.3);
    tower(-0.02, 0.16, 0.4);
    tower(0.19, 0.11, 0.27);
    let wall = Rect::from_min_max(
        pos2(base.x - height * 0.2, base.y - height * 0.16),
        pos2(base.x + height * 0.3, base.y),
    );
    painter.rect_filled(wall, 1, scene.castle_dark);
}

#[derive(Clone, Copy)]
struct Scene {
    sky_top: Color32,
    sky_bottom: Color32,
    sun: Color32,
    far: Color32,
    near: Color32,
    ground: Color32,
    water: Color32,
    trunk: Color32,
    leaf: Color32,
    blossom: Color32,
    castle: Color32,
    castle_dark: Color32,
    window: Color32,
}

const SCENES: [Scene; 4] = [
    Scene {
        sky_top: Color32::from_rgb(134, 190, 238),
        sky_bottom: Color32::from_rgb(232, 243, 251),
        sun: Color32::from_rgb(255, 247, 208),
        far: Color32::from_rgb(129, 167, 165),
        near: Color32::from_rgb(104, 157, 123),
        ground: Color32::from_rgb(104, 160, 134),
        water: Color32::from_rgb(124, 194, 213),
        trunk: Color32::from_rgb(108, 84, 67),
        leaf: Color32::from_rgb(82, 146, 99),
        blossom: Color32::from_rgb(228, 158, 188),
        castle: Color32::from_rgb(169, 160, 181),
        castle_dark: Color32::from_rgb(132, 125, 150),
        window: Color32::from_rgb(255, 218, 148),
    },
    Scene {
        sky_top: Color32::from_rgb(198, 164, 232),
        sky_bottom: Color32::from_rgb(252, 231, 239),
        sun: Color32::from_rgb(255, 241, 207),
        far: Color32::from_rgb(173, 148, 196),
        near: Color32::from_rgb(137, 157, 135),
        ground: Color32::from_rgb(125, 155, 127),
        water: Color32::from_rgb(181, 184, 232),
        trunk: Color32::from_rgb(108, 84, 91),
        leaf: Color32::from_rgb(104, 156, 112),
        blossom: Color32::from_rgb(242, 176, 205),
        castle: Color32::from_rgb(157, 141, 176),
        castle_dark: Color32::from_rgb(120, 106, 141),
        window: Color32::from_rgb(255, 225, 169),
    },
    Scene {
        sky_top: Color32::from_rgb(246, 190, 132),
        sky_bottom: Color32::from_rgb(252, 237, 214),
        sun: Color32::from_rgb(255, 245, 202),
        far: Color32::from_rgb(183, 146, 118),
        near: Color32::from_rgb(130, 146, 118),
        ground: Color32::from_rgb(133, 148, 116),
        water: Color32::from_rgb(151, 179, 182),
        trunk: Color32::from_rgb(102, 80, 68),
        leaf: Color32::from_rgb(94, 137, 90),
        blossom: Color32::from_rgb(228, 164, 140),
        castle: Color32::from_rgb(174, 150, 143),
        castle_dark: Color32::from_rgb(136, 114, 113),
        window: Color32::from_rgb(255, 230, 163),
    },
    Scene {
        sky_top: Color32::from_rgb(132, 200, 190),
        sky_bottom: Color32::from_rgb(232, 249, 241),
        sun: Color32::from_rgb(255, 250, 210),
        far: Color32::from_rgb(127, 171, 162),
        near: Color32::from_rgb(87, 154, 126),
        ground: Color32::from_rgb(93, 162, 132),
        water: Color32::from_rgb(121, 201, 197),
        trunk: Color32::from_rgb(101, 84, 71),
        leaf: Color32::from_rgb(64, 145, 108),
        blossom: Color32::from_rgb(226, 177, 196),
        castle: Color32::from_rgb(149, 164, 173),
        castle_dark: Color32::from_rgb(112, 129, 139),
        window: Color32::from_rgb(255, 226, 167),
    },
];

/// Deterministic non-cryptographic seed for purely decorative variation.
pub(crate) fn seed_for(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Paints a text pill centered at `center` with a colored dot on the left.
pub(crate) fn paint_badge(
    painter: &Painter,
    center: Pos2,
    label: &str,
    dot: Color32,
    font: FontId,
) -> Rect {
    let galley = painter.layout_no_wrap(label.to_owned(), font, INK);
    let size = vec2(galley.size().x + 30.0, 24.0);
    let rect = Rect::from_center_size(center, size);
    painter.rect_filled(
        rect,
        12,
        Color32::from_rgba_unmultiplied(255, 255, 255, 238),
    );
    painter.rect_stroke(
        rect,
        12,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 220)),
        StrokeKind::Inside,
    );
    painter.circle_filled(pos2(rect.left() + 13.0, rect.center().y), 3.4, dot);
    painter.galley(
        pos2(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
        galley,
        INK,
    );
    rect
}

/// Returns a soft colored fill and ink for a compact chip.
pub(crate) fn chip_colors(kind: ChipKind) -> (Color32, Color32) {
    match kind {
        ChipKind::Neutral => (NEUTRAL_SOFT, MUTED),
        ChipKind::Blue => (ACCENT_SOFT, ACCENT_INK),
        ChipKind::Green => (GREEN_SOFT, GREEN),
        ChipKind::Orange => (ORANGE_SOFT, ORANGE),
        ChipKind::Purple => (PURPLE_SOFT, PURPLE),
    }
}

/// Semantic chip palettes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChipKind {
    Neutral,
    Blue,
    Green,
    Orange,
    Purple,
}

#[cfg(test)]
mod tests {
    use super::{CornerRadius, Rect, geometry::rounded_outline, pos2};
    use eframe::egui::Vec2;

    #[test]
    fn rounded_outline_stays_inside_the_rectangle() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), Vec2::new(120.0, 80.0));
        let points = rounded_outline(rect, CornerRadius::same(12), 4);
        assert!(points.len() > 8);
        for point in points {
            assert!(rect.contains(point));
        }
    }
}
