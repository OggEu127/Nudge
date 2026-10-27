//! Legitimate Microsoft device authorization and Minecraft account coordination.
//!
//! A publisher-owned registered public client ID is mandatory. No client secret,
//! borrowed application ID, password login, or offline ownership bypass exists.
//! Refresh tokens are stored only in the system credential store; SQLite holds
//! public account metadata. Minecraft service contracts are isolated here because
//! their successful-response schemas are not publicly documented by Mojang.

use std::{
    fmt,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use launcher_minecraft::launch::LaunchIdentity;
use launcher_net::api::{ApiClient, ApiResponse};
use md5::{Digest as _, Md5};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha1::{Digest as _, Sha1};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const SCOPE: &str = "XboxLive.signin XboxLive.offline_access";
const KEYRING_SERVICE: &str = "io.github.OggEu127.Nudge.microsoft";

/// Authentication failures, with all upstream bodies and tokens redacted.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Cancellation was requested by the user.
    #[error("account operation cancelled")]
    Cancelled,
    /// A publisher-owned public application registration must be configured.
    #[error("configure Nudge's registered Microsoft public client ID (a non-nil UUID)")]
    ClientId,
    /// The pending device code expired.
    #[error("Microsoft device authorization expired; start login again")]
    Expired,
    /// The user denied authorization.
    #[error("Microsoft device authorization was declined")]
    Declined,
    /// The stored refresh credential is missing, revoked, or expired.
    #[error("Microsoft authorization is no longer valid; sign in again")]
    ReauthenticationRequired,
    /// An upstream service rejected a request.
    #[error("{stage} rejected the request (HTTP {status}, service code {code:?})")]
    Rejected {
        /// Fixed operation name, not upstream text.
        stage: &'static str,
        /// HTTP status.
        status: u16,
        /// Numeric Xbox error, when supplied.
        code: Option<u64>,
    },
    /// An upstream response was structurally invalid.
    #[error("{0} returned an invalid response")]
    InvalidResponse(&'static str),
    /// Ownership was not demonstrated by the authorized Minecraft service.
    #[error("the account has no playable Minecraft Java entitlement")]
    NoEntitlement,
    /// The profile associated with a refresh changed unexpectedly.
    #[error("the authorized Minecraft profile differs from the selected account")]
    ProfileMismatch,
    /// A profile cannot silently replace credentials from another app registration.
    #[error(
        "this profile is saved under another Microsoft client ID; sign it out before changing application registration"
    )]
    ApplicationChanged,
    /// No matching account is saved.
    #[error("account {0} is not saved")]
    AccountMissing(Uuid),
    /// An offline player name is not a valid Minecraft username.
    #[error("offline player names must contain 1–16 ASCII letters, digits, or underscores")]
    OfflineName,
    /// A different local account already uses this player name or UUID.
    #[error("another local account already uses this offline player name")]
    OfflineConflict,
    /// The OS credential store is absent, locked, or rejected the operation.
    #[error(
        "cannot {0} the system credential store; unlock or configure it (no plaintext fallback)"
    )]
    CredentialStore(&'static str),
    /// A database operation failed.
    #[error("account database operation failed for `{path}`")]
    Database {
        /// Account metadata file.
        path: PathBuf,
        /// Database cause; token values never reach SQL.
        #[source]
        source: rusqlite::Error,
    },
    /// Account storage paths could not be safely prepared.
    #[error("cannot prepare safe account storage at `{0}`")]
    Storage(PathBuf),
    /// A blocking credential/database operation panicked or was aborted.
    #[error("account storage worker failed")]
    Worker,
    /// Another launcher process is modifying this account store.
    /// Retained for callers that may have used this error in older versions.
    #[error("another Nudge process is using account storage at `{0}`")]
    Busy(PathBuf),
    /// A bounded HTTP transport failed.
    #[error(transparent)]
    Network(#[from] launcher_net::api::Error),
}

/// How a local account can obtain a launch identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// A Microsoft-authorized Minecraft profile with a token in the OS vault.
    Microsoft,
    /// A local-only identity that has never been authenticated by Microsoft.
    Offline,
}

impl AccountKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Offline => "offline",
        }
    }

    fn from_database(value: &str) -> Result<Self, Error> {
        match value {
            "microsoft" => Ok(Self::Microsoft),
            "offline" => Ok(Self::Offline),
            _ => Err(Error::InvalidResponse("saved account kind")),
        }
    }
}

/// Nonsecret account metadata, safe to display or persist in SQLite.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Minecraft profile UUID, used as the account selection key.
    pub id: Uuid,
    /// Current Minecraft profile name.
    pub name: String,
    /// Microsoft-authorized account or a local-only offline identity.
    pub kind: AccountKind,
    /// Publisher's registered Microsoft public application ID; empty offline.
    pub client_id: String,
}

struct SavedAccount {
    id: Option<String>,
    name: Option<String>,
    client_id: Option<String>,
    kind: Option<String>,
}

fn parse_saved_account(row: SavedAccount) -> Result<Account, Error> {
    let id = row
        .id
        .as_deref()
        .ok_or(Error::InvalidResponse("saved account metadata"))
        .and_then(|id| {
            Uuid::parse_str(id).map_err(|_| Error::InvalidResponse("saved account metadata"))
        })?;
    let name = row
        .name
        .ok_or(Error::InvalidResponse("saved account metadata"))?;
    let client_id = row
        .client_id
        .ok_or(Error::InvalidResponse("saved account metadata"))?;
    let kind = row
        .kind
        .ok_or(Error::InvalidResponse("saved account metadata"))?;
    let kind = AccountKind::from_database(&kind)?;
    validate_account(id, &name, kind, &client_id)?;
    Ok(Account {
        id,
        name,
        kind,
        client_id,
    })
}

/// Pending device authorization. Its private device secret must not be logged.
pub struct DeviceAuthorization {
    /// Official Microsoft verification page, without credentials in its URL.
    pub verification_uri: Url,
    /// Short code to show to the user (not a password).
    pub user_code: String,
    /// Authorization lifetime advertised by Microsoft, in seconds.
    pub expires_in: u64,
    client_id: String,
    device_code: Zeroizing<String>,
    deadline: tokio::time::Instant,
    interval: Duration,
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("device_code", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Replaceable credential boundary; implementations must never log secrets.
pub trait CredentialStore: Send + Sync + 'static {
    /// Reads a saved refresh token. Never put its returned value in a log.
    fn read(&self, key: String) -> impl Future<Output = Result<Zeroizing<String>, Error>> + Send;
    /// Saves a rotated refresh token before it can be lost on a later API error.
    fn write(
        &self,
        key: String,
        token: Zeroizing<String>,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    /// Removes the credential; an absent entry is treated as success.
    fn delete(&self, key: String) -> impl Future<Output = Result<(), Error>> + Send;
}

/// System-provided Windows Credential Manager, macOS Keychain or Secret Service.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentials;

fn credential_read_error(error: &keyring::Error) -> Error {
    match error {
        keyring::Error::NoEntry => Error::ReauthenticationRequired,
        _ => Error::CredentialStore("read"),
    }
}

impl CredentialStore for SystemCredentials {
    async fn read(&self, key: String) -> Result<Zeroizing<String>, Error> {
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &key)
                .map_err(|_| Error::CredentialStore("open"))?;
            entry
                .get_password()
                .map(Zeroizing::new)
                .map_err(|error| credential_read_error(&error))
        })
        .await
        .map_err(|_| Error::Worker)?
    }

    async fn write(&self, key: String, token: Zeroizing<String>) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &key)
                .map_err(|_| Error::CredentialStore("open"))?;
            entry
                .set_password(&token)
                .map_err(|_| Error::CredentialStore("write"))
        })
        .await
        .map_err(|_| Error::Worker)?
    }

    async fn delete(&self, key: String) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &key)
                .map_err(|_| Error::CredentialStore("open"))?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(Error::CredentialStore("delete from")),
            }
        })
        .await
        .map_err(|_| Error::Worker)?
    }
}

