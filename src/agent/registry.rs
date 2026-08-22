//! A register of the MCP sessions running on this machine, over sockets rather than files.
//!
//! MCP mode picks a loopback port and a fresh token every time it starts, so the URL an agent
//! needs exists only inside the window that printed it. That is fine for one window and no
//! help at all from a second one, or from a shell: there is no way to ask "what is running,
//! and how do I reach it?".
//!
//! So every session listens on an *abstract* Unix socket named `@irontsc/<pid>` and answers
//! one question, `describe`, with the JSON that says where it is and what it is connected to.
//! Abstract sockets suit this better than a directory of files:
//!
//! * Nothing is written anywhere. The token stays in memory, where the rest of it lives.
//! * The name is the kernel's, and it goes when the process does -- there is no stale entry to
//!   prune after a crash, and no cleanup to get wrong.
//! * The peer's user id comes from the kernel with the connection, so a session can refuse
//!   anyone but its owner rather than trusting file permissions to have been set right.
//!
//! Listing them means enumerating the names, which `/proc/net/unix` has.
//!
//! Windows has the same shape with different parts: a named pipe, `\\.\pipe\irontsc-<pid>`,
//! which the object manager drops when its last handle closes, and which is enumerated by
//! listing `\\.\pipe\` as a directory. Its default security comes from the creating process's
//! token, so it is reachable by its owner (and, as everything is, by an administrator).
//! Anywhere else the register is simply empty, and MCP mode works as it always did.

use serde::{Deserialize, Serialize};

/// The name every session's socket starts with, followed by its PID.
const PREFIX: &str = "irontsc/";

/// The one thing a session will answer.
const DESCRIBE: &str = "describe";

/// What a session says about itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descriptor {
    pub pid: u32,
    /// `http` for a session a window is sharing, `stdio` for one an MCP client started.
    pub transport: String,
    /// The URL to hand an MCP client, token and all. Absent on stdio, which has no address.
    #[serde(default)]
    pub url: Option<String>,
    /// The remote computer, and the user signed in to it: what tells one session from another
    /// when several are up.
    #[serde(default)]
    pub computer: String,
    #[serde(default)]
    pub username: String,
    /// Seconds since the epoch, for showing how long it has been up.
    #[serde(default)]
    pub started_at: u64,
}

impl Descriptor {
    /// How a person picks this one out of a list.
    pub fn label(&self) -> String {
        match (self.username.trim(), self.computer.trim()) {
            ("", "") => format!("session {}", self.pid),
            ("", computer) => computer.to_owned(),
            (username, "") => username.to_owned(),
            (username, computer) => format!("{username}@{computer}"),
        }
    }

    /// True when `needle` picks this session out: its PID, its port, or part of the computer
    /// or user name.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return false;
        }
        if needle == self.pid.to_string() {
            return true;
        }
        if let Some(port) = self.port()
            && needle == port.to_string()
        {
            return true;
        }
        self.computer.to_lowercase().contains(&needle)
            || self.username.to_lowercase().contains(&needle)
    }

    /// The loopback port, dug back out of the URL for the sake of matching on it.
    pub fn port(&self) -> Option<u16> {
        let url = self.url.as_ref()?;
        let after_host = url.split("://").nth(1)?;
        let authority = after_host.split('/').next()?;
        authority.rsplit(':').next()?.parse().ok()
    }
}

