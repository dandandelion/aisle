//! GNOME Shell screenshot D-Bus API: prompt-free full-screen capture on
//! GNOME Wayland *and* X11 sessions (spec §7).

use std::path::PathBuf;

use async_trait::async_trait;
use zbus::Connection;

use super::{runtime_base, CapturedImage, ScreenCapture};
use crate::error::{Error, Result};

pub struct GnomeShell;

/// `gnome-shell --version` -> "GNOME Shell 50.6" -> 50; unknown -> 0 (probe
/// the API anyway).
fn shell_version_under(major_max: u32) -> bool {
    let out = std::process::Command::new("gnome-shell").arg("--version").output();
    let ver = match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            text.split_whitespace().last()
                .and_then(|v| v.split('.').next())
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0)
        }
        Err(_) => 0,
    };
    ver <= major_max
}

#[zbus::proxy(
    interface = "org.gnome.Shell.Screenshot",
    default_service = "org.gnome.Shell.Screenshot",
    default_path = "/org/gnome/Shell/Screenshot"
)]
trait GnomeScreenshot {
    // introspected signature: (include_cursor, flash, filename) -> (success, filename_used)
    fn screenshot(
        &self,
        include_cursor: bool,
        flash: bool,
        filename: &str,
    ) -> zbus::Result<(bool, String)>;
}

fn runtime_dir() -> PathBuf {
    runtime_base().join("aisle")
}

#[async_trait]
impl ScreenCapture for GnomeShell {
    fn name(&self) -> &'static str {
        "gnome-shell"
    }

    async fn probe(&self) -> bool {
        // GNOME >= 47 whitelists callers of the screenshot API (verified on
        // 50.6: third-party senders get AccessDenied). On those, the Tier A
        // extension is the prompt-free path; skip the D-Bus backend.
        if !shell_version_under(47) {
            tracing::debug!("gnome-shell >= 47: screenshot API locked; skipping D-Bus backend");
            return false;
        }
        // Proxy::new performs introspection on the bus name: fails fast if
        // gnome-shell's screenshot interface is absent (non-GNOME desktops).
        match Connection::session().await {
            Ok(conn) => GnomeScreenshotProxy::new(&conn).await.is_ok(),
            Err(_) => false,
        }
    }

    async fn capture(&self) -> Result<CapturedImage> {
        let conn = Connection::session()
            .await
            .map_err(|e| Error::Capture(format!("session bus: {e}")))?;
        let proxy = GnomeScreenshotProxy::new(&conn)
            .await
            .map_err(|e| Error::Capture(format!("org.gnome.Shell.Screenshot: {e}")))?;

        let dir = runtime_dir();
        std::fs::create_dir_all(&dir).map_err(|e| Error::Capture(e.to_string()))?;
        let file = dir.join(format!("cap-{}.png", uuid::Uuid::new_v4()));

        let (ok, _uri) = proxy
            .screenshot(false, false, file.to_str().unwrap_or_default())
            .await
            .map_err(|e| Error::Capture(format!("screenshot call: {e}")))?;
        if !ok {
            return Err(Error::Capture("gnome-shell refused the capture".into()));
        }

        let png = tokio::fs::read(&file)
            .await
            .map_err(|e| Error::Capture(format!("read capture: {e}")))?;
        let _ = tokio::fs::remove_file(&file).await;

        let (width, height) =
            super::png_dims(&png).ok_or_else(|| Error::Capture("bad PNG from gnome-shell".into()))?;
        tracing::info!(width, height, "captured via gnome-shell");
        Ok(CapturedImage { png, width, height })
    }
}

