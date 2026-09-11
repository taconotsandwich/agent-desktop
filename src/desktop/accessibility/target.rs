use super::{AtspiConnection, FlatElement, WalkOpts};
use crate::{
    error::fail,
    platform::drivers::WindowInfo,
    types::{Ref, ToolError},
};
use atspi::{
    AccessibilityConnection,
    proxy::accessible::{AccessibleProxy, ObjectRefExt},
    proxy::proxy_ext::ProxyExt,
};

pub async fn proxy<'a>(
    conn: &'a AccessibilityConnection,
    reference: &Ref,
) -> Result<AccessibleProxy<'a>, ToolError> {
    let (bus, path) = super::decode_element_ref(&reference.0)
        .ok_or_else(|| fail("stale_element", "Invalid accessibility reference"))?;
    AccessibleProxy::builder(conn.connection())
        .destination(bus)
        .map_err(bus_error)?
        .path(path)
        .map_err(bus_error)?
        .cache_properties(atspi::zbus::proxy::CacheProperties::No)
        .build()
        .await
        .map_err(bus_error)
}

fn bus_error(error: impl std::fmt::Display) -> ToolError {
    fail("accessibility_unavailable", error.to_string())
}

pub async fn elements(
    atspi: &AtspiConnection,
    window: &WindowInfo,
) -> Result<(Vec<FlatElement>, bool, Vec<String>), ToolError> {
    let conn = atspi.get().await.map_err(|error| error.tool(false))?;
    let pid = window.pid.ok_or_else(|| {
        fail(
            "accessibility_unavailable",
            "Window has no process identity",
        )
    })?;
    let registry = conn
        .root_accessible_on_registry()
        .await
        .map_err(bus_error)?;
    let dbus = zbus::fdo::DBusProxy::new(conn.connection())
        .await
        .map_err(bus_error)?;
    for child in registry.get_children().await.map_err(bus_error)? {
        let app = child
            .into_accessible_proxy(conn.connection())
            .await
            .map_err(bus_error)?;
        let owner = dbus
            .get_connection_unix_process_id(app.inner().destination().clone())
            .await;
        if owner.ok() != Some(pid) {
            continue;
        }
        let children = app.get_children().await.map_err(bus_error)?;
        let mut candidates = Vec::new();
        for child in children {
            let frame = child
                .into_accessible_proxy(conn.connection())
                .await
                .map_err(bus_error)?;
            let role = frame.get_role().await.map_err(bus_error)?;
            if !matches!(
                role,
                atspi::Role::Frame | atspi::Role::Dialog | atspi::Role::Window
            ) {
                continue;
            }
            if frame
                .get_state()
                .await
                .map_err(bus_error)?
                .contains(atspi::State::Defunct)
            {
                continue;
            }
            let name = frame.name().await.unwrap_or_default();
            let mut score = if name == window.title { 4 } else { 0 };
            if let Ok(proxies) = frame.proxies().await
                && let Ok(component) = proxies.component().await
                && let Ok((x, y, w, h)) = component.get_extents(atspi::CoordType::Screen).await
            {
                let geo = &window.geometry;
                if (x - geo.x).abs() <= 32
                    && (y - geo.y).abs() <= 64
                    && (i64::from(w) - i64::from(geo.w)).abs() <= 64
                    && (i64::from(h) - i64::from(geo.h)).abs() <= 96
                {
                    score += 2;
                }
            }
            candidates.push((score, frame));
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        if candidates.is_empty() {
            return Err(fail(
                "accessibility_unavailable",
                "Application exposes no window roots",
            ));
        }
        if candidates.len() > 1 && (candidates[0].0 == 0 || candidates[0].0 == candidates[1].0) {
            return Err(fail(
                "ambiguous_accessibility_root",
                "Cannot associate this window with one accessibility root",
            ));
        }
        let root = candidates.remove(0).1;
        return super::walk(
            &conn,
            root,
            WalkOpts {
                max_depth: 20,
                max_elements: 800,
                role_filter: None,
                name_contains: None,
            },
        )
        .await
        .map_err(|error| error.tool(false));
    }
    Err(fail(
        "accessibility_unavailable",
        format!("No AT-SPI application registered for pid {pid}"),
    ))
}

pub async fn action(
    atspi: &AtspiConnection,
    element: &FlatElement,
    requested: Option<&str>,
) -> Result<(), ToolError> {
    let conn = atspi.get().await.map_err(|error| error.tool(false))?;
    let proxy = proxy(&conn, &element.id).await?;
    validate(&proxy, element).await?;
    let proxies = proxy.proxies().await.map_err(bus_error)?;
    let actions = proxies
        .action()
        .await
        .map_err(|_| fail("unsupported", "Element has no Action interface"))?;
    let available = actions.get_actions().await.map_err(bus_error)?;
    let index = match requested {
        Some(name) => available
            .iter()
            .position(|action| action.name.eq_ignore_ascii_case(name)),
        None => available.iter().position(|action| {
            ["click", "press", "activate"]
                .iter()
                .any(|name| action.name.eq_ignore_ascii_case(name))
        }),
    }
    .ok_or_else(|| {
        fail(
            "unsupported",
            "Requested action is not exposed by this element",
        )
    })?;
    let acknowledged = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        actions.do_action(index as i32),
    )
    .await
    .map_err(|_| {
        fail(
            "action_pending",
            "Application has not acknowledged the action; observe its state before acting again",
        )
    })?
    .map_err(bus_error)?;
    if !acknowledged {
        return Err(fail(
            "action_failed",
            "Application rejected the accessibility action",
        ));
    }
    Ok(())
}

