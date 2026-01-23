#![windows_subsystem = "windows"]

use arboard::Clipboard;
use headless_chrome::{protocol::cdp::Page, protocol::cdp::Emulation, Browser, LaunchOptionsBuilder, Tab};
use msgbox::IconType;
use pulldown_cmark::{html, CowStr, Event, Options as MdOptions, Parser as MdParser};
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use winreg::{enums::*, RegKey};

const WIDTH: u32 = 375;
const MATHJAX_JS: &str = include_str!("../mathjax.min.js");

const CSS: &str = r#"
body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    line-height: 1.6; color: #24292e; background: #fff;
    margin: 0; padding: 16px; 
    text-align: justify; overflow-wrap: break-word;
    -webkit-font-smoothing: antialiased;
}
h1, h2, h3, h4 { 
    border-bottom: 1px solid #eaecef; padding-bottom: .3em; margin-top: 1.2em; font-weight: 600; 
}
pre, code { background-color: #f6f8fa; font-family: "SFMono-Regular", Consolas, monospace; }
pre { padding: 12px; overflow-x: auto; border-radius: 6px; font-size: 0.9em; }
code { padding: .2em .4em; border-radius: 3px; font-size: .9em; }
img { display: block; max-width: 100%; margin: 12px auto; border-radius: 4px; }
blockquote { border-left: 4px solid #dfe2e5; color: #6a737d; padding: 0 1em; margin: 0; }
table { display: block; overflow-x: auto; border-collapse: collapse; margin-bottom: 16px; font-size: 0.9em; }
th, td { border: 1px solid #dfe2e5; padding: 6px 10px; }
mjx-container[display="true"] { overflow-x: visible!important; margin: 1em 0!important; }
::-webkit-scrollbar { display: none; }
"#;


struct TempFile(PathBuf);
impl TempFile {
    fn new(suffix: &str) -> Self {
        let mut path = env::temp_dir();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        path.push(format!("md2png_{}.{}", nanos, suffix));
        Self(path)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

fn main() {
    if let Err(e) = run() {
        msgbox::create("Md2Png 错误", &format!("{}", e), IconType::Error).ok();
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let file_arg = env::args().nth(1).map(PathBuf::from);

    let (content, output_path) = match file_arg {
        Some(path) if path.exists() => {
            let text = fs::read_to_string(&path)?;
            let stem = path.file_stem().unwrap_or(OsStr::new("output")).to_string_lossy();
            let parent = path.parent().unwrap_or(Path::new("."));
            (text, get_unique_path(parent, &stem))
        }
        _ => {
            let mut text = String::new();
            for _ in 0..3 {
                if let Ok(mut cb) = Clipboard::new() {
                    if let Ok(t) = cb.get_text() { text = t; break; }
                }
                thread::sleep(Duration::from_millis(100));
            }
            if text.trim().is_empty() { return Err("剪贴板为空或无法读取".into()); }
            
            let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            (text, get_unique_path(&cwd, "Markdown_Clipboard"))
        }
    };

    let html = generate_html(&content);
    let temp_file = TempFile::new("html");
    fs::write(&temp_file.0, html)?;

    let png = render_screenshot(&temp_file.0)?;
    fs::write(&output_path, png)?;

    // 成功不提示，直接退出
    Ok(())
}

fn get_unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut path = dir.join(format!("{}.png", stem));
    let mut i = 1;
    while path.exists() {
        path = dir.join(format!("{} ({}).png", stem, i));
        i += 1;
    }
    path
}

fn generate_html(md: &str) -> String {
    let mut opts = MdOptions::empty();
    opts.insert(MdOptions::ENABLE_TABLES);
    opts.insert(MdOptions::ENABLE_STRIKETHROUGH);
    opts.insert(MdOptions::ENABLE_MATH);

    let parser = MdParser::new_ext(md, opts).map(|ev| match ev {
        Event::InlineMath(c) => Event::Text(CowStr::from(format!("${}$", c))),
        Event::DisplayMath(c) => Event::Text(CowStr::from(format!("$${}$$", c))),
        _ => ev,
    });

    let mut body = String::new();
    html::push_html(&mut body, parser);

    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <style>{}</style>
        <script>
            function setDone() {{ document.body.setAttribute('data-ready', '1'); }}
            setTimeout(setDone, 4000);
            MathJax = {{
                tex: {{ inlineMath: [['$', '$'], ['\\(', '\\)']], displayMath: [['$$', '$$']] }},
                startup: {{ pageReady: () => MathJax.startup.defaultPageReady().then(setDone).catch(setDone) }}
            }};
        </script>
        <script>{}</script></head><body>{}</body></html>"#,
        CSS, MATHJAX_JS, body
    )
}

fn render_screenshot(html_path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let browser_exe = find_browser().ok_or("未找到 Chrome 或 Edge 浏览器")?;

    let opts = LaunchOptionsBuilder::default()
        .headless(true)
        .window_size(Some((WIDTH, 800)))
        .path(Some(browser_exe))
        .args(vec![
            OsStr::new("--no-sandbox"), OsStr::new("--disable-gpu"), 
            OsStr::new("--hide-scrollbars")
        ])
        .build()?;

    let browser = Browser::new(opts)?;
    let tab = browser.new_tab()?;

    set_viewport(&tab, WIDTH, 800)?;
    tab.navigate_to(&format!("file://{}", html_path.display()))?;
    tab.wait_until_navigated()?;

    let start = Instant::now();
    loop {
        if start.elapsed().as_secs() > 5 { break; }
        if let Ok(res) = tab.evaluate("document.body.getAttribute('data-ready')", false) {
             if res.value.as_ref().and_then(|v| v.as_str()) == Some("1") { break; }
        }
        thread::sleep(Duration::from_millis(50));
    }

    let h_script = "Math.max(document.body.scrollHeight, document.body.offsetHeight, document.documentElement.scrollHeight)";
    let height = tab.evaluate(h_script, false)
        .map(|v| v.value.unwrap().as_f64().unwrap_or(800.0) as u32)
        .unwrap_or(800);

    set_viewport(&tab, WIDTH, height)?;
    
    tab.evaluate("document.fonts.ready.then(() => new Promise(r => requestAnimationFrame(r)));", true)?;

    let png = tab.capture_screenshot(Page::CaptureScreenshotFormatOption::Png, Some(100), None, true)?;
    Ok(png)
}

fn set_viewport(tab: &Tab, width: u32, height: u32) -> Result<(), Box<dyn Error>> {
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width, height, 
        device_scale_factor: 4.0,
        mobile: true,
        scale: None, screen_width: None, screen_height: None, position_x: None, position_y: None,
        dont_set_visible_size: None, screen_orientation: None, viewport: None, device_posture: None, display_feature: None,
    })?;
    Ok(())
}

fn find_browser() -> Option<PathBuf> {
    let roots = [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER];
    let keys = [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe",
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe",
    ];
    for root in roots {
        for key in keys {
            if let Ok(reg) = RegKey::predef(root).open_subkey(key) {
                if let Ok(val) = reg.get_value::<String, _>("") {
                    let p = PathBuf::from(val);
                    if p.exists() { return Some(p); }
                }
            }
        }
    }

    let mut candidates = vec![
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".to_string(),
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe".to_string(),
        r"C:\Program Files\Google\Chrome\Application\chrome.exe".to_string(),
    ];
    
    if let Ok(local_app) = env::var("LOCALAPPDATA") {
        candidates.push(format!(r"{}\Microsoft\Edge\Application\msedge.exe", local_app));
        candidates.push(format!(r"{}\Google\Chrome\Application\chrome.exe", local_app));
    }

    for p in candidates {
        let pb = PathBuf::from(p);
        if pb.exists() { return Some(pb); }
    }
    None
}