// llmd — keep a local LLM server resident on Android.
//
// llama.cpp's llama-server already speaks the OpenAI API, so any app on the
// device can POST to 127.0.0.1:PORT/v1/chat/completions. The part Android makes
// hard is keeping it *alive*: the low-memory killer reaps big background
// processes, and there's no service manager for a raw binary. llmd is that
// service manager — it launches llama-server, watches /health, and respawns it
// with backoff whenever it dies (OOM, crash, reboot-relaunch).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::Local;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let res = match args.get(1).map(String::as_str) {
        Some("run") => cmd_run(&args[2..]),
        Some("status") => cmd_status(&args[2..]),
        Some("stop") => cmd_stop(&args[2..]),
        _ => {
            eprintln!(
                "llmd — resident local-LLM supervisor (root)\n\
                 \n\
                 run    --server P --model P [--port N] [--state DIR] [--log F] [-- <server args>]\n\
                 status [--port N] [--state DIR]\n\
                 stop   [--state DIR]\n\
                 \n\
                 Wraps llama.cpp llama-server; respawns it on death with backoff."
            );
            Err("no command".into())
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn opt<'a>(a: &'a [String], k: &str) -> Option<&'a str> {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).map(String::as_str)
}

// Args after a literal `--` are forwarded verbatim to llama-server.
fn passthrough(a: &[String]) -> Vec<String> {
    match a.iter().position(|x| x == "--") {
        Some(i) => a[i + 1..].to_vec(),
        None => Vec::new(),
    }
}

fn stamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

fn state_dir(a: &[String]) -> PathBuf {
    PathBuf::from(opt(a, "--state").unwrap_or("/data/local/tmp/llmd"))
}

// GET /health over a raw socket (no HTTP-client dependency). llama-server
// answers 200 once the model is loaded, 503 while still loading.
fn health_ok(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut s) = TcpStream::connect_timeout(
        &addr.parse().unwrap(),
        Duration::from_millis(800),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    if s.write_all(b"GET /health HTTP/1.0\r\nHost: x\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 256];
    match s.read(&mut buf) {
        Ok(n) => std::str::from_utf8(&buf[..n]).map(|r| r.contains(" 200")).unwrap_or(false),
        Err(_) => false,
    }
}

fn pid_alive(pid: i64) -> bool {
    // Android has procfs; a live pid has /proc/<pid>.
    PathBuf::from(format!("/proc/{pid}")).exists()
}

fn read_pid(dir: &PathBuf, name: &str) -> Option<i64> {
    std::fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()
}

fn cmd_run(a: &[String]) -> R<()> {
    let server = opt(a, "--server").ok_or("run needs --server <llama-server path>")?;
    let model = opt(a, "--model").ok_or("run needs --model <gguf path>")?;
    let port: u16 = opt(a, "--port").and_then(|s| s.parse().ok()).unwrap_or(8080);
    let dir = state_dir(a);
    let log = opt(a, "--log").map(String::from);
    let extra = passthrough(a);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("llmd.pid"), std::process::id().to_string())?;

    let logline = move |msg: &str| {
        let line = format!("{} llmd {msg}", stamp());
        println!("{line}");
        let _ = std::io::stdout().flush();
        if let Some(f) = &log {
            if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                let _ = writeln!(fh, "{line}");
            }
        }
    };

    logline(&format!("supervising {server} model={model} port={port}"));
    let mut backoff = 1u64;

    loop {
        let mut child = Command::new(server)
            .args(["-m", model, "--host", "127.0.0.1", "--port", &port.to_string()])
            .args(&extra)
            .spawn()
            .map_err(|e| format!("spawn {server}: {e}"))?;
        let child_pid = child.id();
        std::fs::write(dir.join("server.pid"), child_pid.to_string())?;
        logline(&format!("spawned llama-server pid={child_pid}"));

        // Watch the child: report ready on first healthy /health, respawn on exit.
        let start = Instant::now();
        let mut announced = false;
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if !announced && health_ok(port) {
                announced = true;
                backoff = 1; // a successful load resets the backoff
                logline(&format!("ready: OpenAI API on http://127.0.0.1:{port}/v1"));
            }
            std::thread::sleep(Duration::from_millis(500));
        };

        // A process that stayed up a while and then died is not a crash loop.
        if start.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        logline(&format!("llama-server exited ({status}); restart in {backoff}s"));
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
}

fn cmd_status(a: &[String]) -> R<()> {
    let dir = state_dir(a);
    let port: u16 = opt(a, "--port").and_then(|s| s.parse().ok()).unwrap_or(8080);
    let sup = read_pid(&dir, "llmd.pid").map(pid_alive).unwrap_or(false);
    let srv_pid = read_pid(&dir, "server.pid");
    let srv = srv_pid.map(pid_alive).unwrap_or(false);
    let healthy = health_ok(port);
    println!(
        "supervisor={} server={} (pid={}) health={} endpoint=http://127.0.0.1:{}/v1",
        yn(sup),
        yn(srv),
        srv_pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        yn(healthy),
        port
    );
    Ok(())
}

fn yn(b: bool) -> &'static str {
    if b { "up" } else { "down" }
}

fn cmd_stop(a: &[String]) -> R<()> {
    let dir = state_dir(a);
    // Kill the supervisor first so it can't respawn the server we're about to kill.
    for name in ["llmd.pid", "server.pid"] {
        if let Some(pid) = read_pid(&dir, name) {
            if pid_alive(pid) {
                let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
                println!("killed {name} pid={pid}");
            }
        }
    }
    Ok(())
}
