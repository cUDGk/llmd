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
        // Only the status line counts (llama-server: "HTTP/1.1 200 OK", or 503 while loading),
        // so a "200" elsewhere in headers can't false-positive.
        Ok(n) => std::str::from_utf8(&buf[..n])
            .ok()
            .and_then(|r| r.lines().next())
            .map(|l| l.contains(" 200"))
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn pid_alive(pid: i64) -> bool {
    // Android has procfs; a live pid has /proc/<pid>.
    PathBuf::from(format!("/proc/{pid}")).exists()
}

// Confirm a pid is still the process we think it is. Pids recycle fast under Android's
// low-memory killer, so a bare /proc check can report an unrelated app as "our" server.
fn pid_is(pid: i64, needle: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .map(|s| s.replace('\0', " ").contains(needle))
        .unwrap_or(false)
}

fn read_pid(dir: &PathBuf, name: &str) -> Option<i64> {
    std::fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()
}

fn cmd_run(a: &[String]) -> R<()> {
    let extra = passthrough(a);
    // Parse llmd's own options only from before `--`; everything after is llama-server's,
    // so a passthrough flag (e.g. `-- --port 9`) can't hijack llmd's own --port/--state.
    let cut = a.iter().position(|x| x == "--").unwrap_or(a.len());
    let a = &a[..cut];
    let server = opt(a, "--server").ok_or("run needs --server <llama-server path>")?;
    let model = opt(a, "--model").ok_or("run needs --model <gguf path>")?;
    let port: u16 = opt(a, "--port").and_then(|s| s.parse().ok()).unwrap_or(8080);
    let dir = state_dir(a);
    let log = opt(a, "--log").map(String::from);
    std::fs::create_dir_all(&dir)?;
    // Refuse a second supervisor on the same state dir — both would fight for the port
    // and only the last llmd.pid would be recorded, leaking the other.
    if let Some(pid) = read_pid(&dir, "llmd.pid") {
        if pid_alive(pid) && pid_is(pid, "llmd") {
            return Err(format!("llmd already running (pid {pid}); run `stop` first").into());
        }
    }
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
        // Non-fatal: a transient FS error here must not abort the loop and orphan the child.
        let _ = std::fs::write(dir.join("server.pid"), child_pid.to_string());
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
                // NB: do NOT reset backoff here. A server that loads, serves briefly, then
                // OOM-crashes would otherwise loop every 1s. Only sustained uptime (below)
                // resets it.
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
    // AND an identity check so a recycled pid isn't reported as our process.
    let sup = read_pid(&dir, "llmd.pid").map(|p| pid_alive(p) && pid_is(p, "llmd")).unwrap_or(false);
    let srv_pid = read_pid(&dir, "server.pid");
    let srv = srv_pid.map(|p| pid_alive(p) && pid_is(p, "llama")).unwrap_or(false);
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
    // Kill the supervisor FIRST and wait for it to actually exit, otherwise it could
    // respawn the server in the gap between our two kills (and overwrite server.pid).
    if let Some(pid) = read_pid(&dir, "llmd.pid") {
        if pid_alive(pid) && pid_is(pid, "llmd") {
            let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
            for _ in 0..40 {
                if !pid_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100)); // up to ~4s
            }
            println!("stopped supervisor pid={pid}");
        }
    }
    // Now the (current) server pid is stable; kill it if it's really llama-server.
    if let Some(pid) = read_pid(&dir, "server.pid") {
        if pid_alive(pid) && pid_is(pid, "llama") {
            let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
            println!("stopped server pid={pid}");
        }
    }
    // Remove pidfiles so a later `status` can't read a recycled pid as "up".
    let _ = std::fs::remove_file(dir.join("llmd.pid"));
    let _ = std::fs::remove_file(dir.join("server.pid"));
    Ok(())
}
