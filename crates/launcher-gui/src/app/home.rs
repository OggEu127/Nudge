//! Page presentation and local interactions.

use super::{
    Align, CARD_GAP, CARD_HEIGHT, CardAction, ChipKind, Color32, ContentKind, ContentListing,
    CornerRadius, FontId, Icon, Instance, Layout, Loader, NudgeApp, Page, Path, Rect, RequestState,
    RichText, Sense, Stroke, StrokeKind, action_button_at, choice_chip, content_kind_label,
    content_row, cover_uv, egui, empty_notice, icon_action_at, open_directory, page_header_with,
    paint_chip_at, paint_ellipsized, pos2, section_title, theme, vec2,
};

impl NudgeApp {
    pub(super) fn home(&mut self, ui: &mut egui::Ui) {
        let mut requested = None;
        page_header_with(
            ui,
            "游戏",
            "选择实例，继续冒险或开始一次新的安装。",
            258.0,
            |ui| {
                if theme::glass_button(
                    ui,
                    "导入整合包",
                    112.0,
                    self.task.is_none(),
                    None,
                    Some(Icon::Package),
                    38.0,
                )
                .clicked()
                {
                    requested = Some(Page::Modpacks);
                }
                if theme::glass_button(
                    ui,
                    "安装游戏",
                    112.0,
                    self.task.is_none(),
                    Some(theme::ACCENT),
                    Some(Icon::Download),
                    38.0,
                )
                .clicked()
                {
                    requested = Some(Page::Downloads);
                }
            },
        );
        if let Some(page) = requested {
            self.navigate(page);
        }

        self.sync_selected_instance();
        if self.instances.is_empty() {
            self.empty_home(ui);
            return;
        }
        let available = ui.available_width();
        let (columns, column_count) = if available >= 1180.0 {
            (3, 3.0)
        } else if available >= 820.0 {
            (2, 2.0)
        } else {
            (1, 1.0)
        };
        let width = (available - CARD_GAP * (column_count - 1.0)) / column_count;
        let instances = self.instances.clone();
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let mut action = None;
        for row in instances.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, instance) in row.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(CARD_GAP);
                    }
                    if let Some(value) = self.instance_card(ui, instance, width) {
                        action = Some((value, instance.clone()));
                    }
                }
            });
            ui.add_space(CARD_GAP);
        }
        if let Some((action, instance)) = action {
            match action {
                CardAction::Detail => self.open_detail(&instance.id, Page::Home),
                CardAction::Launch => self.launch_or_resume(&instance),
            }
        }
    }

    pub(super) fn empty_home(&mut self, ui: &mut egui::Ui) {
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let width = ui.available_width();
            let (art, _) = ui.allocate_exact_size(vec2(width, 168.0), Sense::hover());
            theme::paint_scenery(ui.painter(), art, theme::seed_for("nudge-empty"), 0, 1.0);
            let painter = ui.painter().clone();
            theme::paint_badge(
                &painter,
                pos2(art.center().x, art.bottom() - 27.0),
                "等待第一块世界",
                theme::ACCENT,
                FontId::proportional(11.0),
            );
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("还没有游戏实例")
                        .size(22.0)
                        .strong()
                        .color(theme::INK),
                );
                ui.label(
                    RichText::new("安装官方版本，或把整合包拖进窗口，实例卡片会出现在这里。")
                        .size(12.0)
                        .color(theme::MUTED),
                );
                ui.add_space(16.0);
                let mut requested = None;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let total = 146.0 * 2.0 + 10.0;
                    ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                    if theme::glass_button(
                        ui,
                        "安装原版",
                        146.0,
                        self.task.is_none(),
                        Some(theme::ACCENT),
                        Some(Icon::Download),
                        40.0,
                    )
                    .clicked()
                    {
                        requested = Some(Page::Downloads);
                    }
                    if theme::glass_button(
                        ui,
                        "导入整合包",
                        146.0,
                        self.task.is_none(),
                        Some(theme::GREEN),
                        Some(Icon::Package),
                        40.0,
                    )
                    .clicked()
                    {
                        requested = Some(Page::Modpacks);
                    }
                });
                if let Some(page) = requested {
                    self.navigate(page);
                }
            });
            ui.add_space(20.0);
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn instance_card(
        &mut self,
        ui: &mut egui::Ui,
        instance: &Instance,
        width: f32,
    ) -> Option<CardAction> {
        let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_HEIGHT), Sense::hover());
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("card_hover"),
            response.container_contains_pointer(),
            0.18,
        );
        let draw_rect = rect.translate(vec2(0.0, -hover * 3.0));
        let painter = ui.painter().clone();
        theme::paint_glass(&painter, draw_rect, theme::RADIUS_CARD, hover, None, true);
        let loader = Loader::detect(instance);
        let running = self.running_instance.as_deref() == Some(instance.id.as_str());
        let (status, status_kind, status_icon) = if running {
            ("运行中", ChipKind::Green, Icon::Spark)
        } else if instance.installed {
            ("就绪", ChipKind::Green, Icon::Check)
        } else {
            ("安装未完成", ChipKind::Orange, Icon::Download)
        };

        let body_top = draw_rect.top();
        let tile_rect = Rect::from_min_size(
            pos2(draw_rect.left() + 18.0, body_top + 14.0),
            vec2(32.0, 32.0),
        );
        painter.rect_filled(
            tile_rect,
            10,
            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
        );
        painter.rect_stroke(
            tile_rect,
            10,
            Stroke::new(1.0, theme::with_alpha(loader.accent(), 90)),
            StrokeKind::Inside,
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(tile_rect.center(), vec2(18.0, 18.0)),
            loader.icon(),
            loader.accent(),
        );
        let text_left = draw_rect.left() + 60.0;
        let text_width = (draw_rect.width() - 60.0 - 48.0).max(20.0);
        paint_ellipsized(
            &painter,
            pos2(text_left, body_top + 20.0),
            &instance.name,
            FontId::proportional(17.0),
            theme::INK,
            text_width,
        );
        paint_ellipsized(
            &painter,
            pos2(text_left, body_top + 42.0),
            &loader.subtitle(instance),
            FontId::proportional(10.5),
            theme::MUTED,
            text_width + 20.0,
        );
        paint_ellipsized(
            &painter,
            pos2(draw_rect.left() + 18.0, body_top + 69.0),
            &loader.description(instance),
            FontId::proportional(11.0),
            theme::MUTED,
            draw_rect.width() - 36.0,
        );

        let mut chip_x = draw_rect.left() + 18.0;
        let chip_y = body_top + 96.0;
        chip_x += paint_chip_at(
            &painter,
            pos2(chip_x, chip_y),
            Some(Icon::Cube),
            &instance.minecraft_version,
            ChipKind::Neutral,
        ) + 6.0;
        chip_x += paint_chip_at(
            &painter,
            pos2(chip_x, chip_y),
            Some(loader.icon()),
            loader.label(),
            loader.chip_kind(),
        ) + 6.0;
        if instance.pack.is_some() {
            chip_x += paint_chip_at(
                &painter,
                pos2(chip_x, chip_y),
                Some(Icon::Package),
                "整合包",
                ChipKind::Green,
            ) + 6.0;
        }
        let status_width = painter
            .layout_no_wrap(status.to_owned(), FontId::proportional(10.0), theme::INK)
            .size()
            .x
            + 32.0;
        if chip_x + status_width <= draw_rect.right() - 18.0 {
            paint_chip_at(
                &painter,
                pos2(chip_x, chip_y),
                Some(status_icon),
                status,
                status_kind,
            );
        }

        painter.rect_stroke(
            draw_rect,
            CornerRadius::same(theme::RADIUS_CARD),
            Stroke::new(
                1.0,
                theme::mix_rgba(
                    Color32::from_rgba_unmultiplied(199, 208, 223, 150),
                    theme::with_alpha(loader.accent(), 190),
                    hover * 0.8,
                ),
            ),
            StrokeKind::Inside,
        );

        let more_rect = Rect::from_min_size(
            pos2(draw_rect.right() - 45.0, body_top + 13.0),
            vec2(28.0, 28.0),
        );
        let more = icon_action_at(
            ui,
            more_rect,
            Icon::More,
            ("card_more", &instance.id),
            "实例详情",
        );
        let label = if running {
            "运行中"
        } else if instance.installed {
            "启动游戏"
        } else {
            "继续安装"
        };
        let icon = if running {
            Icon::Spark
        } else if instance.installed {
            Icon::Play
        } else {
            Icon::Download
        };
        let accent = if running {
            theme::GREEN
        } else if instance.installed {
            loader.accent()
        } else {
            theme::ORANGE
        };
        let launch_rect = Rect::from_min_size(
            pos2(draw_rect.left() + 18.0, draw_rect.bottom() - 16.0 - 40.0),
            vec2(draw_rect.width() - 36.0, 40.0),
        );
        let launch = action_button_at(
            ui,
            launch_rect,
            ("card_launch", &instance.id),
            label,
            self.task.is_none() && !running,
            Some(accent),
            Some(icon),
            running,
        );
        if more.clicked() {
            Some(CardAction::Detail)
        } else if launch.clicked() {
            Some(CardAction::Launch)
        } else {
            None
        }
    }

    pub(super) fn paint_instance_cover(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        instance: &Instance,
        zoom: f32,
    ) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let directory = self.data_dir.join("instances").join(&instance.id);
        if let Some(path) = self.covers.cover_path(&instance.id, &directory) {
            self.covers.ensure(&instance.id, &path);
            if let Some(texture) = self.covers.texture(&instance.id).cloned() {
                let uv = cover_uv(texture.size_vec2(), rect, zoom);
                egui::Image::new(egui::load::SizedTexture::from_handle(&texture))
                    .uv(uv)
                    .corner_radius(CornerRadius {
                        nw: theme::RADIUS_CARD,
                        ne: theme::RADIUS_CARD,
                        sw: 0,
                        se: 0,
                    })
                    .paint_at(ui, rect);
                let painter = ui.painter().with_clip_rect(rect);
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        rect.left_top(),
                        pos2(rect.left() + rect.width() * 0.55, rect.top()),
                        pos2(rect.left() + rect.width() * 0.3, rect.bottom()),
                        rect.left_bottom(),
                    ],
                    Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                    Stroke::NONE,
                ));
                return;
            }
        }
        let loader = Loader::detect(instance);
        theme::paint_scenery(
            ui.painter(),
            rect,
            theme::seed_for(&instance.id),
            loader.scenery_variant(),
            zoom,
        );
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn content_page(&mut self, ui: &mut egui::Ui, kind: ContentKind) {
        let title = match kind {
            ContentKind::Mods => "模组",
            ContentKind::ResourcePacks => "资源包",
            ContentKind::Worlds => "世界存档",
        };
        let subtitle = match kind {
            ContentKind::Mods => "读取当前实例的 game/mods，停用的 .jar.disabled 也会显示。",
            ContentKind::ResourcePacks => "读取当前实例的 game/resourcepacks，不跟随符号链接。",
            ContentKind::Worlds => "读取包含 level.dat 的本地世界存档目录。",
        };
        let mut refresh = false;
        page_header_with(ui, title, subtitle, 130.0, |ui| {
            if theme::glass_button(
                ui,
                "刷新列表",
                116.0,
                self.task.is_none(),
                None,
                Some(Icon::Refresh),
                36.0,
            )
            .clicked()
            {
                refresh = true;
            }
        });
        if refresh {
            self.refresh_content(kind);
        }

        if self.instances.is_empty() {
            empty_notice(
                ui,
                "还没有可管理的实例",
                "先安装一个 Minecraft 实例，模组、资源包与世界存档会按实例隔离显示。",
                "安装游戏",
                Icon::Download,
                || self.navigate(Page::Downloads),
            );
            return;
        }

        self.sync_selected_instance();
        let mut chosen = None;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "当前实例",
                "列表来自本地实例记录，切换实例后会重新扫描对应目录。",
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(7.0, 7.0);
                for instance in self.instances.iter() {
                    if choice_chip(
                        ui,
                        &instance.name,
                        instance.id == self.selected_instance,
                        Loader::detect(instance).accent(),
                    )
                    .clicked()
                    {
                        chosen = Some(instance.id.clone());
                    }
                }
            });
        });
        ui.add_space(16.0);
        if let Some(id) = chosen
            && id != self.selected_instance
        {
            self.selected_instance = id;
            self.content_target = None;
            self.content = None;
        }
        self.ensure_content(kind);

        let target = (self.selected_instance.clone(), kind);
        let matches = self.content_target.as_ref() == Some(&target);
        let listing = self
            .content
            .as_ref()
            .filter(|listing| listing.instance == target.0 && listing.kind == kind)
            .cloned();
        if let Some(listing) = listing {
            self.content_listing(ui, &listing);
        } else if matches && self.requests.content == RequestState::Loading {
            theme::section_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0).color(theme::ACCENT));
                    ui.label(
                        RichText::new(format!("正在扫描{}目录…", content_kind_label(kind)))
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
            });
        } else if matches {
            theme::section_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(
                    ui,
                    "暂时无法读取目录",
                    "请检查实例目录权限，或点击“刷新列表”重试。",
                );
            });
        }
    }

    pub(super) fn content_listing(&mut self, ui: &mut egui::Ui, listing: &ContentListing) {
        let mut open = false;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let text_width = (ui.available_width() - 126.0).max(100.0);
                ui.allocate_ui_with_layout(
                    vec2(text_width, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(text_width);
                        section_title(
                            ui,
                            &format!("{} 项", listing.entries.len()),
                            &format!("{}", listing.folder.display()),
                        );
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::glass_button(
                        ui,
                        "打开文件夹",
                        112.0,
                        true,
                        None,
                        Some(Icon::Folder),
                        34.0,
                    )
                    .clicked()
                    {
                        open = true;
                    }
                });
            });
            ui.add_space(8.0);
            if listing.entries.is_empty() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "这个实例还没有{}。",
                            content_kind_label(listing.kind)
                        ))
                        .size(12.0)
                        .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("打开文件夹放入内容后，点击刷新即可看到。")
                            .size(10.5)
                            .color(theme::FAINT),
                    );
                });
                ui.add_space(10.0);
                return;
            }
            let height = (ui.available_height() - 8.0).clamp(160.0, 430.0);
            egui::ScrollArea::vertical()
                .id_salt(("content_rows", listing.kind))
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for entry in &listing.entries {
                        content_row(ui, listing.kind, entry);
                        ui.add_space(4.0);
                    }
                });
        });
        if open {
            self.open_directory(&listing.folder);
        }
    }

    pub(super) fn open_directory(&mut self, path: &Path) {
        match open_directory(path) {
            Ok(()) => self.status = format!("已在文件资源管理器中打开 {}", path.display()),
            Err(error) => self.fail_error(error),
        }
    }
}
