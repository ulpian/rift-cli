//! Where an agent is installed on this machine — **one rule, read by every
//! surface**.
//!
//! `rift init` wired the agents whose command was on `PATH`. Canopy's
//! pre-tick also counted a Codex IDE extension. `rift doctor` read `PATH` a
//! third time through a scanner that appended the executable suffix the native
//! one forgot. So one clone read "Codex: detected" in the dashboard and
//! "no new selected agents" from `init`, and a `claude.exe` on a Windows
//! `PATH` was invisible to the native scanner altogether. This module is the
//! single answer: the evidence classes, the order they are reported in, and
//! the environment they are read from — **injected**, so no test reads the
//! machine it runs on.
//!
//! Detection is not eligibility: what is *found* here is filtered against the
//! alpha set by the callers, never by this module.
//!
//! Every probe checks every platform layout it knows, whatever platform it
//! runs on. A fixture built on Linux for the macOS bundle layout must find
//! the same install the macOS box does, and a `claude.exe` planted by a test
//! must be found on Linux too; the cost is a handful of `is_file` calls.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Editor host in which a VS Code-compatible extension is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeIdeHost {
    /// Visual Studio Code stable.
    VsCode,
    /// Visual Studio Code Insiders.
    VsCodeInsiders,
    /// Cursor's VS Code-compatible extension host.
    Cursor,
    /// Windsurf's VS Code-compatible extension host.
    Windsurf,
}

impl NativeIdeHost {
    /// Every host, in the order installs are reported.
    pub const ALL: [Self; 4] = [
        Self::VsCode,
        Self::VsCodeInsiders,
        Self::Cursor,
        Self::Windsurf,
    ];

    /// Concise human-facing host name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::VsCodeInsiders => "VS Code Insiders",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
        }
    }

    /// The host's extension root, relative to the user's home directory.
    const fn extensions_dir(self) -> &'static str {
        match self {
            Self::VsCode => ".vscode/extensions",
            Self::VsCodeInsiders => ".vscode-insiders/extensions",
            Self::Cursor => ".cursor/extensions",
            Self::Windsurf => ".windsurf/extensions",
        }
    }
}

/// The kind of evidence that an agent is installed, in reporting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeInstallSource {
    /// A command of the agent's name on `PATH`.
    PathCommand,
    /// The vendor's standalone install: the native installer's binary in its
    /// documented location, or the desktop application bundle.
    VendorInstall,
    /// A VS Code-compatible extension whose own manifest names the vendor.
    IdeExtension(NativeIdeHost),
    /// A JetBrains plugin directory. The plugin ships no runnable binary, so
    /// this source only ever says "installed", never a version Rift can run.
    JetBrainsPlugin,
}

impl NativeInstallSource {
    /// The words a status line uses for this source.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::PathCommand => "PATH command".to_owned(),
            Self::VendorInstall => "vendor install".to_owned(),
            Self::IdeExtension(host) => format!("{} extension", host.display_name()),
            Self::JetBrainsPlugin => "JetBrains plugin".to_owned(),
        }
    }
}

/// One piece of evidence that an agent is installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInstallation {
    /// Where the evidence came from.
    pub source: NativeInstallSource,
    /// The version the verifying manifest declares, when it declares one. A
    /// bare binary carries none: this module never runs what it finds.
    pub version: Option<String>,
    /// A runnable binary this install provides, when one was found beside it.
    pub executable: Option<PathBuf>,
    /// The install root — the extension or plugin directory, the application
    /// bundle — or the binary itself for a bare command.
    pub path: PathBuf,
}

impl NativeInstallation {
    /// The sentence fragment a status line prints: the source, then the
    /// version when known — `VS Code extension 2.1.267`, `PATH command`.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.version {
            Some(version) => format!("{} {version}", self.source.label()),
            None => self.source.label(),
        }
    }
}

