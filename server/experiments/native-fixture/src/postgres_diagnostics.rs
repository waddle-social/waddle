//! Failure-only observations of the owned postmaster and direct children.
//! Never reads command lines, environment, SQL, private data or credential files.
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TaskState {
    Running,
    Sleeping,
    Uninterruptible,
    Stopped,
    Zombie,
    Dead,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum WaitChannel {
    Symbol(String),
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ObservationError {
    Io {
        pid: Option<u32>,
        errno: Option<i32>,
    },
    Oversized {
        pid: u32,
    },
    Invalid {
        pid: u32,
    },
}
impl fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for ObservationError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IoCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub reads: u64,
    pub writes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum IoObservation {
    Counters(IoCounters),
    Unreadable(ObservationError),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskObservation {
    pub pid: u32,
    pub parent: u32,
    pub start_ticks: u64,
    pub state: TaskState,
    pub wait: WaitChannel,
    pub io: IoObservation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PostgresSnapshot {
    pub tasks: Vec<TaskObservation>,
    pub truncated: bool,
    pub observation_error: Option<ObservationError>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    parent: u32,
    start_ticks: u64,
    state: TaskState,
}

fn bounded(path: &Path, limit: u64, pid: u32) -> Result<Option<Vec<u8>>, ObservationError> {
    let result = (|| -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    })();
    match result {
        Ok(bytes) if bytes.len() as u64 <= limit => Ok(Some(bytes)),
        Ok(_) => Err(ObservationError::Oversized { pid }),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            Ok(None)
        }
        Err(error) => Err(ObservationError::Io {
            pid: Some(pid),
            errno: error.raw_os_error(),
        }),
    }
}
fn identity(pid: u32) -> Result<Option<Identity>, ObservationError> {
    let Some(bytes) = bounded(Path::new(&format!("/proc/{pid}/stat")), 8192, pid)? else {
        return Ok(None);
    };
    parse_stat(&bytes)
        .map(Some)
        .ok_or(ObservationError::Invalid { pid })
}
fn parse_stat(bytes: &[u8]) -> Option<Identity> {
    let text = std::str::from_utf8(bytes).ok()?;
    let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
    let state = match *fields.first()? {
        "R" => TaskState::Running,
        "S" | "I" => TaskState::Sleeping,
        "D" => TaskState::Uninterruptible,
        "T" | "t" => TaskState::Stopped,
        "Z" => TaskState::Zombie,
        "X" | "x" => TaskState::Dead,
        _ => TaskState::Other,
    };
    Some(Identity {
        parent: fields.get(1)?.parse().ok()?,
        start_ticks: fields.get(19)?.parse().ok()?,
        state,
    })
}
fn wait_symbol(bytes: &[u8], pid: u32) -> Result<WaitChannel, ObservationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ObservationError::Invalid { pid })?
        .trim_end();
    if text == "0" {
        return Ok(WaitChannel::Unavailable);
    }
    if text.is_empty()
        || text.len() > 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ObservationError::Invalid { pid });
    }
    Ok(WaitChannel::Symbol(text.to_owned()))
}
fn io_counters(bytes: &[u8]) -> Option<IoCounters> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut counters = [None; 4];
    for line in text.lines() {
        let (key, value) = line.split_once(':')?;
        let slot = match key {
            "read_bytes" => 0,
            "write_bytes" => 1,
            "syscr" => 2,
            "syscw" => 3,
            _ => continue,
        };
        counters[slot] = Some(value.trim().parse::<u64>().ok()?);
    }
    Some(IoCounters {
        read_bytes: counters[0]?,
        write_bytes: counters[1]?,
        reads: counters[2]?,
        writes: counters[3]?,
    })
}