/// Exact authentication service endpoints. Production constructors never take
/// these from archives or remote instance metadata.
#[derive(Clone)]
pub struct Endpoints {
    /// OAuth device authorization endpoint.
    pub device_code: Url,
    /// OAuth polling and refresh endpoint.
    pub token: Url,
    /// Xbox user-token endpoint.
    pub xbox: Url,
    /// Xbox Security Token Service endpoint.
    pub xsts: Url,
    /// Minecraft launcher-token endpoint.
    pub minecraft: Url,
    /// Minecraft Java entitlement endpoint.
    pub entitlements: Url,
    /// Minecraft profile endpoint.
    pub profile: Url,
}

impl Endpoints {
    fn official() -> Result<Self, Error> {
        let parse =
            |value| Url::parse(value).map_err(|_| Error::InvalidResponse("built-in endpoints"));
        Ok(Self {
            device_code: parse(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
            )?,
            token: parse("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")?,
            xbox: parse("https://user.auth.xboxlive.com/user/authenticate")?,
            xsts: parse("https://xsts.auth.xboxlive.com/xsts/authorize")?,
            minecraft: parse("https://api.minecraftservices.com/launcher/login")?,
            entitlements: parse("https://api.minecraftservices.com/entitlements/license")?,
            profile: parse("https://api.minecraftservices.com/minecraft/profile")?,
        })
    }
}

/// Account coordinator shared by CLI and GUI. Clones serialize mutations through
/// one gate so a refresh cannot race a logout or a rotated-token write.
#[derive(Clone)]
pub struct AuthService<S = SystemCredentials> {
    http: ApiClient,
    endpoints: Endpoints,
    store: Arc<S>,
    database: PathBuf,
    gate: Arc<Mutex<()>>,
    namespace: String,
}

struct AccountLease<'a> {
    _file: std::fs::File,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl AuthService<SystemCredentials> {
    /// Creates the production official-service account coordinator.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn new(data_root: impl AsRef<Path>) -> Result<Self, Error> {
        Self::with_parts(
            data_root,
            ApiClient::new(concat!("Nudge/", env!("CARGO_PKG_VERSION")))?,
            Endpoints::official()?,
            SystemCredentials,
        )
    }
}

impl<S: CredentialStore> AuthService<S> {
    /// Injects a credential store and scoped service transport for tests.
    ///
    /// # Errors
    /// Returns an error if a service endpoint embeds credentials or a fragment.
    pub fn with_parts(
        data_root: impl AsRef<Path>,
        http: ApiClient,
        endpoints: Endpoints,
        store: S,
    ) -> Result<Self, Error> {
        for url in [
            &endpoints.device_code,
            &endpoints.token,
            &endpoints.xbox,
            &endpoints.xsts,
            &endpoints.minecraft,
            &endpoints.entitlements,
            &endpoints.profile,
        ] {
            if !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || url.query().is_some()
            {
                return Err(Error::InvalidResponse("configured endpoints"));
            }
        }
        let root = std::path::absolute(data_root.as_ref())
            .map_err(|_| Error::Storage(data_root.as_ref().to_owned()))?;
        let namespace = hex::encode(Sha1::digest(root.as_os_str().as_encoded_bytes()));
        Ok(Self {
            http,
            endpoints,
            store: Arc::new(store),
            database: root.join("accounts.sqlite3"),
            gate: Arc::new(Mutex::new(())),
            namespace,
        })
    }