/// The machine facts detection reads. Built from the process environment in
/// production ([`DetectionEnvironment::from_process`]) and from temporary
/// directories in tests; nothing in this module reads an environment variable
/// itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetectionEnvironment {
    /// The `PATH` to scan for commands.
    pub search_path: Option<OsString>,
    /// The user's home directory: extension roots, the native installer's
    /// binary and JetBrains plugin roots all hang off it.
    pub home: Option<PathBuf>,
    /// Roots under which desktop applications are installed — `/Applications`
    /// on macOS, `%LOCALAPPDATA%\Programs` on Windows, `/usr/share` and `/opt`
    /// on Linux — in probe order.
    pub app_roots: Vec<PathBuf>,
    /// Codex's home, where it records which project hooks the user trusted.
    /// Read, never written; `None` means Codex's trust is not read at all.
    pub codex_home: Option<PathBuf>,
}

impl DetectionEnvironment {
    /// The running process's environment: `PATH`, the home directory and the
    /// platform's application roots.
    #[must_use]
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let mut app_roots = Vec::new();
        if cfg!(target_os = "macos") {
            app_roots.push(PathBuf::from("/Applications"));
            if let Some(home) = &home {
                app_roots.push(home.join("Applications"));
            }
        } else if cfg!(windows) {
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                app_roots.push(PathBuf::from(local).join("Programs"));
            } else if let Some(home) = &home {
                app_roots.push(home.join("AppData/Local/Programs"));
            }
            if let Some(programs) = std::env::var_os("ProgramFiles") {
                app_roots.push(PathBuf::from(programs));
            }
        } else {
            app_roots.push(PathBuf::from("/usr/share"));
            app_roots.push(PathBuf::from("/opt"));
            if let Some(home) = &home {
                app_roots.push(home.join(".local/share"));
            }
        }
        Self {
            search_path: std::env::var_os("PATH"),
            home,
            app_roots,
            codex_home: crate::native::codex_home_from_process(),
        }
    }
}

/// Every install of the agent with registry token `token` the environment
/// shows, in reporting order: the `PATH` command, then vendor installs, then
/// IDE extensions host by host, then JetBrains plugins. Empty means not
/// detected.
#[must_use]
pub fn installations(token: &str, env: &DetectionEnvironment) -> Vec<NativeInstallation> {
    let mut found = Vec::new();
    if let Some(executable) = path_command(token, env.search_path.as_deref()) {
        found.push(NativeInstallation {
            source: NativeInstallSource::PathCommand,
            version: None,
            path: executable.clone(),
            executable: Some(executable),
        });
    }
    found.extend(vendor_installs(token, env));
    if let Some(home) = env.home.as_deref() {
        found.extend(ide_extensions(token, home));
        found.extend(jetbrains_plugins(token, home));
    }
    found
}

/// The first runnable binary among `installations`, in reporting order.
#[must_use]
pub fn first_executable(installations: &[NativeInstallation]) -> Option<PathBuf> {
    installations
        .iter()
        .find_map(|installation| installation.executable.clone())
}

/// The command names an agent answers to on `PATH`.
#[must_use]
pub const fn command_names(token: &str) -> &'static [&'static str] {
    match token.as_bytes() {
        b"claude-code" => &["claude"],
        b"codex" => &["codex"],
        b"gemini" => &["gemini"],
        b"antigravity" => &["antigravity", "agy"],
        b"cursor" => &["cursor", "cursor-agent"],
        b"copilot-cli" => &["copilot"],
        b"opencode" => &["opencode"],
        b"factory-droid" => &["droid"],
        b"ollama" => &["ollama"],
        _ => &[],
    }
}

/// The first `PATH` entry holding one of the agent's command names.
fn path_command(token: &str, search_path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let search_path = search_path?;
    std::env::split_paths(search_path).find_map(|dir| find_executable(&dir, command_names(token)))
}

/// The spellings a command has on disk. The bare name is the POSIX binary;
/// `.exe` is a native Windows binary; `.cmd` is the shim npm writes on
/// Windows, which is how a globally installed Claude Code or Codex appears
/// there. All three are tried on every platform (see the module doc).
const EXECUTABLE_SUFFIXES: [&str; 3] = ["", ".exe", ".cmd"];

/// The first of `names`, in any spelling, that is a file under `dir`.
fn find_executable(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        EXECUTABLE_SUFFIXES.iter().find_map(|suffix| {
            let candidate = dir.join(format!("{name}{suffix}"));
            candidate.is_file().then_some(candidate)
        })
    })
}

