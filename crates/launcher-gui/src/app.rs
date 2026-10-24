//! Native presentation and ephemeral interaction state.

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
use std::process::Command;

use anyhow::Result;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Margin, Order, Rect, RichText,
    Sense, Stroke, StrokeKind, UiBuilder, Vec2, Vec2b, pos2, vec2,
};
use launcher_core::{
    auth::{Account, AccountKind},
    config::{ConfigStore, LauncherConfig},
    instances::{InstallProgress, Instance, InstanceManager},
};
use launcher_minecraft::VersionEntry;
use launcher_modpack::{InstallPlan, LoaderKind};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::{
    branding,
    covers::{self, CoverLoader},
    errors::Failure,
    jobs::{self, Action, ContentEntry, ContentKind, ContentListing, Event, EventSink, Outcome},
    logs::LogBuffer,
    theme::{self, ChipKind, Icon},
};

const EVENT_CAPACITY: usize = 64;
const TOP_BAR_HEIGHT: f32 = 60.0;
const SIDEBAR_WIDTH: f32 = 206.0;
const SIDEBAR_MARGIN: f32 = 16.0;
const STATUS_HEIGHT: f32 = 42.0;
const CARD_GAP: f32 = 18.0;
const CARD_HEIGHT: f32 = 196.0;
/// Keeps page edges clear of the overlay scroll bar and its 1 px border.
const SCROLL_GUTTER: f32 = 10.0;
const POPUP_WIDTH: f32 = 330.0;
const POPUP_PADDING: f32 = 16.0;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Page {
    Home,
    Mods,
    ResourcePacks,
    Worlds,
    Modpacks,
    Downloads,
    Settings,
    More,
    Detail,
}