    /// Lists public metadata only; this does not touch the OS credential store.
    ///
    /// # Errors
    /// Returns safe-path or database failures. Invalid individual rows are
    /// omitted with a warning; the stored rows are not changed.
    pub async fn accounts(&self) -> Result<Vec<Account>, Error> {
        let mut accounts = Vec::new();
        let mut skipped = 0;
        for row in self.saved_accounts().await? {
            match parse_saved_account(row) {
                Ok(account) => accounts.push(account),
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            tracing::warn!(skipped, "invalid account metadata rows omitted from list");
        }
        Ok(accounts)
    }

    async fn saved_accounts(&self) -> Result<Vec<SavedAccount>, Error> {
        self.database(|connection| {
            let mut statement = connection
                .prepare("SELECT id, name, client_id, kind FROM accounts ORDER BY name, id")?;
            let mut rows = statement.query([])?;
            let mut accounts = Vec::new();
            while let Some(row) = rows.next()? {
                accounts.push(SavedAccount {
                    id: row.get(0).ok(),
                    name: row.get(1).ok(),
                    client_id: row.get(2).ok(),
                    kind: row.get(3).ok(),
                });
            }
            Ok(accounts)
        })
        .await
    }

    /// Creates or returns a local offline identity.
    ///
    /// Offline identities are neither authorized nor ownership-checked by
    /// Microsoft. They use Minecraft's standard deterministic offline UUID so
    /// the same player name maps to the same UUID across launchers. Repeating a
    /// name with identical casing returns the existing offline account.
    ///
    /// # Errors
    /// Rejects invalid names, conflicting local accounts, cancellation, or
    /// database failures. It never writes a refresh token.
    pub async fn add_offline(
        &self,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Account, Error> {
        let name = validate_offline_name(name)?;
        let _guard = self.lock(cancel).await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let id = offline_uuid(&name);
        for row in self.saved_accounts().await? {
            let saved_id = row
                .id
                .as_deref()
                .ok_or(Error::InvalidResponse("saved account metadata"))?;
            let saved_name = row
                .name
                .as_deref()
                .ok_or(Error::InvalidResponse("saved account metadata"))?;
            if Uuid::parse_str(saved_id).is_ok_and(|saved| saved == id)
                || saved_name.eq_ignore_ascii_case(&name)
            {
                let existing = parse_saved_account(row)?;
                if existing.kind == AccountKind::Offline && existing.id == id {
                    return Ok(existing);
                }
                return Err(Error::OfflineConflict);
            }
        }
        let account = Account {
            id,
            name,
            kind: AccountKind::Offline,
            client_id: String::new(),
        };
        self.save_account(&account).await?;
        Ok(account)
    }

    /// Starts a user-initiated Microsoft public-client device login.
    ///
    /// # Errors
    /// Requires the publisher's configured non-nil application UUID and a valid
    /// official response. It does not silently authenticate any account.
    pub async fn begin_login(
        &self,
        client_id: &str,
        cancel: &CancellationToken,
    ) -> Result<DeviceAuthorization, Error> {
        let client_id = validate_client_id(client_id)?;
        let started = tokio::time::Instant::now();
        let response = self
            .http
            .post_form(
                &self.endpoints.device_code,
                &[("client_id", &client_id), ("scope", SCOPE)],
                cancel,
            )
            .await?;
        let body: DeviceResponse = decode_success(&response, "Microsoft device authorization")?;
        if body.device_code.is_empty()
            || body.device_code.len() > 16 * 1024
            || body.user_code.is_empty()
            || body.user_code.len() > 64
            || body.user_code.chars().any(char::is_control)
            || !(1..=3600).contains(&body.expires_in)
            || !(1..=60).contains(&body.interval)
            || !verification_uri_allowed(&body.verification_uri)
        {
            return Err(Error::InvalidResponse("Microsoft device authorization"));
        }
        Ok(DeviceAuthorization {
            verification_uri: body.verification_uri,
            user_code: body.user_code,
            expires_in: body.expires_in,
            client_id,
            device_code: Zeroizing::new(body.device_code),
            deadline: started + Duration::from_secs(body.expires_in),
            interval: Duration::from_secs(body.interval),
        })
    }

    /// Polls at Microsoft's required interval, then verifies Java entitlement and
    /// profile before persisting any account. The device secret is consumed.
    ///
    /// # Errors
    /// Returns cancellation, expiry, rejection, entitlement, credential-store or
    /// account-metadata failures. Never treats an unsuccessful response as login.
    pub async fn complete_login(
        &self,
        authorization: DeviceAuthorization,
        cancel: &CancellationToken,
    ) -> Result<Account, Error> {
        let mut token = self.poll(&authorization, cancel).await?;
        let _guard = self.lock(cancel).await?;
        let session = self.minecraft_session(&token.access_token, cancel).await?;
        let account = Account {
            id: session.id,
            name: session.name,
            kind: AccountKind::Microsoft,
            client_id: authorization.client_id,
        };
        match self.account(account.id).await {
            Ok(saved) if saved.client_id != account.client_id => {
                return Err(Error::ApplicationChanged);
            }
            Ok(_) | Err(Error::AccountMissing(_)) => {}
            Err(error) => return Err(error),
        }
        let refresh = token
            .refresh_token
            .take()
            .ok_or(Error::InvalidResponse("Microsoft refresh credential"))?;
        let credential_key = self.credential_key(&account);
        self.store
            .write(credential_key.clone(), Zeroizing::new(refresh))
            .await?;
        if let Err(error) = self.save_account(&account).await {
            // Do not leave a refresh token that no saved account can address.
            if self.store.delete(credential_key).await.is_err() {
                tracing::warn!("failed to remove refresh token after account persistence failure");
            }
            return Err(error);
        }
        Ok(account)
    }

    /// Refreshes the selected official account and verifies its current Java
    /// entitlement and matching profile before producing a launch identity.
    ///
    /// # Errors
    /// Returns an error for unavailable credentials, revoked authorization,
    /// missing entitlement, profile mismatch, cancellation, or service failure.
    pub async fn identity(
        &self,
        id: Uuid,
        cancel: &CancellationToken,
    ) -> Result<LaunchIdentity, Error> {
        let account = self.account(id).await?;
        if account.kind == AccountKind::Offline {
            return LaunchIdentity::offline(account.name, account.id)
                .map_err(|_| Error::InvalidResponse("offline player identity"));
        }
        let _guard = self.lock(cancel).await?;
        let account = self.account(id).await?;
        if account.kind != AccountKind::Microsoft {
            return Err(Error::InvalidResponse("saved account kind changed"));
        }
        let refresh = self.store.read(self.credential_key(&account)).await?;
        if !valid_secret(&refresh) {
            return Err(Error::ReauthenticationRequired);
        }
        let response = self
            .http
            .post_form(
                &self.endpoints.token,
                &[
                    ("client_id", &account.client_id),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh),
                    ("scope", SCOPE),
                ],
                cancel,
            )
            .await?;
        let mut token = parse_oauth_token(&response)?;
        // Refresh rotation is committed before entitlement/profile requests: a
        // later network failure must not strand an already consumed old token.
        if let Some(rotated) = token.refresh_token.take() {
            self.store
                .write(self.credential_key(&account), Zeroizing::new(rotated))
                .await?;
        }
        let session = self.minecraft_session(&token.access_token, cancel).await?;
        if session.id != id {
            return Err(Error::ProfileMismatch);
        }
        let mut identity = LaunchIdentity::new(
            session.name.clone(),
            session.id,
            session.access_token.to_string(),
        )
        .map_err(|_| Error::InvalidResponse("Minecraft profile"))?
        .with_client_id(account.client_id.clone())
        .map_err(|_| Error::ClientId)?;
        if let Some(xuid) = session.xuid {
            identity = identity
                .with_xuid(xuid)
                .map_err(|_| Error::InvalidResponse("Xbox user ID"))?;
        }
        self.save_account(&Account {
            name: session.name,
            ..account
        })
        .await?;
        Ok(identity)
    }

    /// Deletes the OS refresh token first, then removes public account metadata.
    /// This is local sign-out, not a claim of global Microsoft token revocation.
    ///
    /// # Errors
    /// On keyring failure metadata is retained so credential deletion can retry.
    pub async fn logout(&self, id: Uuid) -> Result<(), Error> {
        let _guard = self.lock(&CancellationToken::new()).await?;
        let account = self.account(id).await?;
        if account.kind == AccountKind::Microsoft {
            self.store.delete(self.credential_key(&account)).await?;
        }
        let text = id.to_string();
        self.database(move |connection| {
            connection
                .execute("DELETE FROM accounts WHERE id = ?1", [text])
                .map(|_| ())
        })
        .await
    }

    fn credential_key(&self, account: &Account) -> String {
        format!("{}:{}:{}", self.namespace, account.client_id, account.id)
    }

    async fn lock(&self, cancel: &CancellationToken) -> Result<AccountLease<'_>, Error> {
        let guard = tokio::select! { biased; () = cancel.cancelled() => return Err(Error::Cancelled), guard = self.gate.lock() => guard };
        let path = self.database.with_file_name("accounts.lock");
        let parent = path.parent().ok_or_else(|| Error::Storage(path.clone()))?;
        crate::instances::create_directory(parent)
            .await
            .map_err(|_| Error::Storage(parent.to_owned()))?;
        safe_regular_path(&path).await?;
        let worker_path = path.clone();
        let file = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&worker_path)
                .map_err(|_| Error::Storage(worker_path))
        })
        .await
        .map_err(|_| Error::Worker)??;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(Error::Cancelled),
                        () = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
                Err(_) => return Err(Error::Storage(path)),
            }
        }
        Ok(AccountLease {
            _file: file,
            _guard: guard,
        })
    }

    async fn poll(
        &self,
        authorization: &DeviceAuthorization,
        cancel: &CancellationToken,
    ) -> Result<OAuthToken, Error> {
        let mut interval = authorization.interval;
        loop {
            let next = tokio::time::Instant::now() + interval;
            if next >= authorization.deadline {
                return Err(Error::Expired);
            }
            tokio::select! { biased; () = cancel.cancelled() => return Err(Error::Cancelled), () = tokio::time::sleep_until(next) => {} }
            let fields = [
                ("client_id", authorization.client_id.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", authorization.device_code.as_str()),
            ];
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Error::Cancelled),
                () = tokio::time::sleep_until(authorization.deadline) => return Err(Error::Expired),
                response = self.http.post_form(&self.endpoints.token, &fields, cancel) => response,
            };
            let response = match response {
                Ok(response) => response,
                Err(launcher_net::api::Error::Timeout | launcher_net::api::Error::Transport) => {
                    // Transient transport failures do not invalidate the device
                    // code. Back off while keeping the authorization deadline.
                    interval = interval.saturating_mul(2).min(Duration::from_secs(30));
                    continue;
                }
                Err(launcher_net::api::Error::Cancelled) => return Err(Error::Cancelled),
                Err(error) => return Err(error.into()),
            };
            if response.status == 429 {
                interval += Duration::from_secs(5);
                continue;
            }
            if (500..600).contains(&response.status) {
                interval = interval.saturating_mul(2).min(Duration::from_secs(30));
                continue;
            }
            if (200..300).contains(&response.status) {
                return parse_oauth_token(&response);
            }
            let error: OAuthFailure = decode(&response, "Microsoft token authorization")?;
            match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                "authorization_declined" | "access_denied" => return Err(Error::Declined),
                "expired_token" | "expired_device_code" => return Err(Error::Expired),
                _ => return Err(rejected(&response, "Microsoft token authorization")),
            }
        }
    }

    async fn minecraft_session(
        &self,
        microsoft_token: &str,
        cancel: &CancellationToken,
    ) -> Result<Session, Error> {
        let xbox = self.http.post_json(&self.endpoints.xbox, &serde_json::json!({
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={microsoft_token}")},
            "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT"
        }), cancel).await?;
        let xbox: XboxToken = decode_success(&xbox, "Xbox authentication")?;
        let xbox_claim = xbox.claim()?;
        let xsts = self
            .http
            .post_json(
                &self.endpoints.xsts,
                &serde_json::json!({
                    "Properties": {"SandboxId": "RETAIL", "UserTokens": [&xbox.token]},
                    "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT"
                }),
                cancel,
            )
            .await?;
        let xsts: XboxToken = decode_success(&xsts, "Xbox security authorization")?;
        let claim = xsts.claim()?;
        if xbox_claim.uhs != claim.uhs {
            return Err(Error::InvalidResponse("Xbox user-hash consistency"));
        }
        let minecraft = self.http.post_json(&self.endpoints.minecraft, &serde_json::json!({
            "xtoken": format!("XBL3.0 x={};{}", claim.uhs, xsts.token), "platform": "PC_LAUNCHER"
        }), cancel).await?;
        let minecraft: MinecraftToken =
            decode_success(&minecraft, "Minecraft launcher authentication")?;
        if !valid_secret(&minecraft.access_token) || minecraft.expires_in == 0 {
            return Err(Error::InvalidResponse("Minecraft launcher authentication"));
        }
        let access_token = Zeroizing::new(minecraft.access_token);
        let mut url = self.endpoints.entitlements.clone();
        url.query_pairs_mut()
            .append_pair("requestId", &Uuid::new_v4().to_string());
        let entitlements = self.http.get_bearer(&url, &access_token, cancel).await?;
        let entitlements: Entitlements =
            decode_success(&entitlements, "Minecraft Java entitlement")?;
        if !entitlements
            .items
            .iter()
            .any(|item| item.name == "game_minecraft")
        {
            return Err(Error::NoEntitlement);
        }
        let profile = self
            .http
            .get_bearer(&self.endpoints.profile, &access_token, cancel)
            .await?;
        let profile: Profile = decode_success(&profile, "Minecraft profile")?;
        validate_profile(profile.id, &profile.name)?;
        Ok(Session {
            id: profile.id,
            name: profile.name,
            access_token,
            xuid: claim.xid.clone(),
        })
    }

    async fn account(&self, id: Uuid) -> Result<Account, Error> {
        let mut unreadable_id = false;
        for row in self.saved_accounts().await? {
            match row.id.as_deref() {
                Some(saved) if Uuid::parse_str(saved).is_ok_and(|saved| saved == id) => {
                    return parse_saved_account(row);
                }
                None => unreadable_id = true,
                _ => {}
            }
        }
        if unreadable_id {
            Err(Error::InvalidResponse("saved account metadata"))
        } else {
            Err(Error::AccountMissing(id))
        }
    }

    async fn save_account(&self, account: &Account) -> Result<(), Error> {
        validate_account(account.id, &account.name, account.kind, &account.client_id)?;
        let account = account.clone();
        self.database(move |connection| connection.execute(
            "INSERT INTO accounts (id, name, client_id, kind) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET name=excluded.name, client_id=excluded.client_id, kind=excluded.kind",
            params![account.id.to_string(), account.name, account.client_id, account.kind.as_str()],
        ).map(|_| ())).await
    }

    async fn database<T, F>(&self, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let path = &self.database;
        let parent = path.parent().ok_or_else(|| Error::Storage(path.clone()))?;
        crate::instances::create_directory(parent)
            .await
            .map_err(|_| Error::Storage(parent.to_owned()))?;
        for checked in [
            path.clone(),
            path.with_file_name("accounts.sqlite3-wal"),
            path.with_file_name("accounts.sqlite3-shm"),
        ] {
            safe_regular_path(&checked).await?;
        }
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let result = (|| {
                let mut connection = Connection::open(&path)?;
                connection.busy_timeout(Duration::from_secs(5))?;
                connection.pragma_update(None, "journal_mode", "WAL")?;
                connection.pragma_update(None, "synchronous", "FULL")?;
                migrate_accounts(&mut connection)?;
                operation(&mut connection)
            })();
            result.map_err(|source| Error::Database { path, source })
        })
        .await
        .map_err(|_| Error::Worker)?
    }
}

