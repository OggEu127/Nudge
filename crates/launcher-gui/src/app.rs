//! Native presentation and ephemeral interaction state.

use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use launcher_core::{
    auth::Account,
    config::{ConfigStore, LauncherConfig},
    instances::{InstallProgress, Instance, InstanceManager},
};
use launcher_minecraft::VersionEntry;
use launcher_modpack::{InstallPlan, LoaderKind};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::{
    errors::Failure,
    jobs::{self, Action, Event, EventSink, Outcome},
    logs::LogBuffer,
};

const EVENT_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Page {
    Home,
    Instances,
    Install,
    Import,
    Accounts,
    Settings,
    Logs,
}

impl Page {
    const ALL: [(Self, &'static str); 7] = [
        (Self::Home, "首页"),
        (Self::Instances, "游戏实例"),
        (Self::Install, "安装游戏"),
        (Self::Import, "导入整合包"),
        (Self::Accounts, "账号管理"),
        (Self::Settings, "设置"),
        (Self::Logs, "日志"),
    ];
}

struct ActiveTask {
    label: String,
    cancel: CancellationToken,
    started: Instant,
}

struct Inspection {
    archive: PathBuf,
    id: String,
    plan: Box<InstallPlan>,
    optional: BTreeSet<PathBuf>,
}

struct DeviceCode {
    verification_uri: String,
    user_code: String,
    expires_at: Instant,
}

pub(crate) struct NudgeApp {
    runtime: tokio::runtime::Handle,
    tracker: TaskTracker,
    manager: InstanceManager,
    store: ConfigStore,
    config: LauncherConfig,
    data_dir: PathBuf,
    sink: EventSink,
    receiver: mpsc::Receiver<Event>,
    page: Page,
    task: Option<ActiveTask>,
    progress: Option<InstallProgress>,
    status: String,
    error: Option<Failure>,
    logs: LogBuffer,
    instances: Vec<Instance>,
    selected_instance: String,
    versions: Vec<VersionEntry>,
    version_filter: String,
    releases_only: bool,
    install_id: String,
    install_version: String,
    install_fabric: String,
    import_path: String,
    dropped_archive: Option<PathBuf>,
    import_id: String,
    inspection: Option<Inspection>,
    java_path: String,
    microsoft_client_id: String,
    minimum_memory: u32,
    maximum_memory: u32,
    concurrency: usize,
    auto_launch: bool,
    demo: bool,
    accounts: Vec<Account>,
    device_code: Option<DeviceCode>,
    started: Instant,
    smoke_test_duration: Option<Duration>,
}

impl NudgeApp {
    pub(crate) fn new(
        context: &eframe::CreationContext<'_>,
        runtime: tokio::runtime::Handle,
        tracker: TaskTracker,
        store: ConfigStore,
        config: LauncherConfig,
        data_dir: PathBuf,
        smoke_test_duration: Option<Duration>,
    ) -> Result<Self> {
        Self::create(
            &context.egui_ctx,
            runtime,
            tracker,
            store,
            config,
            data_dir,
            smoke_test_duration,
        )
    }