impl Page {
    const TOP: [(Self, &'static str); 6] = [
        (Self::Home, "游戏"),
        (Self::Mods, "模组"),
        (Self::Modpacks, "整合包"),
        (Self::Downloads, "下载"),
        (Self::Settings, "设置"),
        (Self::More, "更多"),
    ];

    const SIDE: [(Self, &'static str, Icon); 5] = [
        (Self::Home, "游戏", Icon::Gamepad),
        (Self::Mods, "模组", Icon::Mods),
        (Self::ResourcePacks, "资源包", Icon::ResourcePack),
        (Self::Worlds, "世界存档", Icon::World),
        (Self::Settings, "设置", Icon::Gear),
    ];

    fn top_index(self) -> usize {
        match self {
            Self::Home | Self::Detail => 0,
            Self::Mods | Self::ResourcePacks | Self::Worlds => 1,
            Self::Modpacks => 2,
            Self::Downloads => 3,
            Self::Settings => 4,
            Self::More => 5,
        }
    }

    fn side_index(self) -> Option<usize> {
        match self {
            Self::Home => Some(0),
            Self::Mods => Some(1),
            Self::ResourcePacks => Some(2),
            Self::Worlds => Some(3),
            Self::Settings => Some(4),
            Self::Modpacks | Self::Downloads | Self::More | Self::Detail => None,
        }
    }
}

#[derive(Clone)]
struct ActiveTask {
    label: String,
    cancel: CancellationToken,
    started: Instant,
}

#[derive(Clone)]
struct Inspection {
    archive: PathBuf,
    id: String,
    plan: Box<InstallPlan>,
    optional: BTreeSet<PathBuf>,
}

#[derive(Clone)]
struct DeviceCode {
    verification_uri: String,
    user_code: String,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccountView {
    Menu,
    Microsoft,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CardAction {
    Detail,
    Launch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct NudgeApp {
    context: egui::Context,
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
    detail_instance: String,
    detail_return: Page,
    versions: Vec<VersionEntry>,
    versions_requested: bool,
    version_filter: String,
    releases_only: bool,
    install_id: String,
    install_version: String,
    install_fabric: String,
    install_use_fabric: bool,
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
    sign_in_pending: bool,
    offline_pending: bool,
    running_instance: Option<String>,
    account_popup_open: bool,
    account_view: AccountView,
    offline_name: String,
    account_button_rect: Option<Rect>,
    popup_rect: Option<Rect>,
    content: Option<ContentListing>,
    content_target: Option<(String, ContentKind)>,
    covers: CoverLoader,
    brand_logo: egui::TextureHandle,
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
        theme::configure(context);
        let (sender, receiver) = mpsc::channel(EVENT_CAPACITY);
        let mut app = Self {
            context: context.clone(),
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
            detail_instance: String::new(),
            detail_return: Page::Home,
            versions: Vec::new(),
            versions_requested: false,
            version_filter: String::new(),
            releases_only: true,
            install_id: String::new(),
            install_version: String::new(),
            install_fabric: String::new(),
            install_use_fabric: false,
            import_path: String::new(),
            dropped_archive: None,
            import_id: String::new(),
            inspection: None,
            demo: false,
            accounts: Vec::new(),
            device_code: None,
            sign_in_pending: false,
            offline_pending: false,
            running_instance: None,
            account_popup_open: false,
            account_view: AccountView::Menu,
            offline_name: String::new(),
            account_button_rect: None,
            popup_rect: None,
            content: None,
            content_target: None,
            covers: CoverLoader::new(),
            brand_logo: branding::logo_texture(context)?,
            started: Instant::now(),
            smoke_test_duration,
        };
        app.start(Action::Refresh, "正在读取游戏实例");
        Ok(app)
    }

    fn start(&mut self, action: Action, label: impl Into<String>) -> bool {
        if self.task.is_some() {
            return false;
        }
        let label = label.into();
        let cancel = CancellationToken::new();
        self.task = Some(ActiveTask {
            label: label.clone(),
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        self.error = None;
        self.progress = None;
        self.status.clone_from(&label);
        self.logs.append(&label);
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
        true
    }

    fn cancel_task(&mut self) {
        if let Some(task) = &self.task {
            let cancel = task.cancel.clone();
            cancel.cancel();
            "正在停止当前任务…".clone_into(&mut self.status);
        }
    }

    fn receive(&mut self) {
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
                        verification_uri: verification_uri.clone(),
                        user_code,
                        expires_at: Instant::now() + Duration::from_secs(expires_in),
                    });
                    self.account_view = AccountView::Microsoft;
                    self.account_popup_open = true;
                    if verification_uri.starts_with("https://") {
                        self.context
                            .open_url(egui::OpenUrl::new_tab(&verification_uri));
                        self.logs.append("已在系统浏览器打开 Microsoft 授权页面。");
                    } else {
                        self.logs
                            .append("授权页面不是 HTTPS，已禁止自动打开，请手动访问。");
                    }
                }
                Event::Finished(result) => {
                    self.task = None;
                    self.progress = None;
                    self.device_code = None;
                    self.sign_in_pending = false;
                    self.offline_pending = false;
                    self.running_instance = None;
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
                let active: HashSet<String> = instances
                    .iter()
                    .map(|instance| instance.id.clone())
                    .collect();
                self.covers.retain_instances(&active);
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
                if self
                    .content_target
                    .as_ref()
                    .is_some_and(|(id, _)| !active.contains(id))
                {
                    self.content_target = None;
                    self.content = None;
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
                self.page = Page::Modpacks;
                "压缩包解析完成，请确认安装方案后开始安装。".clone_into(&mut self.status);
            }
            Outcome::Installed(instance) => self.installed(*instance),
            Outcome::Message(message) => {
                self.logs.append(&message);
                self.status = message;
            }
            Outcome::Accounts(accounts) => {
                self.accounts = accounts;
                self.reconcile_selected_account();
            }
            Outcome::AccountCreated { account, accounts } => {
                self.accounts = accounts;
                self.select_account(account.id);
                self.account_view = AccountView::Menu;
                "离线账户已创建，仅保存在本机。".clone_into(&mut self.status);
            }
            Outcome::SignedIn(account) => {
                self.accounts.retain(|existing| existing.id != account.id);
                self.accounts.push(account.clone());
                self.accounts
                    .sort_by_key(|item| item.name.to_ascii_lowercase());
                self.select_account(account.id);
                self.account_view = AccountView::Menu;
                self.status = format!("已登录：{}", account.name);
            }
            Outcome::Content(listing) => {
                self.content = Some(listing);
            }
            Outcome::CoverInstalled { id, path } => self.cover_installed(&id, &path),
        }
    }

    fn installed(&mut self, instance: Instance) {
        self.selected_instance.clone_from(&instance.id);
        self.status = format!("已安装：{}", instance.name);
        let id = instance.id.clone();
        self.instances.retain(|existing| existing.id != id);
        self.instances.push(instance);
        self.instances.sort_by(|left, right| left.id.cmp(&right.id));
        self.content_target = None;
        self.content = None;
        self.page = Page::Home;
        if self.config.game.auto_launch_after_install
            && (self.demo || self.selected_account().is_some())
            && self.start(
                Action::Launch {
                    id: id.clone(),
                    demo: self.demo,
                },
                "正在启动 Minecraft",
            )
        {
            self.running_instance = Some(id);
        }
    }

    fn reconcile_selected_account(&mut self) {
        let selected = self
            .config
            .auth
            .selected_account
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());
        if selected.is_some_and(|id| self.accounts.iter().any(|account| account.id == id)) {
            return;
        }
        let next = self.accounts.first().map(|account| account.id.to_string());
        if self.config.auth.selected_account != next {
            self.config.auth.selected_account = next;
            if let Err(error) = self.store.save(&self.config) {
                self.fail_error(error);
            }
        }
    }

    fn cover_installed(&mut self, id: &str, path: &Path) {
        self.covers.reload(id);
        self.status = format!("封面已更新：{}", path.display());
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

    fn selected_account(&self) -> Option<Account> {
        let selected = self.config.auth.selected_account.as_deref()?;
        self.accounts
            .iter()
            .find(|account| account.id.to_string() == selected)
            .cloned()
    }

    fn select_account(&mut self, id: Uuid) {
        let name = self
            .accounts
            .iter()
            .find(|account| account.id == id)
            .map_or_else(|| "账户".to_owned(), |account| account.name.clone());
        self.config.auth.selected_account = Some(id.to_string());
        if let Err(error) = self.store.save(&self.config) {
            self.fail_error(error);
        } else {
            self.status = format!("已切换账户：{name}");
        }
    }

    fn sync_selected_instance(&mut self) {
        if self.instances.is_empty() {
            self.selected_instance.clear();
            return;
        }
        if !self
            .instances
            .iter()
            .any(|instance| instance.id == self.selected_instance)
        {
            self.selected_instance = self.instances[0].id.clone();
        }
    }

    fn ensure_versions(&mut self) {
        if self.versions_requested || self.task.is_some() {
            return;
        }
        self.versions_requested = true;
        self.start(Action::Versions, "正在获取 Mojang 版本清单");
    }

    fn ensure_content(&mut self, kind: ContentKind) {
        if self.task.is_some() || self.instances.is_empty() {
            return;
        }
        self.sync_selected_instance();
        let id = self.selected_instance.clone();
        let target = (id.clone(), kind);
        if self.content_target.as_ref() == Some(&target) {
            return;
        }
        self.content_target = Some(target);
        self.content = None;
        self.start(
            Action::Content { id, kind },
            format!("正在读取{}", content_kind_label(kind)),
        );
    }

    fn refresh_content(&mut self, kind: ContentKind) {
        self.content_target = None;
        self.content = None;
        self.ensure_content(kind);
    }

    fn begin_sign_in(&mut self) {
        self.account_popup_open = true;
        self.account_view = AccountView::Microsoft;
        if self.task.is_some() {
            return;
        }
        let Some(client_id) = self.config.auth.microsoft_client_id.clone() else {
            "请先在设置中填写自行注册的微软公共客户端应用标识。".clone_into(&mut self.status);
            return;
        };
        self.sign_in_pending = true;
        if !self.start(Action::SignIn(client_id), "正在等待微软账号授权") {
            self.sign_in_pending = false;
        }
    }

    fn begin_offline_account(&mut self) {
        self.account_popup_open = true;
        self.account_view = AccountView::Offline;
        if self.task.is_some() {
            return;
        }
        let name = self.offline_name.trim();
        if !valid_offline_name(name) {
            self.fail(Failure::message(
                "离线玩家名称需要 1–16 位 ASCII 字母、数字或下划线。",
            ));
            return;
        }
        self.offline_pending = true;
        if !self.start(Action::AddOffline(name.to_owned()), "正在创建离线账户") {
            self.offline_pending = false;
        }
    }

    fn launch_or_resume(&mut self, instance: &Instance) {
        if self.task.is_some() {
            return;
        }
        self.selected_instance.clone_from(&instance.id);
        if !instance.installed {
            self.start(Action::Resume(instance.id.clone()), "正在继续安装游戏实例");
            return;
        }
        if !self.demo && self.selected_account().is_none() {
            self.open_account_popup();
            "启动前请先登录账户；也可以选择离线账户，或在设置中启用官方试玩模式。"
                .clone_into(&mut self.status);
            return;
        }
        if self.start(
            Action::Launch {
                id: instance.id.clone(),
                demo: self.demo,
            },
            "正在启动 Minecraft",
        ) {
            self.running_instance = Some(instance.id.clone());
        }
    }

    fn open_account_popup(&mut self) {
        self.account_popup_open = true;
        self.account_view = AccountView::Menu;
    }

    fn navigate(&mut self, page: Page) {
        self.page = page;
        if page == Page::Downloads {
            self.ensure_versions();
        }
    }

    fn open_detail(&mut self, id: &str, from: Page) {
        id.clone_into(&mut self.detail_instance);
        self.detail_return = from;
        self.page = Page::Detail;
    }

    fn inspect_current(&mut self) {
        if self.task.is_some() {
            return;
        }
        if launcher_core::instances::validate_id(self.import_id.trim()).is_err() {
            self.fail(Failure::message(
                "实例标识无效：仅限 ASCII 字母、数字、点、连字符与下划线。",
            ));
            return;
        }
        self.inspection = None;
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
}

// --------------------------------------------------------------------------
// Shell chrome
// --------------------------------------------------------------------------

impl NudgeApp {
    fn render(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        self.covers.poll(&context);
        self.dropped_files(&context);
        self.handle_shortcuts(&context);
        let full = ui.max_rect();
        let maximized = context.input(|input| input.viewport().maximized.unwrap_or(false));
        theme::paint_shell(
            ui.painter(),
            full,
            if maximized { 0 } else { theme::RADIUS_SHELL },
        );
        Self::resize_handles(ui, full, maximized);
        self.top_bar(ui, full, maximized);
        let status_top = full.bottom() - STATUS_HEIGHT;
        let sidebar_rect = Rect::from_min_max(
            pos2(
                full.left() + SIDEBAR_MARGIN,
                full.top() + TOP_BAR_HEIGHT + 2.0,
            ),
            pos2(
                full.left() + SIDEBAR_MARGIN + SIDEBAR_WIDTH,
                status_top - 12.0,
            ),
        );
        let main_rect = Rect::from_min_max(
            pos2(
                sidebar_rect.right() + 18.0,
                full.top() + TOP_BAR_HEIGHT + 2.0,
            ),
            pos2(full.right() - 20.0, status_top - 12.0),
        );
        let status_rect = Rect::from_min_max(
            pos2(sidebar_rect.left() + 2.0, status_top + 6.0),
            pos2(full.right() - 20.0, full.bottom() - 8.0),
        );
        self.sidebar(ui, sidebar_rect);
        self.main_area(ui, main_rect);
        self.status_bar(ui, status_rect);
        self.account_popup(&context, full);
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        if context.input(|input| input.key_pressed(egui::Key::Escape)) && self.account_popup_open {
            self.account_popup_open = false;
        }
        if context.input(|input| input.modifiers.ctrl) {
            let target = context.input(|input| {
                [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                    egui::Key::Num5,
                    egui::Key::Num6,
                ]
                .iter()
                .position(|key| input.key_pressed(*key))
            });
            if let Some(index) = target
                && let Some((page, _)) = Page::TOP.get(index)
            {
                self.navigate(*page);
            }
        }
    }

    fn resize_handles(ui: &mut egui::Ui, full: Rect, maximized: bool) {
        if maximized {
            return;
        }
        let edge = 6.0;
        let corner = 12.0;
        let handles = [
            (
                Rect::from_min_max(full.left_top(), pos2(full.right(), full.top() + edge)),
                egui::ResizeDirection::North,
                egui::CursorIcon::ResizeVertical,
            ),
            (
                Rect::from_min_max(
                    pos2(full.left(), full.bottom() - edge),
                    pos2(full.right(), full.bottom()),
                ),
                egui::ResizeDirection::South,
                egui::CursorIcon::ResizeVertical,
            ),
            (
                Rect::from_min_max(
                    pos2(full.left(), full.top() + edge),
                    pos2(full.left() + edge, full.bottom() - edge),
                ),
                egui::ResizeDirection::West,
                egui::CursorIcon::ResizeHorizontal,
            ),
            (
                Rect::from_min_max(
                    pos2(full.right() - edge, full.top() + edge),
                    pos2(full.right(), full.bottom() - edge),
                ),
                egui::ResizeDirection::East,
                egui::CursorIcon::ResizeHorizontal,
            ),
            (
                Rect::from_min_max(full.left_top(), full.left_top() + vec2(corner, corner)),
                egui::ResizeDirection::NorthWest,
                egui::CursorIcon::ResizeNwSe,
            ),
            (
                Rect::from_min_max(
                    pos2(full.right() - corner, full.top()),
                    pos2(full.right(), full.top() + corner),
                ),
                egui::ResizeDirection::NorthEast,
                egui::CursorIcon::ResizeNeSw,
            ),
            (
                Rect::from_min_max(
                    pos2(full.left(), full.bottom() - corner),
                    pos2(full.left() + corner, full.bottom()),
                ),
                egui::ResizeDirection::SouthWest,
                egui::CursorIcon::ResizeNeSw,
            ),
            (
                Rect::from_min_max(
                    pos2(full.right() - corner, full.bottom() - corner),
                    full.right_bottom(),
                ),
                egui::ResizeDirection::SouthEast,
                egui::CursorIcon::ResizeNwSe,
            ),
        ];
        for (index, (rect, direction, cursor)) in handles.into_iter().enumerate() {
            let response = ui.interact(
                rect,
                Id::unique(("window_resize", index)),
                Sense::click_and_drag(),
            );
            if response.hovered() {
                ui.ctx().set_cursor_icon(cursor);
            }
            if response.is_pointer_button_down_on()
                && ui.ctx().input(|input| input.pointer.any_pressed())
            {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn top_bar(&mut self, ui: &mut egui::Ui, full: Rect, maximized: bool) {
        let context = ui.ctx().clone();
        let painter = ui.painter().clone();
        let font = FontId::proportional(12.5);
        let widths: Vec<f32> = Page::TOP
            .iter()
            .map(|(_, label)| {
                painter
                    .layout_no_wrap((*label).to_owned(), font.clone(), theme::INK)
                    .size()
                    .x
                    + 28.0
            })
            .collect();
        let nav_total = widths.iter().sum::<f32>() + 8.0;
        let nav_rect = Rect::from_center_size(
            pos2(full.center().x, full.top() + TOP_BAR_HEIGHT / 2.0),
            vec2(nav_total, 40.0),
        );
        let controls_width = 34.0 * 3.0 + 8.0;
        let controls_left = full.right() - 16.0 - controls_width;

        let icon_height = 36.0;
        let icon_rect = Rect::from_min_size(
            pos2(
                full.left() + 24.0,
                full.top() + (TOP_BAR_HEIGHT - icon_height) / 2.0,
            ),
            vec2(icon_height * branding::ICON_ASPECT_RATIO, icon_height),
        );
        painter.image(
            self.brand_logo.id(),
            icon_rect,
            branding::ICON_UV,
            Color32::WHITE,
        );
        let wordmark_height = 17.0;
        let wordmark_width = wordmark_height * branding::WORDMARK_ASPECT_RATIO;
        let wordmark_left = icon_rect.right() + 8.0;
        let wordmark_rect = Rect::from_center_size(
            pos2(
                wordmark_left + wordmark_width / 2.0,
                full.top() + TOP_BAR_HEIGHT / 2.0 - 2.0,
            ),
            vec2(wordmark_width, wordmark_height),
        );
        painter.image(
            self.brand_logo.id(),
            wordmark_rect,
            branding::WORDMARK_UV,
            Color32::WHITE,
        );
        painter.text(
            pos2(wordmark_rect.left() + 0.5, wordmark_rect.bottom() + 1.0),
            Align2::LEFT_TOP,
            "MINECRAFT LAUNCHER",
            FontId::proportional(8.5),
            theme::MUTED,
        );

        theme::paint_glass(&painter, nav_rect, 20, 0.0, None, false);

        let mut item_rects = Vec::with_capacity(Page::TOP.len());
        let mut x = nav_rect.left() + 4.0;
        for ((_, _label), width) in Page::TOP.iter().zip(&widths) {
            item_rects.push(Rect::from_min_size(
                pos2(x, nav_rect.top() + 4.0),
                vec2(*width, nav_rect.height() - 8.0),
            ));
            x += width;
        }

        let selected_index = self.page.top_index();
        let animated_index = context.animate_value_with_time(
            Id::unique("top_nav_indicator"),
            selected_index as f32,
            0.22,
        );
        let lower = animated_index.floor().max(0.0) as usize;
        let upper = (lower + 1).min(item_rects.len() - 1);
        let fraction = animated_index - lower as f32;
        let left = egui::lerp(
            item_rects[lower].left()..=item_rects[upper].left(),
            fraction,
        );
        let right = egui::lerp(
            item_rects[lower].right()..=item_rects[upper].right(),
            fraction,
        );
        let indicator = Rect::from_min_max(
            pos2(left, nav_rect.top() + 4.0),
            pos2(right, nav_rect.bottom() - 4.0),
        );
        theme::paint_selection_glass(&painter, indicator, 15);

        let mut requested = None;
        for (index, ((page, label), rect)) in Page::TOP.iter().zip(&item_rects).enumerate() {
            let response = ui.interact(*rect, Id::unique(("top_nav", index)), Sense::click());
            let hover = context.animate_bool_with_time(
                Id::unique(("top_nav_hover", index)),
                response.hovered(),
                0.14,
            );
            if hover > 0.01 && selected_index != index {
                painter.rect_filled(
                    rect.shrink(2.0),
                    CornerRadius::same(14),
                    theme::mix_rgba(
                        theme::with_alpha(theme::ACCENT_SOFT, 0),
                        theme::with_alpha(theme::ACCENT_SOFT, 170),
                        hover,
                    ),
                );
            }
            let ink = if selected_index == index {
                theme::ACCENT_INK
            } else if hover > 0.2 {
                theme::INK
            } else {
                theme::MUTED
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                *label,
                FontId::proportional(12.5),
                ink,
            );
            if response.clicked() {
                requested = Some(*page);
            }
        }
        if let Some(page) = requested {
            self.navigate(page);
        }

        let control_size = vec2(34.0, 28.0);
        let mut window_action = None;
        for (index, (icon, danger, tooltip)) in [
            (Icon::Minus, false, "最小化"),
            (
                if maximized {
                    Icon::Restore
                } else {
                    Icon::Maximize
                },
                false,
                if maximized { "还原" } else { "最大化" },
            ),
            (Icon::Close, true, "关闭"),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = Rect::from_min_size(
                pos2(controls_left + index as f32 * 34.0, full.top() + 16.0),
                control_size,
            );
            if chrome_button(ui, rect, icon, danger, tooltip).clicked() {
                window_action = Some(match index {
                    0 => WindowAction::Minimize,
                    1 => WindowAction::ToggleMaximize,
                    _ => WindowAction::Close,
                });
            }
        }
        if let Some(action) = window_action {
            Self::window_action(&context, action, maximized);
        }

        let drag_top = full.top() + 8.0;
        let drag_bottom = full.top() + TOP_BAR_HEIGHT;
        let left_drag = Rect::from_min_max(
            pos2(full.left() + 8.0, drag_top),
            pos2(nav_rect.left() - 10.0, drag_bottom),
        );
        let right_drag = Rect::from_min_max(
            pos2(nav_rect.right() + 10.0, drag_top),
            pos2(controls_left - 10.0, drag_bottom),
        );
        for (index, rect) in [left_drag, right_drag].into_iter().enumerate() {
            if rect.width() <= 0.0 {
                continue;
            }
            let response = ui.interact(
                rect,
                Id::unique(("window_drag", index)),
                Sense::click_and_drag(),
            );
            let response = response.on_hover_cursor(egui::CursorIcon::Grab);
            if response.double_clicked() {
                Self::window_action(&context, WindowAction::ToggleMaximize, maximized);
            }
            if response.drag_started() {
                context.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            response.context_menu(|ui| {
                if ui.button("最小化").clicked() {
                    context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    ui.close();
                }
                if ui
                    .button(if maximized {
                        "还原窗口"
                    } else {
                        "最大化窗口"
                    })
                    .clicked()
                {
                    context.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    ui.close();
                }
                if ui.button("关闭 Nudge").clicked() {
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                    ui.close();
                }
            });
        }
    }

    fn window_action(context: &egui::Context, action: WindowAction, maximized: bool) {
        match action {
            WindowAction::Minimize => {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            WindowAction::ToggleMaximize => {
                context.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            WindowAction::Close => {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn sidebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter().clone();
        theme::paint_glass(&painter, rect, theme::RADIUS_RAIL, 0.0, None, true);
        let inner = rect.shrink2(vec2(14.0, 16.0));
        painter.text(
            pos2(inner.left() + 6.0, inner.top()),
            Align2::LEFT_TOP,
            "工作区",
            FontId::proportional(9.5),
            theme::FAINT,
        );
        let mut requested = None;
        let mut y = inner.top() + 25.0;
        for (index, (page, label, icon)) in Page::SIDE.iter().enumerate() {
            let item = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 42.0));
            let selected = self.page.side_index() == Some(index);
            if sidebar_item(ui, item, selected, *icon, label).clicked() {
                requested = Some(*page);
            }
            y += 46.0;
        }
        if let Some(page) = requested {
            self.navigate(page);
        }

        let button_rect = Rect::from_min_size(
            pos2(inner.left(), inner.bottom() - 56.0),
            vec2(inner.width(), 56.0),
        );
        self.account_button_rect = Some(button_rect);
        let account = self.selected_account();
        let hover = ui.ctx().animate_bool_with_time(
            Id::unique("account_button_hover"),
            ui.ctx()
                .pointer_hover_pos()
                .is_some_and(|position| button_rect.contains(position)),
            0.16,
        );
        theme::paint_glass(
            &painter,
            button_rect,
            theme::RADIUS_BUTTON + 2,
            hover,
            account.as_ref().map(|account| account_tint(account.kind)),
            false,
        );
        let avatar_rect = Rect::from_center_size(
            pos2(button_rect.left() + 26.0, button_rect.center().y),
            vec2(30.0, 30.0),
        );
        if let Some(account) = &account {
            theme::paint_avatar(
                &painter,
                avatar_rect,
                &account.name,
                account_tint(account.kind),
            );
        } else {
            painter.circle_filled(
                avatar_rect.center(),
                15.0,
                Color32::from_rgba_unmultiplied(236, 242, 252, 240),
            );
            theme::paint_icon(
                &painter,
                Rect::from_center_size(avatar_rect.center(), vec2(17.0, 17.0)),
                Icon::User,
                theme::ACCENT,
            );
        }
        let name = account.as_ref().map_or("登录账户", |account| &account.name);
        let detail = account.as_ref().map_or("Microsoft 或离线账户", |account| {
            account_kind_label(account.kind)
        });
        painter.text(
            pos2(button_rect.left() + 48.0, button_rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            ellipsize(name, 13),
            FontId::proportional(12.5),
            theme::INK,
        );
        painter.text(
            pos2(button_rect.left() + 48.0, button_rect.center().y + 7.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(9.5),
            theme::MUTED,
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(
                pos2(button_rect.right() - 17.0, button_rect.center().y),
                vec2(15.0, 15.0),
            ),
            Icon::ChevronDown,
            theme::MUTED,
        );
        let response = ui.interact(button_rect, Id::unique("account_button"), Sense::click());
        if response.clicked() {
            if self.account_popup_open {
                self.account_popup_open = false;
            } else {
                self.open_account_popup();
            }
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }

    fn main_area(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min))
                .id_salt("main_area"),
        );
        child.set_clip_rect(rect);
        child.set_width(rect.width());
        child.spacing_mut().item_spacing = vec2(9.0, 9.0);
        egui::ScrollArea::vertical()
            .id_salt(("page_scroll", self.page))
            .auto_shrink(Vec2b::FALSE)
            .show(&mut child, |ui| {
                ui.set_width((ui.available_width() - SCROLL_GUTTER).max(200.0));
                ui.add_space(2.0);
                if let Some(error) = self.error.clone() {
                    if Self::error_banner(ui, &error) {
                        self.error = None;
                    }
                    ui.add_space(12.0);
                }
                match self.page {
                    Page::Home => self.home(ui),
                    Page::Mods => self.content_page(ui, ContentKind::Mods),
                    Page::ResourcePacks => {
                        self.content_page(ui, ContentKind::ResourcePacks);
                    }
                    Page::Worlds => self.content_page(ui, ContentKind::Worlds),
                    Page::Modpacks => self.modpacks(ui),
                    Page::Downloads => self.downloads(ui),
                    Page::Settings => self.settings(ui),
                    Page::More => self.more(ui),
                    Page::Detail => self.detail(ui),
                }
                ui.add_space(20.0);
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter().clone();
        painter.line_segment(
            [
                pos2(rect.left(), rect.top() + 0.5),
                pos2(rect.right(), rect.top() + 0.5),
            ],
            Stroke::new(1.0, theme::BORDER),
        );
        let status_color = if self.task.is_some() {
            theme::ACCENT
        } else if self.error.is_some() {
            theme::ORANGE
        } else {
            theme::GREEN
        };
        painter.circle_filled(pos2(rect.left() + 8.0, rect.center().y), 3.5, status_color);
        let text = if let Some(task) = &self.task {
            let percent = self.progress.as_ref().map_or(0.0, |progress| {
                progress_fraction(progress.completed, progress.total) * 100.0
            });
            format!(
                "{} · {} 秒 · {:.0}%",
                task.label,
                task.started.elapsed().as_secs(),
                percent
            )
        } else {
            self.status.clone()
        };
        if self.task.is_some() {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left(), rect.top() + 0.5), vec2(rect.width(), 2.0)),
                0,
                Color32::from_rgba_unmultiplied(222, 232, 248, 220),
            );
            if let Some(progress) = &self.progress {
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(rect.left(), rect.top() + 0.5),
                        vec2(
                            rect.width() * progress_fraction(progress.completed, progress.total),
                            2.0,
                        ),
                    ),
                    0,
                    theme::ACCENT,
                );
            }
        }
        paint_ellipsized(
            &painter,
            pos2(rect.left() + 20.0, rect.center().y),
            &text,
            FontId::proportional(11.0),
            theme::MUTED,
            (rect.width() - 130.0).max(80.0),
        );
        if self.task.is_some() {
            let cancel_rect = Rect::from_min_size(
                pos2(rect.right() - 82.0, rect.center().y - 14.0),
                vec2(72.0, 28.0),
            );
            let mut cancel_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(cancel_rect)
                    .layout(Layout::left_to_right(Align::Center))
                    .id_salt("status_cancel"),
            );
            cancel_ui.set_clip_rect(cancel_rect);
            if theme::glass_button(
                &mut cancel_ui,
                "停止",
                72.0,
                true,
                Some(theme::DANGER),
                None,
                28.0,
            )
            .clicked()
            {
                self.cancel_task();
            }
        }
    }
}

// --------------------------------------------------------------------------
// Home and instance cards
// --------------------------------------------------------------------------

impl NudgeApp {
    fn home(&mut self, ui: &mut egui::Ui) {
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

    fn empty_home(&mut self, ui: &mut egui::Ui) {
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
    fn instance_card(
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

    fn paint_instance_cover(&mut self, ui: &egui::Ui, rect: Rect, instance: &Instance, zoom: f32) {
        let directory = self.data_dir.join("instances").join(&instance.id);
        if let Some(path) = covers::find_cover(&directory) {
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
    fn content_page(&mut self, ui: &mut egui::Ui, kind: ContentKind) {
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
                for instance in &self.instances {
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
        } else if matches && self.task.is_some() {
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

    fn content_listing(&mut self, ui: &mut egui::Ui, listing: &ContentListing) {
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

    fn open_directory(&mut self, path: &Path) {
        match open_directory(path) {
            Ok(()) => self.status = format!("已在文件资源管理器中打开 {}", path.display()),
            Err(error) => self.fail_error(error),
        }
    }
}

// --------------------------------------------------------------------------
// Detail, modpacks, downloads, settings and more
// --------------------------------------------------------------------------

impl NudgeApp {
    #[allow(clippy::too_many_lines)]
    fn detail(&mut self, ui: &mut egui::Ui) {
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

    fn open_game_folder(&mut self, id: &str) {
        match self.manager.game_directory(id) {
            Ok(path) => self.open_directory(&path),
            Err(error) => self.fail_error(error),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn modpacks(&mut self, ui: &mut egui::Ui) {
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
            let mut inspection = self.inspection.clone();
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
    fn downloads(&mut self, ui: &mut egui::Ui) {
        let mut refresh_versions = false;
        page_header_with(
            ui,
            "下载",
            "从 Mojang 官方版本清单安装原版或 Fabric 实例；下载先校验元数据与摘要。",
            136.0,
            |ui| {
                if theme::glass_button(
                    ui,
                    "刷新版本",
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
            self.versions_requested = false;
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
                    RichText::new("尚未获取版本列表。正在读取，或点击右上角“刷新版本”。")
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

    #[allow(clippy::too_many_lines)]
    fn settings(&mut self, ui: &mut egui::Ui) {
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

    fn save_settings(&mut self) {
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
    fn more(&mut self, ui: &mut egui::Ui) {
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

// --------------------------------------------------------------------------
// Account popup
// --------------------------------------------------------------------------

impl NudgeApp {
    fn popup_target_height(&self) -> f32 {
        // The popup is anchored by its lower edge, so its height must be known
        // before the content is laid out. Keep these estimates close to the
        // content: the previous fixed heights left a large empty pane under
        // short menus, while clipping any line would hide errors.
        let extra = 22.0 * f32::from(u8::from(self.error.is_some()));
        match self.account_view {
            AccountView::Menu if self.selected_account().is_some() => {
                let selected = self.config.auth.selected_account.as_deref();
                let others = self
                    .accounts
                    .iter()
                    .filter(|account| selected != Some(account.id.to_string().as_str()))
                    .count()
                    .min(3);
                let list = if others == 0 {
                    0.0
                } else {
                    22.0 + 47.0 * f32::from(u8::try_from(others).unwrap_or(3))
                };
                214.0 + list
            }
            AccountView::Menu => 226.0,
            AccountView::Microsoft if self.device_code.is_some() => 310.0 + extra,
            AccountView::Microsoft if self.config.auth.microsoft_client_id.is_none() => {
                192.0 + extra
            }
            AccountView::Microsoft => 196.0 + extra,
            AccountView::Offline => {
                272.0 + extra + 20.0 * f32::from(u8::from(self.offline_pending))
            }
        }
    }

    fn account_popup(&mut self, context: &egui::Context, full: Rect) {
        let t = context.animate_bool_with_time(
            Id::unique("account_popup_visible"),
            self.account_popup_open,
            0.18,
        );
        if !self.account_popup_open && t <= 0.01 {
            self.popup_rect = None;
            return;
        }
        let Some(button_rect) = self.account_button_rect else {
            return;
        };
        let target_height = self.popup_target_height();
        let height =
            context.animate_value_with_time(Id::unique("account_popup_height"), target_height, 0.2);
        let anchor = pos2(button_rect.left(), button_rect.top() - 8.0);
        let area = egui::Area::new(Id::unique("account_popup"))
            .order(Order::Foreground)
            .fixed_pos(anchor)
            .pivot(Align2::LEFT_BOTTOM)
            .constrain_to(full.shrink(8.0))
            .interactable(self.account_popup_open)
            .show(context, |ui| {
                ui.multiply_opacity(t);
                let (rect, _) =
                    ui.allocate_exact_size(vec2(POPUP_WIDTH, height.max(120.0)), Sense::hover());
                let painter = ui.painter().clone();
                theme::paint_glass(&painter, rect, 24, 0.0, None, true);
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(rect.left() + 8.0, rect.top() + 1.0),
                        vec2(rect.width() - 16.0, 1.0),
                    ),
                    0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                );
                let mut popup_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect.shrink2(vec2(POPUP_PADDING, 14.0)))
                        .layout(Layout::top_down(Align::Min))
                        .id_salt("account_popup_content"),
                );
                popup_ui.set_clip_rect(rect.shrink(5.0));
                popup_ui.set_width(rect.width() - POPUP_PADDING * 2.0);
                popup_ui.spacing_mut().item_spacing = vec2(7.0, 6.0);
                self.account_popup_content(&mut popup_ui);
                rect
            });
        self.popup_rect = Some(area.inner);
        let scaling = 0.965 + 0.035 * t;
        let pivot = area.inner.left_bottom();
        let translation = pivot.to_vec2() * (1.0 - scaling);
        context.transform_layer_shapes_after_rounding(
            area.response.layer_id,
            egui::emath::TSTransform::new(translation, scaling),
        );

        if self.account_popup_open
            && context.input(|input| input.pointer.any_pressed())
            && let Some(position) = context.input(|input| input.pointer.interact_pos())
        {
            let in_popup = area.inner.contains(position);
            let in_button = self
                .account_button_rect
                .is_some_and(|rect| rect.contains(position));
            if !in_popup && !in_button {
                self.account_popup_open = false;
            }
        }
        if self.sign_in_pending || self.offline_pending {
            context.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn account_popup_content(&mut self, ui: &mut egui::Ui) {
        match self.account_view {
            AccountView::Menu => self.account_menu(ui),
            AccountView::Microsoft => self.microsoft_view(ui),
            AccountView::Offline => self.offline_view(ui),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn account_menu(&mut self, ui: &mut egui::Ui) {
        let account = self.selected_account();
        let Some(account) = account else {
            ui.label(
                RichText::new("登录账户")
                    .size(17.0)
                    .strong()
                    .color(theme::INK),
            );
            ui.label(
                RichText::new(
                    "选择一个账户用于启动 Minecraft。Nudge 永远不会询问或保存 Microsoft 密码。",
                )
                .size(10.5)
                .color(theme::MUTED)
                .line_height(Some(15.0)),
            );
            ui.add_space(4.0);
            let can_add = self.task.is_none();
            if theme::glass_button(
                ui,
                "使用 Microsoft 登录",
                ui.available_width(),
                can_add && self.config.auth.microsoft_client_id.is_some(),
                Some(theme::ACCENT),
                Some(Icon::Microsoft),
                42.0,
            )
            .clicked()
            {
                self.begin_sign_in();
            }
            if self.config.auth.microsoft_client_id.is_none() {
                ui.label(
                    RichText::new("尚未配置微软公共客户端应用标识，请先前往设置。")
                        .size(9.5)
                        .color(theme::ORANGE),
                );
            }
            if theme::glass_button(
                ui,
                "离线账户（不验证正版）",
                ui.available_width(),
                can_add,
                None,
                Some(Icon::User),
                42.0,
            )
            .clicked()
            {
                self.account_view = AccountView::Offline;
            }
            ui.label(
                RichText::new("离线账户只保存在本机，不包含 Microsoft 凭据，也不是正版认证。")
                    .size(9.5)
                    .color(theme::FAINT)
                    .line_height(Some(14.0)),
            );
            return;
        };

        let tint = account_tint(account.kind);
        ui.label(
            RichText::new("当前账户")
                .size(10.0)
                .strong()
                .color(theme::MUTED),
        );
        let row = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 56.0));
        let (_, _) = ui.allocate_exact_size(row.size(), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(row, 14, Color32::from_rgba_unmultiplied(255, 255, 255, 190));
        painter.rect_stroke(
            row,
            14,
            Stroke::new(1.0, theme::with_alpha(tint, 90)),
            StrokeKind::Inside,
        );
        theme::paint_avatar(
            &painter,
            Rect::from_center_size(pos2(row.left() + 30.0, row.center().y), vec2(34.0, 34.0)),
            &account.name,
            tint,
        );
        paint_ellipsized(
            &painter,
            pos2(row.left() + 56.0, row.center().y - 9.0),
            &account.name,
            FontId::proportional(13.5),
            theme::INK,
            row.width() - 120.0,
        );
        painter.text(
            pos2(row.left() + 56.0, row.center().y + 9.0),
            Align2::LEFT_CENTER,
            account_kind_label(account.kind),
            FontId::proportional(9.5),
            theme::MUTED,
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(pos2(row.right() - 21.0, row.center().y), vec2(15.0, 15.0)),
            Icon::Check,
            theme::GREEN,
        );
        ui.add_space(2.0);
        let selected = self.config.auth.selected_account.clone();
        let others: Vec<Account> = self
            .accounts
            .iter()
            .filter(|item| selected.as_deref() != Some(item.id.to_string().as_str()))
            .cloned()
            .collect();
        let mut chosen = None;
        if !others.is_empty() {
            ui.label(
                RichText::new("切换账户")
                    .size(10.0)
                    .strong()
                    .color(theme::MUTED),
            );
            egui::ScrollArea::vertical()
                .id_salt("saved_accounts")
                .max_height(124.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for item in &others {
                        if account_row(ui, item, false).clicked() {
                            chosen = Some(item.id);
                        }
                        ui.add_space(3.0);
                    }
                });
        }
        if let Some(id) = chosen {
            self.select_account(id);
        }
        let can_add = self.task.is_none();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 7.0;
            let each = ((ui.available_width() - 7.0) / 2.0).max(70.0);
            if theme::glass_button(ui, "添加微软", each, can_add, None, Some(Icon::Plus), 34.0)
                .clicked()
            {
                self.account_view = AccountView::Microsoft;
            }
            if theme::glass_button(ui, "添加离线", each, can_add, None, Some(Icon::Plus), 34.0)
                .clicked()
            {
                self.account_view = AccountView::Offline;
            }
        });
        if theme::glass_button(
            ui,
            "退出当前账户",
            ui.available_width(),
            can_add,
            Some(theme::DANGER),
            Some(Icon::Logout),
            32.0,
        )
        .clicked()
        {
            self.start(Action::SignOut(account.id), "正在移除账户");
        }
        ui.label(
            RichText::new("Microsoft 刷新令牌保存在系统凭据库；离线账户没有任何凭据。")
                .size(9.0)
                .color(theme::FAINT)
                .line_height(Some(13.0)),
        );
    }

    #[allow(clippy::too_many_lines)]
    fn microsoft_view(&mut self, ui: &mut egui::Ui) {
        if popup_back(ui) {
            self.account_view = AccountView::Menu;
            return;
        }
        ui.label(
            RichText::new("使用 Microsoft 登录")
                .size(17.0)
                .strong()
                .color(theme::INK),
        );
        ui.label(
            RichText::new("通过系统浏览器完成微软官方授权，然后在下方确认登录状态。")
                .size(10.5)
                .color(theme::MUTED)
                .line_height(Some(15.0)),
        );
        if self.config.auth.microsoft_client_id.is_none() {
            warning_note(
                ui,
                "尚未配置自行注册的公共客户端应用标识。Nudge 不会借用其他启动器的应用 ID。",
            );
            if theme::glass_button(
                ui,
                "前往设置",
                ui.available_width(),
                true,
                Some(theme::ACCENT),
                Some(Icon::Gear),
                38.0,
            )
            .clicked()
            {
                self.account_popup_open = false;
                self.navigate(Page::Settings);
            }
            return;
        }
        if self.sign_in_pending || self.task.is_some() {
            if let Some(code) = self.device_code.clone() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                    ui.label(
                        RichText::new("等待微软授权…")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                });
                let code_rect =
                    Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 44.0));
                let (_, _) = ui.allocate_exact_size(code_rect.size(), Sense::hover());
                let painter = ui.painter().clone();
                painter.rect_filled(
                    code_rect,
                    12,
                    Color32::from_rgba_unmultiplied(238, 245, 255, 235),
                );
                painter.rect_stroke(
                    code_rect,
                    12,
                    Stroke::new(1.0, theme::ACCENT_LINE),
                    StrokeKind::Inside,
                );
                painter.text(
                    code_rect.center(),
                    Align2::CENTER_CENTER,
                    ellipsize(&code.user_code, 24),
                    FontId::monospace(20.0),
                    theme::ACCENT_INK,
                );
                let remaining = code
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    .as_secs();
                ui.label(
                    RichText::new(format!("代码将在 {remaining} 秒后失效。"))
                        .size(9.5)
                        .color(theme::MUTED),
                );
                let safe_uri = code.verification_uri.starts_with("https://");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 7.0;
                    let each = ((ui.available_width() - 7.0) / 2.0).max(80.0);
                    if theme::glass_button(
                        ui,
                        "打开授权页",
                        each,
                        safe_uri,
                        Some(theme::ACCENT),
                        Some(Icon::External),
                        34.0,
                    )
                    .clicked()
                    {
                        self.context
                            .open_url(egui::OpenUrl::new_tab(&code.verification_uri));
                    }
                    if theme::glass_button(ui, "复制代码", each, true, None, Some(Icon::Copy), 34.0)
                        .clicked()
                    {
                        ui.ctx().copy_text(code.user_code.clone());
                    }
                });
                if !safe_uri {
                    ui.label(
                        RichText::new("授权页面地址不是 HTTPS，已禁止自动打开。")
                            .size(9.5)
                            .color(theme::DANGER),
                    );
                }
            } else {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(15.0).color(theme::ACCENT));
                    ui.label(
                        RichText::new("正在向微软请求设备登录代码…")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                });
            }
            if let Some(task) = &self.task {
                let cancel = task.cancel.clone();
                if theme::glass_button(
                    ui,
                    "取消登录",
                    ui.available_width(),
                    true,
                    Some(theme::DANGER),
                    None,
                    32.0,
                )
                .clicked()
                {
                    cancel.cancel();
                }
            }
        } else {
            if theme::glass_button(
                ui,
                "开始 Microsoft 登录",
                ui.available_width(),
                self.task.is_none(),
                Some(theme::ACCENT),
                Some(Icon::Microsoft),
                42.0,
            )
            .clicked()
            {
                self.begin_sign_in();
            }
            ui.label(
                RichText::new(
                    "登录会在系统默认浏览器中进行。Nudge 不会看到、请求或保存你的 Microsoft 密码。",
                )
                .size(9.5)
                .color(theme::FAINT)
                .line_height(Some(14.0)),
            );
        }
        if let Some(error) = &self.error {
            ui.label(
                RichText::new(&error.summary)
                    .size(9.5)
                    .color(theme::DANGER)
                    .line_height(Some(14.0)),
            );
        }
    }

    fn offline_view(&mut self, ui: &mut egui::Ui) {
        if popup_back(ui) {
            self.account_view = AccountView::Menu;
            return;
        }
        ui.label(
            RichText::new("离线账户")
                .size(17.0)
                .strong()
                .color(theme::INK),
        );
        ui.label(
            RichText::new("创建一个只保存在本地的玩家身份。它不会验证 Minecraft 正版授权。")
                .size(10.5)
                .color(theme::MUTED)
                .line_height(Some(15.0)),
        );
        field_label(ui, "玩家名称");
        let response = glass_text_field(ui, &mut self.offline_name, "1–16 位字母、数字或下划线");
        let entered =
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        ui.label(
            RichText::new("离线账户没有密码，也不能伪装成已通过正版认证。")
                .size(9.5)
                .color(theme::ORANGE)
                .line_height(Some(14.0)),
        );
        ui.add_space(2.0);
        let valid = valid_offline_name(self.offline_name.trim());
        let ready = self.task.is_none() && valid;
        if theme::glass_button(
            ui,
            "创建并选择离线账户",
            ui.available_width(),
            ready,
            Some(theme::GREEN),
            Some(Icon::User),
            40.0,
        )
        .clicked()
            || (entered && ready)
        {
            self.begin_offline_account();
        }
        if self.offline_pending {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(13.0).color(theme::ACCENT));
                ui.label(
                    RichText::new("正在创建本地身份…")
                        .size(10.0)
                        .color(theme::MUTED),
                );
            });
        }
        if let Some(error) = &self.error {
            ui.label(
                RichText::new(&error.summary)
                    .size(9.5)
                    .color(theme::DANGER)
                    .line_height(Some(14.0)),
            );
        }
    }
}

impl NudgeApp {
    fn error_banner(ui: &mut egui::Ui, error: &Failure) -> bool {
        let mut dismiss = false;
        egui::Frame::new()
            .fill(theme::DANGER_SOFT)
            .corner_radius(14)
            .stroke(Stroke::new(1.0, Color32::from_rgb(247, 202, 207)))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::paint_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            pos2(ui.cursor().min.x + 9.0, ui.cursor().min.y + 10.0),
                            vec2(18.0, 18.0),
                        ),
                        Icon::Alert,
                        theme::DANGER,
                    );
                    ui.add_space(20.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&error.summary)
                                .size(12.0)
                                .strong()
                                .color(theme::DANGER),
                        )
                        .truncate(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("关闭").size(10.5).color(theme::DANGER),
                                )
                                .fill(Color32::TRANSPARENT)
                                .corner_radius(7),
                            )
                            .clicked()
                        {
                            dismiss = true;
                        }
                    });
                });
                if !error.details.is_empty() {
                    egui::CollapsingHeader::new(
                        RichText::new("技术详情（原始错误信息）")
                            .size(10.5)
                            .color(theme::DANGER),
                    )
                    .id_salt("error_details")
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&error.details)
                                            .monospace()
                                            .size(10.5)
                                            .color(theme::DANGER),
                                    )
                                    .wrap(),
                                );
                            });
                    });
                }
            });
        dismiss
    }

    fn dropped_files(&mut self, context: &egui::Context) {
        let files = context.input(|input| input.raw.dropped_files.clone());
        if files.is_empty() {
            return;
        }
        if files.len() != 1 || self.task.is_some() {
            self.fail(Failure::message(
                "请等待当前任务结束，再拖入一个本地压缩包或封面图片。",
            ));
            return;
        }
        let path = files[0].path().to_path_buf();
        if path.as_os_str().is_empty() {
            self.fail(Failure::message("无法读取拖入文件的本地路径。"));
            return;
        }
        let lower = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if matches!(lower.as_str(), "png" | "jpg" | "jpeg" | "webp") {
            let id = if self.page == Page::Detail && !self.detail_instance.is_empty() {
                self.detail_instance.clone()
            } else {
                self.selected_instance.clone()
            };
            if id.is_empty() {
                self.fail(Failure::message("请先选择一个游戏实例，再拖入封面图片。"));
                return;
            }
            self.start(
                Action::ImportCover { id, source: path },
                "正在处理并保存实例封面",
            );
            return;
        }

        self.import_path = path.display().to_string();
        self.dropped_archive = Some(path.clone());
        if self.import_id.is_empty() {
            self.import_id = suggested_id(
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("modpack"),
            );
        }
        self.inspection = None;
        self.page = Page::Modpacks;
        self.inspect_current();
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
        if self.smoke_test_duration.is_some() || self.task.is_some() || self.covers.has_pending() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

impl Drop for NudgeApp {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.cancel.cancel();
        }
    }
}