fn migrate_accounts(connection: &mut Connection) -> rusqlite::Result<()> {
    let schema: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema == 2 {
        return Ok(());
    }
    // BEGIN IMMEDIATE serializes all coordinators/processes, including public
    // account-list calls that do not own the refresh-rotation file lease.
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let schema: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema > 2 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, client_id TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'microsoft');")?;
    let has_kind = {
        let mut statement = transaction.prepare("PRAGMA table_info(accounts)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        columns
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|column| column == "kind")
    };
    if !has_kind {
        transaction.execute(
            "ALTER TABLE accounts ADD COLUMN kind TEXT NOT NULL DEFAULT 'microsoft'",
            [],
        )?;
    }
    transaction.pragma_update(None, "user_version", 2)?;
    transaction.commit()
}

fn validate_client_id(value: &str) -> Result<String, Error> {
    let id = Uuid::parse_str(value).map_err(|_| Error::ClientId)?;
    if id.is_nil() {
        return Err(Error::ClientId);
    }
    Ok(id.hyphenated().to_string())
}

fn validate_account(id: Uuid, name: &str, kind: AccountKind, client_id: &str) -> Result<(), Error> {
    if kind == AccountKind::Microsoft {
        validate_client_id(client_id)?;
    } else if !client_id.is_empty() {
        return Err(Error::InvalidResponse("offline account metadata"));
    }
    validate_profile(id, name)
}

fn validate_profile(id: Uuid, name: &str) -> Result<(), Error> {
    if id.is_nil() || !valid_player_name(name) {
        return Err(Error::InvalidResponse("Minecraft profile"));
    }
    Ok(())
}

fn valid_player_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 16
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_offline_name(name: &str) -> Result<String, Error> {
    if !valid_player_name(name) {
        return Err(Error::OfflineName);
    }
    Ok(name.to_owned())
}

/// Standard Java `UUID.nameUUIDFromBytes("OfflinePlayer:<name>")` construction.
fn offline_uuid(name: &str) -> Uuid {
    let mut digest = Md5::digest(format!("OfflinePlayer:{name}").as_bytes());
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    Uuid::from_bytes(digest.into())
}

async fn safe_regular_path(path: &Path) -> Result<(), Error> {
    crate::instances::check_no_links(path)
        .await
        .map_err(|_| Error::Storage(path.to_owned()))?;
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::Storage(path.to_owned())),
    }
}

