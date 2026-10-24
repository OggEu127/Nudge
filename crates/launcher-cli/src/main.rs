//! Nudge command-line frontend.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use launcher_core::{
    Launcher,
    config::{AppPaths, ConfigStore},
    instances::InstanceManager,
    logging::{self, LogFormat},
};
use launcher_minecraft::{VersionEntry, VersionManifest};

#[derive(Debug, Parser)]
#[command(name = "nudge", version, about = "Nudge Minecraft launcher CLI")]
struct Cli {
    /// Override the native configuration file path.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Override application data and isolated instance directory.
    #[arg(long, global = true, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,

    /// Emit structured JSON logs on stderr.
    #[arg(long, global = true)]
    json_logs: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect or initialize launcher configuration.
    Config(ConfigArgs),
    /// Authorize, inspect or remove legitimate Microsoft Minecraft accounts.
    Account(AccountArgs),
    /// Fetch and inspect Mojang's official Minecraft version manifest.
    Manifest(ManifestArgs),
    /// Inspect a local MRPACK archive without installing it.
    Inspect {
        /// Source ZIP/MRPACK file (format is detected from its contents).
        archive: PathBuf,
        /// Target instance ID used for the plan.
        #[arg(long)]
        instance: String,
    },
    /// Import or resume a local MRPACK client instance.
    Import {
        /// Source ZIP/MRPACK file.
        archive: PathBuf,
        /// New instance ID, or a previous import of this exact archive.
        #[arg(long)]
        instance: String,
        /// Bind the import to the archive SHA-256 printed by `inspect`.
        #[arg(long, value_name = "SHA256")]
        archive_sha256: Option<String>,
        /// Select all optional client files.
        #[arg(long, conflicts_with = "optional")]
        include_optional: bool,
        /// Select one optional client file (repeatable, game-relative path).
        #[arg(long, value_name = "PATH", conflicts_with = "include_optional")]
        optional: Vec<PathBuf>,
    },
    /// Install or resume an isolated vanilla instance.
    Install {
        /// Mojang version ID.
        version: String,
        /// Instance ID (letters, numbers, dot, hyphen, underscore).
        #[arg(long)]
        instance: String,
        /// Install an exact Fabric Loader version instead of vanilla.
        #[arg(long)]
        fabric: Option<String>,
    },
    /// List local instances.
    Instances,
    /// Resume an interrupted vanilla, Fabric or MRPACK installation.
    Resume {
        /// Previously saved instance ID.
        instance: String,
    },
    /// Hash-check every required file of an instance.
    Verify {
        /// Instance ID.
        instance: String,
    },
    /// Discover a compatible local Java runtime.
    Java {
        /// Required Java major version.
        #[arg(long, default_value_t = 21)]
        major: u32,
        /// Explicit Java executable path.
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    /// Launch a verified instance using a Microsoft account or the official demo.
    Launch {
        /// Instance ID.
        instance: String,
        /// Explicit Java executable.
        #[arg(long)]
        java: Option<PathBuf>,
        /// Use Minecraft's official restricted demo mode.
        #[arg(long)]
        demo: bool,
        /// Authorized Minecraft profile UUID (defaults to the selected account).
        #[arg(long, conflicts_with = "demo")]
        account: Option<uuid::Uuid>,
        /// Automatically stop after this many seconds for diagnostics.
        #[arg(long)]
        run_for_seconds: Option<u64>,
    },
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the effective launcher configuration as JSON.
    Show,
    /// Create the default configuration if it does not exist.
    Init,
    /// Change non-secret preferences after validation.
    Set(SettingsArgs),
}

#[derive(Debug, Args)]
struct SettingsArgs {
    #[arg(long)]
    java: Option<PathBuf>,
    #[arg(long)]
    min_memory: Option<u32>,
    #[arg(long)]
    max_memory: Option<u32>,
    #[arg(long)]
    concurrency: Option<std::num::NonZeroUsize>,
    /// Your own registered Microsoft public-client application ID.
    #[arg(long)]
    microsoft_client_id: Option<String>,
    #[arg(long)]
    auto_launch: Option<bool>,
}

#[derive(Debug, Args)]
struct AccountArgs {
    #[command(subcommand)]
    command: AccountCommand,
}

#[derive(Debug, Subcommand)]
enum AccountCommand {
    /// Authorize through the Microsoft device-code flow; never asks for a password.
    Login {
        /// Your registered public-client application ID (or configured setting).
        #[arg(long)]
        client_id: Option<String>,
    },
    /// List non-secret local account metadata.
    List,
    /// Select a previously authorized Minecraft profile.
    Select { account: uuid::Uuid },
    /// Remove an account and its refresh token from the OS credential vault.
    Logout { account: uuid::Uuid },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum VersionFilter {
    All,
    Release,
    Snapshot,
}

#[derive(Debug, Args)]
struct ManifestArgs {
    /// Version category to include.
    #[arg(long, value_enum, default_value_t = VersionFilter::All)]
    kind: VersionFilter,

