//! Debug-only automation seam for `scripts/verify-desktop-smoke.mjs`.
//!
//! Compiled only under `debug_assertions` (see the `mod` line in `main.rs`)
//! and active only when `OBSINK_AUTOMATION_PORT` is set: a `TcpListener` on
//! `127.0.0.1:<port>` takes one JSON line per connection and answers with one
//! JSON line, `{"ok":true,"value":...}` or `{"ok":false,"error":"..."}`.
//!
//! Operations (`op`):
//! - `ping`
//! - `eval {window, js}`: `js` is the body of an async function run inside
//!   that window's web view; its return value comes back as `value`. The
//!   body can click the UI (`document.querySelector(...)`) or call the same
//!   commands the UI calls (`await window.__TAURI_INTERNALS__.invoke(...)`).
//!   WKWebView reports script errors only to its own log, so a syntax error
//!   in `js` surfaces as the 30 s timeout.
//! - `show {window}` / `hide {window}`: shows the window without activating
//!   the app or focusing it (the smoke only shows a window to capture it);
//!   `popover` is centred first because nothing anchors it to the tray icon.
//! - `bounds {window}`: outer position and size in points, for
//!   `screencapture -R`.
//!
//! Build the binary it lives in with `npm run build -w desktop && cargo build
//! -p obsink-desktop --features tauri/custom-protocol` so the frontend is
//! embedded (a plain debug build loads the Vite dev URL instead).
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Listener, Manager, WebviewWindow};

const RESULT_EVENT: &str = "automation://result";
const EVAL_TIMEOUT: Duration = Duration::from_secs(30);

static PENDING: OnceLock<Mutex<HashMap<String, mpsc::Sender<Value>>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<String, mpsc::Sender<Value>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Starts the listener when `OBSINK_AUTOMATION_PORT` names a port; a no-op
/// otherwise.
pub(crate) fn start_if_requested(app: AppHandle) {
    let Some(port) = std::env::var("OBSINK_AUTOMATION_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
    else {
        return;
    };
    // One listener routes every eval result to the request that waits for it.
    app.listen(RESULT_EVENT, |event| {
        let Ok(message) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return;
        };
        if let Some(tx) = pending().lock().unwrap().remove(id) {
            let _ = tx.send(message);
        }
    });
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("automation: cannot listen on 127.0.0.1:{port}: {err}");
            return;
        }
    };
    eprintln!("automation: listening on 127.0.0.1:{port}");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = handle_connection(&app, stream);
        }
    });
}

fn handle_connection(app: &AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(request) => match dispatch(app, &request) {
            Ok(value) => json!({ "ok": true, "value": value }),
            Err(error) => json!({ "ok": false, "error": error }),
        },
        Err(err) => json!({ "ok": false, "error": format!("bad request: {err}") }),
    };
    writeln!(stream, "{reply}")
}

fn window(app: &AppHandle, request: &Value) -> Result<WebviewWindow, String> {
    let label = request
        .get("window")
        .and_then(Value::as_str)
        .ok_or("missing window")?;
    app.get_webview_window(label)
        .ok_or_else(|| format!("no window {label}"))
}

fn dispatch(app: &AppHandle, request: &Value) -> Result<Value, String> {
    match request.get("op").and_then(Value::as_str) {
        Some("ping") => Ok(json!("pong")),
        Some("eval") => {
            let js = request
                .get("js")
                .and_then(Value::as_str)
                .ok_or("missing js")?;
            eval(&window(app, request)?, js)
        }
        Some("show") => {
            // Show without activating the app or taking focus, so a smoke run
            // that wants a screenshot still leaves the keyboard where it was.
            // An inactive app's window stays behind the active app's, so it
            // floats while shown; `hide` puts it back (the popover always
            // floats).
            let target = window(app, request)?;
            if target.label() == "popover" {
                target.center().map_err(|e| e.to_string())?;
            } else {
                target.set_always_on_top(true).map_err(|e| e.to_string())?;
            }
            target.show().map_err(|e| e.to_string())?;
            if target.label() == "popover" {
                app.emit_to(target.label(), "popover://opened", ())
                    .map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        Some("hide") => {
            let target = window(app, request)?;
            target.hide().map_err(|e| e.to_string())?;
            if target.label() != "popover" {
                target.set_always_on_top(false).map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        Some("bounds") => {
            let target = window(app, request)?;
            let scale = target.scale_factor().map_err(|e| e.to_string())?;
            let position = target
                .outer_position()
                .map_err(|e| e.to_string())?
                .to_logical::<f64>(scale);
            let size = target
                .outer_size()
                .map_err(|e| e.to_string())?
                .to_logical::<f64>(scale);
            Ok(json!({
                "x": position.x,
                "y": position.y,
                "width": size.width,
                "height": size.height,
                "scale": scale,
            }))
        }
        other => Err(format!("unknown op {other:?}")),
    }
}

fn eval(target: &WebviewWindow, body: &str) -> Result<Value, String> {
    let id = format!(
        "{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let (tx, rx) = mpsc::channel();
    pending().lock().unwrap().insert(id.clone(), tx);
    let id_js = serde_json::to_string(&id).map_err(|e| e.to_string())?;
    let event_js = serde_json::to_string(RESULT_EVENT).map_err(|e| e.to_string())?;
    let script = format!(
        r#"(async () => {{
  const id = {id_js};
  let ok = true;
  let value;
  try {{
    value = await (async () => {{
{body}
    }})();
  }} catch (error) {{
    ok = false;
    value = String((error && error.message) || error);
  }}
  if (value === undefined) value = null;
  try {{ JSON.stringify(value); }} catch (_) {{ value = String(value); }}
  await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {{
    event: {event_js},
    payload: {{ id, ok, value }},
  }});
}})();"#
    );
    target.eval(script).map_err(|e| e.to_string())?;
    match rx.recv_timeout(EVAL_TIMEOUT) {
        Ok(message) => {
            let ok = message.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let value = message.get("value").cloned().unwrap_or(Value::Null);
            if ok {
                Ok(value)
            } else {
                Err(format!("eval threw: {value}"))
            }
        }
        Err(_) => {
            pending().lock().unwrap().remove(&id);
            Err("eval timed out: syntax error in js, or the window is not loaded".to_string())
        }
    }
}