fn verification_uri_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.query().is_none()
        && matches!(
            url.host_str(),
            Some(
                "microsoft.com"
                    | "www.microsoft.com"
                    | "login.microsoftonline.com"
                    | "login.live.com"
            )
        )
}

fn valid_secret(secret: &str) -> bool {
    !secret.is_empty() && secret.len() <= 128 * 1024 && !secret.chars().any(char::is_control)
}

fn decode<T: DeserializeOwned>(response: &ApiResponse, stage: &'static str) -> Result<T, Error> {
    serde_json::from_slice(response.body()).map_err(|_| Error::InvalidResponse(stage))
}

fn decode_success<T: DeserializeOwned>(
    response: &ApiResponse,
    stage: &'static str,
) -> Result<T, Error> {
    if !(200..300).contains(&response.status) {
        return Err(rejected(response, stage));
    }
    decode(response, stage)
}

fn rejected(response: &ApiResponse, stage: &'static str) -> Error {
    #[derive(Deserialize)]
    struct XboxError {
        #[serde(rename = "XErr")]
        code: Option<u64>,
    }
    let code = serde_json::from_slice::<XboxError>(response.body())
        .ok()
        .and_then(|body| body.code);
    Error::Rejected {
        stage,
        status: response.status,
        code,
    }
}

fn parse_oauth_token(response: &ApiResponse) -> Result<OAuthToken, Error> {
    if !(200..300).contains(&response.status) {
        let failure: OAuthFailure = decode(response, "Microsoft token authorization")?;
        if matches!(
            failure.error.as_str(),
            "invalid_grant" | "interaction_required"
        ) {
            return Err(Error::ReauthenticationRequired);
        }
        return Err(rejected(response, "Microsoft token authorization"));
    }
    let token: OAuthToken = decode(response, "Microsoft token authorization")?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
        || token.expires_in == 0
        || !valid_secret(&token.access_token)
        || token
            .refresh_token
            .as_ref()
            .is_some_and(|token| !valid_secret(token))
    {
        return Err(Error::InvalidResponse("Microsoft token authorization"));
    }
    Ok(token)
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: Url,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}
const fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct OAuthFailure {
    error: String,
}

#[derive(Deserialize)]
struct OAuthToken {
    token_type: String,
    expires_in: u64,
    access_token: String,
    refresh_token: Option<String>,
}

impl Drop for OAuthToken {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(refresh) = self.refresh_token.as_mut() {
            refresh.zeroize();
        }
    }
}

#[derive(Deserialize)]
struct XboxToken {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    claims: XboxClaims,
    #[serde(rename = "NotAfter")]
    not_after: chrono::DateTime<chrono::Utc>,
}

impl XboxToken {
    fn claim(&self) -> Result<&XboxClaim, Error> {
        if !valid_secret(&self.token)
            || self.claims.xui.len() != 1
            || self.not_after <= chrono::Utc::now()
        {
            return Err(Error::InvalidResponse("Xbox authentication"));
        }
        let claim = &self.claims.xui[0];
        if claim.uhs.is_empty()
            || !claim.uhs.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || claim
                .xid
                .as_ref()
                .is_some_and(|id| id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(Error::InvalidResponse("Xbox authentication claims"));
        }
        Ok(claim)
    }
}