/// A session's presence in the register. Dropping it stops answering, which is what closing
/// MCP mode does.
#[derive(Debug)]
pub struct Beacon {
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for Beacon {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Seconds since the epoch, or zero on a clock that predates it.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The abstract socket name a session with this PID listens on.
fn socket_name(pid: u32) -> String {
    format!("{PREFIX}{pid}")
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader as AsyncBufReader};
    use tokio_util::sync::CancellationToken;

    use super::{Beacon, DESCRIBE, Descriptor, PREFIX, socket_name};

    /// Starts answering `describe` on this process's socket.
    ///
    /// The accept loop runs on `handle`, which is the runtime MCP mode already has: this is a
    /// few bytes of traffic when somebody asks, and nothing at all the rest of the time.
    pub fn announce(
        descriptor: Descriptor,
        handle: &tokio::runtime::Handle,
    ) -> std::io::Result<Beacon> {
        let address = SocketAddr::from_abstract_name(socket_name(descriptor.pid))?;
        let listener = UnixListener::bind_addr(&address)?;
        listener.set_nonblocking(true)?;

        let cancel = CancellationToken::new();
        let accept_cancel = cancel.clone();
        let answer = serde_json::to_string(&descriptor).map_err(std::io::Error::other)?;

        let listener = {
            let _runtime = handle.enter();
            tokio::net::UnixListener::from_std(listener)?
        };

        handle.spawn(async move {
            // Only the user who owns the session may ask about it. The kernel supplies the
            // peer's id with the connection, so this cannot be talked out of.
            let owner = nix_uid();
            loop {
                let stream = tokio::select! {
                    () = accept_cancel.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _address)) => stream,
                        Err(error) => {
                            tracing::warn!(%error, "session register stopped accepting");
                            break;
                        }
                    },
                };

                let peer = stream.peer_cred().map(|cred| cred.uid()).ok();
                if peer != Some(owner) {
                    tracing::warn!("refused a session register query from another user");
                    continue;
                }

                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut stream = stream;
                    let (reader, mut writer) = stream.split();
                    let mut request = String::new();
                    if AsyncBufReader::new(reader)
                        .read_line(&mut request)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if request.trim() != DESCRIBE {
                        return;
                    }
                    let _ = writer.write_all(answer.as_bytes()).await;
                    let _ = writer.write_all(b"\n").await;
                    let _ = writer.flush().await;
                });
            }
        });

        Ok(Beacon { cancel })
    }

    /// Every session answering right now, newest first.
    ///
    /// Blocking, and meant to be: the CLI calls it, the sockets are local, and a session that
    /// has just died simply refuses the connection.
    pub fn list() -> Vec<Descriptor> {
        let mut sessions: Vec<Descriptor> =
            names().iter().filter_map(|name| describe(name)).collect();
        // Newest first, which is the one a person most likely means.
        sessions.sort_by_key(|session| std::cmp::Reverse(session.started_at));
        sessions
    }

    /// Asks one session to describe itself.
    fn describe(name: &str) -> Option<Descriptor> {
        let address = SocketAddr::from_abstract_name(name).ok()?;
        let mut stream = UnixStream::connect_addr(&address).ok()?;
        // A session that is wedged must not wedge the CLI with it.
        let timeout = std::time::Duration::from_millis(500);
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));

        writeln!(stream, "{DESCRIBE}").ok()?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).ok()?;
        serde_json::from_str(&answer).ok()
    }

    /// The abstract socket names that belong to us, read out of the kernel's own list.
    ///
    /// `/proc/net/unix` writes an abstract name with a leading `@`, the same way `ss` shows it.
    fn names() -> Vec<String> {
        let Ok(contents) = std::fs::read_to_string("/proc/net/unix") else {
            return Vec::new();
        };

        let mut names = Vec::new();
        for line in contents.lines() {
            // The path is the last column, and is the only one that can hold a space, so it is
            // taken as the whole tail rather than split further.
            let Some(path) = line.split_whitespace().last() else {
                continue;
            };
            if let Some(name) = path.strip_prefix('@')
                && name.starts_with(PREFIX)
                && !names.iter().any(|seen| seen == name)
            {
                names.push(name.to_owned());
            }
        }
        names
    }

    fn nix_uid() -> u32 {
        // SAFETY: getuid cannot fail and touches nothing of ours.
        unsafe { libc::getuid() }
    }
}

