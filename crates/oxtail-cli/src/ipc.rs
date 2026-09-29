//! Single-instance handling over a local socket (named pipe on Windows,
//! abstract/Unix socket elsewhere).
//!
//! The socket name is derived from the data folder's `instance_key()`, so two
//! portable copies never talk to each other. Protocol: the client connects,
//! writes one JSON [`OpenRequest`] followed by `\n`, and waits for the line
//! `ok`. The server forwards the request into the GUI through a channel and
//! wakes the UI.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, NameType as _, Stream,
    ToFsName as _, ToNsName as _, prelude::*,
};
use oxtail_gui::{ExternalOpen, OpenRequest};

/// Longest request line the server accepts.
const MAX_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
/// How long a client waits for the running instance to acknowledge.
const ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// The outcome of [`acquire`].
pub enum Role {
    /// This process is the (only) instance and must serve requests.
    Server(Server),
    /// A running instance took the request; this process should exit.
    Forwarded,
    /// Single-instance handling is unavailable (no usable socket name, or the
    /// other instance did not answer); run standalone.
    Standalone,
}

/// Server side: incoming requests and the slot for the UI context.
pub struct Server {
    /// Receives the requests other invocations sent.
    pub external: ExternalOpen,
}

/// The socket name for `key`. `dir` is used only where namespaced names do
/// not exist (macOS): the socket file lives in the data folder.
fn socket_name(key: &str, dir: Option<&Path>) -> Option<Name<'static>> {
    let leaf = format!("oxtail-{key}");
    if GenericNamespaced::is_supported() {
        return leaf.to_ns_name::<GenericNamespaced>().ok();
    }
    let dir = dir?;
    let path: PathBuf = dir.join(format!("{leaf}.sock"));
    path.into_os_string()
        .to_fs_name::<GenericFilePath>()
        .ok()
        .map(Name::into_owned)
}

fn try_forward(name: Name<'static>, request: &OpenRequest) -> bool {
    let request = request.clone();
    let (tx, rx) = unbounded::<bool>();
    // A hung instance must not hang us: do the exchange on a helper thread.
    let spawned = thread::Builder::new()
        .name("oxtail-ipc-client".into())
        .spawn(move || {
            let ok = (|| -> std::io::Result<bool> {
                let stream = Stream::connect(name)?;
                let mut reader = BufReader::new(stream);
                let mut line = serde_json::to_string(&request)?;
                line.push('\n');
                reader.get_mut().write_all(line.as_bytes())?;
                reader.get_mut().flush()?;
                let mut ack = String::new();
                reader.read_line(&mut ack)?;
                Ok(ack.trim() == "ok")
            })();
            let _ = tx.send(ok.unwrap_or(false));
        });
    spawned.is_ok() && rx.recv_timeout(ACK_TIMEOUT).unwrap_or(false)
}

/// Decides whether this process is the first instance.
///
/// If another instance answers on the socket, `request` is forwarded to it
/// and [`Role::Forwarded`] is returned. Otherwise a listener is created and
/// [`Role::Server`] is returned.
pub fn acquire(key: &str, dir: Option<&Path>, request: &OpenRequest) -> Role {
    let Some(name) = socket_name(key, dir) else {
        return Role::Standalone;
    };
    if try_forward(name.clone(), request) {
        return Role::Forwarded;
    }
    // Nobody answered: either nobody listens, or a stale socket file is left.
    match ListenerOptions::new()
        .name(name.clone())
        .try_overwrite(true)
        .create_sync()
    {
        Ok(listener) => Role::Server(start_server(listener)),
        Err(e) => {
            tracing::warn!("single-instance socket unavailable: {e}");
            // A race: another instance bound between our connect and bind.
            if try_forward(name, request) {
                Role::Forwarded
            } else {
                Role::Standalone
            }
        }
    }
}

fn start_server(listener: interprocess::local_socket::Listener) -> Server {
    let (tx, rx) = unbounded::<OpenRequest>();
    let slot = Arc::new(Mutex::new(None));
    let external = ExternalOpen {
        rx,
        ctx_slot: slot.clone(),
    };
    let spawned = thread::Builder::new()
        .name("oxtail-ipc-server".into())
        .spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => {
                        let tx = tx.clone();
                        let slot = slot.clone();
                        // One short-lived thread per connection: a client that
                        // stalls mid-request never blocks the next one.
                        let _ = thread::Builder::new()
                            .name("oxtail-ipc-conn".into())
                            .spawn(move || handle_connection(stream, &tx, &slot));
                    }
                    Err(e) => {
                        tracing::warn!("ipc accept failed: {e}");
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the ipc server thread: {e}");
    }
    Server { external }
}

fn handle_connection(
    stream: Stream,
    tx: &Sender<OpenRequest>,
    slot: &Arc<Mutex<Option<egui::Context>>>,
) {
    use std::io::Read as _;
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<OpenRequest>(line.trim()) else {
        tracing::warn!("ignoring a malformed request from another instance");
        return;
    };
    let _ = tx.send(request);
    if let Ok(guard) = slot.lock()
        && let Some(ctx) = guard.as_ref()
    {
        ctx.request_repaint();
    }
    let stream = reader.into_inner().into_inner();
    let mut stream = stream;
    let _ = stream.write_all(b"ok\n");
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_key(tag: &str) -> String {
        format!(
            "test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        )
        .replace([' ', ':', '{', '}', '.'], "")
    }

    #[test]
    fn second_instance_forwards_to_the_first() {
        let key = unique_key("fwd");
        let dir = tempfile::tempdir().unwrap();
        let req = OpenRequest::file("/tmp/one.log");
        let Role::Server(server) = acquire(&key, Some(dir.path()), &req) else {
            panic!("first instance must become the server");
        };
        let second = OpenRequest {
            files: vec!["/tmp/two.log".into(), "/tmp/three.log".into()],
            tail_lines: Some(7),
            ..OpenRequest::default()
        };
        assert!(matches!(
            acquire(&key, Some(dir.path()), &second),
            Role::Forwarded
        ));
        let got = server
            .external
            .rx
            .recv_timeout(Duration::from_secs(10))
            .expect("request arrives");
        assert_eq!(got.files, second.files);
        assert_eq!(got.tail_lines, Some(7));
    }

    #[test]
    fn different_keys_do_not_talk() {
        let dir = tempfile::tempdir().unwrap();
        let req = OpenRequest::file("/tmp/x.log");
        let a = acquire(&unique_key("a"), Some(dir.path()), &req);
        let b = acquire(&unique_key("b"), Some(dir.path()), &req);
        assert!(matches!(a, Role::Server(_)));
        assert!(matches!(b, Role::Server(_)));
    }
}