impl Drop for XboxToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}
#[derive(Deserialize)]
struct XboxClaims {
    xui: Vec<XboxClaim>,
}
#[derive(Deserialize)]
struct XboxClaim {
    uhs: String,
    xid: Option<String>,
}
#[derive(Deserialize)]
struct MinecraftToken {
    access_token: String,
    expires_in: u64,
}
#[derive(Deserialize)]
struct Entitlements {
    items: Vec<Entitlement>,
}
#[derive(Deserialize)]
struct Entitlement {
    name: String,
}
#[derive(Deserialize)]
struct Profile {
    id: Uuid,
    name: String,
}
struct Session {
    id: Uuid,
    name: String,
    access_token: Zeroizing<String>,
    xuid: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_net::policy::NetworkPolicy;
    use std::{
        collections::BTreeMap,
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    use wiremock::{
        Mock, MockServer, Request, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    // Deliberately synthetic identifiers: tests must never pose as a real
    // Microsoft application or account.
    const APP: &str = "11111111-1111-4111-8111-111111111111";
    const PROFILE: &str = "22222222222242228222222222222222";
    const REFRESH: &str = "fixture-private-refresh-token";
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn missing_system_credential_requests_sign_in() {
        assert!(matches!(
            super::credential_read_error(&keyring::Error::NoEntry),
            Error::ReauthenticationRequired
        ));
    }

    #[derive(Clone, Default)]
    struct MemoryStore {
        tokens: Arc<StdMutex<BTreeMap<String, Zeroizing<String>>>>,
        fail_write: Arc<AtomicBool>,
        fail_delete: Arc<AtomicBool>,
        break_database: Arc<StdMutex<Option<PathBuf>>>,
    }

    impl CredentialStore for MemoryStore {
        fn read(
            &self,
            key: String,
        ) -> impl Future<Output = Result<Zeroizing<String>, Error>> + Send {
            std::future::ready(
                self.tokens
                    .lock()
                    .map_err(|_| Error::Worker)
                    .and_then(|tokens| {
                        tokens
                            .get(&key)
                            .cloned()
                            .ok_or(Error::CredentialStore("read"))
                    }),
            )
        }
        fn write(
            &self,
            key: String,
            token: Zeroizing<String>,
        ) -> impl Future<Output = Result<(), Error>> + Send {
            if self.fail_write.load(Ordering::SeqCst) {
                return std::future::ready(Err(Error::CredentialStore("write")));
            }
            std::future::ready(
                self.tokens
                    .lock()
                    .map_err(|_| Error::Worker)
                    .map(|mut tokens| {
                        tokens.insert(key, token);
                        if let Ok(path) = self.break_database.lock()
                            && let Some(path) = path.as_ref()
                        {
                            let _ = std::fs::remove_file(path);
                            let _ = std::fs::create_dir(path);
                        }
                    }),
            )
        }
        fn delete(&self, key: String) -> impl Future<Output = Result<(), Error>> + Send {
            if self.fail_delete.load(Ordering::SeqCst) {
                return std::future::ready(Err(Error::CredentialStore("delete")));
            }
            std::future::ready(
                self.tokens
                    .lock()
                    .map_err(|_| Error::Worker)
                    .map(|mut tokens| {
                        tokens.remove(&key);
                    }),
            )
        }
    }

    fn service(
        root: &Path,
        server: &MockServer,
        store: MemoryStore,
    ) -> Result<AuthService<MemoryStore>, Box<dyn std::error::Error>> {
        let url = |name| Url::parse(&format!("{}/{name}", server.uri()));
        Ok(AuthService::with_parts(
            root,
            ApiClient::with_policy("Nudge-test", NetworkPolicy::LoopbackOnly, 8192)?,
            Endpoints {
                device_code: url("device")?,
                token: url("token")?,
                xbox: url("xbox")?,
                xsts: url("xsts")?,
                minecraft: url("minecraft")?,
                entitlements: url("entitlements")?,
                profile: url("profile")?,
            },
            store,
        )?)
    }

    async fn device(server: &MockServer) {
        Mock::given(method("POST")).and(path("/device")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code":"fixture-device-secret", "user_code":"ABCD-EFGH", "verification_uri":"https://www.microsoft.com/link", "expires_in": 900, "interval":1
        }))).mount(server).await;
    }

    async fn tokens(server: &MockServer, pending: bool) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        let response_calls = calls.clone();
        Mock::given(method("POST")).and(path("/token")).respond_with(move |_: &Request| {
            let attempt = response_calls.fetch_add(1, Ordering::SeqCst);
            if pending && attempt == 0 { return ResponseTemplate::new(400).set_body_json(serde_json::json!({"error":"authorization_pending"})); }
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"token_type":"Bearer", "expires_in":3600, "access_token":"fixture-microsoft-token", "refresh_token":REFRESH}))
        }).mount(server).await;
        calls
    }

    async fn chain(server: &MockServer, entitlement: bool, profile_id: &str) {
        Mock::given(method("POST")).and(path("/xbox")).and(header("x-xbl-contract-version", "1"))
            .and(body_json(serde_json::json!({"Properties":{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":"d=fixture-microsoft-token"},"RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Token":"fixture-xbox-token","NotAfter":"2099-01-01T00:00:00Z","DisplayClaims":{"xui":[{"uhs":"123456"}]}}))).mount(server).await;
        Mock::given(method("POST")).and(path("/xsts"))
            .and(body_json(serde_json::json!({"Properties":{"SandboxId":"RETAIL","UserTokens":["fixture-xbox-token"]},"RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Token":"fixture-xsts-token","NotAfter":"2099-01-01T00:00:00Z","DisplayClaims":{"xui":[{"uhs":"123456","xid":"1234"}]}}))).mount(server).await;
        Mock::given(method("POST")).and(path("/minecraft"))
            .and(body_json(serde_json::json!({"xtoken":"XBL3.0 x=123456;fixture-xsts-token","platform":"PC_LAUNCHER"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"fixture-minecraft-token","expires_in":86400}))).mount(server).await;
        let items = if entitlement {
            serde_json::json!([{"name":"game_minecraft"},{"name":"product_minecraft"}])
        } else {
            serde_json::json!([])
        };
        Mock::given(method("GET"))
            .and(path("/entitlements"))
            .and(header("authorization", "Bearer fixture-minecraft-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": items})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/profile"))
            .and(header("authorization", "Bearer fixture-minecraft-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id":profile_id,"name":"NudgePlayer"})),
            )
            .mount(server)
            .await;
    }

    async fn pending_login(auth: &AuthService<MemoryStore>) -> Result<DeviceAuthorization, Error> {
        let mut authorization = auth.begin_login(APP, &CancellationToken::new()).await?;
        // Exercise real HTTP polling quickly; the production response interval
        // remains strictly checked and is never overridden outside this module.
        authorization.interval = Duration::from_millis(10);
        Ok(authorization)
    }

    async fn seed(auth: &AuthService<MemoryStore>) -> Result<Account, Error> {
        let account = Account {
            id: Uuid::parse_str(PROFILE).map_err(|_| Error::Worker)?,
            name: "NudgePlayer".into(),
            kind: AccountKind::Microsoft,
            client_id: APP.into(),
        };
        auth.save_account(&account).await?;
        auth.store
            .write(
                auth.credential_key(&account),
                Zeroizing::new("fixture-old-refresh".into()),
            )
            .await?;
        Ok(account)
    }

    #[tokio::test]
    async fn offline_accounts_use_standard_uuids_and_never_official_credentials() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let store = MemoryStore::default();
        let auth = service(root.path(), &server, store.clone())?;
        store.fail_delete.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let account = auth.add_offline("Notch", &cancel).await?;
        assert_eq!(
            account.id.to_string(),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        assert_eq!(account.kind, AccountKind::Offline);
        assert_eq!(account.client_id, "");
        assert_eq!(auth.accounts().await?, vec![account.clone()]);
        // Repeating the same casing selects the existing deterministic identity.
        assert_eq!(auth.add_offline("Notch", &cancel).await?, account);
        assert!(matches!(
            auth.add_offline("notch", &cancel).await,
            Err(Error::OfflineConflict)
        ));
        assert!(matches!(
            auth.add_offline("bad name", &cancel).await,
            Err(Error::OfflineName)
        ));
        let identity = auth.identity(account.id, &cancel).await?;
        assert!(!format!("{identity:?}").contains("OfflinePlayer"));
        // Offline logout must not depend on the OS credential store.
        auth.logout(account.id).await?;
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        assert!(
            server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn damaged_account_row_does_not_hide_or_block_healthy_accounts() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        let cancel = CancellationToken::new();
        let healthy = auth.add_offline("Healthy", &cancel).await?;
        let damaged = auth.add_offline("Damaged", &cancel).await?;
        let damaged_id = damaged.id.to_string();
        auth.database(move |connection| {
            connection
                .execute(
                    "UPDATE accounts SET kind = 'unknown' WHERE id = ?1",
                    [damaged_id],
                )
                .map(|_| ())
        })
        .await?;

        assert_eq!(auth.accounts().await?, vec![healthy.clone()]);
        assert!(matches!(
            auth.logout(damaged.id).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            auth.add_offline("Damaged", &cancel).await,
            Err(Error::InvalidResponse(_))
        ));
        auth.logout(healthy.id).await?;
        assert_eq!(auth.add_offline("Another", &cancel).await?.name, "Another");
        assert_eq!(auth.accounts().await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn unreadable_account_columns_cannot_be_mistaken_for_missing_accounts() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        let cancel = CancellationToken::new();
        let healthy = auth.add_offline("Healthy", &cancel).await?;
        let damaged = auth.add_offline("Damaged", &cancel).await?;
        let damaged_id = damaged.id.to_string();
        auth.database(move |connection| {
            connection
                .execute(
                    "UPDATE accounts SET kind = x'ff' WHERE id = ?1",
                    [damaged_id],
                )
                .map(|_| ())
        })
        .await?;

        assert_eq!(auth.accounts().await?, vec![healthy.clone()]);
        assert!(matches!(
            auth.account(damaged.id).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            auth.add_offline("Damaged", &cancel).await,
            Err(Error::InvalidResponse(_))
        ));
        auth.logout(healthy.id).await?;

        let unreadable = auth.add_offline("Unreadable", &cancel).await?;
        let unreadable_id = unreadable.id.to_string();
        auth.database(move |connection| {
            connection
                .execute(
                    "UPDATE accounts SET id = x'ff' WHERE id = ?1",
                    [unreadable_id],
                )
                .map(|_| ())
        })
        .await?;
        assert!(matches!(
            auth.account(unreadable.id).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            auth.add_offline("Another", &cancel).await,
            Err(Error::InvalidResponse(_))
        ));
        Ok(())
    }

    #[test]
    fn concurrent_version_one_migrations_preserve_accounts() -> TestResult {
        const WORKERS: usize = 12;
        let root = tempfile::tempdir()?;
        let path = root.path().join("accounts.sqlite3");
        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE accounts (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, client_id TEXT NOT NULL); PRAGMA user_version=1;")?;
        connection.execute(
            "INSERT INTO accounts (id, name, client_id) VALUES (?1, 'NudgePlayer', ?2)",
            params![PROFILE, APP],
        )?;
        drop(connection);
        let connections = (0..WORKERS)
            .map(|_| {
                let connection = Connection::open(&path)?;
                connection.busy_timeout(Duration::from_secs(5))?;
                Ok(connection)
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let barrier = Arc::new(std::sync::Barrier::new(WORKERS));
        let workers: Vec<_> = connections
            .into_iter()
            .map(|mut connection| {
                let barrier = barrier.clone();
                std::thread::spawn(move || -> rusqlite::Result<(String, i64)> {
                    barrier.wait();
                    migrate_accounts(&mut connection)?;
                    let kind = connection.query_row(
                        "SELECT kind FROM accounts WHERE id = ?1",
                        [PROFILE],
                        |row| row.get(0),
                    )?;
                    let schema =
                        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
                    Ok((kind, schema))
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(
                worker.join().map_err(|_| "migration worker panicked")??,
                ("microsoft".to_owned(), 2)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn version_one_account_metadata_is_migrated_without_losing_accounts() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        let path = root.path().join("accounts.sqlite3");
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(&format!(
                "CREATE TABLE accounts (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, client_id TEXT NOT NULL);
                 PRAGMA user_version=1;
                 INSERT INTO accounts (id, name, client_id) VALUES ('{PROFILE}', 'NudgePlayer', '{APP}');"
            ))?;
            connection.pragma_update(None, "user_version", 1)?;
        }
        let accounts = auth.accounts().await?;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, Uuid::parse_str(PROFILE)?);
        assert_eq!(accounts[0].kind, AccountKind::Microsoft);
        let connection = Connection::open(&path)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 2);
        let mut statement = connection.prepare("PRAGMA table_info(accounts)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(columns.iter().any(|name| name == "kind"));
        Ok(())
    }

    #[tokio::test]
    async fn device_login_refresh_and_logout_use_only_secure_credentials() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        let calls = tokens(&server, true).await;
        chain(&server, true, PROFILE).await;
        let authorization = pending_login(&auth).await?;
        assert!(!format!("{authorization:?}").contains("fixture-device-secret"));
        let account = auth
            .complete_login(authorization, &CancellationToken::new())
            .await?;
        assert_eq!(account.name, "NudgePlayer");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(auth.accounts().await?, vec![account.clone()]);
        let identity = auth.identity(account.id, &CancellationToken::new()).await?;
        assert!(!format!("{identity:?}").contains("fixture-minecraft-token"));
        let requests = server
            .received_requests()
            .await
            .ok_or("missing request recorder")?;
        let forms: Vec<BTreeMap<String, String>> = requests
            .iter()
            .filter(|request| request.url.path() == "/token")
            .map(|request| {
                url::form_urlencoded::parse(&request.body)
                    .into_owned()
                    .collect()
            })
            .collect();
        assert_eq!(forms[0]["device_code"], "fixture-device-secret");
        assert_eq!(forms[2]["grant_type"], "refresh_token");
        assert_eq!(forms[2]["refresh_token"], REFRESH);
        for file in std::fs::read_dir(root.path())? {
            let bytes = std::fs::read(file?.path())?;
            assert!(
                !bytes
                    .windows(REFRESH.len())
                    .any(|window| window == REFRESH.as_bytes())
            );
            assert!(
                !bytes
                    .windows(23)
                    .any(|window| window == b"fixture-minecraft-token")
            );
        }
        auth.logout(account.id).await?;
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        assert!(auth.store.tokens.lock().map_err(|_| "poisoned")?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn missing_java_entitlement_never_creates_an_account_or_reads_profile() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        tokens(&server, false).await;
        chain(&server, false, PROFILE).await;
        assert!(matches!(
            auth.complete_login(pending_login(&auth).await?, &CancellationToken::new())
                .await,
            Err(Error::NoEntitlement)
        ));
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        assert!(auth.store.tokens.lock().map_err(|_| "poisoned")?.is_empty());
        assert!(
            !server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .iter()
                .any(|request| request.url.path() == "/profile")
        );
        Ok(())
    }

    #[tokio::test]
    async fn rotation_is_saved_even_when_later_profile_does_not_match() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        let account = seed(&auth).await?;
        tokens(&server, false).await;
        chain(&server, true, "d5e9d4cd68aa473fa7cf4495945a9cd6").await;
        assert!(matches!(
            auth.identity(account.id, &CancellationToken::new()).await,
            Err(Error::ProfileMismatch)
        ));
        assert_eq!(
            auth.store
                .read(auth.credential_key(&account))
                .await?
                .as_str(),
            REFRESH
        );
        assert_eq!(auth.accounts().await?, vec![account]);
        Ok(())
    }

    #[tokio::test]
    async fn revoked_refresh_requires_interactive_login_without_fallback() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        let account = seed(&auth).await?;
        Mock::given(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"error":"invalid_grant", "error_description":REFRESH}),
            ))
            .mount(&server)
            .await;
        let error = auth
            .identity(account.id, &CancellationToken::new())
            .await
            .err()
            .ok_or("expected rejected refresh")?;
        assert!(matches!(error, Error::ReauthenticationRequired));
        assert!(!format!("{error:?} {error}").contains(REFRESH));
        assert_eq!(
            server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_and_expiry_do_not_issue_token_requests() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        let authorization = pending_login(&auth).await?;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            auth.complete_login(authorization, &cancelled).await,
            Err(Error::Cancelled)
        ));
        let mut authorization = pending_login(&auth).await?;
        authorization.deadline = tokio::time::Instant::now();
        assert!(matches!(
            auth.complete_login(authorization, &CancellationToken::new())
                .await,
            Err(Error::Expired)
        ));
        assert!(
            !server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .iter()
                .any(|request| request.url.path() == "/token")
        );
        Ok(())
    }

    #[tokio::test]
    async fn timed_out_device_poll_reduces_frequency_before_retrying() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let mut auth = service(root.path(), &server, MemoryStore::default())?;
        auth.http = ApiClient::with_policy_and_timeout(
            "Nudge-test",
            NetworkPolicy::LoopbackOnly,
            8192,
            Duration::from_millis(250),
        )?;
        device(&server).await;
        chain(&server, true, PROFILE).await;
        let calls = Arc::new(StdMutex::new(Vec::<std::time::Instant>::new()));
        let recorded = calls.clone();
        Mock::given(path("/token")).respond_with(move |_: &Request| {
            let first = if let Ok(mut times) = recorded.lock() { times.push(std::time::Instant::now()); times.len() == 1 } else { false };
            let response = ResponseTemplate::new(200).set_body_json(serde_json::json!({"token_type":"Bearer", "expires_in":3600, "access_token":"fixture-microsoft-token", "refresh_token":REFRESH}));
            if first { response.set_delay(Duration::from_millis(750)) } else { response }
        }).mount(&server).await;
        let authorization = pending_login(&auth).await?;
        auth.complete_login(authorization, &CancellationToken::new())
            .await?;
        let times = calls.lock().map_err(|_| "poisoned recorder")?;
        assert_eq!(times.len(), 2);
        // 250 ms timeout plus doubled 10 ms interval, allowing only timer
        // granularity, never an immediate or original-frequency retry.
        assert!(times[1].duration_since(times[0]) >= Duration::from_millis(268));
        Ok(())
    }

    #[tokio::test]
    async fn device_poll_recovers_from_transient_non_json_server_error() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        chain(&server, true, PROFILE).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let response_calls = calls.clone();
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(move |_: &Request| {
                if response_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(502).set_body_string("<html>temporary outage</html>")
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "token_type":"Bearer", "expires_in":3600,
                        "access_token":"fixture-microsoft-token", "refresh_token":REFRESH
                    }))
                }
            })
            .mount(&server)
            .await;
        auth.complete_login(pending_login(&auth).await?, &CancellationToken::new())
            .await?;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn xbox_rejections_and_inconsistent_claims_never_reach_minecraft() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        tokens(&server, false).await;
        Mock::given(path("/xbox")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Token":"fixture-xbox-token","NotAfter":"2099-01-01T00:00:00Z","DisplayClaims":{"xui":[{"uhs":"123456"}]}}))).mount(&server).await;
        let rejected = Mock::given(path("/xsts"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"XErr":2_148_916_233_u64,"Message":REFRESH})),
            )
            .mount_as_scoped(&server)
            .await;
        let error = auth
            .complete_login(pending_login(&auth).await?, &CancellationToken::new())
            .await
            .err()
            .ok_or("expected Xbox rejection")?;
        assert!(matches!(
            error,
            Error::Rejected {
                code: Some(2_148_916_233),
                ..
            }
        ));
        assert!(!format!("{error:?} {error}").contains(REFRESH));
        drop(rejected);
        Mock::given(path("/xsts")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Token":"fixture-xsts-token","NotAfter":"2099-01-01T00:00:00Z","DisplayClaims":{"xui":[{"uhs":"different"}]}}))).mount(&server).await;
        assert!(matches!(
            auth.complete_login(pending_login(&auth).await?, &CancellationToken::new())
                .await,
            Err(Error::InvalidResponse("Xbox user-hash consistency"))
        ));
        assert!(
            !server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .iter()
                .any(|request| request.url.path() == "/minecraft")
        );
        Ok(())
    }

    #[tokio::test]
    async fn declined_and_malformed_authorizations_never_become_accounts() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        let decline = Mock::given(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error":"authorization_declined"})),
            )
            .mount_as_scoped(&server)
            .await;
        assert!(matches!(
            auth.complete_login(pending_login(&auth).await?, &CancellationToken::new())
                .await,
            Err(Error::Declined)
        ));
        drop(decline);
        Mock::given(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token":REFRESH})),
            )
            .mount(&server)
            .await;
        let error = auth
            .complete_login(pending_login(&auth).await?, &CancellationToken::new())
            .await
            .err()
            .ok_or("expected malformed response")?;
        assert!(matches!(
            error,
            Error::InvalidResponse("Microsoft token authorization")
        ));
        assert!(!format!("{error:?} {error}").contains(REFRESH));
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        Ok(())
    }

    #[tokio::test]
    async fn slow_down_adds_five_seconds_and_can_be_cancelled_during_wait() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        device(&server).await;
        Mock::given(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(serde_json::json!({"error":"slow_down"})),
            )
            .mount(&server)
            .await;
        let authorization = pending_login(&auth).await?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task =
            tokio::spawn(async move { auth.complete_login(authorization, &task_cancel).await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel.cancel();
        assert!(matches!(task.await?, Err(Error::Cancelled)));
        assert_eq!(
            server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .iter()
                .filter(|request| request.url.path() == "/token")
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn device_authorization_rejects_unregistered_ids_and_untrusted_browser_urls() -> TestResult
    {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        for id in ["", "not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
            assert!(matches!(
                auth.begin_login(id, &CancellationToken::new()).await,
                Err(Error::ClientId)
            ));
        }
        assert!(
            server
                .received_requests()
                .await
                .ok_or("missing recorder")?
                .is_empty()
        );
        Mock::given(path("/device")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"device_code":"secret", "user_code":"CODE", "verification_uri":"https://attacker.example/link", "expires_in":900,"interval":1}))).mount(&server).await;
        assert!(matches!(
            auth.begin_login(APP, &CancellationToken::new()).await,
            Err(Error::InvalidResponse(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn keyring_write_and_delete_failures_do_not_claim_success() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let store = MemoryStore::default();
        let auth = service(root.path(), &server, store.clone())?;
        device(&server).await;
        tokens(&server, false).await;
        chain(&server, true, PROFILE).await;
        store.fail_write.store(true, Ordering::SeqCst);
        assert!(matches!(
            auth.complete_login(pending_login(&auth).await?, &CancellationToken::new())
                .await,
            Err(Error::CredentialStore("write"))
        ));
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        store.fail_write.store(false, Ordering::SeqCst);
        let account = seed(&auth).await?;
        store.fail_delete.store(true, Ordering::SeqCst);
        assert!(matches!(
            auth.logout(account.id).await,
            Err(Error::CredentialStore("delete"))
        ));
        assert_eq!(auth.accounts().await?, vec![account.clone()]);
        store.fail_delete.store(false, Ordering::SeqCst);
        auth.logout(account.id).await?;
        assert_eq!(auth.accounts().await?, Vec::<Account>::new());
        Ok(())
    }

    #[tokio::test]
    async fn account_metadata_failure_removes_the_new_refresh_token() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let store = MemoryStore::default();
        let auth = service(root.path(), &server, store.clone())?;
        device(&server).await;
        tokens(&server, false).await;
        chain(&server, true, PROFILE).await;
        auth.accounts().await?;
        *store
            .break_database
            .lock()
            .map_err(|_| "poisoned account database fixture")? =
            Some(root.path().join("accounts.sqlite3"));
        let error = auth
            .complete_login(pending_login(&auth).await?, &CancellationToken::new())
            .await
            .expect_err("account metadata persistence must fail");
        assert!(matches!(error, Error::Storage(_)), "{error}");
        assert!(store.tokens.lock().map_err(|_| "poisoned")?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn independent_coordinators_cannot_race_rotation_and_roots_do_not_share_tokens()
    -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let store = MemoryStore::default();
        let first = service(root.path(), &server, store.clone())?;
        let second = service(root.path(), &server, store.clone())?;
        let third = service(other.path(), &server, store)?;
        let account = seed(&first).await?;
        let lease = first.lock(&CancellationToken::new()).await?;
        let cancelled = CancellationToken::new();
        let waiting = second.lock(&cancelled);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut waiting)
                .await
                .is_err()
        );
        cancelled.cancel();
        assert!(matches!(waiting.await, Err(Error::Cancelled)));
        assert!(
            third
                .store
                .read(third.credential_key(&account))
                .await
                .is_err()
        );
        drop(lease);
        second.lock(&CancellationToken::new()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn independent_coordinator_waits_until_account_lock_is_released() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let first = service(root.path(), &server, MemoryStore::default())?;
        let second = service(root.path(), &server, MemoryStore::default())?;
        let lease = first.lock(&CancellationToken::new()).await?;
        let cancel = CancellationToken::new();
        let waiting = second.lock(&cancel);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut waiting)
                .await
                .is_err()
        );
        drop(lease);
        tokio::time::timeout(Duration::from_secs(2), waiting).await??;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn account_database_and_lock_symlinks_are_rejected() -> TestResult {
        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let sentinel = root.path().join("sentinel");
        std::fs::write(&sentinel, "untouched")?;
        let auth = service(root.path(), &server, MemoryStore::default())?;
        std::os::unix::fs::symlink(&sentinel, root.path().join("accounts.sqlite3"))?;
        assert!(matches!(auth.accounts().await, Err(Error::Storage(_))));
        std::os::unix::fs::symlink(&sentinel, root.path().join("accounts.lock"))?;
        assert!(matches!(
            auth.lock(&CancellationToken::new()).await,
            Err(Error::Storage(_))
        ));
        assert_eq!(std::fs::read_to_string(sentinel)?, "untouched");
        Ok(())
    }
}