#[cfg(windows)]
mod windows {
    use std::io::{BufRead as _, BufReader, Write as _};

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader as AsyncBufReader};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio_util::sync::CancellationToken;

    use super::{Beacon, DESCRIBE, Descriptor, PREFIX, socket_name};

    /// The directory the object manager keeps named pipes in.
    const PIPES: &str = r"\\.\pipe\";

    fn pipe_path(pid: u32) -> String {
        format!("{PIPES}{}", socket_name(pid).replace('/', "-"))
    }

    /// Starts answering `describe` on this process's pipe.
    ///
    /// A pipe serves one client per instance, so the loop hands each connection off and opens
    /// the next instance behind it. Security is the default for the process token, which is
    /// the owner, the system and administrators -- the same reach they have over the process
    /// holding the token in memory anyway.
    pub fn announce(
        descriptor: Descriptor,
        handle: &tokio::runtime::Handle,
    ) -> std::io::Result<Beacon> {
        let path = pipe_path(descriptor.pid);
        let answer = serde_json::to_string(&descriptor).map_err(std::io::Error::other)?;

        let first = {
            let _runtime = handle.enter();
            ServerOptions::new().first_pipe_instance(true).create(&path)?
        };

        let cancel = CancellationToken::new();
        let accept_cancel = cancel.clone();
        handle.spawn(async move {
            let mut server = first;
            loop {
                let connected = tokio::select! {
                    () = accept_cancel.cancelled() => break,
                    connection = server.connect() => match connection {
                        Ok(()) => server,
                        Err(error) => {
                            tracing::warn!(%error, "session register stopped accepting");
                            break;
                        }
                    },
                };

                server = match ServerOptions::new().create(&path) {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::warn!(%error, "session register could not reopen its pipe");
                        break;
                    }
                };

                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut connected = connected;
                    let (reader, mut writer) = tokio::io::split(&mut connected);
                    let mut request = String::new();
                    if AsyncBufReader::new(reader)
                        .read_line(&mut request)
                        .await
                        .is_err()
                        || request.trim() != DESCRIBE
                    {
                        return;
                    }
                    let _ = writer.write_all(answer.as_bytes()).await;
                    let _ = writer.write_all(b"\n").await;
                    let _ = writer.flush().await;
                });
            }
        });

        Ok(Beacon { cancel })
    }

    /// Every session answering right now, newest first.
    pub fn list() -> Vec<Descriptor> {
        let Ok(entries) = std::fs::read_dir(PIPES) else {
            return Vec::new();
        };

        let prefix = PREFIX.replace('/', "-");
        let mut sessions: Vec<Descriptor> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(&prefix).then(|| describe(&name))?
            })
            .collect();
        sessions.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        sessions
    }

    /// Asks one session to describe itself. A pipe is a file, so this is one open and two
    /// lines of traffic.
    fn describe(name: &str) -> Option<Descriptor> {
        let mut pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{PIPES}{name}"))
            .ok()?;
        writeln!(pipe, "{DESCRIBE}").ok()?;
        let mut answer = String::new();
        BufReader::new(pipe).read_line(&mut answer).ok()?;
        serde_json::from_str(&answer).ok()
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod elsewhere {
    use super::{Beacon, Descriptor};

    pub fn announce(
        _descriptor: Descriptor,
        _handle: &tokio::runtime::Handle,
    ) -> std::io::Result<Beacon> {
        Err(std::io::Error::other(
            "the session register needs abstract sockets or named pipes",
        ))
    }

    pub fn list() -> Vec<Descriptor> {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
pub use linux::{announce, list};
#[cfg(windows)]
pub use windows::{announce, list};
#[cfg(not(any(target_os = "linux", windows)))]
pub use elsewhere::{announce, list};

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(pid: u32) -> Descriptor {
        Descriptor {
            pid,
            transport: "http".to_owned(),
            url: Some("http://127.0.0.1:7444/mcp?t=secret".to_owned()),
            computer: "desktop.example".to_owned(),
            username: "someone".to_owned(),
            started_at: now(),
        }
    }

    #[test]
    fn reads_the_port_back_out_of_the_url() {
        assert_eq!(descriptor(1).port(), Some(7444));
    }

    #[test]
    fn matches_on_pid_port_computer_and_user() {
        let session = descriptor(4242);
        assert!(session.matches("4242"), "its PID");
        assert!(session.matches("7444"), "its port");
        assert!(session.matches("DESKTOP"), "part of the computer, any case");
        assert!(session.matches("some"), "part of the user name");
        assert!(!session.matches("elsewhere"));
        assert!(!session.matches("  "));
    }

    #[test]
    fn labels_a_session_by_who_and_where() {
        assert_eq!(descriptor(1).label(), "someone@desktop.example");
        let mut anonymous = descriptor(2);
        anonymous.username = String::new();
        anonymous.computer = String::new();
        assert_eq!(anonymous.label(), "session 2");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_session_describes_itself_and_stops_when_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let mine = descriptor(std::process::id());

        let beacon = announce(mine.clone(), runtime.handle()).expect("announces");
        // The accept loop only runs while the runtime is being driven, so the query goes on a
        // thread of its own and the runtime is turned over here.
        let query = std::thread::spawn(list);
        let listed = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::task::spawn_blocking(|| ()).await.expect("idles");
                loop {
                    if query.is_finished() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the query finishes");
            query.join().expect("the query thread lives")
        });

        let found = listed
            .iter()
            .find(|session| session.pid == mine.pid)
            .expect("this session is listed while it answers");
        assert_eq!(found.url, mine.url);

        drop(beacon);
        // The socket name goes with the listener, so nothing answers on it any more.
        let after = std::thread::spawn(list).join().expect("the query thread lives");
        assert!(
            !after.iter().any(|session| session.pid == mine.pid),
            "a withdrawn session is not listed"
        );
    }
}
