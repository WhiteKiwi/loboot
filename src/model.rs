//! Strict, shell-free service manifests.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

pub const MAX_ENVIRONMENT_ENTRIES: usize = 64;
pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
pub const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4096;
/// Includes one '=' and one terminating NUL byte for every assignment.
pub const MAX_ENVIRONMENT_TOTAL_BYTES: usize = 32768;

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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
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
            "expected an absolute path without traversal or control characters",
        ));
    }
    Ok(())
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<()> {
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(Error::new(
            "invalid_environment",
            "environment exceeds the 64-entry limit",
        ));
    }
    let mut total_bytes = 0;
    for (name, value) in environment {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes.len() > MAX_ENVIRONMENT_NAME_BYTES
            || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
            || !bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            return Err(Error::new(
                "invalid_environment",
                "environment names must match [A-Za-z_][A-Za-z0-9_]* and be at most 128 bytes",
            ));
        }
        if value.len() > MAX_ENVIRONMENT_VALUE_BYTES {
            return Err(Error::new(
                "invalid_environment",
                "environment values must be at most 4096 UTF-8 bytes",
            ));
        }
        // All Unicode noncharacters are refused, including the last two code
        // points in every plane. systemd rejects these even when XML can encode them.
        if value.chars().any(|ch| {
            let code = ch as u32;
            ch.is_control() || (0xfdd0..=0xfdef).contains(&code) || (code & 0xffff) >= 0xfffe
        }) {
            return Err(Error::new(
                "invalid_environment",
                "environment values must not contain control characters or Unicode noncharacters",
            ));
        }
        // Individual and entry-count limits above keep this addition bounded.
        total_bytes += name.len() + value.len() + 2;
        if total_bytes > MAX_ENVIRONMENT_TOTAL_BYTES {
            return Err(Error::new(
                "invalid_environment",
                "environment assignments exceed the 32768-byte total limit",
            ));
        }
        if name == "PATH"
            && value.split(':').any(|directory| {
                directory.is_empty()
                    || !Path::new(directory).is_absolute()
                    || directory.split('/').any(|part| part == "." || part == "..")
            })
        {
            return Err(Error::new(
                "invalid_environment",
                "PATH must contain nonempty absolute directories separated by colons, without dot or traversal components",
            ));
        }
    }
    Ok(())
}

impl Manifest {
    /// Validate format without requiring a previously installed executable to still exist.
    pub fn validate_syntax(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_path(&self.executable)?;
        validate_path(&self.cwd)?;
        validate_environment(&self.environment)?;
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
    let content = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("invalid_manifest", "manifest must contain valid UTF-8"))?;
    // TOML diagnostics include source snippets, keys and values. Keep malformed
    // environment contents out of both human and JSON error output.
    let manifest: Manifest = toml::from_str(content).map_err(|_| {
        Error::new(
            "invalid_manifest",
            "manifest must be valid TOML with only supported fields and field types",
        )
    })?;
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_MANIFEST: &str =
        "id='a'\nexecutable='/usr/bin/true'\nargs=[]\ncwd='/tmp'\nmode='once'\nrestart='never'\n";

    fn manifest() -> Manifest {
        toml::from_str(BASE_MANIFEST).unwrap()
    }

    #[test]
    fn environment_is_optional_and_empty_serialization_is_unchanged() {
        let absent = manifest();
        let empty: Manifest = toml::from_str(&format!("{BASE_MANIFEST}\n[environment]\n")).unwrap();
        assert!(absent.environment.is_empty());
        assert_eq!(empty, absent);
        assert_eq!(
            toml::to_string(&empty).unwrap(),
            toml::to_string(&absent).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            serde_json::json!({
                "id": "a",
                "executable": "/usr/bin/true",
                "args": [],
                "cwd": "/tmp",
                "mode": "once",
                "restart": "never"
            })
        );
        assert!(!toml::to_string(&empty).unwrap().contains("environment"));
    }

    #[test]
    fn environment_preserves_strict_required_fields_and_types() {
        for field in ["id", "executable", "args", "cwd", "mode", "restart"] {
            let text = BASE_MANIFEST
                .lines()
                .filter(|line| !line.starts_with(&format!("{field}=")))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(toml::from_str::<Manifest>(&text).is_err(), "{field}");
        }
        for extra in [
            "environmet = {}",
            "environment = []",
            "environment = 'not a table'",
            "[environment]\nA = 1",
            "[environment]\nA = true",
            "[environment]\nA = ['not a string']",
            "[environment]\nA = { child = 'not a string' }",
            "[environment]\nA = 'first'\nA = 'duplicate'",
        ] {
            assert!(toml::from_str::<Manifest>(&format!("{BASE_MANIFEST}{extra}")).is_err());
        }
        let mut json = serde_json::to_value(manifest()).unwrap();
        json["environment"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Manifest>(json).is_err());
    }

    #[test]
    fn environment_names_are_portable_and_bounded() {
        for name in [
            "A",
            "z",
            "_",
            "_a_Z09",
            &"A".repeat(MAX_ENVIRONMENT_NAME_BYTES),
        ] {
            let mut m = manifest();
            m.environment.insert(name.into(), String::new());
            assert!(m.validate_syntax().is_ok());
        }
        for name in [
            "",
            "1FIRST",
            "A=B",
            "A-B",
            "a.b",
            " A",
            "A ",
            "A\n",
            "é",
            "A\0",
            &"A".repeat(MAX_ENVIRONMENT_NAME_BYTES + 1),
        ] {
            let mut m = manifest();
            m.environment
                .insert(name.into(), "must-not-appear-in-errors".into());
            let error = m.validate_syntax().unwrap_err();
            assert_eq!(error.code, "invalid_environment");
            assert!(!error.to_string().contains("must-not-appear-in-errors"));
        }
    }