fn observe(pid: u32, leader: u32) -> Result<Option<TaskObservation>, ObservationError> {
    let Some(before) = identity(pid)? else {
        return Ok(None);
    };
    if pid != leader && before.parent != leader {
        return Ok(None);
    }
    let Some(wait) = bounded(Path::new(&format!("/proc/{pid}/wchan")), 65, pid)? else {
        return Ok(None);
    };
    let wait = wait_symbol(&wait, pid)?;
    let io = match bounded(Path::new(&format!("/proc/{pid}/io")), 4096, pid) {
        Ok(Some(bytes)) => io_counters(&bytes)
            .map(IoObservation::Counters)
            .unwrap_or(IoObservation::Unreadable(ObservationError::Invalid { pid })),
        Ok(None) => IoObservation::Unavailable,
        Err(error) => IoObservation::Unreadable(error),
    };
    let Some(after) = identity(pid)? else {
        return Ok(None);
    };
    // A child can disappear/reuse its PID during the wchan read. Only retain
    // the observation if both identity reads still bind our exact child.
    if before.parent != after.parent
        || before.start_ticks != after.start_ticks
        || (pid != leader && after.parent != leader)
    {
        return Ok(None);
    }
    Ok(Some(TaskObservation {
        pid,
        parent: after.parent,
        start_ticks: after.start_ticks,
        state: after.state,
        wait,
        io,
    }))
}

pub fn capture(leader: u32) -> PostgresSnapshot {
    capture_bounded(leader, 64, Duration::from_millis(100))
}
fn capture_bounded(leader: u32, limit: usize, budget: Duration) -> PostgresSnapshot {
    let mut result = PostgresSnapshot {
        tasks: Vec::new(),
        truncated: false,
        observation_error: None,
    };
    let deadline = Instant::now() + budget;
    if limit == 0 || Instant::now() >= deadline {
        result.truncated = true;
        return result;
    }
    match observe(leader, leader) {
        Ok(Some(task)) => result.tasks.push(task),
        Ok(None) => {}
        Err(error) => {
            result.observation_error = Some(error);
        }
    }
    let children = match bounded(
        Path::new(&format!("/proc/{leader}/task/{leader}/children")),
        8192,
        leader,
    ) {
        Ok(Some(children)) => children,
        Ok(None) => return result,
        Err(error) => {
            result.observation_error.get_or_insert(error);
            return result;
        }
    };
    let children = match std::str::from_utf8(&children) {
        Ok(children) => children,
        Err(_) => {
            result
                .observation_error
                .get_or_insert(ObservationError::Invalid { pid: leader });
            return result;
        }
    };
    for child in children.split_whitespace() {
        if result.tasks.len() >= limit || Instant::now() >= deadline {
            result.truncated = true;
            break;
        }
        let pid = match child.parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => {
                result
                    .observation_error
                    .get_or_insert(ObservationError::Invalid { pid: leader });
                continue;
            }
        };
        match observe(pid, leader) {
            Ok(Some(task)) => result.tasks.push(task),
            Ok(None) => {}
            Err(error) => {
                result.observation_error.get_or_insert(error);
            }
        }
    }
    result.tasks.sort_by_key(|task| task.pid);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn diagnostics_bound_bytes_records_and_untrusted_text() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("oversized");
        fs::write(&path, [b'x'; 100])?;
        assert_eq!(
            bounded(&path, 16, 17),
            Err(ObservationError::Oversized { pid: 17 })
        );
        assert_eq!(
            bounded(directory.path(), 16, 17),
            Err(ObservationError::Io {
                pid: Some(17),
                errno: Some(libc::EISDIR)
            })
        );
        let error = wait_symbol(b"/private/credential-material", 17)
            .expect_err("paths are not kernel symbols");
        assert!(!serde_json::to_string(&error)?.contains("credential-material"));
        assert_eq!(
            wait_symbol(b"do_signal_stop", 17)?,
            WaitChannel::Symbol("do_signal_stop".to_owned())
        );
        let snapshot = capture_bounded(std::process::id(), 0, Duration::from_millis(1));
        assert!(snapshot.truncated && snapshot.tasks.is_empty());
        Ok(())
    }
}
