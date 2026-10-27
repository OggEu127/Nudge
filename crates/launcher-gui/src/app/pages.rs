//! Page presentation and local interactions.

use super::{
    Action, Align, Align2, ChipKind, CornerRadius, FontId, Icon, Inspection, InstallPlan, Instance,
    Layout, Loader, Margin, NonZeroUsize, NudgeApp, Page, PathBuf, Rect, RichText, Sense, Stroke,
    StrokeKind, chip, egui, ellipsize, field_label, glass_text_field, info_row, instance_id_status,
    loader_kind_label, metadata_line, page_header, page_header_with, paint_ellipsized,
    plan_loader_supported, pos2, section_title, section_title_with_icon, selectable_row, theme,
    vec2, warning_note,
};

impl NudgeApp {
    #[allow(clippy::too_many_lines)]
    pub(super) fn detail(&mut self, ui: &mut egui::Ui) {
        let instance = self
            .instances
            .iter()
            .find(|instance| instance.id == self.detail_instance)
            .cloned();
        ui.horizontal(|ui| {
            if theme::glass_button(ui, "返回", 84.0, true, None, Some(Icon::Back), 30.0).clicked()
            {
                self.page = self.detail_return;
            }
            ui.add_space(2.0);
            ui.label(
                RichText::new("游戏 / 实例详情")
                    .size(11.0)
                    .color(theme::MUTED),
            );
        });
        ui.add_space(12.0);
        let Some(instance) = instance else {
            section_title(
                ui,
                "实例不存在",
                "该实例可能已被移除，或实例记录尚未读取完成。",
            );
            ui.add_space(12.0);
            theme::section_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if theme::glass_button(
                        ui,
                        "返回游戏",
                        140.0,
                        true,
                        None,
                        Some(Icon::Back),
                        38.0,
                    )
                    .clicked()
                    {
                        self.page = Page::Home;
                    }
                    if theme::glass_button(
                        ui,
                        "刷新实例",
                        140.0,
                        self.task.is_none(),
                        Some(theme::ACCENT),
                        Some(Icon::Refresh),
                        38.0,
                    )
                    .clicked()
                    {
                        self.start(Action::Refresh, "正在读取游戏实例");
                    }
                });
            });
            return;
        };

        let loader = Loader::detect(&instance);
        let hero = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 214.0));
        let (_, response) = ui.allocate_exact_size(hero.size(), Sense::hover());
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("detail_hover"),
            response.hovered(),
            0.18,
        );
        self.paint_instance_cover(ui, hero, &instance, 1.0 + hover * 0.025);
        let painter = ui.painter().clone();
        painter.rect_stroke(
            hero,
            CornerRadius::same(theme::RADIUS_CARD),
            Stroke::new(1.0, theme::with_alpha(loader.accent(), 100)),
            StrokeKind::Inside,
        );
        let status = if self.running_instance.as_deref() == Some(instance.id.as_str()) {
            ("运行中", theme::GREEN)
        } else if instance.installed {
            ("安装完成", theme::GREEN)
        } else {
            ("安装未完成", theme::ORANGE)
        };
        let font = FontId::proportional(10.5);
        let badge_width = painter
            .layout_no_wrap(status.0.to_owned(), font.clone(), theme::INK)
            .size()
            .x
            + 30.0;
        theme::paint_badge(
            &painter,
            pos2(hero.right() - 16.0 - badge_width / 2.0, hero.top() + 16.0),
            status.0,
            status.1,
            font,
        );
        ui.add_space(16.0);

        ui.horizontal_top(|ui| {
            let width = (ui.available_width() - 125.0).max(180.0);
            ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(width);
                paint_ellipsized(
                    ui.painter(),
                    pos2(ui.cursor().min.x, ui.cursor().min.y + 12.0),
                    &instance.name,
                    FontId::proportional(27.0),
                    theme::INK,
                    width - 8.0,
                );
                ui.add_space(28.0);
                ui.label(
                    RichText::new(loader.subtitle(&instance))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if theme::glass_button(
                    ui,
                    "打开游戏目录",
                    116.0,
                    true,
                    None,
                    Some(Icon::Folder),
                    30.0,
                )
                .clicked()
                {
                    self.open_game_folder(&instance.id);
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            chip(
                ui,
                Some(Icon::Cube),
                &instance.minecraft_version,
                ChipKind::Neutral,
            );
            chip(ui, Some(loader.icon()), loader.label(), loader.chip_kind());
            if instance.pack.is_some() {
                chip(ui, Some(Icon::Package), "整合包", ChipKind::Green);
            }
            chip(
                ui,
                Some(Icon::Shield),
                if instance.installed {
                    "文件就绪"
                } else {
                    "可继续安装"
                },
                if instance.installed {
                    ChipKind::Green
                } else {
                    ChipKind::Orange
                },
            );
            chip(
                ui,
                Some(Icon::Image),
                "拖动图片可更换封面",
                ChipKind::Neutral,
            );
        });
        ui.add_space(14.0);

        let running = self.running_instance.as_deref() == Some(instance.id.as_str());
        let idle = self.task.is_none() && !running;
        let label = if running {
            "Minecraft 正在运行"
        } else if instance.installed {
            "启动游戏"
        } else {
            "继续安装"
        };
        let primary = theme::glass_button(
            ui,
            label,
            ui.available_width(),
            idle,
            Some(if running {
                theme::GREEN
            } else if instance.installed {
                loader.accent()
            } else {
                theme::ORANGE
            }),
            Some(if instance.installed {
                Icon::Play
            } else {
                Icon::Download
            }),
            44.0,
        );
        if primary.clicked() {
            self.launch_or_resume(&instance);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let each = ((ui.available_width() - 16.0) / 3.0).max(80.0);
            if theme::glass_button(
                ui,
                "校验文件",
                each,
                idle && instance.installed,
                None,
                Some(Icon::Shield),
                36.0,
            )
            .clicked()
            {
                self.start(Action::Verify(instance.id.clone()), "正在校验游戏实例");
            }
            if theme::glass_button(ui, "修复安装", each, idle, None, Some(Icon::Refresh), 36.0)
                .clicked()
            {
                self.start(Action::Resume(instance.id.clone()), "正在继续安装");
            }
            if theme::glass_button(ui, "打开目录", each, true, None, Some(Icon::Folder), 36.0)
                .clicked()
            {
                self.open_game_folder(&instance.id);
            }
        });
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(7.0, 7.0);
            if theme::glass_button(ui, "模组", 92.0, true, None, Some(Icon::Mods), 34.0).clicked()
            {
                self.selected_instance.clone_from(&instance.id);
                self.content_target = None;
                self.content = None;
                self.navigate(Page::Mods);
            }
            if theme::glass_button(
                ui,
                "资源包",
                104.0,
                true,
                None,
                Some(Icon::ResourcePack),
                34.0,
            )
            .clicked()
            {
                self.selected_instance.clone_from(&instance.id);
                self.content_target = None;
                self.content = None;
                self.navigate(Page::ResourcePacks);
            }
            if theme::glass_button(ui, "世界存档", 116.0, true, None, Some(Icon::World), 34.0)
                .clicked()
            {
                self.selected_instance.clone_from(&instance.id);
                self.content_target = None;
                self.content = None;
                self.navigate(Page::Worlds);
            }
        });
        ui.add_space(16.0);

        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "实例信息",
                "字段来自本地实例记录；封面可拖入 PNG / JPG / WebP 自定义。",
            );
            ui.add_space(4.0);
            info_row(ui, "实例标识", &instance.id);
            info_row(ui, "Minecraft", &instance.minecraft_version);
            info_row(ui, "加载器", loader.label());
            info_row(
                ui,
                "安装状态",
                if instance.installed {
                    "已完成"
                } else {
                    "未完成，可继续安装"
                },
            );
            metadata_line(
                ui,
                "实例目录",
                &self
                    .data_dir
                    .join("instances")
                    .join(&instance.id)
                    .display()
                    .to_string(),
            );
            if let Some(pack) = &instance.pack {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                info_row(ui, "整合包", &format!("{} {}", pack.name, pack.version));
                info_row(ui, "受管理文件", &format!("{} 个", pack.files.len()));
                metadata_line(ui, "归档 SHA-256", &pack.archive_sha256);
            }
            ui.add_space(8.0);
            ui.checkbox(
                &mut self.demo,
                "使用 Minecraft 官方试玩模式启动（无需账号）",
            );
            if self.demo {
                ui.label(
                    RichText::new(
                        "试玩模式由 Minecraft 官方接口提供受限内容，不读取本地账户凭据。",
                    )
                    .size(10.5)
                    .color(theme::MUTED),
                );
            }
        });
    }

    pub(super) fn open_game_folder(&mut self, id: &str) {
        match self.manager.game_directory(id) {
            Ok(path) => self.open_directory(&path),
            Err(error) => self.fail_error(error),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn modpacks(&mut self, ui: &mut egui::Ui) {
        page_header_with(
            ui,
            "整合包",
            "把 .mrpack 或包含 modrinth.index.json 的 ZIP 拖进窗口；格式按内容识别，而不是只看扩展名。",
            120.0,
            |ui| {
                if theme::glass_button(
                    ui,
                    "当前实例",
                    108.0,
                    !self.instances.is_empty(),
                    None,
                    Some(Icon::Gamepad),
                    36.0,
                )
                .clicked()
                {
                    self.page = Page::Home;
                }
            },
        );

        let drop_rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 96.0));
        let (_, drop_response) = ui.allocate_exact_size(drop_rect.size(), Sense::hover());
        let hover = ui.ctx().animate_bool_with_time(
            drop_response.id.with("drop_hover"),
            drop_response.hovered(),
            0.16,
        );
        theme::paint_glass(
            ui.painter(),
            drop_rect,
            theme::RADIUS_SECTION,
            hover,
            Some(theme::GREEN),
            false,
        );
        let painter = ui.painter().clone();
        theme::paint_icon(
            &painter,
            Rect::from_center_size(
                pos2(drop_rect.left() + 42.0, drop_rect.center().y),
                vec2(30.0, 30.0),
            ),
            Icon::Package,
            theme::GREEN,
        );
        painter.text(
            pos2(drop_rect.left() + 70.0, drop_rect.center().y - 10.0),
            Align2::LEFT_CENTER,
            "拖入整合包，开始一次完整的冒险",
            FontId::proportional(15.0),
            theme::INK,
        );
        painter.text(
            pos2(drop_rect.left() + 70.0, drop_rect.center().y + 12.0),
            Align2::LEFT_CENTER,
            "解析阶段不会下载或写入；确认方案后才开始安装。",
            FontId::proportional(10.5),
            theme::MUTED,
        );
        ui.add_space(16.0);

        let mut inspect = false;
        let mut install = false;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "选择压缩包",
                "也可以填写文件路径；一次只处理一个压缩包。",
            );
            ui.add_space(8.0);
            field_label(ui, "压缩包路径");
            let path_response = glass_text_field(
                ui,
                &mut self.import_path,
                "例如 D:\\Packs\\adventure.mrpack",
            );
            if path_response.changed() {
                self.dropped_archive = None;
                self.inspection = None;
            }
            ui.add_space(8.0);
            field_label(ui, "实例标识");
            let id_response = glass_text_field(ui, &mut self.import_id, "例如 adventure-pack");
            if id_response.changed() {
                self.inspection = None;
            }
            let id_valid = instance_id_status(ui, &self.import_id);
            ui.add_space(10.0);
            let ready = self.task.is_none() && !self.import_path.trim().is_empty() && id_valid;
            if theme::glass_button(
                ui,
                "解析压缩包",
                ui.available_width(),
                ready,
                Some(theme::GREEN),
                Some(Icon::Package),
                42.0,
            )
            .clicked()
            {
                inspect = true;
            }
        });
        if inspect {
            self.inspect_current();
        }

        if self.inspection.is_some() {
            ui.add_space(16.0);
            let mut inspection = self.inspection.take();
            if let Some(current) = &mut inspection {
                let Inspection { plan, optional, .. } = current;
                let plan: &InstallPlan = plan;
                let mut requested = false;
                theme::section_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        let width = (ui.available_width() - 104.0).max(120.0);
                        ui.allocate_ui_with_layout(
                            vec2(width, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(width);
                                ui.label(
                                    RichText::new(format!("{} · {}", plan.name, plan.version))
                                        .size(19.0)
                                        .strong()
                                        .color(theme::INK),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "Minecraft {} · {} 个必需文件 · {} 个覆盖文件",
                                        plan.minecraft_version,
                                        plan.required_files.len(),
                                        plan.overrides.len()
                                    ))
                                    .size(10.5)
                                    .color(theme::MUTED),
                                );
                            },
                        );
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            chip(
                                ui,
                                Some(Icon::Package),
                                plan.loader
                                    .as_ref()
                                    .map_or("原版", |loader| loader_kind_label(loader.kind)),
                                ChipKind::Orange,
                            );
                        });
                    });
                    ui.add_space(10.0);
                    metadata_line(ui, "目标目录", &plan.target_directory.display().to_string());
                    metadata_line(ui, "压缩包 SHA-256", &plan.archive_sha256);
                    if !plan.excluded_files.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "已自动排除 {} 个仅服务端需要的文件。",
                                plan.excluded_files.len()
                            ))
                            .size(10.5)
                            .color(theme::FAINT),
                        );
                    }
                    if !plan.optional_files.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("可选客户端文件（默认不勾选）")
                                .size(11.5)
                                .strong()
                                .color(theme::INK),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("optional_files")
                            .max_height(132.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for file in &plan.optional_files {
                                    let name = file
                                        .path
                                        .file_name()
                                        .and_then(|value| value.to_str())
                                        .unwrap_or("可选文件");
                                    let mut selected = optional.contains(&file.path);
                                    if ui
                                        .checkbox(&mut selected, ellipsize(name, 28))
                                        .on_hover_text(file.path.display().to_string())
                                        .changed()
                                    {
                                        if selected {
                                            optional.insert(file.path.clone());
                                        } else {
                                            optional.remove(&file.path);
                                        }
                                    }
                                }
                            });
                    }
                    if !plan.unsupported_dependencies.is_empty() {
                        ui.add_space(10.0);
                        warning_note(ui, "此整合包声明了当前版本暂不支持的依赖，无法安全安装。");
                    }
                    if !plan_loader_supported(plan) {
                        ui.add_space(10.0);
                        warning_note(
                            ui,
                            "当前仅实现 Fabric 安装；请改用 Fabric 整合包，或等待对应加载器支持。",
                        );
                    }
                    ui.add_space(12.0);
                    let allowed = self.task.is_none()
                        && plan_loader_supported(plan)
                        && plan.unsupported_dependencies.is_empty();
                    if theme::glass_button(
                        ui,
                        "确认方案并安装整合包",
                        ui.available_width(),
                        allowed,
                        Some(theme::GREEN),
                        Some(Icon::Download),
                        42.0,
                    )
                    .clicked()
                    {
                        requested = true;
                    }
                });
                if requested {
                    install = true;
                }
            }
            if let Some(updated) = inspection {
                if install {
                    let action = Action::Import {
                        archive: updated.archive.clone(),
                        id: updated.id.clone(),
                        optional: updated.optional.clone(),
                        archive_sha256: updated.plan.archive_sha256.clone(),
                    };
                    self.inspection = None;
                    self.start(action, "正在导入整合包");
                } else {
                    self.inspection = Some(updated);
                }
            }
        } else {
            ui.add_space(16.0);
            theme::section_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(
                    ui,
                    "尚未解析压缩包",
                    "解析后会展示名称、目标目录、可选文件与完整安装方案。",
                );
            });
        }

        let packs: Vec<Instance> = self
            .instances
            .iter()
            .filter(|instance| instance.pack.is_some())
            .cloned()
            .collect();
        if !packs.is_empty() {
            ui.add_space(16.0);
            let mut open = None;
            theme::section_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(ui, "已安装整合包", "点击条目查看实例详情。");
                ui.add_space(4.0);
                for instance in &packs {
                    let pack = instance.pack.as_ref();
                    let subtitle = pack.map_or_else(
                        || "本地整合包".to_owned(),
                        |pack| format!("{} {}", pack.name, pack.version),
                    );
                    if selectable_row(
                        ui,
                        self.selected_instance == instance.id,
                        &instance.name,
                        &subtitle,
                        Some(Icon::Package),
                    )
                    .clicked()
                    {
                        open = Some(instance.id.clone());
                    }
                }
            });
            if let Some(id) = open {
                self.open_detail(&id, Page::Modpacks);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn settings(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            "设置",
            "改动在点击保存后写入本地配置；游戏运行参数用于下一次启动。",
        );
        let memory_valid = self.minimum_memory <= self.maximum_memory;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "游戏运行",
                "Java 路径留空时，会自动查找符合版本要求的运行时。",
            );
            ui.add_space(6.0);
            field_label(ui, "Java 可执行文件");
            glass_text_field(
                ui,
                &mut self.java_path,
                "留空自动查找，例如 C:\\Java\\bin\\java.exe",
            );
            ui.add_space(9.0);
            ui.columns(2, |columns| {
                field_label(&mut columns[0], "最小内存（MiB）");
                columns[0].add(
                    egui::DragValue::new(&mut self.minimum_memory)
                        .range(128..=131_072)
                        .speed(64.0),
                );
                field_label(&mut columns[1], "最大内存（MiB）");
                columns[1].add(
                    egui::DragValue::new(&mut self.maximum_memory)
                        .range(128..=131_072)
                        .speed(64.0),
                );
            });
            if !memory_valid {
                ui.label(
                    RichText::new("最小内存不能大于最大内存。")
                        .size(10.0)
                        .color(theme::DANGER),
                );
            }
            ui.add_space(5.0);
            ui.checkbox(
                &mut self.auto_launch,
                "安装成功后自动启动游戏（须已登录、选择离线账户或启用试玩）",
            );
            ui.checkbox(&mut self.demo, "以 Minecraft 官方试玩模式启动（无需账号）");
        });

        ui.add_space(14.0);
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "下载",
                "并发数越高，带宽占用越大；网络不稳时可适当调低。",
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("最大并发下载数")
                        .size(11.5)
                        .color(theme::MUTED),
                );
                ui.add(
                    egui::DragValue::new(&mut self.concurrency)
                        .range(1..=64)
                        .speed(0.2),
                );
            });
        });

        ui.add_space(14.0);
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "Microsoft 登录",
                "必须使用你自己注册的公共客户端应用；Nudge 不内置、也不借用其他启动器的应用标识。",
            );
            ui.add_space(6.0);
            field_label(ui, "公共客户端应用标识");
            glass_text_field(
                ui,
                &mut self.microsoft_client_id,
                "Microsoft Entra 应用（客户端）ID",
            );
            ui.label(
                RichText::new(
                    "访问 Minecraft Java 版接口通常还需要该应用获得微软批准。没有配置时，登录按钮会保持禁用。",
                )
                .size(10.0)
                .color(theme::FAINT),
            );
        });

        ui.add_space(14.0);
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if theme::glass_button(
                ui,
                "保存设置",
                ui.available_width(),
                memory_valid,
                Some(theme::ACCENT),
                Some(Icon::Check),
                44.0,
            )
            .clicked()
            {
                self.save_settings();
            }
            ui.add_space(8.0);
            metadata_line(ui, "配置文件", &self.store.path().display().to_string());
            metadata_line(ui, "应用数据", &self.data_dir.display().to_string());
        });
    }

    pub(super) fn save_settings(&mut self) {
        let mut candidate = self.config.clone();
        candidate.game.java_executable =
            (!self.java_path.trim().is_empty()).then(|| PathBuf::from(self.java_path.trim()));
        candidate.auth.microsoft_client_id = (!self.microsoft_client_id.trim().is_empty())
            .then(|| self.microsoft_client_id.trim().to_owned());
        candidate.game.min_memory_mib = self.minimum_memory;
        candidate.game.max_memory_mib = self.maximum_memory;
        candidate.game.auto_launch_after_install = self.auto_launch;
        if let Some(value) = NonZeroUsize::new(self.concurrency) {
            candidate.downloads.max_concurrent_downloads = value;
        }
        match self.store.save(&candidate) {
            Ok(()) => {
                self.config = candidate;
                "设置已保存。".clone_into(&mut self.status);
            }
            Err(error) => self.fail_error(error),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn more(&mut self, ui: &mut egui::Ui) {
        page_header(ui, "更多", "任务日志、本地路径与版本信息。");
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 176.0).max(120.0);
                ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(width);
                    section_title_with_icon(
                        ui,
                        Icon::Logs,
                        "任务与日志",
                        "最多保留最近 2000 行 / 512 KiB；原始游戏输出按不可信纯文本显示。",
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::glass_button(
                        ui,
                        "复制",
                        70.0,
                        self.logs.iter().next().is_some(),
                        None,
                        Some(Icon::Copy),
                        32.0,
                    )
                    .clicked()
                    {
                        let text = self.logs.iter().collect::<Vec<_>>().join("\n");
                        ui.ctx().copy_text(text);
                    }
                    if theme::glass_button(ui, "清空", 70.0, true, None, Some(Icon::Close), 32.0)
                        .clicked()
                    {
                        self.logs.clear();
                    }
                });
            });
            ui.add_space(8.0);
            let log_height = 260.0;
            egui::Frame::new()
                .fill(theme::LOG_BG)
                .corner_radius(16)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("log_scroll")
                        .max_height(log_height)
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if self.logs.iter().next().is_none() {
                                ui.label(
                                    RichText::new(
                                        "暂无日志。安装、校验或启动游戏后，输出会显示在这里。",
                                    )
                                    .size(11.0)
                                    .color(theme::with_alpha(theme::LOG_INK, 150)),
                                );
                                return;
                            }
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for line in self.logs.iter() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(line)
                                            .monospace()
                                            .size(11.0)
                                            .color(theme::LOG_INK),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                });
        });

        ui.add_space(14.0);
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "本地数据",
                "令牌只进入系统凭据库；离线账户没有任何 Microsoft 凭据。",
            );
            ui.add_space(4.0);
            metadata_line(ui, "配置文件", &self.store.path().display().to_string());
            metadata_line(ui, "应用数据", &self.data_dir.display().to_string());
            ui.add_space(8.0);
            if theme::glass_button(
                ui,
                "打开应用数据目录",
                168.0,
                true,
                None,
                Some(Icon::Folder),
                36.0,
            )
            .clicked()
            {
                let directory = self.data_dir.clone();
                self.open_directory(&directory);
            }
        });

        ui.add_space(14.0);
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "关于 Nudge",
                &format!("版本 {} · Windows 桌面版", env!("CARGO_PKG_VERSION")),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Nudge 是一个本地优先的 Minecraft Java 版启动器：安装、下载校验、账号与游戏进程均由 Rust 核心处理。界面使用 eframe/egui 绘制，不依赖 WebView。",
                )
                .size(11.0)
                .color(theme::MUTED)
                .line_height(Some(17.0)),
            );
            ui.add_space(6.0);
            metadata_line(ui, "许可证", "GPL-3.0-only");
            metadata_line(ui, "开发者", "Nudge contributors");
            ui.label(
                RichText::new("Microsoft 与 Minecraft 是 Microsoft Corporation 的商标。")
                    .size(9.5)
                    .color(theme::FAINT),
            );
        });
    }
}
