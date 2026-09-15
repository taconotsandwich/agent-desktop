use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    env, fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EXTENSION: &str = "agent-desktop@local";
const EXTENSION_JS: &str = include_str!("../../packaging/gnome/agent-desktop@local/extension.js");
const EXTENSION_METADATA: &str =
    include_str!("../../packaging/gnome/agent-desktop@local/metadata.json");

struct Paths {
    data: PathBuf,
    bin: PathBuf,
}

impl Paths {
    fn discover(bin: Option<PathBuf>) -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required")?;
        ensure!(home.is_absolute(), "HOME must be absolute");
        let data = env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        ensure!(data.is_absolute(), "XDG_DATA_HOME must be absolute");
        let bin = bin.unwrap_or_else(|| home.join(".local/bin"));
        ensure!(bin.is_absolute(), "--bin-dir must be absolute");
        Ok(Self { data, bin })
    }

    fn executable(&self) -> PathBuf {
        self.data
            .join("agent-desktop/versions")
            .join(VERSION)
            .join("agent-desktop")
    }

    fn extension(&self) -> PathBuf {
        self.data.join("gnome-shell/extensions").join(EXTENSION)
    }
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("File needs a parent directory")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn install(paths: &Paths, source: &Path) -> Result<PathBuf> {
    let destination = paths.executable();
    let bytes = fs::read(source).context("Read the packaged executable")?;
    if fs::read(&destination).ok().as_deref() != Some(&bytes) {
        write_file(&destination, &bytes, 0o755)?;
    }
    let executable = fs::canonicalize(&destination)?;
    agent_desktop::platform::kde::register_application(&paths.data, &executable)?;
    write_file(
        &paths.extension().join("extension.js"),
        EXTENSION_JS.as_bytes(),
        0o644,
    )?;
    write_file(
        &paths.extension().join("metadata.json"),
        EXTENSION_METADATA.as_bytes(),
        0o644,
    )?;
    fs::create_dir_all(&paths.bin)?;
    let link = paths.bin.join("agent-desktop");
    let temporary = tempfile::Builder::new()
        .prefix(".agent-desktop-")
        .tempdir_in(&paths.bin)?;
    let staged = temporary.path().join("agent-desktop");
    symlink(&executable, &staged)?;
    fs::rename(staged, link).context("Install the agent-desktop command")?;
    Ok(executable)
}

pub fn setup(bin: Option<PathBuf>) -> Result<()> {
    let paths = Paths::discover(bin)?;
    let executable = install(&paths, &env::current_exe()?)?;
    for (program, arguments) in [
        (
            "update-desktop-database",
            vec![paths.data.join("applications").into_os_string()],
        ),
        ("kbuildsycoca6", vec!["--noincremental".into()]),
    ] {
        if available(program) {
            let status = Command::new(program)
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !status.is_ok_and(|status| status.success()) {
                eprintln!(
                    "Could not refresh {program}; log out and back in to refresh desktop registration."
                );
            }
        }
    }
    println!("Installed {}", executable.display());
    println!("Command: {}", paths.bin.join("agent-desktop").display());
    println!(
        "KDE screenshot authorization registered. GNOME extension installed (Shell 49); log out and back in, then run: gnome-extensions enable {EXTENSION}"
    );
    Ok(())
}

#[derive(Serialize)]
struct Check {
    name: String,
    ok: bool,
    required: bool,
    detail: String,
}

