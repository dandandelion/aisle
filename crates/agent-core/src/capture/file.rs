//! File backend — reads a fixed PNG. Test/CI infrastructure, selected with
//! `capture.backend = "file:/path.png"`. Lets the full attach pipeline
//! (capture -> store -> prepare -> last-2 policy) run in headless CI and
//! on desktops where live capture is restricted.

use std::path::PathBuf;

use async_trait::async_trait;

use super::{png_dims, CapturedImage, ScreenCapture};
use crate::error::{Error, Result};

pub struct FileCapture {
    path: PathBuf,
}

impl FileCapture {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn from_spec(spec: &str) -> Option<Self> {
        spec.strip_prefix("file:").map(|p| Self { path: p.into() })
    }
}

#[async_trait]
impl ScreenCapture for FileCapture {
    fn name(&self) -> &'static str {
        "file"
    }

    async fn probe(&self) -> bool {
        self.path.exists()
    }

    async fn capture(&self) -> Result<CapturedImage> {
        let png = std::fs::read(&self.path).map_err(|e| Error::Capture(format!("file backend: {e}")))?;
        let (width, height) = png_dims(&png).ok_or_else(|| Error::Capture("not a PNG".into()))?;
        Ok(CapturedImage { png, width, height })
    }
}