// --------------------------------------------------------------------------
// Small widgets, vectors and formatting
// --------------------------------------------------------------------------

fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    page_header_with(ui, title, subtitle, 0.0, |_| {});
}

fn page_header_with(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    actions_width: f32,
    actions: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal_top(|ui| {
        let available = ui.available_width();
        let text_width = (available - actions_width - 12.0).max(140.0);
        ui.allocate_ui_with_layout(vec2(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_width);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).size(28.0).strong().color(theme::INK));
            ui.add(egui::Label::new(RichText::new(subtitle).size(11.5).color(theme::MUTED)).wrap());
        });
        if actions_width > 0.0 {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                actions(ui);
            });
        }
    });
    ui.add_space(16.0);
}

fn section_title(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(15.0).strong().color(theme::INK));
    ui.add(egui::Label::new(RichText::new(subtitle).size(10.0).color(theme::MUTED)).wrap());
}

fn section_title_with_icon(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        theme::paint_icon(ui.painter(), rect, icon, theme::MUTED);
        ui.label(RichText::new(title).size(15.0).strong().color(theme::INK));
    });
    ui.add(egui::Label::new(RichText::new(subtitle).size(10.0).color(theme::MUTED)).wrap());
}

fn field_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(10.0).strong().color(theme::MUTED));
    ui.add_space(2.0);
}

