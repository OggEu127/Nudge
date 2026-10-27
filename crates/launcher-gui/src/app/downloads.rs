//! Version-list loading and game installation page.

use super::{
    Action, Icon, NudgeApp, RequestState, choice_chip, field_label, glass_text_field,
    glass_text_field_with_width, instance_id_status, page_header_with, section_title, suggested_id,
    theme, version_kind_label, version_row,
};
use eframe::egui::{self, RichText};

impl NudgeApp {
    #[allow(clippy::too_many_lines)]
    pub(super) fn downloads(&mut self, ui: &mut egui::Ui) {
        let mut refresh_versions = false;
        page_header_with(
            ui,
            "下载",
            "从 Mojang 官方版本清单安装原版或 Fabric 实例；下载先校验元数据与摘要。",
            136.0,
            |ui| {
                if theme::glass_button(
                    ui,
                    if self.requests.versions == RequestState::Failed {
                        "重试获取版本"
                    } else {
                        "刷新版本"
                    },
                    120.0,
                    self.task.is_none(),
                    None,
                    Some(Icon::Refresh),
                    36.0,
                )
                .clicked()
                {
                    refresh_versions = true;
                }
            },
        );
        if refresh_versions {
            self.requests.versions = RequestState::Idle;
            self.ensure_versions();
        }
        self.ensure_versions();

        let mut start_install = false;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "安装 Minecraft",
                "实例标识只用于本地目录；下载、依赖解析和 Java 检查都由 Rust 核心完成。",
            );
            ui.add_space(8.0);
            ui.columns(2, |columns| {
                field_label(&mut columns[0], "实例标识");
                glass_text_field(
                    &mut columns[0],
                    &mut self.install_id,
                    "例如 vanilla-1.21",
                );
                field_label(&mut columns[1], "Minecraft 版本");
                glass_text_field(
                    &mut columns[1],
                    &mut self.install_version,
                    "例如 1.21.1",
                );
            });
            let id_valid = instance_id_status(ui, &self.install_id);
            ui.add_space(10.0);
            field_label(ui, "加载器");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                if choice_chip(ui, "原版", !self.install_use_fabric, theme::ACCENT).clicked() {
                    self.install_use_fabric = false;
                }
                if choice_chip(ui, "Fabric", self.install_use_fabric, theme::PURPLE).clicked() {
                    self.install_use_fabric = true;
                }
                ui.label(
                    RichText::new("Forge / NeoForge / Quilt 安装暂未接入核心")
                        .size(10.0)
                        .color(theme::FAINT),
                );
            });
            if self.install_use_fabric {
                ui.add_space(8.0);
                field_label(ui, "Fabric 加载器版本");
                glass_text_field(
                    ui,
                    &mut self.install_fabric,
                    "填写准确的加载器版本，例如 0.16.9",
                );
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new(
                    "实例标识仅限 ASCII 字母、数字、点、连字符与下划线，不能以点结尾或使用 Windows 保留设备名。",
                )
                .size(10.0)
                .color(theme::FAINT),
            );
            let loader_ready = !self.install_use_fabric
                || !self.install_fabric.trim().is_empty();
            let ready = self.task.is_none()
                && id_valid
                && !self.install_version.trim().is_empty()
                && loader_ready;
            ui.add_space(10.0);
            let response = theme::glass_button(
                ui,
                "开始安装",
                ui.available_width(),
                ready,
                Some(if self.install_use_fabric {
                    theme::PURPLE
                } else {
                    theme::ACCENT
                }),
                Some(Icon::Download),
                44.0,
            );
            if !ready && self.task.is_none() {
                ui.label(
                    RichText::new("填写有效的实例标识、游戏版本与加载器版本后即可开始。")
                        .size(10.0)
                        .color(theme::MUTED),
                );
            }
            if response.clicked() {
                start_install = true;
            }
        });
        if start_install {
            self.start(
                Action::Install {
                    id: self.install_id.trim().to_owned(),
                    version: self.install_version.trim().to_owned(),
                    fabric: self
                        .install_use_fabric
                        .then(|| self.install_fabric.trim().to_owned()),
                },
                "正在安装游戏",
            );
        }

        ui.add_space(16.0);
        let mut chosen_version = None;
        theme::section_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_title(
                ui,
                "官方版本",
                "列表来自 Mojang 版本清单；安装前仍会校验所选版本元数据与文件摘要。",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("筛选").size(11.0).color(theme::MUTED));
                // In a horizontal row the field must leave room for the
                // checkbox; letting it consume `available_width` would push
                // the checkbox off-screen and widen the version rows.
                let field_width = (ui.available_width() - 104.0).clamp(150.0, 360.0);
                glass_text_field_with_width(ui, &mut self.version_filter, "版本号", field_width);
                ui.checkbox(&mut self.releases_only, "仅正式版");
            });
            ui.add_space(4.0);
            if self.versions.is_empty() {
                ui.label(
                    RichText::new(match self.requests.versions {
                        RequestState::Loading => "正在获取官方版本列表…",
                        RequestState::Failed => "获取版本列表失败，请点击右上角“重试获取版本”。",
                        RequestState::Ready => "官方版本列表为空，可刷新后重试。",
                        RequestState::Idle => "等待获取官方版本列表。",
                    })
                    .size(11.0)
                    .color(theme::MUTED),
                );
                return;
            }
            let versions: Vec<(String, String, String)> = self
                .versions
                .iter()
                .filter(|version| {
                    (!self.releases_only || version.kind.as_str() == "release")
                        && version.id.contains(&self.version_filter)
                })
                .map(|version| {
                    (
                        version.id.clone(),
                        version_kind_label(version.kind.as_str()).to_owned(),
                        version.release_time.format("%Y-%m-%d").to_string(),
                    )
                })
                .collect();
            if versions.is_empty() {
                ui.label(
                    RichText::new("没有符合筛选条件的版本。")
                        .size(11.0)
                        .color(theme::MUTED),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("version_list")
                .max_height(294.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (id, kind, date) in versions {
                        if version_row(ui, self.install_version == id, &id, &kind, &date).clicked()
                        {
                            chosen_version = Some(id);
                        }
                    }
                });
        });
        if let Some(version) = chosen_version {
            if self.install_id.trim().is_empty() {
                self.install_id = suggested_id(&format!("mc-{version}"));
            }
            self.install_version = version;
        }
    }
}
