//! Opt-in native credential-vault acceptance; never runs in ordinary CI.

#![cfg(windows)]

use launcher_core::auth::{CredentialStore, SystemCredentials};
use zeroize::Zeroizing;

#[tokio::test]
#[ignore = "requires an interactive Windows user's credential vault; writes only a unique test entry"]
async fn windows_system_credentials_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let key = format!("acceptance-test-{}", uuid::Uuid::new_v4());
    let store = SystemCredentials;
    let result = async {
        store
            .write(key.clone(), Zeroizing::new("synthetic-test-value".into()))
            .await?;
        let value = store.read(key.clone()).await?;
        if value.as_str() != "synthetic-test-value" {
            return Err(launcher_core::auth::Error::InvalidResponse(
                "credential readback",
            ));
        }
        store
            .write(key.clone(), Zeroizing::new("rotated-test-value".into()))
            .await?;
        let rotated = store.read(key.clone()).await?;
        if rotated.as_str() != "rotated-test-value" {
            return Err(launcher_core::auth::Error::InvalidResponse(
                "rotated credential readback",
            ));
        }
        Ok::<_, launcher_core::auth::Error>(())
    }
    .await;
    // Attempt cleanup even if write/read failed. The random key cannot collide
    // with a user account, and no real refresh token is used by this test.
    let cleanup = store.delete(key.clone()).await;
    result?;
    cleanup?;
    assert!(store.read(key.clone()).await.is_err());
    store.delete(key).await?; // Deletion of an absent entry is idempotent.
    Ok(())
}