fn glass_text_field(ui: &mut egui::Ui, value: &mut String, hint: &str) -> egui::Response {
    let width = ui.available_width();
    glass_text_field_with_width(ui, value, hint, width)
}

/// A glass text field with an explicit outer width.
///
/// Used inside horizontal rows where the field must not claim all remaining
/// space. The frame's horizontal inner margins (2 × 11 px) are excluded from
/// the text area so the rendered field matches `width`.
fn glass_text_field_with_width(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 235))
        .corner_radius(12)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(Margin::symmetric(11, 8))
        .show(ui, |ui| {
            ui.set_width((width - 22.0).max(0.0));
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner
}

fn info_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_sized(
            [112.0, 18.0],
            egui::Label::new(RichText::new(label).size(10.5).color(theme::MUTED)),
        );
        ui.add(
            egui::Label::new(
                RichText::new(ellipsize(value, 64))
                    .size(11.5)
                    .color(theme::INK),
            )
            .truncate(),
        )
        .on_hover_text(value);
    });
}

fn metadata_line(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{label}："))
                .size(10.0)
                .color(theme::FAINT),
        );
        ui.add(
            egui::Label::new(
                RichText::new(ellipsize(value, 58))
                    .size(10.0)
                    .color(theme::MUTED),
            )
            .truncate(),
        )
        .on_hover_text(value);
    });
}