/// The vendor's standalone installs of `token`.
///
/// * Claude Code's native installer puts `claude` in `~/.local/bin` on every
///   platform.
/// * Antigravity is a VS Code-derived desktop IDE: the bundle under an
///   application root, verified by its own `product.json`.
/// * Codex ships a desktop application bundle beside the CLI.
fn vendor_installs(token: &str, env: &DetectionEnvironment) -> Vec<NativeInstallation> {
    let mut found = Vec::new();
    match token {
        "claude-code" => {
            if let Some(executable) = env
                .home
                .as_deref()
                .and_then(|home| find_executable(&home.join(".local/bin"), &["claude"]))
            {
                found.push(NativeInstallation {
                    source: NativeInstallSource::VendorInstall,
                    version: None,
                    path: executable.clone(),
                    executable: Some(executable),
                });
            }
        }
        "antigravity" => {
            found.extend(
                env.app_roots
                    .iter()
                    .filter_map(|root| vscode_fork_app(root, "Antigravity", "antigravity")),
            );
        }
        "codex" => {
            found.extend(
                env.app_roots
                    .iter()
                    .filter_map(|root| desktop_app(root, "Codex", "openai")),
            );
        }
        _ => {}
    }
    found
}

/// A VS Code-derived desktop IDE named `app` under `root`, in any of the
/// three platform layouts, verified by the `product.json` every such IDE
/// ships: its `applicationName` or `nameShort` must name `cli`. The version
/// is the bundled `package.json`'s; the executable is the CLI shim the IDE
/// installs beside its resources, when present.
fn vscode_fork_app(root: &Path, app: &str, cli: &str) -> Option<NativeInstallation> {
    let layouts = [
        root.join(format!("{app}.app/Contents/Resources/app")),
        root.join(app).join("resources/app"),
        root.join(cli).join("resources/app"),
    ];
    layouts.into_iter().find_map(|resources| {
        let product: serde_json::Value = read_json(&resources.join("product.json"))?;
        let names_cli = |key: &str| {
            product
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(cli))
        };
        if !names_cli("applicationName") && !names_cli("nameShort") {
            return None;
        }
        let version =
            read_json(&resources.join("package.json")).and_then(|package: serde_json::Value| {
                package.get("version")?.as_str().map(str::to_owned)
            });
        let executable = find_executable(&resources.join("bin"), &[cli]);
        Some(NativeInstallation {
            source: NativeInstallSource::VendorInstall,
            version,
            executable,
            path: resources,
        })
    })
}

/// A desktop application bundle named `app` under `root`: a macOS bundle
/// whose `Info.plist` names `vendor` in its bundle identifier, or a Windows
/// or Linux install directory holding the application's own binary. The
/// version is the plist's `CFBundleShortVersionString` where one exists.
fn desktop_app(root: &Path, app: &str, vendor: &str) -> Option<NativeInstallation> {
    let bundle = root.join(format!("{app}.app"));
    let plist = bundle.join("Contents/Info.plist");
    if let Ok(text) = std::fs::read_to_string(&plist) {
        if !text.contains(&format!("com.{vendor}.")) {
            return None;
        }
        return Some(NativeInstallation {
            source: NativeInstallSource::VendorInstall,
            version: plist_string(&text, "CFBundleShortVersionString"),
            executable: None,
            path: bundle,
        });
    }
    let dir = root.join(app);
    let executable = find_executable(&dir, &[app, &app.to_ascii_lowercase()])?;
    Some(NativeInstallation {
        source: NativeInstallSource::VendorInstall,
        version: None,
        executable: Some(executable),
        path: dir,
    })
}

/// The `<string>` that follows `<key>{key}</key>` in a property list, without
/// parsing the plist: enough for a version, and nothing here depends on it.
fn plist_string(text: &str, key: &str) -> Option<String> {
    let after_key = text.split_once(&format!("<key>{key}</key>"))?.1;
    let after_open = after_key.split_once("<string>")?.1;
    let value = after_open.split_once("</string>")?.0.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The vendor and extension name a host's `package.json` must declare for
/// the agent, and where the extension keeps a runnable binary, if it does.
struct ExtensionShape {
    publisher: &'static str,
    name: &'static str,
    /// Directories, relative to the extension root, in which the agent's
    /// command may sit. Codex ships one directory per platform; Claude Code
    /// one binary.
    binary_dirs: &'static [&'static str],
}

