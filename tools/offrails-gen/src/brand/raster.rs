//! SVG → PNG for the favicon set and the social card, with what the machine already has: an
//! installed Chrome, Chromium or Edge (headless `--screenshot`, the browser smokes' discovery), else
//! `rsvg-convert` (librsvg). No crate, no download. Neither: the PNGs are left as they are and the
//! caller says so (the SVGs are always current).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub enum Renderer {
    Chrome(PathBuf),
    Rsvg,
}

impl Renderer {
    pub fn name(&self) -> String {
        match self {
            Renderer::Chrome(p) => format!("headless {}", p.display()),
            Renderer::Rsvg => "rsvg-convert".to_owned(),
        }
    }
}

/// `CHROME_PATH` / `CHROME`, then the usual install places on macOS, Windows and Linux
/// (tools/browser-smoke/cdp.mjs looks in the same ones), then `rsvg-convert` on PATH.
/// `OFFRAILS_BRAND_RENDERER=chrome|rsvg|none` forces one.
pub fn find() -> Option<Renderer> {
    let forced = std::env::var("OFFRAILS_BRAND_RENDERER").unwrap_or_default();
    if forced == "none" {
        return None;
    }
    if forced != "rsvg"
        && let Some(c) = find_chrome()
    {
        return Some(Renderer::Chrome(c));
    }
    if forced != "chrome" && on_path("rsvg-convert") {
        return Some(Renderer::Rsvg);
    }
    None
}

pub fn find_chrome() -> Option<PathBuf> {
    for var in ["CHROME_PATH", "CHROME"] {
        if let Ok(p) = std::env::var(var)
            && !p.is_empty()
        {
            let p = PathBuf::from(p);
            return p.is_file().then_some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Applications"));
        }
        for root in roots {
            for app in [
                "Google Chrome.app/Contents/MacOS/Google Chrome",
                "Chromium.app/Contents/MacOS/Chromium",
                "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ] {
                candidates.push(root.join(app));
            }
        }
    } else if cfg!(windows) {
        for var in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(var) {
                let root = PathBuf::from(root);
                candidates.push(
                    root.join("Google")
                        .join("Chrome")
                        .join("Application")
                        .join("chrome.exe"),
                );
                candidates.push(root.join("Chromium").join("Application").join("chrome.exe"));
                candidates.push(
                    root.join("Microsoft")
                        .join("Edge")
                        .join("Application")
                        .join("msedge.exe"),
                );
            }
        }
    } else {
        let names = [
            "google-chrome-stable",
            "google-chrome",
            "chromium",
            "chromium-browser",
            "chrome",
            "microsoft-edge",
        ];
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                for n in names {
                    candidates.push(dir.join(n));
                }
            }
        }
        candidates.push(PathBuf::from("/opt/google/chrome/chrome"));
        candidates.push(PathBuf::from("/snap/bin/chromium"));
    }
    candidates.into_iter().find(|c| c.is_file())
}

fn on_path(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `file:///…` for a local path (forward slashes, so Windows paths work too).
pub fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("//?/");
    let mut enc = String::new();
    for c in s.chars() {
        match c {
            ' ' => enc.push_str("%20"),
            '#' => enc.push_str("%23"),
            '%' => enc.push_str("%25"),
            '?' => enc.push_str("%3F"),
            c => enc.push(c),
        }
    }
    if enc.starts_with('/') {
        format!("file://{enc}")
    } else {
        format!("file:///{enc}")
    }
}

/// Renders `svg` (a file) to a `w`×`h` PNG at `out`. `work` is a scratch directory.
pub fn render(
    r: &Renderer,
    svg: &Path,
    w: u32,
    h: u32,
    out: &Path,
    work: &Path,
) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    match r {
        Renderer::Chrome(chrome) => {
            // An HTML page showing the SVG at exactly w×h on a transparent background.
            let page = work.join(format!("shot-{w}x{h}.html"));
            std::fs::write(
                &page,
                format!(
                    "<!doctype html><html><head><style>html,body{{margin:0;background:transparent;overflow:hidden}}img{{display:block;width:{w}px;height:{h}px}}</style></head><body><img src=\"{}\"></body></html>",
                    file_url(svg)
                ),
            )
            .map_err(|e| e.to_string())?;
            let profile = work.join(format!("profile-{w}x{h}"));
            let mut cmd = Command::new(chrome);
            cmd.args([
                "--headless=new",
                "--disable-gpu",
                "--hide-scrollbars",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--force-device-scale-factor=1",
                "--default-background-color=00000000",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--window-size={w},{h}"))
            .arg(format!("--screenshot={}", out.display()));
            if cfg!(target_os = "linux") && (std::env::var_os("CI").is_some() || is_root()) {
                cmd.arg("--no-sandbox");
            }
            cmd.arg(file_url(&page));
            run_with_timeout(cmd, Duration::from_secs(60))?;
        }
        Renderer::Rsvg => {
            let mut cmd = Command::new("rsvg-convert");
            cmd.args(["-w", &w.to_string(), "-h", &h.to_string(), "-o"])
                .arg(out)
                .arg(svg);
            run_with_timeout(cmd, Duration::from_secs(60))?;
        }
    }
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(format!("{} wrote no {}", r.name(), out.display())),
    }
}

fn is_root() -> bool {
    std::env::var("USER").is_ok_and(|u| u == "root")
}

fn run_with_timeout(mut cmd: Command, limit: Duration) -> Result<(), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start {:?}: {e}", cmd.get_program()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("{:?} exited with {status}", cmd.get_program()));
            }
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{:?} took more than {}s",
                    cmd.get_program(),
                    limit.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// One PNG in an ICO container (valid since Windows Vista; every current browser reads it).
pub fn ico(png: &[u8], size: u8) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0, 1, 0];
    // width, height (0 means 256), colors, reserved, planes (1), bits per pixel (32)
    out.extend_from_slice(&[size, size, 0, 0, 1, 0, 32, 0]);
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(png);
    out
}

/// The `(width, height)` in a PNG's IHDR, or `None` if it isn't a PNG.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Some((be(16), be(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ico_wraps_one_png() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x30\0\0\0\x30rest";
        let ico = ico(png, 48);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(ico[6], 48);
        assert_eq!(
            u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize,
            png.len()
        );
        assert_eq!(u32::from_le_bytes(ico[18..22].try_into().unwrap()), 22);
        assert_eq!(&ico[22..], png);
        assert_eq!(png_size(png), Some((48, 48)));
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn file_urls_work_on_every_os() {
        assert_eq!(file_url(Path::new("/tmp/a b.svg")), "file:///tmp/a%20b.svg");
        assert_eq!(
            file_url(Path::new(r"C:\app\static\favicon.svg")),
            "file:///C:/app/static/favicon.svg"
        );
    }
}