fn warning_note(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::ORANGE_SOFT)
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).size(10.5).color(theme::ORANGE)).wrap());
        });
}

fn empty_notice(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    action: &str,
    action_icon: Icon,
    mut run: impl FnMut(),
) {
    theme::section_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(title).size(18.0).strong().color(theme::INK));
            ui.label(RichText::new(subtitle).size(11.0).color(theme::MUTED));
            ui.add_space(12.0);
            if theme::glass_button(
                ui,
                action,
                160.0,
                true,
                Some(theme::ACCENT),
                Some(action_icon),
                38.0,
            )
            .clicked()
            {
                run();
            }
            ui.add_space(6.0);
        });
    });
}

fn sidebar_item(
    ui: &mut egui::Ui,
    rect: Rect,
    selected: bool,
    icon: Icon,
    label: &str,
) -> egui::Response {
    let response = ui.interact(rect, Id::unique(("sidebar", label)), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.16);
    let painter = ui.painter().clone();
    if selected {
        theme::paint_selection_glass(&painter, rect, 14);
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left() + 1.0, rect.center().y - 10.0),
                vec2(3.0, 20.0),
            ),
            2,
            theme::ACCENT,
        );
    } else if hover > 0.01 {
        painter.rect_filled(
            rect,
            14,
            theme::mix_rgba(
                theme::with_alpha(theme::NEUTRAL_SOFT, 0),
                Color32::from_rgba_unmultiplied(244, 247, 252, 230),
                hover,
            ),
        );
    }
    let ink = if selected {
        theme::ACCENT_INK
    } else if hover > 0.2 {
        theme::INK
    } else {
        theme::MUTED
    };
    theme::paint_icon(
        &painter,
        Rect::from_center_size(pos2(rect.left() + 24.0, rect.center().y), vec2(17.0, 17.0)),
        icon,
        ink,
    );
    painter.text(
        pos2(rect.left() + 41.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        ink,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chrome_button(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: Icon,
    danger: bool,
    tooltip: &str,
) -> egui::Response {
    let response = ui.interact(
        rect,
        Id::unique((
            "chrome",
            tooltip,
            rect.min.x.to_bits(),
            rect.min.y.to_bits(),
        )),
        Sense::click(),
    );
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    if hover > 0.01 {
        let fill = if danger {
            theme::mix_rgba(
                theme::with_alpha(theme::DANGER_SOFT, 0),
                Color32::from_rgba_unmultiplied(255, 232, 235, 235),
                hover,
            )
        } else {
            theme::mix_rgba(
                theme::with_alpha(theme::NEUTRAL_SOFT, 0),
                Color32::from_rgba_unmultiplied(244, 247, 252, 235),
                hover,
            )
        };
        ui.painter()
            .rect_filled(rect.shrink(1.0), CornerRadius::same(9), fill);
    }
    let ink = if danger && hover > 0.2 {
        theme::DANGER
    } else if hover > 0.2 {
        theme::INK
    } else {
        theme::MUTED
    };
    theme::paint_icon(
        ui.painter(),
        Rect::from_center_size(
            rect.center(),
            vec2(rect.width().min(16.0), rect.height().min(16.0)),
        ),
        icon,
        ink,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.on_hover_text(tooltip)
}

#[allow(clippy::too_many_arguments)]
fn action_button_at(
    ui: &mut egui::Ui,
    rect: Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    enabled: bool,
    accent: Option<Color32>,
    icon: Option<Icon>,
    active_style: bool,
) -> egui::Response {
    let response = ui.interact(rect, Id::unique(salt), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        enabled && response.hovered(),
        0.16,
    );
    let painter = ui.painter().with_clip_rect(rect);
    if enabled || active_style {
        theme::paint_glass(&painter, rect, theme::RADIUS_BUTTON, hover, accent, true);
    } else {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_BUTTON),
            Color32::from_rgba_unmultiplied(247, 248, 251, 225),
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(theme::RADIUS_BUTTON),
            Stroke::new(1.0, theme::with_alpha(theme::BORDER, 160)),
            StrokeKind::Inside,
        );
    }
    if response.is_pointer_button_down_on() && enabled {
        painter.rect_filled(
            rect.shrink(0.5),
            CornerRadius::same(theme::RADIUS_BUTTON),
            Color32::from_rgba_unmultiplied(63, 77, 108, 18),
        );
    }
    let ink = if !enabled && !active_style {
        theme::FAINT
    } else if let Some(accent) = accent {
        theme::blend(accent, theme::INK, 0.42)
    } else {
        theme::INK
    };
    let font = FontId::proportional(12.5);
    let galley = painter.layout_no_wrap(label.to_owned(), font, ink);
    let icon_size = 15.0;
    let gap = 7.0;
    let icon_space = if icon.is_some() { icon_size + gap } else { 0.0 };
    let content_width = galley.size().x + icon_space;
    let start = rect.center().x - content_width / 2.0;
    if let Some(icon) = icon {
        theme::paint_icon(
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

fn icon_action_at(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: Icon,
    salt: impl std::hash::Hash + std::fmt::Debug,
    tooltip: &str,
) -> egui::Response {
    let response = ui.interact(rect, Id::unique(salt), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    if hover > 0.01 {
        ui.painter().rect_filled(
            rect,
            9,
            theme::mix_rgba(
                theme::with_alpha(theme::NEUTRAL_SOFT, 0),
                Color32::from_rgba_unmultiplied(241, 245, 252, 230),
                hover,
            ),
        );
    }
    theme::paint_icon(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(17.0, 17.0)),
        icon,
        if hover > 0.2 {
            theme::INK
        } else {
            theme::MUTED
        },
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
}

fn choice_chip(ui: &mut egui::Ui, label: &str, selected: bool, accent: Color32) -> egui::Response {
    let font = FontId::proportional(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), theme::INK);
    let width = galley.size().x + 26.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect,
            15,
            theme::mix_rgba(
                theme::with_alpha(accent, 16),
                theme::with_alpha(accent, 38),
                1.0,
            ),
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            15,
            Stroke::new(1.0, theme::with_alpha(accent, 170)),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(
            rect,
            15,
            theme::mix_rgba(
                Color32::from_rgba_unmultiplied(248, 250, 253, 220),
                Color32::from_rgba_unmultiplied(240, 245, 252, 235),
                hover,
            ),
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            15,
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font,
        if selected {
            theme::blend(accent, theme::INK, 0.35)
        } else {
            theme::MUTED
        },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chip(ui: &mut egui::Ui, icon: Option<Icon>, label: &str, kind: ChipKind) {
    let (fill, ink) = theme::chip_colors(kind);
    let font = FontId::proportional(10.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let icon_width = if icon.is_some() { 16.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_width + 16.0, 23.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 7, fill);
    let mut x = rect.left() + 8.0;
    if let Some(icon) = icon {
        theme::paint_icon(
            painter,
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)),
            icon,
            ink,
        );
        x += 16.0;
    }
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
}

fn paint_chip_at(
    painter: &egui::Painter,
    left_center: egui::Pos2,
    icon: Option<Icon>,
    label: &str,
    kind: ChipKind,
) -> f32 {
    let (fill, ink) = theme::chip_colors(kind);
    let font = FontId::proportional(10.0);
    let galley = painter.layout_no_wrap(label.to_owned(), font, ink);
    let icon_width = if icon.is_some() { 16.0 } else { 0.0 };
    let width = galley.size().x + icon_width + 16.0;
    let rect = Rect::from_min_size(pos2(left_center.x, left_center.y - 11.5), vec2(width, 23.0));
    painter.rect_filled(rect, 7, fill);
    let mut x = rect.left() + 8.0;
    if let Some(icon) = icon {
        theme::paint_icon(
            painter,
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)),
            icon,
            ink,
        );
        x += 16.0;
    }
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    width
}

fn selectable_row(
    ui: &mut egui::Ui,
    selected: bool,
    title: &str,
    subtitle: &str,
    icon: Option<Icon>,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 46.0), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 10, theme::ACCENT_SOFT);
        painter.rect_stroke(
            rect,
            10,
            Stroke::new(1.0, theme::ACCENT_LINE),
            StrokeKind::Inside,
        );
    } else if hover > 0.01 {
        painter.rect_filled(
            rect,
            10,
            theme::mix_rgba(
                theme::with_alpha(theme::NEUTRAL_SOFT, 0),
                theme::NEUTRAL_SOFT,
                hover,
            ),
        );
    }
    let mut x = rect.left() + 12.0;
    if let Some(icon) = icon {
        theme::paint_icon(
            painter,
            Rect::from_center_size(pos2(x + 8.0, rect.center().y), vec2(16.0, 16.0)),
            icon,
            if selected {
                theme::ACCENT
            } else {
                theme::MUTED
            },
        );
        x += 28.0;
    }
    painter.text(
        pos2(x, rect.center().y - 7.0),
        Align2::LEFT_CENTER,
        ellipsize(title, 44),
        FontId::proportional(12.5),
        theme::INK,
    );
    painter.text(
        pos2(x, rect.center().y + 8.0),
        Align2::LEFT_CENTER,
        ellipsize(subtitle, 54),
        FontId::proportional(9.5),
        theme::MUTED,
    );
    if selected {
        theme::paint_icon(
            painter,
            Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(15.0, 15.0)),
            Icon::Check,
            theme::ACCENT,
        );
    } else {
        theme::paint_icon(
            painter,
            Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(14.0, 14.0)),
            Icon::ChevronRight,
            theme::FAINT,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn account_row(ui: &mut egui::Ui, account: &Account, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    let painter = ui.painter();
    let fill = if selected {
        theme::ACCENT_SOFT
    } else {
        theme::mix_rgba(
            theme::with_alpha(theme::NEUTRAL_SOFT, 0),
            theme::NEUTRAL_SOFT,
            hover * 0.8,
        )
    };
    painter.rect_filled(rect, 10, fill);
    if selected {
        painter.rect_stroke(
            rect,
            10,
            Stroke::new(1.0, theme::ACCENT_LINE),
            StrokeKind::Inside,
        );
    }
    theme::paint_avatar(
        painter,
        Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(27.0, 27.0)),
        &account.name,
        account_tint(account.kind),
    );
    paint_ellipsized(
        painter,
        pos2(rect.left() + 43.0, rect.center().y),
        &account.name,
        FontId::proportional(11.5),
        theme::INK,
        (rect.width() - 100.0).max(40.0),
    );
    painter.text(
        pos2(rect.right() - 18.0, rect.center().y),
        Align2::RIGHT_CENTER,
        account_kind_label(account.kind),
        FontId::proportional(9.0),
        theme::MUTED,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn version_row(
    ui: &mut egui::Ui,
    selected: bool,
    id: &str,
    kind: &str,
    date: &str,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 42.0), Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.14);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 10, theme::ACCENT_SOFT);
        painter.rect_stroke(
            rect,
            10,
            Stroke::new(1.0, theme::ACCENT_LINE),
            StrokeKind::Inside,
        );
    } else if hover > 0.01 {
        painter.rect_filled(
            rect,
            10,
            theme::mix_rgba(
                theme::with_alpha(theme::NEUTRAL_SOFT, 0),
                theme::NEUTRAL_SOFT,
                hover,
            ),
        );
    }
    theme::paint_icon(
        painter,
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(15.0, 15.0)),
        Icon::Cube,
        if selected {
            theme::ACCENT
        } else {
            theme::MUTED
        },
    );
    painter.text(
        pos2(rect.left() + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        id,
        FontId::proportional(12.0),
        theme::INK,
    );
    painter.text(
        pos2(rect.right() - 84.0, rect.center().y),
        Align2::RIGHT_CENTER,
        kind,
        FontId::proportional(9.5),
        theme::MUTED,
    );
    painter.text(
        pos2(rect.right() - 14.0, rect.center().y),
        Align2::RIGHT_CENTER,
        date,
        FontId::proportional(9.0),
        theme::FAINT,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn content_row(ui: &mut egui::Ui, kind: ContentKind, entry: &ContentEntry) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 48.0), Sense::hover());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(
            rect,
            10,
            Color32::from_rgba_unmultiplied(247, 249, 253, 235),
        );
    }
    let (icon, tint) = match kind {
        ContentKind::Mods => (Icon::Mods, theme::PURPLE),
        ContentKind::ResourcePacks => (Icon::ResourcePack, theme::ACCENT),
        ContentKind::Worlds => (Icon::World, theme::GREEN),
    };
    let tile = Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(32.0, 32.0));
    painter.rect_filled(
        tile,
        10,
        theme::mix_rgba(theme::with_alpha(tint, 0), theme::with_alpha(tint, 26), 1.0),
    );
    theme::paint_icon(painter, tile.shrink(7.0), icon, tint);
    paint_ellipsized(
        painter,
        pos2(rect.left() + 44.0, rect.center().y - 8.0),
        &entry.name,
        FontId::proportional(11.5),
        if entry.enabled {
            theme::INK
        } else {
            theme::FAINT
        },
        (rect.width() - 180.0).max(60.0),
    );
    let detail = if let Some(size) = entry.size {
        format!("{} · {}", entry.detail, format_bytes(size))
    } else {
        entry.detail.clone()
    };
    painter.text(
        pos2(rect.left() + 44.0, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        ellipsize(&detail, 70),
        FontId::proportional(9.5),
        theme::MUTED,
    );
    painter.text(
        pos2(rect.right() - 16.0, rect.center().y),
        Align2::RIGHT_CENTER,
        if entry.enabled {
            "已启用"
        } else {
            "已停用"
        },
        FontId::proportional(9.0),
        if entry.enabled {
            theme::GREEN
        } else {
            theme::ORANGE
        },
    );
}