    fn create(
        context: &egui::Context,
        runtime: tokio::runtime::Handle,
        tracker: TaskTracker,
        store: ConfigStore,
        config: LauncherConfig,
        data_dir: PathBuf,
        smoke_test_duration: Option<Duration>,
    ) -> Result<Self> {
        context.set_theme(egui::Theme::Dark);
        let (sender, receiver) = mpsc::channel(EVENT_CAPACITY);
        let mut app = Self {
            runtime,
            tracker,
            manager: InstanceManager::new(&data_dir)?,
            java_path: config
                .game
                .java_executable
                .as_ref()
                .map_or_else(String::new, |path| path.display().to_string()),
            microsoft_client_id: config.auth.microsoft_client_id.clone().unwrap_or_default(),
            minimum_memory: config.game.min_memory_mib,
            maximum_memory: config.game.max_memory_mib,
            concurrency: config.downloads.max_concurrent_downloads.get(),
            auto_launch: config.game.auto_launch_after_install,
            store,
            config,
            data_dir,
            sink: EventSink::new(sender, context.clone()),
            receiver,
            page: Page::Home,
            task: None,
            progress: None,
            status: "就绪".to_owned(),
            error: None,
            logs: LogBuffer::default(),
            instances: Vec::new(),
            selected_instance: String::new(),
            versions: Vec::new(),
            version_filter: String::new(),
            releases_only: true,
            install_id: String::new(),
            install_version: String::new(),
            install_fabric: String::new(),
            import_path: String::new(),
            dropped_archive: None,
            import_id: String::new(),
            inspection: None,
            demo: false,
            accounts: Vec::new(),
            device_code: None,
            started: Instant::now(),
            smoke_test_duration,
        };
        app.start(Action::Refresh, "正在读取游戏实例");
        Ok(app)
    }

    fn start(&mut self, action: Action, label: &str) {
        if self.task.is_some() {
            return;
        }
        let cancel = CancellationToken::new();
        self.task = Some(ActiveTask {
            label: label.to_owned(),
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        self.error = None;
        self.progress = None;
        label.clone_into(&mut self.status);
        self.logs.append(label);
        jobs::start(
            &self.runtime,
            &self.tracker,
            self.manager
                .clone()
                .with_concurrency(self.config.downloads.max_concurrent_downloads),
            self.config.clone(),
            action,
            cancel,
            self.sink.clone(),
        );
    }

    fn receive(&mut self) {
        // A bounded batch prevents a noisy child process from starving painting.
        for _ in 0..EVENT_CAPACITY {
            let Ok(event) = self.receiver.try_recv() else {
                break;
            };
            match event {
                Event::Progress(value) => self.progress = Some(value),
                Event::Log(text) => self.logs.append(&text),
                Event::DeviceCode {
                    verification_uri,
                    user_code,
                    expires_in,
                } => {
                    self.device_code = Some(DeviceCode {
                        verification_uri,
                        user_code,
                        expires_at: Instant::now() + Duration::from_secs(expires_in),
                    });
                    self.page = Page::Accounts;
                }
                Event::Finished(result) => {
                    self.task = None;
                    self.progress = None;
                    self.device_code = None;
                    match result {
                        Ok(outcome) => self.complete(outcome),
                        Err(error) => self.fail(error),
                    }
                }
            }
        }
    }

    fn complete(&mut self, outcome: Outcome) {
        "就绪".clone_into(&mut self.status);
        match outcome {
            Outcome::Instances(instances) => {
                self.instances = instances;
                if !self
                    .instances
                    .iter()
                    .any(|instance| instance.id == self.selected_instance)
                {
                    self.selected_instance = self
                        .instances
                        .first()
                        .map_or_else(String::new, |instance| instance.id.clone());
                }
                self.start(Action::Accounts, "正在读取已保存的账号");
            }
            Outcome::Versions(versions) => {
                if self.install_version.is_empty() {
                    self.install_version = versions
                        .iter()
                        .find(|version| version.kind.as_str() == "release")
                        .map_or_else(String::new, |version| version.id.clone());
                }
                self.status = format!("已获取 {} 个官方版本", versions.len());
                self.versions = versions;
            }
            Outcome::Plan { archive, id, plan } => {
                self.inspection = Some(Inspection {
                    archive,
                    id,
                    plan,
                    optional: BTreeSet::new(),
                });
                self.page = Page::Import;
                "压缩包解析完成，请确认安装方案后开始安装。".clone_into(&mut self.status);
            }
            Outcome::Installed(instance) => {
                let instance = *instance;
                self.selected_instance.clone_from(&instance.id);
                self.status = format!("已安装：{}", instance.name);
                let id = instance.id.clone();
                self.instances.retain(|existing| existing.id != id);
                self.instances.push(instance);
                self.instances.sort_by(|left, right| left.id.cmp(&right.id));
                self.page = Page::Instances;
                if self.config.game.auto_launch_after_install {
                    if self.demo || self.config.auth.selected_account.is_some() {
                        self.start(
                            Action::Launch {
                                id,
                                demo: self.demo,
                            },
                            "正在启动 Minecraft",
                        );
                    } else {
                        self.logs
                            .append("自动启动需要先选择已授权的微软账号，或明确启用官方试玩模式。");
                    }
                }
            }
            Outcome::Message(message) => {
                self.logs.append(&message);
                self.status = message;
            }
            Outcome::Accounts(accounts) => {
                self.accounts = accounts;
                if !self.accounts.iter().any(|account| {
                    self.config.auth.selected_account.as_deref() == Some(&account.id.to_string())
                }) {
                    self.config.auth.selected_account =
                        self.accounts.first().map(|account| account.id.to_string());
                    if let Err(error) = self.store.save(&self.config) {
                        self.fail_error(error);
                    }
                }
            }
            Outcome::SignedIn(account) => {
                self.config.auth.selected_account = Some(account.id.to_string());
                self.status = format!("已登录：{}", account.name);
                self.accounts.retain(|existing| existing.id != account.id);
                self.accounts.push(account);
                if let Err(error) = self.store.save(&self.config) {
                    self.fail_error(error);
                }
            }
        }
    }

    fn fail_error(&mut self, error: impl Into<anyhow::Error>) {
        self.fail(Failure::from_error(&error.into()));
    }

    fn fail(&mut self, error: Failure) {
        self.logs.append(&format!("错误：{}", error.summary));
        if !error.details.is_empty() {
            self.logs.append(&error.details);
        }
        self.error = Some(error);
        "请查看错误提示与日志。现有安装恢复状态已保留。".clone_into(&mut self.status);
    }

    fn navigation(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .exact_size(170.0)
            .show(ui, |ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new("Nudge")
                        .size(30.0)
                        .strong()
                        .color(Color32::from_rgb(116, 209, 184)),
                );
                ui.label(RichText::new("让 Minecraft 随你所想。").small().weak());
                ui.add_space(24.0);
                for (page, label) in Page::ALL {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        self.page = page;
                    }
                    ui.add_space(8.0);
                }
                ui.separator();
                ui.small(format!("v{} · Rust 原生应用", env!("CARGO_PKG_VERSION")));
                ui.add_space(16.0);
                ui.small("拖入 .mrpack 文件，即可解析整合包。");
            });
    }

