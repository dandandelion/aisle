//! GNOME Shell screenshot D-Bus API: prompt-free full-screen capture on
//! GNOME Wayland *and* X11 sessions (spec §7).

use std::path::PathBuf;

use async_trait::async_trait;
use zbus::Connection;

use super::{runtime_base, CapturedImage, ScreenCapture};
use crate::error::{Error, Result};

pub struct GnomeShell;

#[zbus::proxy(
    interface = "org.gnome.Shell.Screenshot",
    default_service = "org.gnome.Shell.Screenshot",
    default_path = "/org/gnome/Shell/Screenshot"
)]
trait GnomeScreenshot {
    fn screenshot(
        &self,
        width: i32,
        height: i32,
        include_cursor: bool,
        filename: &str,
    ) -> zbus::Result<bool>;
}

fn runtime_dir() -> PathBuf {
    runtime_base().join("sideagent")
}

#[async_trait]
impl ScreenCapture for GnomeShell {
    fn name(&self) -> &'static str {
        "gnome-shell"
    }

    async fn probe(&self) -> bool {
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

        // 0,0 = current output, full size (gnome-shell convention)
        let ok = proxy
            .screenshot(0, 0, false, file.to_str().unwrap_or_default())
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

