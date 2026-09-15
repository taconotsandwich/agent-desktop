use crate::error::fail;
use crate::platform::drivers::WindowInfo;
use crate::types::ToolError;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub desktop_file: PathBuf,
    pub startup_class: String,
    pub executable: String,
}

fn normalized(value: &str) -> String {
    value.trim_end_matches(".desktop").to_ascii_lowercase()
}

impl Application {
    pub fn matches(&self, query: &str) -> bool {
        self.desktop_file.to_string_lossy() == query
            || [&self.id, &self.name, &self.startup_class, &self.executable]
                .iter()
                .any(|value| !value.is_empty() && normalized(value) == normalized(query))
    }

    pub fn owns(&self, window: &WindowInfo) -> bool {
        if !window.app_id.is_empty() {
            return self.matches(&window.app_id);
        }
        window.class.split('.').any(|class| self.matches(class))
            || self.matches(&window.class)
            || window
                .pid
                .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .is_some_and(|name| {
                    name == self.executable
                        && !matches!(name.as_str(), "env" | "sh" | "bash" | "flatpak")
                })
    }

    pub async fn launch(&self) -> Result<(), ToolError> {
        let result = tokio::process::Command::new("gio")
            .arg("launch")
            .arg(&self.desktop_file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .status()
            .await
            .map_err(|error| fail("launch_failed", error.to_string()))?;
        if !result.success() {
            return Err(fail("launch_failed", format!("gio launch exited {result}")));
        }
        Ok(())
    }
}

/// Desktop file ID as the freedesktop spec defines it for display: the path
/// under the applications directory with `/` replaced by `-`. Unlike the
/// crate's `DesktopEntry::id()`, the `.desktop` suffix is preserved for
/// compatibility with the ids this server has always reported.
fn entry_id(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.rsplit_once("/applications/") {
        Some((_, relative)) => relative.replace('/', "-"),
        None => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

pub fn catalog() -> Vec<Application> {
    let locales = freedesktop_desktop_entry::get_languages_from_env();
    let mut seen = BTreeSet::new();
    let mut apps = Vec::new();
    for entry in freedesktop_desktop_entry::desktop_entries(&locales) {
        if entry.type_() != Some("Application") || entry.no_display() || entry.hidden() {
            continue;
        }
        let id = entry_id(&entry.path);
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(name) = entry.name(&locales) else {
            continue;
        };
        let executable = entry
            .parse_exec()
            .ok()
            .and_then(|argv| argv.first().cloned())
            .and_then(|program| {
                Path::new(&program)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        apps.push(Application {
            id,
            name: name.into_owned(),
            desktop_file: entry.path.clone(),
            startup_class: entry.startup_wm_class().unwrap_or("").into(),
            executable,
        });
    }
    apps.sort_by(|a, b| a.id.cmp(&b.id));
    apps
}

pub fn find(query: &str) -> Result<Application, ToolError> {
    let applications = catalog();
    if let Some(app) = applications.iter().find(|app| {
        normalized(&app.id) == normalized(query) || app.desktop_file.to_string_lossy() == query
    }) {
        return Ok(app.clone());
    }
    let mut matches: Vec<_> = applications
        .into_iter()
        .filter(|app| app.matches(query))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(fail(
            "app_not_found",
            format!("No installed application matches {query:?}; use agentdesktop.listApps()"),
        )),
        _ => Err(fail(
            "ambiguous_app",
            format!(
                "Use an exact desktop ID: {:?}",
                matches.iter().map(|app| &app.id).collect::<Vec<_>>()
            ),
        )),
    }
}
