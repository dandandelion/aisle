//! Screen capture backends (spec §7). Compositor-native, prompt-free APIs
//! first; portal last. Autodetect order: gnome → kwin → grim → x11 → portal.

pub mod gnome;
pub mod grim;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::CaptureSection;
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[async_trait]
pub trait ScreenCapture: Send + Sync {
    fn name(&self) -> &'static str;
    /// Cheap liveness check (bus name present / binary on PATH).
    async fn probe(&self) -> bool;
    async fn capture(&self) -> Result<CapturedImage>;
}

/// All candidate backends, in preference order.
fn candidates() -> Vec<Arc<dyn ScreenCapture>> {
    vec![
        Arc::new(gnome::GnomeShell),
        Arc::new(grim::Grim),
    ]
}

/// Resolve the configured backend ("auto" probes, others force).
/// Selection is cached by the Agent on first use.
pub async fn resolve(cfg: &CaptureSection) -> Vec<Arc<dyn ScreenCapture>> {
    match cfg.backend.as_str() {
        "none" => return vec![],
        "gnome" => return vec![Arc::new(gnome::GnomeShell)],
        "grim" => return vec![Arc::new(grim::Grim)],
        _ => {}
    }
    let picked: Vec<Arc<dyn ScreenCapture>> =
        futures::future::join_all(candidates().iter().map(|b| async move {
            if b.probe().await { Some(b.clone()) } else { None }
        }))
        .await
        .into_iter()
        .flatten()
        .collect();
    if picked.is_empty() {
        tracing::warn!("no screen-capture backend available; attach_screen will fail");
    } else {
        tracing::info!("capture backend: {}", picked[0].name());
    }
    picked
}

pub fn no_backend_error() -> Error {
    Error::Capture("no working capture backend on this system (see docs/spec.md §7)".into())
}

/// Where live captures land: SIDEAGENT_RUNTIME > XDG_RUNTIME_DIR > temp.
pub fn runtime_base() -> std::path::PathBuf {
    std::env::var("SIDEAGENT_RUNTIME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR").map(std::path::PathBuf::from))
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Width/height from a PNG's IHDR without a full decode.
pub fn png_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((w, h))
}