fn extension_shape(token: &str) -> Option<ExtensionShape> {
    match token {
        "claude-code" => Some(ExtensionShape {
            publisher: "anthropic",
            name: "claude-code",
            binary_dirs: &["resources/native-binary"],
        }),
        "codex" => Some(ExtensionShape {
            publisher: "openai",
            name: "chatgpt",
            binary_dirs: &[
                "bin/windows-x86_64",
                "bin/windows-aarch64",
                "bin/darwin-arm64",
                "bin/darwin-x86_64",
                "bin/linux-x86_64",
                "bin/linux-aarch64",
            ],
        }),
        _ => None,
    }
}

/// The agent's verified extension in every host that has one: the highest
/// version per host, symlinked directories and manifests skipped.
fn ide_extensions(token: &str, home: &Path) -> Vec<NativeInstallation> {
    let Some(shape) = extension_shape(token) else {
        return Vec::new();
    };
    NativeIdeHost::ALL
        .into_iter()
        .filter_map(|host| newest_extension(token, host, &home.join(host.extensions_dir()), &shape))
        .collect()
}

fn newest_extension(
    token: &str,
    host: NativeIdeHost,
    extensions_dir: &Path,
    shape: &ExtensionShape,
) -> Option<NativeInstallation> {
    let entries = std::fs::read_dir(extensions_dir).ok()?;
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return None;
            }
            let extension_path = entry.path();
            let manifest_path = extension_path.join("package.json");
            if is_symlink(&manifest_path) {
                return None;
            }
            let value: serde_json::Value = read_json(&manifest_path)?;
            let declares = |key: &str, expected: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            };
            if !declares("publisher", shape.publisher) || !declares("name", shape.name) {
                return None;
            }
            let version = value.get("version")?.as_str()?.to_owned();
            let executable = shape
                .binary_dirs
                .iter()
                .find_map(|dir| find_executable(&extension_path.join(dir), command_names(token)));
            Some(NativeInstallation {
                source: NativeInstallSource::IdeExtension(host),
                version: Some(version),
                executable,
                path: extension_path,
            })
        })
        .max_by(|left, right| {
            version_key(left.version.as_deref()).cmp(&version_key(right.version.as_deref()))
        })
}

/// The JetBrains plugin roots under a home directory, one per platform; all
/// three are probed (see the module doc).
const JETBRAINS_ROOTS: [&str; 3] = [
    "Library/Application Support/JetBrains",
    ".local/share/JetBrains",
    "AppData/Roaming/JetBrains",
];

/// The directory-name prefix of the agent's JetBrains plugin, when it has one.
const fn jetbrains_plugin_prefix(token: &str) -> Option<&'static str> {
    match token.as_bytes() {
        b"claude-code" => Some("claude-code"),
        _ => None,
    }
}

/// The agent's plugin in every JetBrains IDE that has one: a plugin directory
/// named for the agent that holds at least one jar. The version is read from
/// the jar's file name when it carries one; the plugin has no binary to run.
fn jetbrains_plugins(token: &str, home: &Path) -> Vec<NativeInstallation> {
    let Some(prefix) = jetbrains_plugin_prefix(token) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for root in JETBRAINS_ROOTS {
        let Ok(ides) = std::fs::read_dir(home.join(root)) else {
            continue;
        };
        let mut ides: Vec<PathBuf> = ides
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && !is_symlink(path))
            .collect();
        ides.sort();
        for ide in ides {
            let Ok(plugins) = std::fs::read_dir(ide.join("plugins")) else {
                continue;
            };
            let mut plugins: Vec<PathBuf> = plugins
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_dir()
                        && !is_symlink(path)
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.to_ascii_lowercase().starts_with(prefix))
                })
                .collect();
            plugins.sort();
            found.extend(plugins.into_iter().filter_map(|plugin| {
                let jars = jar_names(&plugin.join("lib"));
                if jars.is_empty() {
                    return None;
                }
                Some(NativeInstallation {
                    source: NativeInstallSource::JetBrainsPlugin,
                    version: jars.iter().find_map(|jar| jar_version(jar)),
                    executable: None,
                    path: plugin,
                })
            }));
        }
    }
    found
}