    fn task_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("task_status").show(ui, |ui| {
            ui.add_space(8.0);
            if let Some(task) = &self.task {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "{} · 已用 {} 秒",
                        task.label,
                        task.started.elapsed().as_secs()
                    ));
                    if ui.button("取消任务 / 停止游戏").clicked() {
                        task.cancel.cancel();
                    }
                });
                if let Some(progress) = &self.progress {
                    let fraction = progress_fraction(progress.completed, progress.total);
                    ui.add(egui::ProgressBar::new(fraction).text(format!(
                        "当前阶段：已完成 {} / {} 个文件",
                        progress.completed, progress.total
                    )));
                    ui.label(
                        RichText::new(progress.path.display().to_string())
                            .small()
                            .weak(),
                    );
                } else {
                    ui.small("任务正在后台处理，您可以继续浏览其他页面。");
                }
            } else {
                ui.label(&self.status);
            }
            ui.add_space(8.0);
        });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.heading("欢迎使用 Nudge");
        ui.label("独立设计、开源实现的 Minecraft Java 版启动器。");
        ui.add_space(22.0);
        if ui.available_width() >= 700.0 {
            ui.columns(2, |columns| {
                self.home_instances_card(&mut columns[0]);
                self.home_import_card(&mut columns[1]);
            });
        } else {
            self.home_instances_card(ui);
            ui.add_space(12.0);
            self.home_import_card(ui);
        }
        ui.add_space(18.0);
        ui.label(
            "自动复用通过完整性校验的本地文件与缓存。安装中断后，可前往「游戏实例」继续安装。",
        );
        ui.label("默认安装完成后不会自动开始游戏。您可在「设置」中明确启用自动启动。");
        ui.add_space(12.0);
        ui.small(format!("数据目录：{}", self.data_dir.display()));
    }

    fn home_instances_card(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.heading(format!("{} 个游戏实例", self.instances.len()));
            ui.add(egui::Label::new("每个实例拥有独立的 Mod、配置和游戏存档。").wrap());
            if ui.button("查看游戏实例").clicked() {
                self.page = Page::Instances;
            }
        });
    }

    fn home_import_card(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.heading("即刻导入整合包");
            ui.add(egui::Label::new("拖入 MRPACK 压缩包，先确认安装方案，再开始安装。").wrap());
            if ui.button("导入整合包").clicked() {
                self.page = Page::Import;
            }
        });
    }

    fn instances(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("游戏实例");
            if ui
                .add_enabled(self.task.is_none(), egui::Button::new("刷新"))
                .clicked()
            {
                self.start(Action::Refresh, "正在读取游戏实例");
            }
        });
        ui.add_space(12.0);
        if self.instances.is_empty() {
            ui.label("还没有游戏实例。您可以安装原版游戏，或导入一个整合包。");
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for instance in &self.instances {
                    let state = if instance.installed {
                        "就绪"
                    } else {
                        "安装未完成 · 可以继续安装"
                    };
                    let label = format!(
                        "{}\n{} · {} · {state}",
                        instance.name, instance.id, instance.metadata.id
                    );
                    if ui
                        .selectable_label(self.selected_instance == instance.id, label)
                        .clicked()
                    {
                        self.selected_instance.clone_from(&instance.id);
                    }
                    ui.separator();
                }
            });
        ui.add_space(12.0);
        ui.checkbox(
            &mut self.demo,
            "使用 Minecraft 官方试玩模式（无需账号，游戏内容受限）",
        );
        let selected = self
            .instances
            .iter()
            .find(|instance| instance.id == self.selected_instance);
        let ready = selected.is_some_and(|instance| instance.installed);
        let has_selected = selected.is_some();
        let idle = self.task.is_none();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    idle && ready && (self.demo || self.config.auth.selected_account.is_some()),
                    egui::Button::new("启动游戏"),
                )
                .clicked()
            {
                self.start(
                    Action::Launch {
                        id: self.selected_instance.clone(),
                        demo: self.demo,
                    },
                    "正在启动 Minecraft",
                );
            }
            if ui
                .add_enabled(idle && ready, egui::Button::new("校验游戏文件"))
                .clicked()
            {
                self.start(
                    Action::Verify(self.selected_instance.clone()),
                    "正在校验游戏实例",
                );
            }
            if ui
                .add_enabled(idle && has_selected, egui::Button::new("修复 / 继续安装"))
                .clicked()
            {
                self.start(
                    Action::Resume(self.selected_instance.clone()),
                    "正在继续安装",
                );
            }
        });
        ui.small("Java 与内存设置为全局共用。游戏运行期间会锁定该实例，直至游戏进程退出。");
        if !self.demo && self.config.auth.selected_account.is_none() {
            ui.label("请先在「账号管理」中选择已授权账号，或明确勾选官方试玩模式。");
        }
    }

    fn install(&mut self, ui: &mut egui::Ui) {
        ui.heading("安装 Minecraft");
        ui.label("安装完成前会校验 Mojang 版本元数据、依赖库与游戏资源的完整性。");
        ui.add_space(12.0);
        egui::Grid::new("install_fields")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("实例标识");
                ui.text_edit_singleline(&mut self.install_id);
                ui.end_row();
                ui.label("Minecraft 版本");
                ui.text_edit_singleline(&mut self.install_version);
                ui.end_row();
                ui.label("Fabric 加载器版本（可选）");
                ui.text_edit_singleline(&mut self.install_fabric);
                ui.end_row();
            });
        ui.small("实例标识仅限英文字母、数字、点、连字符与下划线，不能以点结尾或使用 Windows 保留设备名。安装 Fabric 时，请填写准确的加载器版本。");
        let id_valid = instance_id_status(ui, &self.install_id);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.task.is_none(), egui::Button::new("获取官方版本列表"))
                .clicked()
            {
                self.start(Action::Versions, "正在获取 Mojang 版本清单");
            }
            if ui
                .add_enabled(
                    self.task.is_none() && id_valid && !self.install_version.trim().is_empty(),
                    egui::Button::new("开始安装"),
                )
                .clicked()
            {
                self.start(
                    Action::Install {
                        id: self.install_id.trim().to_owned(),
                        version: self.install_version.trim().to_owned(),
                        fabric: (!self.install_fabric.trim().is_empty())
                            .then(|| self.install_fabric.trim().to_owned()),
                    },
                    "正在安装游戏",
                );
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("筛选版本");
            ui.text_edit_singleline(&mut self.version_filter);
            ui.checkbox(&mut self.releases_only, "仅显示正式版");
        });
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for version in self.versions.iter().filter(|version| {
                    (!self.releases_only || version.kind.as_str() == "release")
                        && version.id.contains(&self.version_filter)
                }) {
                    if ui
                        .selectable_label(
                            self.install_version == version.id,
                            format!(
                                "{} · {}",
                                version.id,
                                version_kind_label(version.kind.as_str())
                            ),
                        )
                        .clicked()
                    {
                        self.install_version.clone_from(&version.id);
                    }
                }
            });
    }

    fn import(&mut self, ui: &mut egui::Ui) {
        ui.heading("导入整合包");
        ui.label("将本地压缩包拖入窗口，或填写文件路径。启动器会根据内部元数据识别真实格式，而非仅看扩展名。");
        let changed = egui::Grid::new("import_fields")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("压缩包路径");
                let path_changed = ui.text_edit_singleline(&mut self.import_path).changed();
                ui.end_row();
                ui.label("实例标识");
                let id_changed = ui.text_edit_singleline(&mut self.import_id).changed();
                ui.end_row();
                (path_changed, id_changed)
            })
            .inner;
        if changed.0 {
            self.dropped_archive = None;
        }
        if changed.0 || changed.1 {
            self.inspection = None;
        }
        let id_valid = instance_id_status(ui, &self.import_id);
        if ui
            .add_enabled(
                self.task.is_none() && !self.import_path.trim().is_empty() && id_valid,
                egui::Button::new("解析压缩包"),
            )
            .clicked()
        {
            self.inspect_current();
        }
        ui.separator();
        let mut requested = None;
        if let Some(inspection) = &mut self.inspection {
            let plan = &inspection.plan;
            ui.heading(format!("{} · {}", plan.name, plan.version));
            ui.label(format!(
                "Minecraft {} · {} 个必需文件 · {} 个覆盖文件 · 已排除 {} 个服务端专用文件",
                plan.minecraft_version,
                plan.required_files.len(),
                plan.overrides.len(),
                plan.excluded_files.len()
            ));
            ui.label(plan.loader.as_ref().map_or_else(
                || "原版".to_owned(),
                |loader| format!("{:?} {}", loader.kind, loader.version),
            ));
            ui.small(format!("目标目录：{}", plan.target_directory.display()));
            ui.small(format!("压缩包 SHA-256：{}", plan.archive_sha256));
            if !plan.optional_files.is_empty() {
                ui.add_space(8.0);
                ui.label("可选客户端文件（默认不勾选）：");
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for file in &plan.optional_files {
                            let mut selected = inspection.optional.contains(&file.path);
                            if ui
                                .checkbox(&mut selected, file.path.display().to_string())
                                .changed()
                            {
                                if selected {
                                    inspection.optional.insert(file.path.clone());
                                } else {
                                    inspection.optional.remove(&file.path);
                                }
                            }
                        }
                    });
            }
            if !plan.unsupported_dependencies.is_empty() {
                ui.colored_label(
                    Color32::YELLOW,
                    "此整合包声明了暂不支持的依赖，当前版本无法安装。",
                );
            }
            if ui
                .add_enabled(
                    self.task.is_none()
                        && plan_loader_supported(plan)
                        && plan.unsupported_dependencies.is_empty(),
                    egui::Button::new("确认方案并安装整合包"),
                )
                .clicked()
            {
                requested = Some(Action::Import {
                    archive: inspection.archive.clone(),
                    id: inspection.id.clone(),
                    optional: inspection.optional.clone(),
                    archive_sha256: inspection.plan.archive_sha256.clone(),
                });
            }
        } else {
            ui.weak("尚未解析压缩包。解析阶段不会下载文件，也不会安装游戏。");
        }
        if let Some(action) = requested {
            self.start(action, "正在导入整合包");
        }
    }

    fn inspect_current(&mut self) {
        self.start(
            Action::Inspect {
                archive: self
                    .dropped_archive
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(self.import_path.trim())),
                id: self.import_id.trim().to_owned(),
            },
            "正在解析压缩包",
        );
    }

    fn accounts(&mut self, ui: &mut egui::Ui) {
        ui.heading("微软账号管理");
        ui.label("请在微软官方网站完成登录。Nudge 不会索取您的密码，并会在保存账号前验证 Minecraft Java 版的游戏权益。");
        ui.small("刷新凭据仅保存在系统凭据管理器中。SQLite 只保存游戏昵称、UUID 与公开的应用客户端标识。");
        ui.add_space(12.0);
        let mut selection_changed = false;
        for account in &self.accounts {
            selection_changed |= ui
                .selectable_value(
                    &mut self.config.auth.selected_account,
                    Some(account.id.to_string()),
                    format!("{} · {}", account.name, account.id),
                )
                .changed();
        }
        if selection_changed && let Err(error) = self.store.save(&self.config) {
            self.fail_error(error);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.task.is_none() && self.config.auth.microsoft_client_id.is_some(),
                    egui::Button::new("登录微软账号"),
                )
                .clicked()
                && let Some(client_id) = self.config.auth.microsoft_client_id.clone()
            {
                self.start(Action::SignIn(client_id), "正在等待微软账号授权");
            }
            if ui
                .add_enabled(self.task.is_none(), egui::Button::new("刷新账号列表"))
                .clicked()
            {
                self.start(Action::Accounts, "正在读取已保存的账号");
            }
            if ui.button("账号设置").clicked() {
                self.page = Page::Settings;
            }
        });
        if let Some(id) = self
            .config
            .auth
            .selected_account
            .as_ref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            && ui
                .add_enabled(self.task.is_none(), egui::Button::new("退出当前账号"))
                .clicked()
        {
            self.start(Action::SignOut(id), "正在移除账号凭据");
        }
        if self.config.auth.microsoft_client_id.is_none() {
            ui.colored_label(Color32::YELLOW, "请先在「设置」中填写自行注册的微软公共客户端应用标识。访问 Minecraft 接口可能还需要微软审核批准。");
        }
        if let Some(code) = &self.device_code {
            ui.add_space(18.0);
            ui.group(|ui| {
                ui.heading("授权 Nudge");
                ui.hyperlink_to("打开微软官方授权页面", &code.verification_uri);
                ui.label(RichText::new(&code.user_code).size(28.0).monospace());
                if ui.button("复制登录代码").clicked() {
                    ui.ctx().copy_text(code.user_code.clone());
                }
                ui.label(format!(
                    "登录代码将在 {} 秒后失效。点击「取消任务 / 停止游戏」可中止授权。",
                    code.expires_at
                        .saturating_duration_since(Instant::now())
                        .as_secs()
                ));
            });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("设置");
        ui.label("Java 路径留空时，会自动查找本机已安装且符合游戏版本要求的 Java 运行时。");
        egui::Grid::new("settings_fields")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("Java 可执行文件");
                ui.text_edit_singleline(&mut self.java_path);
                ui.end_row();
                ui.label("最小内存（MiB）");
                ui.add(egui::DragValue::new(&mut self.minimum_memory).range(128..=131_072));
                ui.end_row();
                ui.label("最大内存（MiB）");
                ui.add(egui::DragValue::new(&mut self.maximum_memory).range(128..=131_072));
                ui.end_row();
                ui.label("最大并发下载数");
                ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=64));
                ui.end_row();
                ui.label("微软公共客户端应用标识");
                ui.text_edit_singleline(&mut self.microsoft_client_id);
                ui.end_row();
            });
        ui.checkbox(
            &mut self.auto_launch,
            "安装成功后自动启动游戏（须先选择已授权账号或明确启用试玩模式）",
        );
        ui.small("请注册您自己的微软公共客户端应用；Minecraft 接口可能需要微软审核。Nudge 不会内置或借用其他启动器的应用标识。");
        ui.add_space(12.0);
        if ui.button("保存设置").clicked() {
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
        ui.small(format!("配置文件：{}", self.store.path().display()));
        ui.small(format!("应用数据目录：{}", self.data_dir.display()));
    }

    fn log_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("任务与游戏日志");
            if ui.button("清空日志").clicked() {
                self.logs.clear();
            }
        });
        ui.small("最多保留最近 2000 行 / 512 KiB 日志。游戏原始日志可能包含英文，均按不可信纯文本显示；启动器不会记录身份凭据。");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in self.logs.iter() {
                    ui.monospace(line);
                }
            });
    }

    fn dropped_files(&mut self, context: &egui::Context) {
        let files = context.input(|input| input.raw.dropped_files.clone());
        if files.is_empty() {
            return;
        }
        if files.len() != 1 || self.task.is_some() {
            self.fail(Failure::message(
                "请等待当前任务结束，再拖入一个本地压缩包。",
            ));
            return;
        }
        let path = files[0].path();
        self.import_path = path.display().to_string();
        // Preserve the actual OS path rather than reopening a lossy display
        // string, which can name another file on Unix or with unusual UTF-16.
        self.dropped_archive = Some(path.to_path_buf());
        if self.import_id.is_empty() {
            self.import_id = suggested_id(
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("modpack"),
            );
        }
        self.inspection = None;
        self.page = Page::Import;
        self.inspect_current();
    }
}

