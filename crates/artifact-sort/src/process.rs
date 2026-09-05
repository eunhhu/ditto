use std::time::Duration;

use ditto_model::CancellationToken;

use crate::SortError;

pub(super) async fn run(
    input: &[u8],
    unique: bool,
    cancellation: CancellationToken,
    remaining: Duration,
) -> Result<Vec<u8>, SortError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut command = tokio::process::Command::new("/usr/bin/sort");
        if unique {
            command.arg("-u");
        }
        unix::run_command(
            command,
            input,
            cancellation,
            remaining.min(Duration::from_secs(5)),
        )
        .await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (input, unique, cancellation, remaining);
        Err(SortError::Unavailable)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::*;
    use std::process::Stdio;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        process::{Child, Command},
    };

    struct OwnedChild {
        child: Child,
        group: Option<i32>,
        _scratch: tempfile::TempDir,
    }
    impl OwnedChild {
        fn kill_group(&self) {
            if let Some(pid) = self.group {
                // SAFETY: the child was placed in a fresh group equal to its PID.
                // Group ownership is cleared synchronously when wait reaps it.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
    }
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            self.kill_group();
        }
    }

    pub(super) async fn run_command(
        mut command: Command,
        input: &[u8],
        cancellation: CancellationToken,
        timeout: Duration,
    ) -> Result<Vec<u8>, SortError> {
        if cancellation.is_cancelled() {
            return Err(SortError::Cancelled);
        }
        if timeout.is_zero() {
            return Err(SortError::Deadline);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let scratch = tempfile::Builder::new()
            .prefix("ditto-sort-")
            .tempdir()
            .map_err(|_| SortError::Unavailable)?;
        command
            .env_clear()
            .env("LC_ALL", "C")
            .env("TMPDIR", scratch.path())
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        // SAFETY: only async-signal-safe setrlimit calls run between fork and exec;
        // no captured locks, allocation, logging, filesystem or environment access.
        unsafe {
            command.pre_exec(|| {
                for (resource, value) in [
                    (libc::RLIMIT_CORE, 0),
                    (libc::RLIMIT_CPU, 2),
                    (libc::RLIMIT_FSIZE, crate::MAX_OUTPUT_BYTES as libc::rlim_t),
                    (libc::RLIMIT_NOFILE, 32),
                ] {
                    let bound = libc::rlimit {
                        rlim_cur: value,
                        rlim_max: value,
                    };
                    if libc::setrlimit(resource, &bound) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        if cancellation.is_cancelled() {
            return Err(SortError::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(SortError::Deadline);
        }
        let child = command.spawn().map_err(|_| SortError::Unavailable)?;
        let group = child.id().map(|pid| pid as i32);
        let mut owned = OwnedChild {
            child,
            group,
            _scratch: scratch,
        };
        let mut stdin = owned.child.stdin.take().ok_or(SortError::Process)?;
        let stdout = owned.child.stdout.take().ok_or(SortError::Process)?;
        let work = async {
            let write = async {
                stdin
                    .write_all(input)
                    .await
                    .map_err(|_| SortError::Process)?;
                stdin.shutdown().await.map_err(|_| SortError::Process)?;
                drop(stdin);
                Ok::<_, SortError>(())
            };
            let read = async {
                let mut bytes = Vec::new();
                stdout
                    .take((crate::MAX_OUTPUT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| SortError::Process)?;
                if bytes.len() > crate::MAX_OUTPUT_BYTES {
                    return Err(SortError::OutputLimit);
                }
                Ok(bytes)
            };
            let (_, bytes) = tokio::try_join!(write, read)?;
            let status = owned.child.wait().await.map_err(|_| SortError::Process)?;
            owned.group = None;
            if !status.success() {
                return Err(SortError::Process);
            }
            Ok(bytes)
        };
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SortError::Cancelled),
            () = tokio::time::sleep_until(deadline) => Err(SortError::Deadline),
            result = work => result,
        };
        if owned.group.is_some() {
            owned.kill_group();
            let reaped = owned.child.wait().await;
            owned.group = None;
            reaped.map_err(|_| SortError::Process)?;
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn pipe_overflow_and_nonzero_exit_are_failures() {
            let mut flood = Command::new("/bin/sh");
            flood.args([
                "-c",
                "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n'; done",
            ]);
            assert_eq!(
                run_command(flood, b"", CancellationToken::new(), Duration::from_secs(3))
                    .await
                    .unwrap_err(),
                SortError::OutputLimit
            );
            let mut fail = Command::new("/bin/sh");
            fail.args(["-c", "exit 7"]);
            assert_eq!(
                run_command(fail, b"", CancellationToken::new(), Duration::from_secs(3))
                    .await
                    .unwrap_err(),
                SortError::Process
            );
        }

        #[tokio::test]
        async fn environment_is_closed_and_only_fixed_values_reach_child() {
            let bytes = run_command(
                Command::new("/usr/bin/env"),
                b"",
                CancellationToken::new(),
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            let text = String::from_utf8(bytes).unwrap();
            let entries: BTreeSet<_> = text
                .lines()
                .map(|line| line.split_once('=').unwrap().0)
                .collect();
            assert_eq!(entries, BTreeSet::from(["LC_ALL", "TMPDIR"]));
            assert!(text.contains("LC_ALL=C\n"));
        }

        use std::collections::BTreeSet;

        #[tokio::test]
        async fn dropping_the_execution_future_kills_and_reaps_the_child() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let mut child = Command::new("/bin/sh");
            child
                .arg("-c")
                .arg("printf '%s' $$ > \"$1\"; exec /bin/sleep 30")
                .arg("fixture")
                .arg(&pid_file);
            let execution = tokio::spawn(run_command(
                child,
                b"",
                CancellationToken::new(),
                Duration::from_secs(5),
            ));
            let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(text) = std::fs::read_to_string(&pid_file)
                        && let Ok(pid) = text.parse()
                    {
                        break pid;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            execution.abort();
            assert!(execution.await.unwrap_err().is_cancelled());
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    // SAFETY: signal zero inspects the child PID without sending a signal.
                    if unsafe { libc::kill(pid, 0) } == -1 {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::ESRCH)
                        );
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn cancellation_and_deadline_reap_the_actual_child() {
            for cancel in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let pid_file = dir.path().join("pid");
                // Test-only fixture. Production has no shell/program injection path.
                let mut child = Command::new("/bin/sh");
                child
                    .arg("-c")
                    .arg("printf '%s' $$ > \"$1\"; exec /bin/sleep 30")
                    .arg("fixture")
                    .arg(&pid_file);
                let token = CancellationToken::new();
                let execution = tokio::spawn(run_command(
                    child,
                    b"",
                    token.clone(),
                    Duration::from_millis(if cancel { 2000 } else { 200 }),
                ));
                let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(text) = std::fs::read_to_string(&pid_file)
                            && let Ok(pid) = text.parse()
                        {
                            break pid;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                if cancel {
                    token.cancel();
                }
                assert_eq!(
                    execution.await.unwrap().unwrap_err(),
                    if cancel {
                        SortError::Cancelled
                    } else {
                        SortError::Deadline
                    }
                );
                // SAFETY: signal zero checks existence and sends no signal.
                assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            }
        }
    }
}