fn jar_names(lib: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(lib) else {
        return Vec::new();
    };
    let mut jars: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
        .collect();
    jars.sort();
    jars
}

/// `plugin-0.1.15.jar` → `0.1.15`: the trailing dotted-digit run before
/// `.jar`, when the name ends in one.
fn jar_version(jar: &str) -> Option<String> {
    let stem = jar.strip_suffix(".jar")?;
    let version = stem.rsplit('-').next()?;
    let dotted_digits = version.chars().all(|c| c.is_ascii_digit() || c == '.');
    (dotted_digits && version.chars().any(|c| c.is_ascii_digit())).then(|| version.to_owned())
}

/// A numeric version ordering: `26.803.61601` sorts above `26.700.1`.
fn version_key(version: Option<&str>) -> Vec<u64> {
    version
        .unwrap_or_default()
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    if is_symlink(path) {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn manifest(publisher: &str, name: &str, version: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "name": name,
            "publisher": publisher,
            "version": version,
        }))
        .unwrap()
    }

    fn env(home: &Path, app_root: &Path, path_dir: Option<&Path>) -> DetectionEnvironment {
        DetectionEnvironment {
            search_path: path_dir.map(|dir| dir.as_os_str().to_os_string()),
            home: Some(home.to_path_buf()),
            app_roots: vec![app_root.to_path_buf()],
            codex_home: None,
        }
    }

    #[test]
    fn an_empty_environment_detects_nothing() {
        for token in ["claude-code", "codex", "antigravity", "ollama", "unknown"] {
            assert!(installations(token, &DetectionEnvironment::default()).is_empty());
        }
    }

    /// The Windows gap: `claude.exe` and npm's `claude.cmd` are commands too.
    #[test]
    fn path_commands_in_all_spellings() {
        for spelling in ["claude", "claude.exe", "claude.cmd"] {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join(spelling), b"");
            let env = DetectionEnvironment {
                search_path: Some(dir.path().as_os_str().to_os_string()),
                ..DetectionEnvironment::default()
            };
            let found = installations("claude-code", &env);
            assert_eq!(found.len(), 1, "{spelling}");
            assert_eq!(found[0].source, NativeInstallSource::PathCommand);
            assert_eq!(
                found[0].executable.as_deref(),
                Some(dir.path().join(spelling).as_path())
            );
            assert!(found[0].version.is_none());
            assert_eq!(found[0].describe(), "PATH command");
        }
    }

    #[test]
    fn a_directory_named_like_the_command_is_not_a_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("codex")).unwrap();
        let env = DetectionEnvironment {
            search_path: Some(dir.path().as_os_str().to_os_string()),
            ..DetectionEnvironment::default()
        };
        assert!(installations("codex", &env).is_empty());
    }

    /// The case this module exists for: Claude Code run only through the VS
    /// Code extension, whose bundled binary is nowhere on `PATH`.
    #[test]
    fn the_claude_code_extension_is_detected_with_its_bundled_binary() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        let extension = home
            .path()
            .join(".vscode/extensions/anthropic.claude-code-2.1.267-win32-x64");
        write(
            &extension.join("package.json"),
            &manifest("Anthropic", "claude-code", "2.1.267"),
        );
        write(&extension.join("resources/native-binary/claude.exe"), b"");

        let found = installations("claude-code", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].source,
            NativeInstallSource::IdeExtension(NativeIdeHost::VsCode)
        );
        assert_eq!(found[0].version.as_deref(), Some("2.1.267"));
        assert_eq!(
            found[0].executable.as_deref(),
            Some(
                extension
                    .join("resources/native-binary/claude.exe")
                    .as_path()
            )
        );
        assert_eq!(found[0].describe(), "VS Code extension 2.1.267");
        assert_eq!(first_executable(&found), found[0].executable);
    }

    #[test]
    fn the_newest_codex_extension_wins_and_impostors_are_ignored() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        let root = home.path().join(".cursor/extensions");
        write(
            &root.join("openai.chatgpt-26.700.1/package.json"),
            &manifest("openai", "chatgpt", "26.700.1"),
        );
        write(
            &root.join("openai.chatgpt-26.803.61601/package.json"),
            &manifest("openai", "chatgpt", "26.803.61601"),
        );
        write(
            &root.join("openai.chatgpt-26.803.61601/bin/linux-x86_64/codex"),
            b"",
        );
        write(
            &root.join("openai.chatgpt-99.0.0/package.json"),
            &manifest("someone-else", "chatgpt", "99.0.0"),
        );

        let found = installations("codex", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].source,
            NativeInstallSource::IdeExtension(NativeIdeHost::Cursor)
        );
        assert_eq!(found[0].version.as_deref(), Some("26.803.61601"));
        assert!(found[0].executable.is_some());
    }

    #[test]
    fn a_symlinked_extension_manifest_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        let extension = home
            .path()
            .join(".vscode/extensions/anthropic.claude-code-1.0.0");
        std::fs::create_dir_all(&extension).unwrap();
        let real = home.path().join("elsewhere.json");
        write(&real, &manifest("anthropic", "claude-code", "1.0.0"));
        if symlink(&real, &extension.join("package.json")).is_err() {
            return;
        }
        assert!(installations("claude-code", &env(home.path(), apps.path(), None)).is_empty());
    }

    #[test]
    fn the_native_installer_binary_is_a_vendor_install() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        write(&home.path().join(".local/bin/claude"), b"");
        let found = installations("claude-code", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, NativeInstallSource::VendorInstall);
        assert!(found[0].executable.is_some());
        assert_eq!(found[0].describe(), "vendor install");
    }

    #[test]
    fn an_antigravity_app_is_a_vendor_install() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        // The macOS layout, with its CLI shim.
        let resources = apps.path().join("Antigravity.app/Contents/Resources/app");
        write(
            &resources.join("product.json"),
            br#"{"nameShort":"Antigravity","applicationName":"antigravity"}"#,
        );
        write(&resources.join("package.json"), br#"{"version":"1.12.3"}"#);
        write(&resources.join("bin/antigravity"), b"");
        // A look-alike under the Windows layout that names another product.
        let other = apps.path().join("Antigravity/resources/app");
        write(
            &other.join("product.json"),
            br#"{"applicationName":"code"}"#,
        );

        let found = installations("antigravity", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, NativeInstallSource::VendorInstall);
        assert_eq!(found[0].version.as_deref(), Some("1.12.3"));
        assert_eq!(
            found[0].executable.as_deref(),
            Some(resources.join("bin/antigravity").as_path())
        );
        assert_eq!(found[0].describe(), "vendor install 1.12.3");
    }

    #[test]
    fn the_codex_desktop_bundle_reads_its_version_from_the_plist() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        write(
            &apps.path().join("Codex.app/Contents/Info.plist"),
            b"<plist><dict><key>CFBundleIdentifier</key><string>com.openai.codex</string>\
              <key>CFBundleShortVersionString</key><string>26.9.1</string></dict></plist>",
        );
        let found = installations("codex", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, NativeInstallSource::VendorInstall);
        assert_eq!(found[0].version.as_deref(), Some("26.9.1"));
        assert!(found[0].executable.is_none());

        // A bundle of the right name from another vendor is not Codex.
        let other = tempfile::tempdir().unwrap();
        write(
            &other.path().join("Codex.app/Contents/Info.plist"),
            b"<key>CFBundleIdentifier</key><string>com.example.codex</string>",
        );
        assert!(installations("codex", &env(home.path(), other.path(), None)).is_empty());
    }

    #[test]
    fn the_jetbrains_plugin_counts_without_a_binary() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        // Spelled as short components on purpose: the repository's redaction
        // false-positive census reads this file, and one long path literal
        // is one high-entropy token.
        let jetbrains = home.path().join(".local").join("share").join("JetBrains");
        let plugin = jetbrains
            .join("Idea2026.2")
            .join("plugins")
            .join("claude-code-plugin");
        write(&plugin.join("lib").join("plugin-0.1.15.jar"), b"");
        // A plugin directory with no jar is a leftover, not an install.
        std::fs::create_dir_all(
            jetbrains
                .join("PyCharm2026.2")
                .join("plugins")
                .join("claude-code-plugin")
                .join("lib"),
        )
        .unwrap();

        let found = installations("claude-code", &env(home.path(), apps.path(), None));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, NativeInstallSource::JetBrainsPlugin);
        assert_eq!(found[0].version.as_deref(), Some("0.1.15"));
        assert!(found[0].executable.is_none());
        assert_eq!(found[0].describe(), "JetBrains plugin 0.1.15");
        assert!(first_executable(&found).is_none());
    }

    /// Reporting order is fixed: PATH, vendor, extensions by host, JetBrains.
    #[test]
    fn evidence_is_reported_in_source_order() {
        let home = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        let path_dir = tempfile::tempdir().unwrap();
        write(&path_dir.path().join("claude"), b"");
        write(&home.path().join(".local/bin/claude"), b"");
        write(
            &home
                .path()
                .join(".windsurf/extensions/anthropic.claude-code-2.0.0/package.json"),
            &manifest("anthropic", "claude-code", "2.0.0"),
        );
        write(
            &home
                .path()
                .join(".vscode/extensions/anthropic.claude-code-2.1.0/package.json"),
            &manifest("anthropic", "claude-code", "2.1.0"),
        );
        write(
            &home
                .path()
                .join("AppData")
                .join("Roaming")
                .join("JetBrains")
                .join("GoLand2026.1")
                .join("plugins")
                .join("claude-code")
                .join("lib")
                .join("x.jar"),
            b"",
        );

        let found = installations(
            "claude-code",
            &env(home.path(), apps.path(), Some(path_dir.path())),
        );
        let sources: Vec<NativeInstallSource> = found.iter().map(|i| i.source).collect();
        assert_eq!(
            sources,
            vec![
                NativeInstallSource::PathCommand,
                NativeInstallSource::VendorInstall,
                NativeInstallSource::IdeExtension(NativeIdeHost::VsCode),
                NativeInstallSource::IdeExtension(NativeIdeHost::Windsurf),
                NativeInstallSource::JetBrainsPlugin,
            ]
        );
        assert_eq!(
            first_executable(&found).as_deref(),
            Some(path_dir.path().join("claude").as_path())
        );
        assert!(
            found[4].version.is_none(),
            "an unversioned jar has no version"
        );
    }

    #[test]
    fn version_keys_order_numerically_and_jar_versions_parse() {
        assert!(version_key(Some("26.803.61601")) > version_key(Some("26.700.1")));
        assert!(version_key(Some("2.1.267")) > version_key(Some("2.1.99")));
        assert!(version_key(None).is_empty());
        assert_eq!(jar_version("plugin-0.1.15.jar").as_deref(), Some("0.1.15"));
        assert_eq!(jar_version("plugin.jar"), None);
        assert_eq!(jar_version("plugin-beta.jar"), None);
        assert_eq!(jar_version("plugin-1.jar").as_deref(), Some("1"));
    }

    #[test]
    fn plist_strings_are_read_by_key() {
        let text = "<key>A</key><string> 1.2 </string><key>B</key><string></string>";
        assert_eq!(plist_string(text, "A").as_deref(), Some("1.2"));
        assert_eq!(plist_string(text, "B"), None);
        assert_eq!(plist_string(text, "C"), None);
    }

    #[test]
    fn the_process_environment_has_at_least_one_application_root() {
        let env = DetectionEnvironment::from_process();
        assert!(!env.app_roots.is_empty());
    }

    #[test]
    fn source_labels_name_the_host() {
        assert_eq!(
            NativeInstallSource::IdeExtension(NativeIdeHost::VsCodeInsiders).label(),
            "VS Code Insiders extension"
        );
        assert_eq!(
            NativeInstallSource::JetBrainsPlugin.label(),
            "JetBrains plugin"
        );
        assert_eq!(NativeIdeHost::Cursor.display_name(), "Cursor");
        assert_eq!(NativeIdeHost::Windsurf.display_name(), "Windsurf");
    }

    #[cfg(unix)]
    fn symlink(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(windows)]
    fn symlink(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(original, link)
    }
}
