//! grim backend: spawn `grim` (wlr-screencopy client) writing a PNG to a
//! temp file. Standard on wlroots compositors (Sway, Hyprland, Wayfire).

use std::path::PathBuf;

use async_trait::async_trait;

use super::{runtime_base, CapturedImage, ScreenCapture};
use crate::error::{Error, Result};

pub struct Grim;

fn grim_path() -> Option<PathBuf> {
    std::path::Path::new("/usr/bin/grim")
        .exists()
        .then(|| "/usr/bin/grim".into())
        .or_else(|| {
            std::env::var("PATH").ok().and_then(|p| {
                p.split(':').map(|d| PathBuf::from(d).join("grim")).find(|p| p.exists())
            })
        })
}

#[async_trait]
impl ScreenCapture for Grim {
    fn name(&self) -> &'static str {
        "grim"
    }

    async fn probe(&self) -> bool {
        grim_path().is_some()
    }

    async fn capture(&self) -> Result<CapturedImage> {
        let grim = grim_path().ok_or_else(|| Error::Capture("grim not installed".into()))?;
        let dir = runtime_base().join("sideagent");
        std::fs::create_dir_all(&dir).map_err(|e| Error::Capture(e.to_string()))?;
        let file = dir.join(format!("cap-{}.png", uuid::Uuid::new_v4()));

        let status = tokio::process::Command::new(grim)
            .arg(&file)
            .kill_on_drop(true)
            .status()
            .await
            .map_err(|e| Error::Capture(format!("grim: {e}")))?;
        if !status.success() {
            return Err(Error::Capture(format!("grim exited {status}")));
        }

        let png = tokio::fs::read(&file)
            .await
            .map_err(|e| Error::Capture(format!("read capture: {e}")))?;
        let _ = tokio::fs::remove_file(&file).await;

        let (width, height) =
            super::png_dims(&png).ok_or_else(|| Error::Capture("bad PNG from grim".into()))?;
        Ok(CapturedImage { png, width, height })
    }
}
