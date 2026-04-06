/// LSP proxy for the Dart language server.
///
/// Intercepts the `initialize` response and injects `triggerCharacters` into
/// `completionProvider` so that Zed triggers completions on `.` and other chars.
///
/// Workaround for https://github.com/zed-extensions/dart/issues/32
///
/// Usage: dart-lsp-proxy <dart-binary> [args...]
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

fn read_lsp_message(reader: &mut dyn Read) -> Option<Vec<u8>> {
    let mut headers = String::new();
    let mut content_length: usize = 0;

    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).ok()?;
        headers.push(byte[0] as char);

        if headers.ends_with("\r\n\r\n") {
            break;
        }
    }

    for line in headers.lines() {
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = value.trim().parse().ok()?;
        }
    }

    if content_length == 0 {
        return Some(vec![]);
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_lsp_message(writer: &mut dyn Write, body: &[u8]) -> std::io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

fn patch_initialize_response(body: &[u8]) -> Vec<u8> {
    let Ok(mut msg) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };

    let Some(completion) = msg
        .get_mut("result")
        .and_then(|r| r.get_mut("capabilities"))
        .and_then(|c| c.get_mut("completionProvider"))
    else {
        return body.to_vec();
    };

    if completion.get("triggerCharacters").is_none() {
        completion["triggerCharacters"] =
            serde_json::json!([".", "=", "(", ",", " "]);
    }

    serde_json::to_vec(&msg).unwrap_or_else(|_| body.to_vec())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: dart-lsp-proxy <dart-binary> [args...]");
        std::process::exit(1);
    }

    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn {}: {e}", args[0]));

    let mut child_stdin = child.stdin.take().unwrap();
    let mut child_stdout = child.stdout.take().unwrap();

    let patched = Arc::new(Mutex::new(false));
    let patched_clone = Arc::clone(&patched);

    // dart stdout → our stdout (with one-time patch)
    let stdout_thread = thread::spawn(move || {
        let mut out = std::io::stdout();
        loop {
            let Some(body) = read_lsp_message(&mut child_stdout) else {
                break;
            };

            let body = {
                let mut done = patched_clone.lock().unwrap();
                if !*done {
                    if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&body) {
                        if msg.get("result").and_then(|r| r.get("capabilities")).is_some() {
                            *done = true;
                            patch_initialize_response(&body)
                        } else {
                            body
                        }
                    } else {
                        body
                    }
                } else {
                    body
                }
            };

            if write_lsp_message(&mut out, &body).is_err() {
                break;
            }
        }
    });

    // our stdin → dart stdin
    let mut stdin = std::io::stdin();
    loop {
        let Some(body) = read_lsp_message(&mut stdin) else {
            break;
        };
        if write_lsp_message(&mut child_stdin, &body).is_err() {
            break;
        }
    }

    drop(child_stdin);
    stdout_thread.join().ok();
    child.wait().ok();
}
