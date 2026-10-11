//! Bound subprocess time and output: a stalled audio server must not hang D-Bus.
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

pub trait BoundedCommand {
    fn bounded_output(&mut self) -> io::Result<Output>;
}

impl BoundedCommand for Command {
    fn bounded_output(&mut self) -> io::Result<Output> {
        run(self, TIMEOUT, OUTPUT_LIMIT)
    }
}

fn capture(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.by_ref().take(limit as u64 + 1).read_to_end(&mut bytes);
        if bytes.len() > limit {
            // Drain concurrently so a chatty subprocess cannot deadlock on a full pipe.
            let _ = io::copy(&mut pipe, &mut io::sink());
            let _ = tx.send(Err(io::Error::other("subprocess output exceeds limit")));
        } else {
            let _ = tx.send(result.map(|_| bytes));
        }
    });
    rx
}

fn run(cmd: &mut Command, timeout: Duration, limit: usize) -> io::Result<Output> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let stdout = capture(child.stdout.take().expect("piped stdout"), limit);
    let stderr = capture(child.stderr.take().expect("piped stderr"), limit);
    let result = (|| {
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "audio command timed out",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        };
        let receive = |rx: mpsc::Receiver<io::Result<Vec<u8>>>| {
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "audio command pipe timed out")
                })?
        };
        Ok(Output {
            status,
            stdout: receive(stdout)?,
            stderr: receive(stderr)?,
        })
    })();
    if result.is_err() {
        // SAFETY: the child was placed in its own process group. Kill that group,
        // including helpers holding our pipes open, then reap the direct child.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_stdout_stderr_and_exit_status() {
        let out = run(
            Command::new("sh").args(["-c", "printf hello; printf error >&2; exit 7"]),
            TIMEOUT,
            100,
        )
        .unwrap();
        assert_eq!(out.stdout, b"hello");
        assert_eq!(out.stderr, b"error");
        assert_eq!(out.status.code(), Some(7));
    }
    #[test]
    fn terminates_stalled_command_and_its_pipe_holders() {
        let start = Instant::now();
        let err = run(
            Command::new("sh").args(["-c", "sleep 10 & wait"]),
            Duration::from_millis(100),
            100,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn rejects_excessive_output_without_deadlock() {
        let err = run(
            Command::new("head").args(["-c", "1000000", "/dev/zero"]),
            TIMEOUT,
            100,
        )
        .unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));
    }
}
