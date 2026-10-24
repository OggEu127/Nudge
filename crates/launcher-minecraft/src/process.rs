//! Process spawning shared by Java inspection and game supervision.

use std::{io, time::Duration};

use tokio::process::{Child, Command};

const BUSY_RETRIES: u32 = 4;
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Spawns a command, retrying a transient executable-busy condition.
///
/// On Unix, a concurrent fork/exec can briefly keep a just-written executable
/// open for writing in another process, producing `ETXTBSY` even though the
/// current process has closed the file. The bounded backoff covers that window
/// without retrying permanent permission, path or format failures.
pub(crate) fn spawn_with_busy_retry(command: &mut Command) -> io::Result<Child> {
    let mut attempts = 0;
    let mut delay = INITIAL_RETRY_DELAY;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy && attempts < BUSY_RETRIES =>
            {
                attempts += 1;
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_permanent_spawn_errors_without_unbounded_retries() {
        let mut command = Command::new("/definitely/not/a/java/runtime");
        let error = spawn_with_busy_retry(&mut command).expect_err("missing executable must fail");
        assert_ne!(error.kind(), io::ErrorKind::ExecutableFileBusy);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retries_until_a_transient_writer_closes_the_executable() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("fixture java");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("write script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable)
            .expect("hold a writer");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let mut command = Command::new(&executable);
        let mut child = spawn_with_busy_retry(&mut command)
            .expect("executable becomes available after the writer closes");
        release.join().expect("writer thread");
        let status = child.wait().await.expect("fixture exit");
        assert!(status.success());
    }
}
