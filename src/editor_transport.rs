use crate::thread_worker::Worker;
use crate::types::*;
use crate::util::*;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::borrow::Cow;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path;
use std::process::{Command, Stdio};

pub struct EditorTransport {
    // Not using Worker here as listener blocks forever and joining its thread
    // would block kak-lsp from exiting.
    pub from_editor: Receiver<EditorRequest>,
    pub to_editor: Worker<EditorResponse, Void>,
}

pub fn start(session: &str, initial_request: Option<String>) -> Result<EditorTransport, i32> {
    // NOTE 1024 is arbitrary
    let channel_capacity = 1024;

    let (sender, receiver) = bounded(channel_capacity);
    let mut path = temp_dir();
    path.push(&session);
    if path.exists() {
        if UnixStream::connect(&path).is_err() {
            if fs::remove_file(&path).is_err() {
                error!(
                    "Failed to clean up dead session at {}",
                    path.to_str().unwrap()
                );
                return Err(1);
            };
        } else {
            error!("Server is already running for session {}", session);
            return Err(1);
        }
    }
    std::thread::spawn(move || {
        if let Some(initial_request) = initial_request {
            let initial_request: EditorRequest =
                toml::from_str(&initial_request).expect("Failed to parse initial request");
            if sender.send(initial_request).is_err() {
                return;
            };
        }
        start_unix(&path, sender);
    });
    let from_editor = receiver;

    let to_editor = Worker::spawn(
        "Messages to editor",
        channel_capacity,
        move |receiver: Receiver<EditorResponse>, _| {
            for response in receiver {
                match Command::new("kak")
                    .args(&["-p", &response.meta.session])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(mut child) => {
                        let stdin = match child.stdin.as_mut() {
                            Some(stdin) => stdin,
                            None => {
                                error!("Failed to get editor stdin");
                                return;
                            }
                        };

                        let client = response.meta.client.as_ref();
                        let command = match client.filter(|&s| !s.is_empty()) {
                            Some(client) => {
                                let command = format!(
                                    "eval -client {} -verbatim -- {}",
                                    client, response.command
                                );
                                debug!("To editor `{}`: {}", response.meta.session, command);
                                Cow::from(command)
                            }
                            None => {
                                debug!(
                                    "To editor `{}`: {}",
                                    response.meta.session, response.command
                                );
                                response.command
                            }
                        };

                        if stdin.write_all(command.as_bytes()).is_err() {
                            error!("Failed to write to editor stdin");
                        }
                        // code should fail earlier if Kakoune was not spawned
                        // otherwise something went completely wrong, better to panic
                        child.wait().unwrap();
                    }
                    Err(e) => error!("Failed to run Kakoune: {}", e),
                }
            }
        },
    );

    Ok(EditorTransport {
        from_editor,
        to_editor,
    })
}

pub fn start_unix(path: &path::Path, sender: Sender<EditorRequest>) {
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to bind: {}", e);
            return;
        }
    };

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let mut request = String::new();
                match stream.read_to_string(&mut request) {
                    Ok(_) => {
                        if request.is_empty() {
                            continue;
                        }
                        debug!("From editor: {}", request);
                        let request: EditorRequest =
                            toml::from_str(&request).expect("Failed to parse editor request");
                        if sender.send(request).is_err() {
                            return;
                        };
                    }
                    Err(e) => {
                        error!("Failed to read from stream: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}
