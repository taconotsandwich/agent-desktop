use crate::error::fail;
use crate::platform::drivers::WindowInfo;
use crate::types::ToolError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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

pub fn catalog() -> Vec<Application> {
    let mut roots = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }
    roots.extend(std::env::split_paths(
        &std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
    ));
    let mut seen = BTreeSet::new();
    let mut apps = Vec::new();
    for root in roots {
        let root = root.join("applications");
        scan(&root, &root, &mut seen, &mut apps, 0);
    }
    apps.sort_by(|a, b| a.id.cmp(&b.id));
    apps
}

fn scan(
    root: &Path,
    dir: &Path,
    seen: &mut BTreeSet<String>,
    apps: &mut Vec<Application>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            scan(root, &path, seen, apps, depth + 1);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "desktop") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let id = relative.to_string_lossy().replace('/', "-");
        if !seen.insert(id.clone()) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut fields = BTreeMap::new();
        let mut active = false;
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                active = line == "[Desktop Entry]";
            }
            if active && let Some((key, value)) = line.split_once('=') {
                fields.insert(key, value);
            }
        }
        if fields.get("Type") != Some(&"Application")
            || fields.get("Hidden") == Some(&"true")
            || fields.get("NoDisplay") == Some(&"true")
        {
            continue;
        }
        let Some(name) = fields.get("Name") else {
            continue;
        };
        let executable = fields
            .get("Exec")
            .and_then(|exec| exec.split_whitespace().next())
            .and_then(|program| Path::new(program.trim_matches('"')).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        apps.push(Application {
            id,
            name: (*name).into(),
            desktop_file: path,
            startup_class: fields.get("StartupWMClass").copied().unwrap_or("").into(),
            executable,
        });
    }
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
