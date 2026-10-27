//! Explicit retirement of destination reservations under the engine lock.

use super::{DownloadJob, DownloadManager, Error, destination_key, invalid, same_expected_content};
use rusqlite::{OptionalExtension as _, params};

impl DownloadManager {
    /// Releases exactly the supplied destinations and their queued task records.
    ///
    /// The caller must hold its instance lock and supply the previous install
    /// plan, including trusted hashes. Every existing reservation and matching
    /// task must still describe that content; a stale plan fails atomically.
    /// Never pass shared assets/libraries still referenced by other instances.
    ///
    /// This only retires metadata. Installed files, verified cache and partials
    /// are retained. Deletion/reinstallation coordinators must journal their
    /// intent before calling this method and keep instance locks until commit.
    /// Retrying an already completed release is safe.
    ///
    /// # Errors
    /// Rejects active engines, unsafe/duplicate destinations, stale integrity
    /// information, malformed records, or database transaction failures.
    pub fn release_destinations(&self, previous: &[DownloadJob]) -> Result<usize, Error> {
        let _lock = self.lock_run()?;
        self.validate_jobs(previous)?;
        let expected: std::collections::BTreeMap<_, _> = previous
            .iter()
            .map(|job| (destination_key(&job.destination), &job.spec))
            .collect();
        let mut database = self.database()?;
        let transaction = database.transaction()?;
        for (path, spec) in &expected {
            let reserved: Option<String> = transaction
                .query_row(
                    "SELECT spec FROM destinations WHERE path=?1",
                    [path],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(reserved) = reserved {
                let reserved = serde_json::from_str(&reserved)
                    .map_err(|_| invalid("persisted destination integrity record is malformed"))?;
                if !same_expected_content(&reserved, spec) {
                    return Err(invalid(
                        "release plan no longer matches destination content",
                    ));
                }
            }
        }
        let retired = {
            let mut statement = transaction.prepare("SELECT id,job FROM downloads")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut retired = Vec::new();
            for row in rows {
                let (id, json) = row?;
                let job: DownloadJob = serde_json::from_str(&json)
                    .map_err(|_| invalid("persistent download job is malformed"))?;
                if let Some(spec) = expected.get(&destination_key(&job.destination)) {
                    if !same_expected_content(&job.spec, spec) {
                        return Err(invalid("release plan no longer matches queued content"));
                    }
                    retired.push(id);
                }
            }
            retired
        };
        for id in retired {
            transaction.execute("DELETE FROM downloads WHERE id=?1", [id])?;
        }
        let mut released = 0;
        for path in expected.keys() {
            released +=
                transaction.execute("DELETE FROM destinations WHERE path=?1", params![path])?;
        }
        transaction.commit()?;
        Ok(released)
    }
}