impl NudgeApp {
    fn render(&mut self, ui: &mut egui::Ui) {
        self.dropped_files(ui.ctx());
        self.task_panel(ui);
        self.navigation(ui);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(12.0);
            if let Some(error) = self.error.clone() {
                ui.group(|ui| {
                    ui.colored_label(Color32::from_rgb(248, 143, 143), &error.summary);
                    if !error.details.is_empty() {
                        ui.collapsing("技术详情（原始错误信息）", |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(180.0)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(&error.details).monospace())
                                            .wrap(),
                                    );
                                });
                        });
                    }
                    if ui.small_button("关闭提示").clicked() {
                        self.error = None;
                    }
                });
                ui.add_space(8.0);
            }
            egui::ScrollArea::vertical()
                .id_salt(("page_scroll", self.page))
                .show(ui, |ui| match self.page {
                    Page::Home => self.home(ui),
                    Page::Instances => self.instances(ui),
                    Page::Install => self.install(ui),
                    Page::Import => self.import(ui),
                    Page::Accounts => self.accounts(ui),
                    Page::Settings => self.settings(ui),
                    Page::Logs => self.log_page(ui),
                });
        });
    }
}

impl eframe::App for NudgeApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        if self
            .smoke_test_duration
            .is_some_and(|duration| self.started.elapsed() >= duration)
        {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.smoke_test_duration.is_some() || self.task.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl Drop for NudgeApp {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.cancel.cancel();
        }
    }
}

