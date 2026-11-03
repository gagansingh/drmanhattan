//! A stand-in release server, and drmanhattan installed in a directory of its own, for the
//! update tests: never GitHub, never the built binary itself.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::bin;

/// A release server: `/latest` redirects to `/tag/<tag>`, and `/download/<tag>/` serves this
/// platform's archive (two scripts that say they are `<tag>`) and its checksum. `/latest`
/// can be pointed at another tag, to publish a release while a daemon runs.
pub struct Releases {
    pub url: String,
    latest: Arc<Mutex<String>>,
}

impl Releases {
    pub fn set_latest(&self, tag: &str) {
        *self.latest.lock().unwrap() = tag.to_string();
    }
}

pub fn asset(tag: &str) -> String {
    let platform = if cfg!(target_os = "macos") { "macos-universal" } else { "linux-x86_64" };
    format!("drmanhattan-{tag}-{platform}.tar.gz")
}

pub fn releases(root: &Path, tag: &str, good_checksum: bool) -> Releases {
    let pack = root.join("pack/drmanhattan");
    fs::create_dir_all(&pack).unwrap();
    let version = tag.trim_start_matches('v');
    for program in ["drmanhattan", "manhattan"] {
        let p = pack.join(program);
        fs::write(&p, format!("#!/bin/sh\necho 'drmanhattan {version}'\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = root.join(asset(tag));
    assert!(std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(root.join("pack"))
        .arg("drmanhattan")
        .status()
        .unwrap()
        .success());
    let sum = if good_checksum {
        let out = std::process::Command::new("shasum").args(["-a", "256"]).arg(&archive).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().to_string()
    } else {
        "0".repeat(64)
    };
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (tag, bytes, name) = (tag.to_string(), fs::read(&archive).unwrap(), asset(tag));
    let base = url.clone();
    let latest = Arc::new(Mutex::new(tag.clone()));
    let served = latest.clone();
    std::thread::spawn(move || {
        while let Ok(req) = server.recv() {
            let path = req.url().to_string();
            let response = if path == "/latest" {
                let now = served.lock().unwrap().clone();
                let to = tiny_http::Header::from_bytes("Location", format!("{base}/tag/{now}")).unwrap();
                tiny_http::Response::from_data(Vec::new()).with_status_code(302).with_header(to)
            } else if path.starts_with("/tag/") {
                tiny_http::Response::from_data(b"release notes".to_vec())
            } else if path == format!("/download/{tag}/{name}") {
                tiny_http::Response::from_data(bytes.clone())
            } else if path == format!("/download/{tag}/{name}.sha256") {
                tiny_http::Response::from_data(format!("{sum}  {name}\n").into_bytes())
            } else {
                tiny_http::Response::from_data(b"not found".to_vec()).with_status_code(404)
            };
            let _ = req.respond(response);
        }
    });
    Releases { url, latest }
}

/// A copy of drmanhattan installed in a directory of its own, as the installer would.
pub fn installed(root: &Path) -> PathBuf {
    let dir = root.join("bin");
    fs::create_dir_all(&dir).unwrap();
    for program in ["drmanhattan", "manhattan"] {
        fs::copy(bin(), dir.join(program)).unwrap();
    }
    dir
}