fn popup_back(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 7, Color32::from_rgba_unmultiplied(244, 247, 252, 230));
    }
    theme::paint_icon(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(15.0, 15.0)),
        Icon::Back,
        theme::MUTED,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn paint_ellipsized(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    let mut display = text.to_owned();
    let mut galley = painter.layout_no_wrap(display.clone(), font.clone(), color);
    if galley.size().x > max_width {
        let approximate = (max_width / (font.size * 0.92)).floor().max(2.0) as usize;
        display = ellipsize(text, approximate);
        galley = painter.layout_no_wrap(display, font, color);
    }
    painter.galley(
        pos2(anchor.x, anchor.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn account_kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Microsoft => "Microsoft 账户",
        AccountKind::Offline => "离线账户 · 仅本地",
    }
}

fn account_tint(kind: AccountKind) -> Color32 {
    match kind {
        AccountKind::Microsoft => theme::ACCENT,
        AccountKind::Offline => theme::GREEN,
    }
}

fn content_kind_label(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Mods => "模组",
        ContentKind::ResourcePacks => "资源包",
        ContentKind::Worlds => "世界存档",
    }
}

fn instance_id_status(ui: &mut egui::Ui, id: &str) -> bool {
    let valid = launcher_core::instances::validate_id(id.trim()).is_ok();
    if !id.trim().is_empty() && !valid {
        ui.label(
            RichText::new("实例标识无效：仅限 ASCII 字母、数字、点、连字符与下划线。")
                .size(9.5)
                .color(theme::DANGER),
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

fn valid_offline_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 16
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn plan_loader_supported(plan: &InstallPlan) -> bool {
    plan.loader
        .as_ref()
        .is_none_or(|loader| loader.kind == LoaderKind::Fabric)
}

fn loader_kind_label(kind: LoaderKind) -> &'static str {
    match kind {
        LoaderKind::Fabric => "Fabric",
        LoaderKind::Quilt => "Quilt",
        LoaderKind::NeoForge => "NeoForge",
        LoaderKind::Forge => "Forge",
    }
}

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

#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.2} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let mut shortened: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

fn cover_uv(size: Vec2, rect: Rect, zoom: f32) -> Rect {
    let source_aspect = size.x / size.y.max(1.0);
    let target_aspect = rect.width() / rect.height().max(1.0);
    let mut uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if source_aspect > target_aspect {
        let width = target_aspect / source_aspect;
        let inset = (1.0 - width) / 2.0;
        uv.min.x = inset;
        uv.max.x = 1.0 - inset;
    } else {
        let height = source_aspect / target_aspect;
        let inset = (1.0 - height) / 2.0;
        uv.min.y = inset;
        uv.max.y = 1.0 - inset;
    }
    let zoom = zoom.clamp(1.0, 1.08);
    let center = uv.center();
    let half = uv.size() * 0.5 / zoom;
    Rect::from_center_size(center, half * 2.0)
}

fn open_directory(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| anyhow::anyhow!("无法访问目录 {}：{error}", path.display()))?;
    if !metadata.is_dir() {
        anyhow::bail!("目标不是目录：{}", path.display());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|error| anyhow::anyhow!("无法打开文件资源管理器：{error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!("文件夹快捷方式仅在 Windows 版中提供。")
    }
}

// --------------------------------------------------------------------------
// Instance classification
// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Loader {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
    Quilt,
}

impl Loader {
    fn detect(instance: &Instance) -> Self {
        let mut text = instance.metadata.id.to_ascii_lowercase();
        if let Some(parent) = &instance.metadata.inherits_from {
            text.push(' ');
            text.push_str(&parent.to_ascii_lowercase());
        }
        if text.contains("neoforge") {
            Self::NeoForge
        } else if text.contains("fabric") {
            Self::Fabric
        } else if text.contains("quilt") {
            Self::Quilt
        } else if text.contains("forge") {
            Self::Forge
        } else {
            Self::Vanilla
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Vanilla => "原版",
            Self::Fabric => "Fabric",
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
            Self::Quilt => "Quilt",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Vanilla => Icon::Cube,
            Self::Fabric | Self::Forge | Self::NeoForge | Self::Quilt => Icon::Mods,
        }
    }

    fn accent(self) -> Color32 {
        match self {
            Self::Vanilla => theme::ACCENT,
            Self::Fabric | Self::Quilt => theme::PURPLE,
            Self::Forge | Self::NeoForge => theme::ORANGE,
        }
    }

    fn chip_kind(self) -> ChipKind {
        match self {
            Self::Vanilla => ChipKind::Blue,
            Self::Fabric | Self::Quilt => ChipKind::Purple,
            Self::Forge | Self::NeoForge => ChipKind::Orange,
        }
    }

    fn scenery_variant(self) -> u8 {
        match self {
            Self::Vanilla => 0,
            Self::Fabric | Self::Quilt => 1,
            Self::Forge | Self::NeoForge => 2,
        }
    }

    fn subtitle(self, instance: &Instance) -> String {
        if let Some(pack) = &instance.pack {
            return format!(
                "Minecraft {} · {} {}",
                instance.minecraft_version, pack.name, pack.version
            );
        }
        match self {
            Self::Vanilla => format!("Minecraft {}", instance.minecraft_version),
            loader => format!(
                "Minecraft {} · {}",
                instance.minecraft_version,
                loader.label()
            ),
        }
    }

    fn description(self, instance: &Instance) -> String {
        if !instance.installed {
            return "安装尚未完成，可继续安装或校验已有文件。".to_owned();
        }
        if instance.pack.is_some() {
            return "来自本地整合包，模组与配置独立管理。".to_owned();
        }
        match self {
            Self::Vanilla => "纯净的官方版本，保留原版体验。".to_owned(),
            Self::Fabric => "轻量、现代的 Fabric 模组环境。".to_owned(),
            Self::Forge => "兼容经典 Forge 模组的游戏环境。".to_owned(),
            Self::NeoForge => "面向新版本生态的 NeoForge 环境。".to_owned(),
            Self::Quilt => "基于 Quilt 的轻量模组环境。".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccountView, ActiveTask, NudgeApp, POPUP_PADDING, POPUP_WIDTH, Page, progress_fraction,
        suggested_id, valid_offline_name,
    };
    use eframe::egui;
    use launcher_core::auth::{Account, AccountKind};
    use launcher_core::config::ConfigStore;
    use launcher_core::instances::Instance;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    use tokio_util::{sync::CancellationToken, task::TaskTracker};
    use uuid::Uuid;

    fn sample_instance(
        id: &str,
        name: &str,
        version: &str,
        installed: bool,
        fabric: bool,
        pack: bool,
    ) -> Instance {
        let metadata = if fabric {
            serde_json::json!({
                "id": format!("fabric-loader-0.16.9-{version}"),
                "inheritsFrom": version
            })
        } else {
            serde_json::json!({ "id": version })
        };
        let mut value = serde_json::json!({
            "schema_version": 1,
            "id": id,
            "name": name,
            "minecraft_version": version,
            "metadata": metadata,
            "installed": installed
        });
        if pack {
            value["pack"] = serde_json::json!({
                "archive_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "name": "冒险整合包",
                "version": "1.0.0",
                "selected_optional": [],
                "files": []
            });
        }
        serde_json::from_value(value).expect("sample instance must deserialize")
    }

    fn account(name: &str, kind: AccountKind) -> Account {
        Account {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            kind,
            client_id: if kind == AccountKind::Microsoft {
                "00000000-0000-0000-0000-000000000001".to_owned()
            } else {
                String::new()
            },
        }
    }

    fn render_frame(
        context: &egui::Context,
        app: &mut NudgeApp,
        size: egui::Vec2,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        context.run_ui(input, |ui| app.render(ui))
    }

    #[allow(clippy::too_many_lines)]
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
        app.instances = vec![
            sample_instance("vanilla", "原版生存", "1.21.1", true, false, false),
            sample_instance("fabric", "Fabric 冒险", "1.21", true, true, false),
            sample_instance("pack", "城堡与樱花", "1.20.1", true, false, true),
            sample_instance("unfinished", "未完成的整合包", "1.20.1", false, false, true),
        ];
        app.accounts = vec![
            account("Steve", AccountKind::Microsoft),
            account("Alex", AccountKind::Offline),
        ];
        app.config.auth.selected_account = Some(app.accounts[0].id.to_string());
        app.device_code = None;
        let pages = [
            Page::Home,
            Page::Mods,
            Page::ResourcePacks,
            Page::Worlds,
            Page::Modpacks,
            Page::Downloads,
            Page::Settings,
            Page::More,
            Page::Detail,
        ];
        for size in [
            egui::vec2(920.0, 640.0),
            egui::vec2(1200.0, 760.0),
            egui::vec2(1600.0, 960.0),
        ] {
            for page in pages {
                app.page = page;
                app.detail_instance = "pack".to_owned();
                let mut output = render_frame(&context, &mut app, size);
                assert_ne!(output.shapes.len(), 0, "page produced no paint output");
                output.textures_delta.clear();
            }
        }

        let missing = {
            app.page = Page::Detail;
            app.detail_instance = "missing".to_owned();
            let mut output = render_frame(&context, &mut app, egui::vec2(920.0, 640.0));
            output.textures_delta.clear();
            output.shapes.len()
        };
        assert_ne!(missing, 0, "missing-instance detail page must render");

        app.page = Page::Home;
        app.account_popup_open = true;
        app.account_view = AccountView::Menu;
        let mut output = render_frame(&context, &mut app, egui::vec2(1200.0, 760.0));
        output.textures_delta.clear();
        assert_ne!(output.shapes.len(), 0, "account menu must render");

        app.account_view = AccountView::Offline;
        let mut output = render_frame(&context, &mut app, egui::vec2(1200.0, 760.0));
        output.textures_delta.clear();
        assert_ne!(output.shapes.len(), 0, "offline account form must render");

        app.account_popup_open = false;
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
    #[allow(clippy::too_many_lines)]
    fn account_popup_content_stays_within_its_estimated_height() -> anyhow::Result<()> {
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
        app.accounts = vec![
            account("Steve", AccountKind::Offline),
            account("Alex", AccountKind::Offline),
            account("Herobrine", AccountKind::Microsoft),
            account("Notch", AccountKind::Microsoft),
        ];
        app.config.auth.selected_account = Some(app.accounts[0].id.to_string());
        let measure = |app: &mut NudgeApp| -> f32 {
            let mut measured = 0.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 1600.0),
                )),
                ..Default::default()
            };
            let mut output = context.run_ui(input, |ui| {
                let mut body = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(
                            egui::pos2(16.0, 16.0),
                            egui::vec2(POPUP_WIDTH - POPUP_PADDING * 2.0, 1560.0),
                        ))
                        .layout(egui::Layout::top_down(egui::Align::Min))
                        .id_salt("measure_popup"),
                );
                body.set_width(POPUP_WIDTH - POPUP_PADDING * 2.0);
                body.spacing_mut().item_spacing = egui::vec2(7.0, 6.0);
                app.account_popup_content(&mut body);
                measured = body.min_rect().bottom() - 16.0;
            });
            output.textures_delta.clear();
            measured
        };

        app.account_popup_open = true;
        app.page = Page::Home;
        let failure = crate::errors::Failure {
            summary: "登录代码已过期，请重新开始登录。".to_owned(),
            details: String::new(),
        };

        app.account_view = AccountView::Menu;
        let no_client = measure(&mut app);
        assert!(no_client + 28.0 <= app.popup_target_height() + 0.5);

        app.accounts.truncate(1);
        app.account_view = AccountView::Menu;
        let single = measure(&mut app);
        assert!(single + 28.0 <= app.popup_target_height() + 0.5);

        app.account_view = AccountView::Menu;
        app.accounts.clear();
        app.config.auth.selected_account = None;
        let signed_out = measure(&mut app);
        assert!(signed_out + 28.0 <= app.popup_target_height() + 0.5);

        app.accounts = vec![
            account("Steve", AccountKind::Offline),
            account("Alex", AccountKind::Offline),
            account("Herobrine", AccountKind::Microsoft),
            account("Notch", AccountKind::Microsoft),
        ];
        app.config.auth.selected_account = Some(app.accounts[0].id.to_string());
        app.account_view = AccountView::Menu;
        let several = measure(&mut app);
        assert!(several + 28.0 <= app.popup_target_height() + 0.5);

        app.account_view = AccountView::Microsoft;
        app.config.auth.microsoft_client_id = None;
        let without_client = measure(&mut app);
        assert!(without_client + 28.0 <= app.popup_target_height() + 0.5);

        app.config.auth.microsoft_client_id =
            Some("00000000-0000-0000-0000-000000000001".to_owned());
        app.error = Some(failure.clone());
        let idle = measure(&mut app);
        assert!(idle + 28.0 <= app.popup_target_height() + 0.5);

        app.sign_in_pending = true;
        app.device_code = Some(super::DeviceCode {
            verification_uri: "https://www.microsoft.com/link".to_owned(),
            user_code: "ABCD1234".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(15),
        });
        app.task = Some(ActiveTask {
            label: "Microsoft sign-in".to_owned(),
            cancel: CancellationToken::new(),
            started: Instant::now(),
        });
        let code = measure(&mut app);
        assert!(code + 28.0 <= app.popup_target_height() + 0.5);

        app.account_view = AccountView::Offline;
        app.sign_in_pending = false;
        app.device_code = None;
        app.offline_pending = true;
        let form = measure(&mut app);
        assert!(form + 28.0 <= app.popup_target_height() + 0.5);

        drop(app);
        assert!(crate::drain_background(
            &runtime,
            &tracker,
            Duration::from_secs(2)
        ));
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

    #[test]
    fn offline_names_use_the_same_restricted_alphabet_as_the_core() {
        assert!(valid_offline_name("Steve_01"));
        assert!(!valid_offline_name(""));
        assert!(!valid_offline_name("玩家"));
        assert!(!valid_offline_name("has space"));
        assert!(!valid_offline_name(&"a".repeat(17)));
    }
}
