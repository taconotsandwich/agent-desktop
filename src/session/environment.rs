use super::io;
use crate::error::BackendError;
use std::collections::BTreeMap;

const SESSION_KEYS: &[&str] = &[
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "DBUS_SYSTEM_BUS_ADDRESS",
    "AT_SPI_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "QT_LINUX_ACCESSIBILITY_ALWAYS_ON",
    "GTK_A11Y",
];

pub fn read_environment(path: &str) -> Result<BTreeMap<String, String>, BackendError> {
    let text = std::fs::read_to_string(path).map_err(io)?;
    let mut env = BTreeMap::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| BackendError::Failed("Malformed session environment".into()))?;
        if !SESSION_KEYS.contains(&key) {
            return Err(BackendError::Failed(format!(
                "Unexpected session variable {key}"
            )));
        }
        env.insert(key.into(), value.into());
    }
    Ok(env)
}

/// Called only in single-threaded main, before the serving runtime exists.
pub fn join_seat(path: &str) -> Result<(), BackendError> {
    let env = read_environment(path)?;
    if !env.contains_key("DBUS_SESSION_BUS_ADDRESS") || !env.contains_key("XDG_RUNTIME_DIR") {
        return Err(BackendError::Failed(
            "Session requires DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR".into(),
        ));
    }
    unsafe {
        for key in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "AT_SPI_BUS_ADDRESS",
            "XAUTHORITY",
            "SESSION_MANAGER",
        ] {
            std::env::remove_var(key);
        }
        for (key, value) in env {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}
