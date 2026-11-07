//! X11 capture backend. v1 spawns the `maim` CLI (installed widely, zero
//! extra deps); pure-rust xcb/composite capture is backlog. Works on any
//! X11 session and under XWayland (where it only sees X clients — the
//! autodetector prefers this only when DISPLAY is set and nothing better
//! answered the probe).

use std::path::PathBuf;

use async_trait::async_trait;

use super::{runtime_base, CapturedImage, ScreenCapture};
use crate::error::{Error, Result};

pub struct Maim;

fn have(cmd: &str) -> bool {
    std::env::var("PATH").map(|p| {
        p.split(':').any(|d| std::path::Path::new(d).join(cmd).exists())
    }).unwrap_or(false)
}

#[async_trait]
impl ScreenCapture for Maim {
    fn name(&self) -> &'static str {
        "x11-maim"
    }

    async fn probe(&self) -> bool {
        // GNOME Wayland still exports DISPLAY=:0 (for XWayland apps); maim
        // there captures only X clients (black desktop). Claim x11 sessions
        // only: XDG_SESSION_TYPE must say x11, or be unset with no
        // WAYLAND_DISPLAY around.
        if !(have("maim") || have("import")) {
            return false;
        }
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            return false;
        }
        match std::env::var("XDG_SESSION_TYPE").as_deref() {
            Ok(s) => s == "x11",
            Err(_) => std::env::var("DISPLAY").is_ok(),
        }
    }

    async fn capture(&self) -> Result<CapturedImage> {
        let base = runtime_base().join("sideagent");
        std::fs::create_dir_all(&base).map_err(|e| Error::Capture(e.to_string()))?;
        let file: PathBuf = base.join(format!("cap-{}.png", uuid::Uuid::new_v4()));

        let status = if have("maim") {
            tokio::process::Command::new("maim").arg(&file).kill_on_drop(true).status().await
        } else {
            // ImageMagick fallback: root window capture
            tokio::process::Command::new("import")
                .args(["-window", "root", "-silent"])
                .arg(&file)
                .kill_on_drop(true)
                .status()
                .await
        }
        .map_err(|e| Error::Capture(format!("x11 capture: {e}")))?;

        if !status.success() {
            return Err(Error::Capture(format!("x11 capture exited {status}")));
        }
        let png = tokio::fs::read(&file)
            .await
            .map_err(|e| Error::Capture(format!("read capture: {e}")))?;
        let _ = tokio::fs::remove_file(&file).await;
        let (width, height) =
            super::png_dims(&png).ok_or_else(|| Error::Capture("bad PNG from x11".into()))?;
        Ok(CapturedImage { png, width, height })
    }
}