    #[test]
    fn environment_entry_limit_is_inclusive() {
        let mut m = manifest();
        for index in 0..MAX_ENVIRONMENT_ENTRIES {
            m.environment.insert(format!("A{index}"), String::new());
        }
        assert!(m.validate_syntax().is_ok());
        m.environment.insert("EXCESS".into(), String::new());
        assert_eq!(m.validate_syntax().unwrap_err().code, "invalid_environment");
    }

    #[test]
    fn environment_value_limits_count_utf8_bytes_not_characters() {
        let mut m = manifest();
        for value in [
            String::new(),
            "x".repeat(MAX_ENVIRONMENT_VALUE_BYTES),
            "é".repeat(MAX_ENVIRONMENT_VALUE_BYTES / 2),
            "🦀".repeat(MAX_ENVIRONMENT_VALUE_BYTES / 4),
            " $HOME ${HOME} ~ 100% ; & = < > \\ \" ' 한국어 ".into(),
        ] {
            m.environment.insert("VALUE".into(), value);
            assert!(m.validate_syntax().is_ok());
        }
        for value in [
            "x".repeat(MAX_ENVIRONMENT_VALUE_BYTES + 1),
            "é".repeat(MAX_ENVIRONMENT_VALUE_BYTES / 2 + 1),
            "🦀".repeat(MAX_ENVIRONMENT_VALUE_BYTES / 4 + 1),
        ] {
            m.environment.insert("VALUE".into(), value);
            assert_eq!(m.validate_syntax().unwrap_err().code, "invalid_environment");
        }
    }

    #[test]
    fn environment_total_includes_assignment_and_nul_bytes() {
        let mut m = manifest();
        // Eight two-byte names + eight 4092-byte values + '=' and NUL per entry.
        for index in 0..8 {
            m.environment.insert(format!("A{index}"), "é".repeat(2046));
        }
        assert_eq!(
            m.environment
                .iter()
                .map(|(name, value)| name.len() + value.len() + 2)
                .sum::<usize>(),
            MAX_ENVIRONMENT_TOTAL_BYTES
        );
        assert!(m.validate_syntax().is_ok());
        m.environment.get_mut("A0").unwrap().push('x');
        assert_eq!(m.validate_syntax().unwrap_err().code, "invalid_environment");
    }

    #[test]
    fn environment_rejects_controls_and_every_unicode_noncharacter() {
        let mut rejected = (0..=0x1f)
            .chain(0x7f..=0x9f)
            .map(|code| char::from_u32(code).unwrap())
            .collect::<Vec<_>>();
        rejected.extend((0xfdd0..=0xfdef).map(|code| char::from_u32(code).unwrap()));
        for plane in 0..=16 {
            for ending in [0xfffe, 0xffff] {
                rejected.push(char::from_u32((plane << 16) | ending).unwrap());
            }
        }
        for ch in rejected {
            let mut m = manifest();
            m.environment
                .insert("VALUE".into(), format!("private-value{ch}"));
            let error = m.validate_syntax().unwrap_err();
            assert_eq!(error.code, "invalid_environment");
            assert!(!error.to_string().contains("private-value"));
        }
        for ch in [0xfdcf, 0xfdf0, 0xfffd, 0x1fffd, 0x10fffd] {
            let mut m = manifest();
            m.environment
                .insert("VALUE".into(), char::from_u32(ch).unwrap().to_string());
            assert!(m.validate_syntax().is_ok());
        }
    }

    #[test]
    fn explicit_path_is_absolute_literal_and_syntax_only() {
        for path in [
            "/usr/bin:/bin",
            "/",
            "/nonexistent-loboot-test/bin:/tmp/$HOME/~/literal",
            "/directory with spaces:/literal%specifier",
        ] {
            let mut m = manifest();
            m.environment.insert("PATH".into(), path.into());
            assert!(m.validate_syntax().is_ok());
        }
        for path in [
            "",
            ":",
            "/bin:",
            ":/bin",
            "/bin::/usr/bin",
            "bin",
            "/bin:relative",
            ".",
            "..",
            "/bin/.",
            "/bin/../usr",
            "/./bin",
            "~/bin",
            "$HOME/bin",
            "/bin\n/injected",
        ] {
            let mut m = manifest();
            m.environment.insert("PATH".into(), path.into());
            assert_eq!(m.validate_syntax().unwrap_err().code, "invalid_environment");
        }
        let mut m = manifest();
        m.environment.insert("path".into(), String::new());
        assert!(m.validate_syntax().is_ok());
    }

    #[test]
    fn manifest_read_errors_never_echo_environment_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("manifest.toml");
        for extra in [
            "[environment]\nTOKEN = ['private-parser-secret']",
            "[environment]\nTOKEN = \"private-parser-secret\\nvalue\"",
            "[environment]\nTOKEN = 'private-parser-secret'\nmalformed",
            "unknown_private_parser_secret = 'private-parser-secret'",
        ] {
            std::fs::write(&path, format!("{BASE_MANIFEST}{extra}")).unwrap();
            let error = read(&path).unwrap_err();
            assert!(matches!(
                error.code,
                "invalid_manifest" | "invalid_environment"
            ));
            assert!(!error.to_string().contains("private-parser-secret"));
            assert!(!error.to_string().contains("unknown_private_parser_secret"));
        }
    }
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
            environment: BTreeMap::new(),
        };
        assert!(m.validate_syntax().is_err());
    }
}
