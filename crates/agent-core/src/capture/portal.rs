//! xdg-desktop-portal Screenshot backend (spec §7, M3 piece): desktop-agnostic
//! capture. The portal may show a consent dialog per request (no permission
//! store for screenshots) — acceptable fallback when no prompt-free native
//! API exists.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedValue, Type};

use super::{CapturedImage, ScreenCapture, png_dims};
use crate::error::{Error, Result};

pub struct Portal;

/// Serialize as `a{sv}` for the Screenshot options dict (empty for now, but
/// typed so the signature resolves).
#[derive(Debug, Default, serde::Serialize, Type)]
struct NoOptions(HashMap<&'static str, OwnedValue>);

#[async_trait]
impl ScreenCapture for Portal {
    fn name(&self) -> &'static str {
        "xdg-portal"
    }

    async fn probe(&self) -> bool {
        let Ok(conn) = Connection::session().await else { return false };
        let Ok(dbus) = zbus::fdo::DBusProxy::builder(&conn).build().await else { return false };
        match zbus::names::BusName::from_static_str("org.freedesktop.portal.Desktop") {
            Ok(name) => dbus.name_has_owner(name).await.unwrap_or(false),
            Err(_) => false,
        }
    }

    async fn capture(&self) -> Result<CapturedImage> {
        let conn = zbus::Connection::session().await
            .map_err(|e| Error::Capture(format!("session bus: {e}")))?;

        // org.freedesktop.portal.Screenshot.Screenshot(parent_window s, options a{sv}) -> o
        let reply = conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Screenshot"),
            "Screenshot",
            &("", NoOptions::default()),
        ).await.map_err(|e| Error::Capture(format!("portal screenshot call: {e}")))?;
        let req = reply.body().deserialize::<ObjectPath<'_>>()
            .map_err(|e| Error::Capture(format!("portal reply: {e}")))?
            .to_owned();

        // org.freedesktop.portal.Request.Response on the request path:
        // (response_code u, options a{sv}) with "uri" on success.
        let rp = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            req.clone(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(|e| Error::Capture(e.to_string()))?;
        let mut stream = rp.receive_signal("Response").await
            .map_err(|e| Error::Capture(e.to_string()))?;

        let item = tokio::time::timeout(Duration::from_secs(45), stream.next())
            .await
            .map_err(|_| Error::Capture("portal consent dialog timed out (45s)".into()))?
            .ok_or_else(|| Error::Capture("portal Response signal lost".into()))?;
        let body = item.body();
        let (code, opts): (u32, HashMap<String, OwnedValue>) = body
            .deserialize()
            .map_err(|e| Error::Capture(format!("portal response: {e}")))?;

        drop(stream);
        drop(rp);

        if code != 0 {
            return Err(Error::Capture(format!(
                "portal screenshot denied/cancelled (code {code})")));
        }
        let uri: String = opts.get("uri")
            .cloned()
            .and_then(|v| <String as std::convert::TryFrom<OwnedValue>>::try_from(v).ok())
            .ok_or_else(|| Error::Capture("portal response has no uri".into()))?;
        let path = uri
            .strip_prefix("file://")
            .ok_or_else(|| Error::Capture(format!("unexpected portal uri {uri}")))?;
        let png = tokio::fs::read(path).await
            .map_err(|e| Error::Capture(format!("read portal file {path}: {e}")))?;
        let _ = tokio::fs::remove_file(path).await;
        let (width, height) = png_dims(&png)
            .ok_or_else(|| Error::Capture("portal capture is not a PNG".into()))?;
        Ok(CapturedImage { png, width, height })
    }
}
