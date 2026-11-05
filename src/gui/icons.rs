//! App icons: the bundle's .icns is converted once to a 64 px PNG with `sips` (cached in
//! ~/Library/Caches/PulseBar/icons) on a worker thread, then uploaded as an egui texture. Linux
//! apps are desktop entries: their PNG icon is read directly, scaled down if large.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;

type Loaded = (String, Option<(Vec<u8>, u32, u32)>);

pub struct Icons {
    tex: HashMap<String, Option<egui::TextureHandle>>,
    req: Sender<String>,
    rx: Receiver<Loaded>,
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("PulseBar/icons")
}

fn icns_path(bundle: &str) -> Option<PathBuf> {
    let info = plist::Value::from_file(Path::new(bundle).join("Contents/Info.plist")).ok()?;
    let d = info.as_dictionary()?;
    let name = d.get("CFBundleIconFile").and_then(|v| v.as_string()).unwrap_or("AppIcon");
    let mut p = Path::new(bundle).join("Contents/Resources").join(name);
    if p.extension().is_none() {
        p.set_extension("icns");
    }
    p.exists().then_some(p)
}

/// The icon of a desktop entry, at most 64 px like the macOS ones.
fn load_desktop(desktop: &str) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(crate::linux::desktop_icon_png(desktop)?).ok()?;
    let img = if img.width() > 64 || img.height() > 64 { img.thumbnail(64, 64) } else { img };
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

fn load(bundle: &str) -> Option<(Vec<u8>, u32, u32)> {
    if bundle.ends_with(".desktop") {
        return load_desktop(bundle);
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let hash = bundle.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
    let png = dir.join(format!("{hash:016x}.png"));
    if !png.exists() {
        let icns = icns_path(bundle)?;
        let ok = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png", "-Z", "64"])
            .arg(&icns)
            .arg("--out")
            .arg(&png)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
    }
    let img = image::open(&png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

impl Icons {
    pub fn new(ctx: egui::Context) -> Self {
        let (req, req_rx) = channel::<String>();
        let (tx, rx) = channel::<Loaded>();
        std::thread::Builder::new()
            .name("pulsebar-icons".into())
            .spawn(move || {
                for bundle in req_rx {
                    let r = load(&bundle);
                    if tx.send((bundle, r)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .ok();
        Self { tex: HashMap::new(), req, rx }
    }

    pub fn get(&mut self, ctx: &egui::Context, bundle: Option<&str>) -> Option<egui::TextureId> {
        while let Ok((b, img)) = self.rx.try_recv() {
            let t = img.map(|(rgba, w, h)| {
                ctx.load_texture(&b, egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba), egui::TextureOptions::LINEAR)
            });
            self.tex.insert(b, t);
        }
        let b = bundle?;
        match self.tex.get(b) {
            Some(t) => t.as_ref().map(|t| t.id()),
            None => {
                self.tex.insert(b.to_string(), None);
                let _ = self.req.send(b.to_string());
                None
            }
        }
    }
}