    /// Maximum number of entries to print.
    #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
    limit: usize,

    /// Print the selected entries as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("invalid positive integer: {error}"))?;
    if parsed == 0 {
        return Err("value must be greater than zero".to_owned());
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    logging::init(
        "info",
        if cli.json_logs {
            LogFormat::Json
        } else {
            LogFormat::Pretty
        },
    )?;

    let config_path = match cli.config {
        Some(path) => path,
        None => AppPaths::discover()?.config_file,
    };
    let store = ConfigStore::new(config_path);
    let config = store
        .load_or_create()
        .with_context(|| format!("could not load `{}`", store.path().display()))?;

    let data_dir = match cli.data_dir {
        Some(path) => path,
        None => AppPaths::discover()?.data_dir,
    };

    Box::pin(execute(cli.command, &data_dir, &config, store.path())).await
}

async fn execute(
    command: Command,
    data_dir: &std::path::Path,
    config: &launcher_core::config::LauncherConfig,
    config_path: &std::path::Path,
) -> Result<()> {
    match command {
        Command::Config(args) => run_config(args.command, config, config_path)?,
        Command::Account(args) => {
            Box::pin(run_account(args.command, data_dir, config, config_path)).await?;
        }
        Command::Manifest(args) => {
            let manifest = Launcher::new()?.version_manifest().await?;
            print_manifest(&manifest, &args)?;
        }
        Command::Inspect { archive, instance } => {
            let manager = InstanceManager::new(data_dir)?;
            let plan = manager.inspect_modpack(&archive, &instance).await?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::Import {
            archive,
            instance,
            archive_sha256,
            include_optional,
            optional,
        } => {
            let manager = InstanceManager::new(data_dir)?
                .with_concurrency(config.downloads.max_concurrent_downloads);
            let options = launcher_core::modpack::ImportOptions {
                expected_archive_sha256: archive_sha256,
                include_all_optional: include_optional,
                optional_files: optional.into_iter().collect(),
            };
            let installed = run_import(&manager, &archive, &instance, options).await?;
            println!("imported {} ({})", installed.name, installed.id);
        }
        Command::Install {
            version,
            instance,
            fabric,
        } => {
            let manager = InstanceManager::new(data_dir)?
                .with_concurrency(config.downloads.max_concurrent_downloads);
            let installed = run_install(&manager, &instance, &version, fabric.as_deref()).await?;
            println!(
                "installed {} ({})",
                installed.id, installed.minecraft_version
            );
        }
        Command::Instances => {
            let manager = InstanceManager::new(data_dir)?;
            for instance in manager.list().await? {
                println!(
                    "{}\t{}\t{}",
                    instance.id,
                    instance.metadata.id,
                    if instance.installed {
                        "ready"
                    } else {
                        "incomplete"
                    }
                );
            }
        }
        Command::Resume { instance } => {
            let manager = InstanceManager::new(data_dir)?
                .with_concurrency(config.downloads.max_concurrent_downloads);
            run_resume(&manager, &instance).await?;
        }
        Command::Verify { instance } => {
            let manager = InstanceManager::new(data_dir)?;
            let record = manager.load(&instance).await?;
            let plan = manager.verify(&record).await?;
            println!("{} verified files for {}", plan.files.len(), record.id);
        }
        Command::Java { major, executable } => {
            let java =
                launcher_minecraft::java::discover_java(major, executable.as_deref()).await?;
            println!("Java {} at {}", java.version, java.executable.display());
        }
        Command::Launch {
            instance,
            java,
            demo,
            account,
            run_for_seconds,
        } => {
            let manager = InstanceManager::new(data_dir)?;
            Box::pin(run_game(
                &manager,
                &instance,
                java.as_deref().or(config.game.java_executable.as_deref()),
                demo,
                account,
                run_for_seconds,
                config,
            ))
            .await?;
        }
    }

    Ok(())
}

fn run_config(
    command: ConfigCommand,
    config: &launcher_core::config::LauncherConfig,
    config_path: &std::path::Path,
) -> Result<()> {
    match command {
        ConfigCommand::Show => {
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        ConfigCommand::Init => {
            println!("configuration ready at {}", config_path.display());
        }
        ConfigCommand::Set(args) => {
            let mut updated = config.clone();
            if let Some(java) = args.java {
                updated.game.java_executable = Some(java);
            }
            if let Some(memory) = args.min_memory {
                updated.game.min_memory_mib = memory;
            }
            if let Some(memory) = args.max_memory {
                updated.game.max_memory_mib = memory;
            }
            if let Some(concurrency) = args.concurrency {
                updated.downloads.max_concurrent_downloads = concurrency;
            }
            if let Some(client_id) = args.microsoft_client_id {
                updated.auth.microsoft_client_id = Some(client_id);
            }
            if let Some(auto) = args.auto_launch {
                updated.game.auto_launch_after_install = auto;
            }
            ConfigStore::new(config_path).save(&updated)?;
            println!("configuration saved at {}", config_path.display());
        }
    }
    Ok(())
}

async fn run_account(
    command: AccountCommand,
    data_dir: &std::path::Path,
    config: &launcher_core::config::LauncherConfig,
    config_path: &std::path::Path,
) -> Result<()> {
    let service = launcher_core::auth::AuthService::new(data_dir)?;
    let mut updated = config.clone();
    match command {
        AccountCommand::List => {
            for account in service.accounts().await? {
                let kind = match account.kind {
                    launcher_core::auth::AccountKind::Microsoft => "microsoft",
                    launcher_core::auth::AccountKind::Offline => "offline",
                };
                println!(
                    "{}\t{}\t{}\t{}",
                    account.id,
                    account.name,
                    kind,
                    if config.auth.selected_account.as_deref()
                        == Some(account.id.to_string().as_str())
                    {
                        "selected"
                    } else {
                        ""
                    }
                );
            }
            return Ok(());
        }
        AccountCommand::Login { client_id } => {
            let client_id = client_id.or_else(|| config.auth.microsoft_client_id.clone())
                .context("register a Microsoft public-client application first, then supply --client-id or configure auth.microsoft_client_id; see docs/authentication.md")?;
            let cancel = tokio_util::sync::CancellationToken::new();
            let login = async {
                let authorization = service.begin_login(&client_id, &cancel).await?;
                println!(
                    "Open {} and enter code {}",
                    authorization.verification_uri, authorization.user_code
                );
                println!(
                    "Waiting for authorization (valid for {} seconds). Ctrl+C cancels.",
                    authorization.expires_in
                );
                service.complete_login(authorization, &cancel).await
            };
            tokio::pin!(login);
            let account = tokio::select! {
                result = &mut login => result?,
                signal = tokio::signal::ctrl_c() => { signal?; cancel.cancel(); login.await? },
            };
            println!(
                "Authorized Minecraft profile {} ({})",
                account.name, account.id
            );
            updated.auth.microsoft_client_id = Some(client_id.clone());
            updated.auth.selected_account = Some(account.id.to_string());
        }
        AccountCommand::Select { account: id } => {
            anyhow::ensure!(
                service
                    .accounts()
                    .await?
                    .iter()
                    .any(|account| account.id == id),
                "account is not saved locally"
            );
            updated.auth.selected_account = Some(id.to_string());
        }
        AccountCommand::Logout { account: id } => {
            service.logout(id).await?;
            if updated.auth.selected_account.as_deref() == Some(id.to_string().as_str()) {
                updated.auth.selected_account = None;
            }
            println!(
                "Removed local account {id}; this does not revoke Microsoft sessions on other devices."
            );
        }
    }
    ConfigStore::new(config_path).save(&updated)?;
    Ok(())
}

async fn run_resume(manager: &InstanceManager, id: &str) -> Result<()> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let resume = manager.resume_instance(id, cancel.clone(), |event| {
        if event.completed.is_multiple_of(100) || event.completed == event.total {
            eprintln!(
                "{}/{} {}",
                event.completed,
                event.total,
                event.path.display()
            );
        }
    });
    tokio::pin!(resume);
    let result = tokio::select! {result=&mut resume=>result,result=tokio::signal::ctrl_c()=>{result?;cancel.cancel();resume.await}};
    println!("resumed {}", result?.id);
    Ok(())
}

async fn run_import(
    manager: &InstanceManager,
    archive: &std::path::Path,
    id: &str,
    options: launcher_core::modpack::ImportOptions,
) -> Result<launcher_core::instances::Instance> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let import = manager.import_modpack(archive, id, options, cancel.clone(), |event| {
        if event.completed.is_multiple_of(100) || event.completed == event.total {
            eprintln!(
                "{}/{} {}",
                event.completed,
                event.total,
                event.path.display()
            );
        }
    });
    tokio::pin!(import);
    let result = tokio::select! {result=&mut import=>result,result=tokio::signal::ctrl_c()=>{result?;cancel.cancel();import.await}};
    Ok(result?)
}

async fn run_install(
    manager: &InstanceManager,
    instance: &str,
    version: &str,
    fabric: Option<&str>,
) -> Result<launcher_core::instances::Instance> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let progress = |event: launcher_core::instances::InstallProgress| {
        if event.completed.is_multiple_of(100) || event.completed == event.total {
            eprintln!(
                "{}/{} {}",
                event.completed,
                event.total,
                event.path.display()
            );
        }
    };
    let install = async {
        if let Some(loader) = fabric {
            manager
                .install_fabric(instance, version, loader, cancel.clone(), progress)
                .await
        } else {
            manager
                .install_vanilla_controlled(instance, version, cancel.clone(), progress)
                .await
        }
    };
    tokio::pin!(install);
    let result = tokio::select! {
        result = &mut install => result,
        result = tokio::signal::ctrl_c() => { result?; cancel.cancel(); install.await },
    };
    Ok(result?)
}

async fn run_game(
    manager: &InstanceManager,
    instance: &str,
    java: Option<&std::path::Path>,
    demo: bool,
    account: Option<uuid::Uuid>,
    duration: Option<u64>,
    config: &launcher_core::config::LauncherConfig,
) -> Result<()> {
    use launcher_minecraft::launch::{LaunchIdentity, LaunchOptions, OutputStream, ProcessEvent};
    let identity = if demo {
        LaunchIdentity::demo()
    } else {
        let account = account.or_else(|| config.auth.selected_account.as_deref().and_then(|id| id.parse().ok()))
            .context("no account selected; use `nudge account login --client-id YOUR_APP_ID` or explicitly select --demo for the official trial")?;
        let service = launcher_core::auth::AuthService::new(manager.root())?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let login = service.identity(account, &cancel);
        tokio::pin!(login);
        tokio::select! {
            result = &mut login => result?,
            signal = tokio::signal::ctrl_c() => { signal?; cancel.cancel(); login.await? },
        }
    };
    let options = LaunchOptions {
        demo,
        min_memory_mib: config.game.min_memory_mib,
        max_memory_mib: config.game.max_memory_mib,
        ..LaunchOptions::default()
    };
    let prepare_cancel = tokio_util::sync::CancellationToken::new();
    let prepare = manager.launch_controlled(instance, &identity, options, java, &prepare_cancel);
    tokio::pin!(prepare);
    let mut running = tokio::select! {
        result = &mut prepare => result?,
        signal = tokio::signal::ctrl_c() => { signal?; prepare_cancel.cancel(); prepare.await? },
    };
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(duration.unwrap_or(86400)));
    tokio::pin!(deadline);
    let mut stopping = false;
    loop {
        tokio::select! {
            event = running.next_event() => match event {
                Some(ProcessEvent::Started { pid }) => eprintln!("Minecraft process started: {pid:?}"),
                Some(ProcessEvent::Output { stream, bytes }) => match stream {
                    OutputStream::Stdout => std::io::stdout().write_all(&bytes)?,
                    OutputStream::Stderr => std::io::stderr().write_all(&bytes)?,
                },
                Some(ProcessEvent::Exited(status)) => { eprintln!("Minecraft exited: {status:?}"); break; },
                None => break,
            },
            result = tokio::signal::ctrl_c(), if !stopping => { result?; let _ = running.cancel(); stopping = true; },
            () = &mut deadline, if duration.is_some() && !stopping => { let _ = running.cancel(); stopping = true; },
        }
    }
    let exit = running.wait().await?;
    anyhow::ensure!(
        exit.success || exit.cancelled,
        "Minecraft exited unsuccessfully with code {:?}",
        exit.code
    );
    Ok(())
}

fn print_manifest(manifest: &VersionManifest, args: &ManifestArgs) -> Result<()> {
    let selected: Vec<&VersionEntry> = manifest
        .versions
        .iter()
        .filter(|version| match args.kind {
            VersionFilter::All => true,
            VersionFilter::Release => version.kind.as_str() == "release",
            VersionFilter::Snapshot => version.kind.as_str() == "snapshot",
        })
        .take(args.limit)
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&selected)?);
        return Ok(());
    }

    println!("latest release:  {}", manifest.latest.release);
    println!("latest snapshot: {}", manifest.latest.snapshot);
    println!();
    for version in selected {
        println!(
            "{:<20} {:<12} {}",
            version.id,
            version.kind.as_str(),
            version.release_time
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, Parser as _};

    #[test]
    fn official_demo_cannot_be_combined_with_an_account() {
        assert!(
            Cli::try_parse_from([
                "nudge",
                "launch",
                "test",
                "--demo",
                "--account",
                "12345678-1234-4234-8234-123456789abc",
            ])
            .is_err()
        );
        let cli = Cli::try_parse_from(["nudge", "launch", "test", "--demo"])
            .expect("explicit official demo flag");
        assert!(matches!(cli.command, Command::Launch { demo: true, .. }));
    }

    #[test]
    fn invalid_bounds_and_identifiers_are_reported_by_the_parser() {
        assert!(Cli::try_parse_from(["nudge", "manifest", "--limit", "0"]).is_err());
        assert!(Cli::try_parse_from(["nudge", "config", "set", "--concurrency", "0"]).is_err());
        assert!(Cli::try_parse_from(["nudge", "account", "select", "not-a-uuid"]).is_err());
        assert!(Cli::try_parse_from(["nudge", "import", "pack.mrpack"]).is_err());
    }

    #[test]
    fn paths_with_spaces_remain_single_arguments() {
        let cli = Cli::try_parse_from([
            "nudge",
            "--data-dir",
            "/data/path with spaces",
            "import",
            "my pack.zip",
            "--instance",
            "my-pack",
            "--include-optional",
        ])
        .expect("normal import arguments");
        assert_eq!(
            cli.data_dir,
            Some(std::path::PathBuf::from("/data/path with spaces"))
        );
        assert!(matches!(
            cli.command,
            Command::Import {
                include_optional: true,
                ..
            }
        ));
    }

    #[test]
    fn import_can_bind_the_reviewed_archive_and_select_optional_files() {
        let cli = Cli::try_parse_from([
            "nudge",
            "import",
            "pack.mrpack",
            "--instance",
            "pack",
            "--archive-sha256",
            "0123456789abcdef",
            "--optional",
            "config/example.toml",
            "--optional",
            "mods/extra.jar",
        ])
        .expect("bound and selectively optional import");
        assert!(matches!(
            cli.command,
            Command::Import {
                archive_sha256: Some(ref digest),
                include_optional: false,
                ref optional,
                ..
            } if digest == "0123456789abcdef"
                && optional == &[
                    std::path::PathBuf::from("config/example.toml"),
                    std::path::PathBuf::from("mods/extra.jar"),
                ]
        ));
        assert!(
            Cli::try_parse_from([
                "nudge",
                "import",
                "pack.mrpack",
                "--instance",
                "pack",
                "--include-optional",
                "--optional",
                "config/example.toml",
            ])
            .is_err()
        );
    }
}