pub async fn validate(proxy: &AccessibleProxy<'_>, element: &FlatElement) -> Result<(), ToolError> {
    if proxy.inner().connection().server_guid().as_str() != element.bus_guid {
        return Err(fail(
            "stale_element",
            "Accessibility bus restarted; observe the target again",
        ));
    }
    if proxy.name().await.map_err(bus_error)? != element.name.text
        || proxy.get_role().await.map_err(bus_error)?.name() != element.role.text
    {
        return Err(fail(
            "stale_element",
            "Element changed; get fresh accessibility state",
        ));
    }
    let states = proxy.get_state().await.map_err(bus_error)?;
    if states.contains(atspi::State::Defunct) {
        return Err(fail(
            "stale_element",
            "Element is defunct; get fresh accessibility state",
        ));
    }
    Ok(())
}

pub async fn set_value(
    atspi: &AtspiConnection,
    element: &FlatElement,
    value: &str,
) -> Result<(), ToolError> {
    let conn = atspi.get().await.map_err(|error| error.tool(false))?;
    let proxy = proxy(&conn, &element.id).await?;
    validate(&proxy, element).await?;
    let proxies = proxy.proxies().await.map_err(bus_error)?;
    if let Ok(editable) = proxies.editable_text().await {
        if editable.set_text_contents(value).await.map_err(bus_error)? {
            return Ok(());
        }
        return Err(fail("action_failed", "Application rejected text editing"));
    }
    if let Ok(numeric) = proxies.value().await {
        let number = value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                fail(
                    "invalid_argument",
                    "Numeric control requires a finite number",
                )
            })?;
        numeric.set_current_value(number).await.map_err(bus_error)?;
        return Ok(());
    }
    Err(fail(
        "unsupported",
        "Element exposes neither EditableText nor Value",
    ))
}

pub async fn select_text(
    atspi: &AtspiConnection,
    element: &FlatElement,
    text: &str,
    prefix: &str,
    suffix: &str,
    selection_type: &str,
) -> Result<(), ToolError> {
    let conn = atspi.get().await.map_err(|error| error.tool(false))?;
    let proxy = proxy(&conn, &element.id).await?;
    validate(&proxy, element).await?;
    let proxies = proxy.proxies().await.map_err(bus_error)?;
    let text_proxy = proxies
        .text()
        .await
        .map_err(|_| fail("unsupported", "Element has no Text interface"))?;
    let count = text_proxy.character_count().await.map_err(bus_error)?;
    if count > 100_000 {
        return Err(fail(
            "unsupported",
            "Text selection exceeds the observation limit",
        ));
    }
    let contents = text_proxy.get_text(0, count).await.map_err(bus_error)?;
    let matches: Vec<_> = contents
        .match_indices(text)
        .filter(|(start, _)| {
            contents[..*start].ends_with(prefix)
                && contents[*start + text.len()..].starts_with(suffix)
        })
        .collect();
    if matches.len() != 1 || text.is_empty() {
        return Err(fail(
            "ambiguous_text",
            "Text must identify exactly one occurrence; supply prefix/suffix",
        ));
    }
    let start = contents[..matches[0].0].chars().count() as i32;
    let end = start + text.chars().count() as i32;
    let ok = match selection_type {
        "cursor_before" => text_proxy
            .set_caret_offset(start)
            .await
            .map_err(bus_error)?,
        "cursor_after" => text_proxy.set_caret_offset(end).await.map_err(bus_error)?,
        "text" => {
            while text_proxy.get_n_selections().await.map_err(bus_error)? > 0 {
                if !text_proxy.remove_selection(0).await.map_err(bus_error)? {
                    break;
                }
            }
            text_proxy
                .add_selection(start, end)
                .await
                .map_err(bus_error)?
        }
        _ => return Err(fail("invalid_argument", "Unknown selectionType")),
    };
    if ok {
        Ok(())
    } else {
        Err(fail("action_failed", "Application rejected text selection"))
    }
}