fn available(program: &str) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|directory| {
        fs::metadata(directory.join(program))
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

pub fn doctor(json: bool, virtual_mode: bool) -> Result<()> {
    let paths = Paths::discover(None)?;
    let desktop = env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, required: bool, detail: String| {
        checks.push(Check {
            name: name.into(),
            ok,
            required,
            detail,
        });
    };
    let executable = paths.executable();
    check(
        "installation",
        executable.is_file(),
        true,
        format!(
            "{}; run agent-desktop setup if missing",
            executable.display()
        ),
    );
    let entry = paths.data.join("applications/agent-desktop.desktop");
    let expected_entry = fs::canonicalize(&executable)
        .ok()
        .and_then(|path| agent_desktop::platform::kde::application_entry(&path).ok());
    let entry_text = fs::read_to_string(&entry).ok();
    check(
        "kde-registration",
        expected_entry.is_some() && entry_text == expected_entry,
        !virtual_mode && desktop.contains("kde"),
        entry.display().to_string(),
    );
    check(
        "gnome-extension",
        fs::read_to_string(paths.extension().join("extension.js"))
            .is_ok_and(|contents| contents == EXTENSION_JS),
        !virtual_mode && desktop.contains("gnome"),
        "Requires GNOME Shell 49; after login run gnome-extensions enable agent-desktop@local"
            .into(),
    );
    if virtual_mode {
        for program in ["dbus-daemon", "kwin_wayland", "kbuildsycoca6", "Xwayland"] {
            check(
                program,
                available(program),
                true,
                format!("Install {program} for managed KDE virtual seats"),
            );
        }
        check(
            "at-spi-bus-launcher",
            available("at-spi-bus-launcher") || available("/usr/libexec/at-spi-bus-launcher"),
            true,
            "Install the AT-SPI accessibility bus launcher".into(),
        );
    } else {
        let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = env::var_os("DISPLAY").is_some();
        check(
            "desktop-session",
            wayland || x11,
            true,
            "Run from your KDE/GNOME graphical session with WAYLAND_DISPLAY or DISPLAY".into(),
        );
        check(
            "session-bus",
            env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
            true,
            "DBUS_SESSION_BUS_ADDRESS must refer to your graphical session bus".into(),
        );
        check(
            "runtime-directory",
            env::var_os("XDG_RUNTIME_DIR").is_some_and(|path| Path::new(&path).is_dir()),
            true,
            "XDG_RUNTIME_DIR must identify your session runtime directory".into(),
        );
        if !wayland && x11 {
            for program in ["xdotool", "xmodmap", "wmctrl", "xprop"] {
                check(
                    program,
                    available(program),
                    true,
                    format!("Install {program} for X11 desktop operations"),
                );
            }
            check(
                "x11-screenshot",
                available("maim") || available("import"),
                true,
                "Install maim or ImageMagick import".into(),
            );
        }
        if desktop.contains("kde") {
            check(
                "kde-cache",
                available("kbuildsycoca6"),
                true,
                "Install KDE kbuildsycoca6 to refresh screenshot authorization".into(),
            );
        }
        if desktop.contains("gnome") {
            check("gnome-activation", false, false, "Extension activation is not probed; verify agent-desktop@local with gnome-extensions info".into());
        }
    }
    let ok = checks.iter().all(|check| !check.required || check.ok);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"ok": ok, "version": VERSION, "checks": checks})
            )?
        );
    } else {
        for check in checks {
            println!(
                "{} {}: {}",
                if check.ok {
                    "OK"
                } else if check.required {
                    "FAIL"
                } else {
                    "NOTE"
                },
                check.name,
                check.detail
            );
        }
        println!(
            "These checks inspect installation and prerequisites; desktop operations require a working session."
        );
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_survives_cache_removal_and_repeated_install() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: root.path().join("data space$%\\name"),
            bin: root.path().join("bin"),
        };
        let source = root.path().join("cache-binary");
        fs::write(&source, b"first binary").unwrap();
        let installed = install(&paths, &source).unwrap();
        install(&paths, &source).unwrap();
        fs::remove_file(&source).unwrap();
        assert_eq!(
            fs::read(paths.bin.join("agent-desktop")).unwrap(),
            b"first binary"
        );
        assert_eq!(
            fs::canonicalize(paths.bin.join("agent-desktop")).unwrap(),
            installed
        );
        assert_eq!(
            fs::metadata(installed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let entry =
            fs::read_to_string(paths.data.join("applications/agent-desktop.desktop")).unwrap();
        assert!(entry.contains("Exec=\""));
        assert!(entry.contains("%%"));
        assert_eq!(
            fs::read_to_string(paths.extension().join("extension.js")).unwrap(),
            EXTENSION_JS
        );
    }

    #[test]
    fn setup_replaces_a_version_without_truncating_the_running_inode() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: root.path().join("data"),
            bin: root.path().join("bin"),
        };
        let source = root.path().join("source");
        fs::write(&source, b"old").unwrap();
        let installed = install(&paths, &source).unwrap();
        let mut old = fs::File::open(&installed).unwrap();
        fs::write(&source, b"new").unwrap();
        install(&paths, &source).unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut old, &mut contents).unwrap();
        assert_eq!(contents, "old");
        assert_eq!(fs::read(installed).unwrap(), b"new");
    }
}
