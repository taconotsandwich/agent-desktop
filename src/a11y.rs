//! Shared AT-SPI2 helper — ONE implementation, all backends use it.
//!
//! BFS walk → `FlatElement` records with trust tags. Per-node failures never
//! abort the walk. (Ported from kde-mcp `atspi/`, ref store adapted.)

use crate::core::RefStore;
use crate::error::BackendError;
use crate::types::{Ref, Trust, TrustedText};
use atspi::AccessibilityConnection;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::proxy::proxy_ext::ProxyExt;
use base64::Engine as _;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Default)]
pub struct AtspiConnection {
    inner: OnceCell<AccessibilityConnection>,
}

impl AtspiConnection {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self) -> Result<&AccessibilityConnection, BackendError> {
        self.inner
            .get_or_try_init(|| async {
                AccessibilityConnection::new()
                    .await
                    .map_err(|e| BackendError::BusDisconnected {
                        detail: format!("atspi connect: {e}"),
                    })
            })
            .await
    }
}

pub fn encode_element_ref(bus: &str, path: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("{bus}|{path}"))
}

pub fn decode_element_ref(payload: &str) -> Option<(String, String)> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (bus, path) = s.split_once('|')?;
    Some((bus.to_string(), path.to_string()))
}

/// Doctor probe: can we open the a11y bus, and how many apps are registered?
pub async fn status(conn: &AtspiConnection) -> (bool, String) {
    let c = match conn.get().await {
        Ok(c) => c,
        Err(e) => return (false, format!("connect: {}", e.tool(false).message)),
    };
    let root = match c.root_accessible_on_registry().await {
        Ok(r) => r,
        Err(e) => return (false, format!("registry root: {e}")),
    };
    match root.child_count().await {
        Ok(n) => (true, format!("registry ok, {n} applications")),
        Err(e) => (false, format!("child_count: {e}")),
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct FlatElement {
    pub id: Ref,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Ref>,
    pub child_count: u32,
    pub role: TrustedText,
    pub name: TrustedText,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<TrustedText>,
    pub states: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[i32; 4]>,
    pub actions: Vec<String>,
}

pub struct WalkOpts {
    pub max_depth: u32,
    pub max_elements: usize,
    pub role_filter: Option<Vec<String>>,
    pub name_contains: Option<String>,
}

pub async fn walk(
    conn: &AccessibilityConnection,
    root: AccessibleProxy<'_>,
    refs: Arc<RefStore>,
    opts: WalkOpts,
) -> Result<(Vec<FlatElement>, bool, Vec<String>), BackendError> {
    let zbus_conn = conn.connection();
    let mut out: Vec<FlatElement> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut queue: VecDeque<(AccessibleProxy<'_>, u32, Option<Ref>)> = VecDeque::new();
    queue.push_back((root, 0, None));
    let mut truncated = false;

    while let Some((proxy, depth, parent_ref)) = queue.pop_front() {
        if out.len() >= opts.max_elements {
            truncated = true;
            break;
        }
        let bus = proxy.inner().destination().to_string();
        let path = proxy.inner().path().to_string();
        let payload = encode_element_ref(&bus, &path);
        let id = refs.mint_elements(vec![payload]).await.pop().unwrap();

        let role_str = match proxy.get_role().await {
            Ok(r) => r.name().to_string(),
            Err(e) => {
                warnings.push(format!("get_role failed at {bus}{path}: {e}"));
                continue;
            }
        };
        let name_str = proxy.name().await.unwrap_or_default();
        let states_vec: Vec<String> = match proxy.get_state().await {
            Ok(set) => set.iter().map(|s| s.to_static_str().to_string()).collect(),
            Err(_) => Vec::new(),
        };
        let child_count = proxy.child_count().await.unwrap_or(0).max(0) as u32;

        let passes_role = match &opts.role_filter {
            Some(allow) => allow.iter().any(|r| r.eq_ignore_ascii_case(&role_str)),
            None => true,
        };
        let passes_name = match &opts.name_contains {
            Some(sub) => name_str.to_lowercase().contains(&sub.to_lowercase()),
            None => true,
        };
        let keep = passes_role && passes_name;

        let (bbox, actions, value) = if keep {
            element_peripherals(&proxy).await
        } else {
            (None, Vec::new(), None)
        };

        if keep {
            out.push(FlatElement {
                id: id.clone(),
                parent_id: parent_ref.clone(),
                child_count,
                role: TrustedText {
                    trust: Trust::Trusted,
                    text: role_str,
                },
                name: TrustedText {
                    trust: Trust::External,
                    text: name_str,
                },
                value,
                states: states_vec,
                bbox,
                actions,
            });
        }

        if depth + 1 > opts.max_depth {
            continue;
        }
        for i in 0..child_count as i32 {
            let child_ref = match proxy.get_child_at_index(i).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            let child_proxy = match child_ref.into_accessible_proxy(zbus_conn).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            queue.push_back((child_proxy, depth + 1, Some(id.clone())));
        }
    }

    if out.is_empty() && warnings.is_empty() {
        warnings.push(
            "AT-SPI tree empty — the target app may have accessibility disabled; enable its accessibility support or use vision fallback"
                .to_string(),
        );
    }
    Ok((out, truncated, warnings))
}

async fn element_peripherals(
    proxy: &AccessibleProxy<'_>,
) -> (Option<[i32; 4]>, Vec<String>, Option<TrustedText>) {
    let proxies = match proxy.proxies().await {
        Ok(p) => p,
        Err(_) => return (None, Vec::new(), None),
    };
    let bbox = match proxies.component().await {
        Ok(c) => match c.get_extents(atspi::CoordType::Screen).await {
            Ok((x, y, w, h)) => Some([x, y, w, h]),
            Err(_) => None,
        },
        Err(_) => None,
    };
    let actions: Vec<String> = match proxies.action().await {
        Ok(a) => match a.get_actions().await {
            Ok(list) => list.into_iter().map(|act| act.name).collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    let value: Option<TrustedText> = match proxies.text().await {
        Ok(t) => match t.character_count().await {
            Ok(n) if n > 0 => match t.get_text(0, n).await {
                Ok(s) if !s.is_empty() => Some(TrustedText {
                    trust: Trust::External,
                    text: s,
                }),
                _ => None,
            },
            _ => None,
        },
        Err(_) => None,
    };
    (bbox, actions, value)
}
