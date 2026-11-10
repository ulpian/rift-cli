//! Undoing the installer's PATH registration on uninstall.
//!
//! The installer appends one line to a shell startup file, always ending in
//! [`MARKER`], and on Windows prepends the installation directory to the user
//! `Path`. Only those are removed: a line without the marker is the user's,
//! whatever it says, and a Windows `Path` entry is removed only when it is the
//! installer's default directory, because a directory the user chose may be
//! shared with other tools.

use std::path::{Path, PathBuf};

/// The comment every startup-file line the installer writes ends with.
const MARKER: &str = "# rift installer";

/// The startup files the installer may have written, relative to `home`.
const STARTUP_FILES: [&str; 5] = [
    ".zshrc",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
];

/// Each startup file that carries an installer line, with the text it would
/// have once those lines are gone.
pub(super) fn planned_removals() -> Vec<(PathBuf, String)> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    let xdg =
        std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let mut files: Vec<PathBuf> = STARTUP_FILES.iter().map(|name| home.join(name)).collect();
    if let Some(dir) = zdotdir {
        files.push(dir.join(".zshrc"));
    }
    files.push(xdg.join("fish/conf.d/rift.fish"));
    files.sort();
    files.dedup();
    files
        .into_iter()
        .filter_map(|file| {
            let text = std::fs::read_to_string(&file).ok()?;
            let kept = without_installer_lines(&text)?;
            Some((file, kept))
        })
        .collect()
}

/// Apply [`planned_removals`]: rewrite each file without the installer's
/// lines, and remove the fish snippet when nothing else is left in it.
///
/// Each file is read again here rather than trusting the plan's text: the
/// rest of the uninstall runs in between, and an edit the user made meanwhile
/// must survive. The new text goes to a sibling of the real file (a linked
/// dotfile is written where it lives, and stays linked) and replaces it in one
/// rename, so a crash never leaves a truncated startup file.
pub(super) fn remove(plan: &[(PathBuf, String)]) -> std::io::Result<()> {
    for (file, _) in plan {
        let real = std::fs::canonicalize(file)?;
        let text = std::fs::read_to_string(&real)?;
        let Some(kept) = without_installer_lines(&text) else {
            continue;
        };
        if kept.trim().is_empty() && file.file_name().is_some_and(|name| name == "rift.fish") {
            std::fs::remove_file(&real)?;
        } else {
            replace(&real, &kept)?;
        }
    }
    Ok(())
}

/// Replace `path` with `text` through a fresh sibling, keeping its mode.
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let name = path
        .file_name()
        .map_or_else(|| "startup".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.rift-tmp-{}", std::process::id()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::set_permissions(&tmp, std::fs::metadata(path)?.permissions())?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// `text` without the lines that end in [`MARKER`], or `None` when it has none.
fn without_installer_lines(text: &str) -> Option<String> {
    let ours = |line: &str| line.trim_end().ends_with(MARKER);
    if !text.lines().any(ours) {
        return None;
    }
    let mut kept = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if !ours(line) {
            kept.push_str(line);
        }
    }
    Some(kept)
}

/// The PowerShell lines that drop `bin` from the user `Path` when it is the
/// installer's default directory; empty otherwise.
#[cfg(windows)]
pub(super) fn windows_path_script(bin: &Path) -> String {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return String::new();
    };
    let default_dir = Path::new(&local).join("Rift").join("bin");
    if bin != default_dir.as_path() {
        return String::new();
    }
    // Read unexpanded and written back as the same kind, as the installer
    // does, so another program's %VARIABLE% entries survive.
    r"
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
if ($key) { try {
  $stored = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  $want = $env:RIFT_UNINSTALL_BIN.TrimEnd('\')
  $kept = @($stored -split ';' | Where-Object { $_ -and [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ine $want })
  if ($kept.Length -ne @($stored -split ';' | Where-Object { $_ }).Length) { $key.SetValue('Path', ($kept -join ';'), $key.GetValueKind('Path')) }
} finally { $key.Close() } }
"
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lines_ending_in_the_marker_are_removed() {
        let text = "alias ll='ls -l'\n\
            case \":$PATH:\" in *\":$HOME/.local/bin:\"*) ;; *) export PATH=\"$HOME/.local/bin:$PATH\" ;; esac # rift installer\n\
            export PATH=\"$HOME/.local/bin:$PATH\" # mine, keep it\n";
        let kept = without_installer_lines(text).expect("an installer line is present");
        assert_eq!(
            kept,
            "alias ll='ls -l'\nexport PATH=\"$HOME/.local/bin:$PATH\" # mine, keep it\n"
        );
        assert_eq!(without_installer_lines(&kept), None);
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_line() {
        let text = "x=1\nexport PATH=\"/opt/rift:$PATH\" # rift installer";
        assert_eq!(without_installer_lines(text).as_deref(), Some("x=1\n"));
        let text = "contains -- \"/opt/rift\" $PATH; or set -gx PATH \"/opt/rift\" $PATH # rift installer\n";
        assert_eq!(without_installer_lines(text).as_deref(), Some(""));
    }

    #[test]
    fn removing_rewrites_startup_files_and_deletes_an_emptied_fish_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        let fish = dir.path().join("rift.fish");
        std::fs::write(&rc, "keep\nline # rift installer\n").unwrap();
        std::fs::write(&fish, "line # rift installer\n").unwrap();
        let plan = vec![
            (rc.clone(), "keep\n".to_owned()),
            (fish.clone(), String::new()),
        ];
        std::fs::write(&rc, "keep\nadded meanwhile\nline # rift installer\n").unwrap();
        remove(&plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            "keep\nadded meanwhile\n",
            "an edit made after the plan survives"
        );
        assert!(!fish.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_startup_file_is_rewritten_where_it_lives_and_stays_linked() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles-zshrc");
        let link = dir.path().join(".zshrc");
        std::fs::write(&real, "keep\nline # rift installer\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        remove(&[(link.clone(), String::new())]).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "keep\n");
    }
}
