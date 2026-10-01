use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "duw-cli-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn duw() -> Command {
    Command::new(env!("CARGO_BIN_EXE_duw"))
}

fn isolated_duw(f: &Fixture) -> Command {
    let mut command = duw();
    command.env("LOCALAPPDATA", f.path.join("data"));
    command.env("XDG_DATA_HOME", f.path.join("data"));
    #[cfg(target_os = "macos")]
    command.env("HOME", f.path.join("data"));
    command
}

fn start_server(mut command: Command) -> (ChildGuard, String) {
    let mut child = ChildGuard(
        command
            .args(["--no-open", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
    let first = lines.next().expect("server did not start").unwrap();
    assert!(first.starts_with("duw: "), "stdout: {first}");
    let address = lines
        .next()
        .unwrap()
        .unwrap()
        .strip_prefix("duw: http://")
        .unwrap()
        .to_string();
    (child, address)
}

fn request(address: &str, method: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    assert_ne!(
        reader.read_line(&mut line).unwrap(),
        0,
        "missing status line"
    );
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut content_length = None;
    loop {
        line.clear();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "missing header end"
        );
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut body = vec![0; content_length.unwrap_or(0)];
    reader.read_exact(&mut body).unwrap();
    (status, String::from_utf8(body).unwrap())
}

fn get_json(address: &str, path: &str) -> serde_json::Value {
    let (status, body) = request(address, "GET", path);
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

fn wait_for_idle(address: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = get_json(address, "/api/state");
        if state["scanning"] == false {
            return state;
        }
        assert!(std::time::Instant::now() < deadline, "scan did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn snapshot_startup_replays_saved_data_and_allows_subtree_rescans() {
    let f = Fixture::new();
    let root = f.path.join("root");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("sub/old.txt"), b"old").unwrap();
    fs::write(root.join("keep.txt"), b"keep").unwrap();
    let mut command = isolated_duw(&f);
    command.arg(&root);
    let (scan_server, address) = start_server(command);
    let saved = wait_for_idle(&address);
    assert_eq!(
        request(&address, "POST", "/api/snapshots?name=replay").0,
        200
    );
    drop(scan_server);

    fs::remove_file(root.join("sub/old.txt")).unwrap();
    fs::write(root.join("sub/new.txt"), b"new file").unwrap();
    fs::write(root.join("keep.txt"), b"changed outside the subtree").unwrap();
    fs::remove_dir(root.join("empty")).unwrap();
    let mut command = isolated_duw(&f);
    command.args(["--snapshot", "replay"]);
    let (snapshot_server, address) = start_server(command);
    let loaded = get_json(&address, "/api/state");
    assert_eq!(loaded["scanning"], false);
    assert_eq!(loaded["root"], saved["root"]);
    assert_eq!(loaded["root_size"], saved["root_size"]);
    assert_eq!(loaded["root_alloc"], saved["root_alloc"]);
    assert_eq!(loaded["stats"], saved["stats"]);
    let node = get_json(&address, "/api/node/0");
    let children = node["children"].as_array().unwrap();
    assert!(children.iter().any(|entry| entry["name"] == "empty"));
    let sub = children
        .iter()
        .find(|entry| entry["name"] == "sub")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let sub_before = get_json(&address, &format!("/api/node/{sub}"));
    assert_eq!(sub_before["children"][0]["name"], "old.txt");
    assert_eq!(
        request(&address, "POST", &format!("/api/rescan/{sub}")).0,
        202
    );
    wait_for_idle(&address);
    let sub_after = get_json(&address, &format!("/api/node/{sub}"));
    assert_eq!(sub_after["children"].as_array().unwrap().len(), 1);
    assert_eq!(sub_after["children"][0]["name"], "new.txt");
    assert_eq!(sub_after["children"][0]["size"], 8);
    let root_after = get_json(&address, "/api/node/0");
    let keep = root_after["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "keep.txt")
        .unwrap();
    assert_eq!(keep["size"], 4);
    drop(snapshot_server);

    // A saved scan remains viewable even when its original disk is absent.
    fs::rename(&root, f.path.join("disconnected-root")).unwrap();
    let output = isolated_duw(&f)
        .args(["--snapshot", "replay", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stats"], saved["stats"]);
    assert!(report["largest"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == "sub/old.txt"));
    let output = isolated_duw(&f)
        .args(["--snapshot", "replay", "--top", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "4\tkeep.txt\n");
}

#[test]
fn snapshot_errors_do_not_fall_back_to_scanning() {
    let f = Fixture::new();
    let output = isolated_duw(&f)
        .args(["--snapshot", "missing", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot load snapshot"));
    assert!(output.stdout.is_empty());
    let output = isolated_duw(&f)
        .args(["--snapshot", "missing", ".", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn json_mode_reports_scanned_files_and_types() {
    let f = Fixture::new();
    fs::write(f.path.join("large.txt"), b"1234567890").unwrap();
    fs::write(f.path.join("ignored.bin"), b"ignored").unwrap();

    let output = duw()
        .args([
            "--json",
            "--exclude",
            "ignored.bin",
            f.path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stats"]["files"], 1);
    assert_eq!(report["types"][0]["ext"], "txt");
    assert_eq!(report["largest"][0]["path"], "large.txt");
}

#[test]
fn top_mode_reads_patterns_from_exclude_file() {
    let f = Fixture::new();
    fs::write(f.path.join("keep.bin"), vec![0u8; 8]).unwrap();
    fs::write(f.path.join("skip.bin"), vec![0u8; 64]).unwrap();
    let excludes = f.path.join("excludes.txt");
    fs::write(&excludes, "# comment\n\n skip.bin \n excludes.txt\n").unwrap();

    let output = duw()
        .args([
            "--top",
            "1",
            "--exclude-from",
            excludes.to_str().unwrap(),
            f.path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\tkeep.bin\n"), "stdout: {stdout:?}");
    assert!(!stdout.contains("skip.bin"), "stdout: {stdout:?}");
}

#[test]
fn invalid_scan_path_returns_a_process_error() {
    let missing = std::env::temp_dir().join(format!(
        "duw-cli-missing-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    let output = duw()
        .args(["--json", missing.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("duw: cannot open"), "stderr: {stderr:?}");
}

#[test]
fn invalid_host_is_reported_before_server_startup() {
    let f = Fixture::new();
    let output = duw()
        .args(["--no-open", "--host", "not-an-ip", f.path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("invalid host address"),
        "stderr: {stderr:?}"
    );
}

#[test]
fn server_mode_binds_and_serves_the_state_endpoint() {
    let f = Fixture::new();
    let mut child = ChildGuard(
        duw()
            .args(["--no-open", "--port", "0", f.path.to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let scanning = lines.next().unwrap().unwrap();
    assert!(
        scanning.starts_with("duw: scanning"),
        "stdout: {scanning:?}"
    );
    let address_line = lines.next().unwrap().unwrap();
    let address = address_line.strip_prefix("duw: http://").unwrap();

    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(b"GET /api/state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "response: {response:?}"
    );
    assert!(response.contains("\"root_id\":0"), "response: {response:?}");
}
