//! Ephemeral navigation, task and dialog state.

use crate::theme::Icon;
use launcher_modpack::InstallPlan;
use std::{collections::BTreeSet, path::PathBuf, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Page {
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
    pub(super) const TOP: [(Self, &'static str); 6] = [
        (Self::Home, "游戏"),
        (Self::Mods, "模组"),
        (Self::Modpacks, "整合包"),
        (Self::Downloads, "下载"),
        (Self::Settings, "设置"),
        (Self::More, "更多"),
    ];

    pub(super) const SIDE: [(Self, &'static str, Icon); 5] = [
        (Self::Home, "游戏", Icon::Gamepad),
        (Self::Mods, "模组", Icon::Mods),
        (Self::ResourcePacks, "资源包", Icon::ResourcePack),
        (Self::Worlds, "世界存档", Icon::World),
        (Self::Settings, "设置", Icon::Gear),
    ];

    pub(super) fn top_index(self) -> usize {
        match self {
            Self::Home | Self::Detail => 0,
            Self::Mods | Self::ResourcePacks | Self::Worlds => 1,
            Self::Modpacks => 2,
            Self::Downloads => 3,
            Self::Settings => 4,
            Self::More => 5,
        }
    }

    pub(super) fn side_index(self) -> Option<usize> {
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
pub(super) struct ActiveTask {
    pub(super) label: String,
    pub(super) cancel: CancellationToken,
    pub(super) started: Instant,
}

#[derive(Clone)]
pub(super) struct Inspection {
    pub(super) archive: PathBuf,
    pub(super) id: String,
    pub(super) plan: Box<InstallPlan>,
    pub(super) optional: BTreeSet<PathBuf>,
}

#[derive(Clone)]
pub(super) struct DeviceCode {
    pub(super) verification_uri: String,
    pub(super) user_code: String,
    pub(super) expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccountView {
    Menu,
    Microsoft,
    Offline,
}
