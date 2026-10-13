//! Strict, shell-free service manifests.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Once,
    Service,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    Never,
    OnFailure,
    Always,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub mode: Mode,
    pub restart: Restart,
}

pub fn validate_id(id: &str) -> Result<()> {
    let b = id.as_bytes();
    if b.is_empty()
        || b.len() > 63
        || !b[0].is_ascii_lowercase()
        || !b
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    {
        return Err(Error::new(
            "invalid_id",
            "ID must match [a-z][a-z0-9-]{0,62}",
        ));
    }
    Ok(())
}

pub(crate) fn validate_path(path: &Path) -> Result<()> {
    let s = path
        .to_str()
        .ok_or_else(|| Error::new("invalid_path", "paths must be UTF-8"))?;
    if !path.is_absolute()
        || s.chars().any(char::is_control)
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || s.split('/').any(|c| c == "." || c == "..")
    {
        return Err(Error::new(
            "invalid_path",
            format!(
                "expected an absolute path without traversal or control characters: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

impl Manifest {
    /// Validate format without requiring a previously installed executable to still exist.
    pub fn validate_syntax(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_path(&self.executable)?;
        validate_path(&self.cwd)?;
        if self
            .args
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
        {
            return Err(Error::new(
                "invalid_argument",
                "arguments must not contain NUL or control characters",
            ));
        }
        if self.mode == Mode::Once && self.restart != Restart::Never {
            return Err(Error::new(
                "invalid_restart",
                "once mode requires restart = \"never\"",
            ));
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_syntax()?;
        let f = crate::store::open_input_file(&self.executable)?;
        let md = f.metadata()?;
        if !md.is_file() || md.permissions().mode() & 0o111 == 0 {
            return Err(Error::new(
                "invalid_executable",
                "executable must be an existing regular executable file",
            ));
        }
        if !crate::store::is_safe_directory(&self.cwd)? {
            return Err(Error::new(
                "invalid_cwd",
                "cwd must be an existing directory",
            ));
        }
        Ok(())
    }
}

pub fn read(path: &Path) -> Result<Manifest> {
    use std::io::Read;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let f = crate::store::open_input_file(&absolute)?;
    if !f.metadata()?.is_file() {
        return Err(Error::new(
            "invalid_manifest",
            "manifest must be a regular file",
        ));
    }
    let mut bytes = Vec::new();
    f.take(1_048_577).read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(Error::new("invalid_manifest", "manifest exceeds 1 MiB"));
    }
    let content =
        std::str::from_utf8(&bytes).map_err(|e| Error::new("invalid_manifest", e.to_string()))?;
    let manifest: Manifest =
        toml::from_str(content).map_err(|e| Error::new("invalid_manifest", e.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_strict() {
        for id in ["", "a/b", "../x", "UPPER", "1name", "a_b", "a\n"] {
            assert!(validate_id(id).is_err(), "{id:?}");
        }
        assert!(validate_id("a-19").is_ok());
        assert!(validate_id(&"a".repeat(64)).is_err());
    }
    #[test]
    fn unknown_fields_are_rejected() {
        let text = "id='a'\nexecutable='/usr/bin/true'\nargs=[]\ncwd='/tmp'\nmode='once'\nrestart='never'\nsecret='no'";
        assert!(toml::from_str::<Manifest>(text).is_err());
    }
    #[test]
    fn traversal_and_controls_are_rejected() {
        for path in ["relative", "/tmp/../bin", "/tmp/./x", "/tmp/a\n"] {
            assert!(validate_path(Path::new(path)).is_err());
        }
        let m = Manifest {
            id: "a".into(),
            executable: "/usr/bin/true".into(),
            args: vec!["bad\0arg".into()],
            cwd: "/tmp".into(),
            mode: Mode::Once,
            restart: Restart::Always,
        };
        assert!(m.validate_syntax().is_err());
    }
}