fn plan_loader_supported(plan: &InstallPlan) -> bool {
    plan.loader
        .as_ref()
        .is_none_or(|loader| loader.kind == LoaderKind::Fabric)
}

fn instance_id_status(ui: &mut egui::Ui, id: &str) -> bool {
    let valid = launcher_core::instances::validate_id(id.trim()).is_ok();
    if !id.trim().is_empty() && !valid {
        ui.colored_label(
            egui::Color32::RED,
            "实例标识无效：仅限 ASCII 字母、数字、点、连字符与下划线，不能以点结尾或使用 Windows 保留设备名。",
        );
    }
    valid
}

fn suggested_id(stem: &str) -> String {
    let value: String = stem
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let value = value.trim_matches('-');
    if value.is_empty() || launcher_core::instances::validate_id(value).is_err() {
        "modpack".to_owned()
    } else {
        value.to_owned()
    }
}

// Progress bars are approximate visual ratios; exact counts remain in the
// adjacent text, so f32 precision is intentionally sufficient here.
#[allow(clippy::cast_precision_loss)]
fn progress_fraction(completed: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (completed as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn version_kind_label(kind: &str) -> &'static str {
    match kind {
        "release" => "正式版",
        "snapshot" => "快照版",
        "old_beta" => "早期 Beta 版",
        "old_alpha" => "早期 Alpha 版",
        _ => "其他版本",
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveTask, NudgeApp, Page, progress_fraction, suggested_id};
    use eframe::egui;
    use launcher_core::config::ConfigStore;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    #[test]
    fn all_pages_render_without_a_display_server() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ConfigStore::new(directory.path().join("config.json"));
        let config = store.load_or_create()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let context = egui::Context::default();
        let tracker = TaskTracker::new();
        let mut app = NudgeApp::create(
            &context,
            runtime.handle().clone(),
            tracker.clone(),
            store,
            config,
            directory.path().join("data"),
            None,
        )?;
        for size in [egui::vec2(760.0, 560.0), egui::vec2(1060.0, 740.0)] {
            for (page, _) in Page::ALL {
                app.page = page;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                let mut output = context.run_ui(input, |ui| app.render(ui));
                assert_ne!(output.shapes.len(), 0, "page produced no paint output");
                // A real renderer applies these uploads; this deliberately
                // rendererless test consumes them without GPU side effects.
                output.textures_delta.clear();
            }
        }
        let cancel = CancellationToken::new();
        app.task = Some(ActiveTask {
            label: "Test flush".to_owned(),
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        let flushed = Arc::new(AtomicBool::new(false));
        let worker_flushed = flushed.clone();
        tracker.spawn_on(
            async move {
                cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                worker_flushed.store(true, Ordering::SeqCst);
            },
            runtime.handle(),
        );
        drop(app);
        assert!(crate::drain_background(
            &runtime,
            &tracker,
            Duration::from_secs(2)
        ));
        assert!(
            flushed.load(Ordering::SeqCst),
            "App drop must allow cancellation cleanup to finish before runtime shutdown"
        );
        runtime.shutdown_timeout(Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn progress_handles_empty_or_out_of_order_stage_counts() {
        assert_eq!(progress_fraction(0, 0), 0.0);
        assert_eq!(progress_fraction(5, 10), 0.5);
        assert_eq!(progress_fraction(20, 10), 1.0);
    }

    #[test]
    fn suggested_instance_ids_never_inherit_zip_paths_or_unicode() {
        assert_eq!(suggested_id("../../My Pack"), "My-Pack");
        assert_eq!(suggested_id("中文"), "modpack");
        assert_eq!(suggested_id("con"), "modpack");
        assert_eq!(suggested_id("NUL"), "modpack");
        assert!(suggested_id(&"a".repeat(100)).len() <= 48);
    }
}
