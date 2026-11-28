//! FTTR UCI input boundary. Callers choose the standard or committed view.
use std::os::unix::fs::DirBuilderExt;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct OptionEntry {
    pub key: String,
    pub value: String,
    pub is_list: bool,
}

#[derive(Clone)]
pub(crate) struct Section {
    pub kind: String,
    pub name: Option<String>,
    pub options: Vec<OptionEntry>,
}

pub(crate) struct View {
    pub sections: Vec<Section>,
}

/// Default libuci context: retains the existing UCI delta semantics.
pub(crate) fn load() -> Result<View, String> {
    let mut uci = openwrt_uci::Uci::new().map_err(|_| "cannot initialize libuci")?;
    export(&mut uci, "fttr")
}

/// Parse an export from a caller-owned context (including private snapshots).
pub(crate) fn export(uci: &mut openwrt_uci::Uci, package: &str) -> Result<View, String> {
    let text = uci
        .export_package_with_names(package, true)
        .map_err(|_| "cannot export UCI package")?;
    View::parse(&text)
}

/// Read only the committed file in an isolated libuci context.
pub(crate) fn committed(path: &Path) -> Result<View, String> {
    let mut snapshot = UciSession::new(path).map_err(|_| "cannot load committed UCI snapshot")?;
    export(&mut snapshot.uci, &snapshot.package)
}

/// Private UCI session for a committed snapshot and isolated transactional writes.
pub(crate) struct UciSession {
    dir: PathBuf,
    pub(crate) conf_dir: PathBuf,
    pub(crate) package: String,
    pub(crate) original: Vec<u8>,
    pub(crate) uci: openwrt_uci::Uci,
}
impl UciSession {
    pub(crate) fn new(config: &Path) -> io::Result<Self> {
        let original = fs::read(config)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for nonce in 0..100 {
            let path =
                std::env::temp_dir().join(format!("fttr-uci-{}-{nonce}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    let setup = (|| -> io::Result<Self> {
                        let conf_dir = path.join("config");
                        let delta_dir = path.join("delta");
                        fs::create_dir(&conf_dir)?;
                        fs::create_dir(&delta_dir)?;
                        let package =
                            format!("fttr_snapshot_{}_{stamp}_{nonce}", std::process::id());
                        fs::write(conf_dir.join(&package), &original)?;
                        let mut uci = openwrt_uci::Uci::new().map_err(io::Error::other)?;
                        uci.set_confdir(&conf_dir).map_err(io::Error::other)?;
                        uci.set_savedir(&delta_dir).map_err(io::Error::other)?;
                        Ok(Self {
                            dir: path.clone(),
                            conf_dir,
                            package,
                            original,
                            uci,
                        })
                    })();
                    if setup.is_err() {
                        let _ = fs::remove_dir_all(&path);
                    }
                    return setup;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("cannot allocate private UCI session"))
    }
}
impl Drop for UciSession {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl View {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut sections: Vec<Section> = Vec::new();
        for line in text.lines() {
            let w = words(line)?;
            if w.is_empty() || w[0].starts_with('#') {
                continue;
            }
            match w[0].as_str() {
                "package" if w.len() == 2 => (),
                "config" if w.len() == 2 || w.len() == 3 => sections.push(Section {
                    kind: w[1].clone(),
                    name: w.get(2).cloned(),
                    options: Vec::new(),
                }),
                "option" | "list" if w.len() == 3 => sections
                    .last_mut()
                    .ok_or("option without section")?
                    .options
                    .push(OptionEntry {
                        key: w[1].clone(),
                        value: w[2].clone(),
                        is_list: w[0] == "list",
                    }),
                _ => return Err("invalid UCI export".into()),
            }
        }
        Ok(Self { sections })
    }
    pub(crate) fn option(&self, section: &str, key: &str) -> Option<String> {
        let options = &self
            .sections
            .iter()
            .find(|s| s.name.as_deref() == Some(section))?
            .options;
        let first = options.iter().find(|o| o.key == key)?;
        if first.is_list {
            Some(
                options
                    .iter()
                    .filter(|o| o.key == key)
                    .map(|o| o.value.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        } else {
            Some(first.value.clone())
        }
    }
}

// UCI export uses shell quoting, including the '\'' sequence inside strings.
pub(crate) fn words(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => {
                word.push(chars.next().ok_or("dangling escape")?);
                started = true;
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated UCI quote".into());
    }
    if started {
        out.push(word);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_preserves_order_lists_empty_values_and_quotes() {
        let view = View::parse("package 'fttr'\nconfig 'item'\n list 'values' 'first value'\n list 'values' 'second'\nconfig 'acs' 'acs'\n option 'username' ''\n option 'password' 'a'\\''b'\n").unwrap();
        assert_eq!(view.sections[0].name, None);
        assert_eq!(view.sections[0].options.len(), 2);
        assert_eq!(view.option("acs", "username").as_deref(), Some(""));
        assert_eq!(view.option("acs", "password").as_deref(), Some("a'b"));
        assert!(View::parse("config 'acs' 'acs'\n option 'password' 'secret\n").is_err());
    }

    #[test]
    fn private_session_exports_and_separates_uncommitted_edits() {
        let path = std::env::temp_dir().join(format!("fttr-config-test-{}", std::process::id()));
        fs::write(&path, "config offload 'offload'\n option bridge 'br-lan'\n list values 'a b'\n list values 'c'\n").unwrap();
        let mut session = UciSession::new(&path).unwrap();
        let initial = export(&mut session.uci, &session.package).unwrap();
        assert_eq!(
            initial.option("offload", "values").as_deref(),
            Some("a b c")
        );
        session
            .uci
            .set(&session.package, "offload", "bridge", "br-test")
            .unwrap();
        let changed = export(&mut session.uci, &session.package).unwrap();
        assert_eq!(
            changed.option("offload", "bridge").as_deref(),
            Some("br-test")
        );
        assert_eq!(
            committed(&path)
                .unwrap()
                .option("offload", "bridge")
                .as_deref(),
            Some("br-lan")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn libuci_normalizes_duplicate_options_and_preserves_values() {
        let path =
            std::env::temp_dir().join(format!("fttr-config-duplicates-{}", std::process::id()));
        fs::write(&path, "config sample 'sample'\n option duplicate 'first'\n option duplicate 'second'\n option empty ''\n option quote 'a'\\''b'\n list values 'one two'\n list values 'three'\n").unwrap();
        let view = committed(&path).unwrap();
        assert_eq!(
            view.option("sample", "duplicate").as_deref(),
            Some("second")
        );
        // libuci treats setting an empty scalar as deleting the option.
        assert_eq!(view.option("sample", "empty"), None);
        assert_eq!(view.option("sample", "quote").as_deref(), Some("a'b"));
        assert_eq!(
            view.option("sample", "values").as_deref(),
            Some("one two three")
        );
        assert_eq!(
            view.sections[0]
                .options
                .iter()
                .filter(|o| o.key == "duplicate")
                .count(),
            1
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn exported_anonymous_section_ids_resolve_references_in_snapshots() {
        let path = std::env::temp_dir().join(format!("fttr-config-anon-{}", std::process::id()));
        fs::write(
            &path,
            "config template\n option name 'Example'\nconfig group\n",
        )
        .unwrap();
        let view = committed(&path).unwrap();
        let template = view.sections[0].name.as_ref().unwrap();
        assert!(template.starts_with("cfg"));
        assert_ne!(view.sections[1].name.as_ref().unwrap(), template);
        let text = format!(
            "config template\n option name 'Example'\nconfig group\n option template '{template}'\n"
        );
        fs::write(&path, text).unwrap();
        let another = committed(&path).unwrap();
        assert_eq!(another.sections[0].name.as_ref(), Some(template));
        assert_eq!(another.sections[1].options[0].value, *template);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn private_session_is_removed_after_success_and_export_failure() {
        let path = std::env::temp_dir().join(format!("fttr-config-cleanup-{}", std::process::id()));
        fs::write(&path, "config sample 'sample'\n option value 'ok'\n").unwrap();
        let mut session = UciSession::new(&path).unwrap();
        let dir = session.dir.clone();
        assert!(export(&mut session.uci, "missing_package").is_err());
        assert!(dir.exists());
        drop(session);
        assert!(!dir.exists());
        let session = UciSession::new(&path).unwrap();
        let dir = session.dir.clone();
        drop(session);
        assert!(!dir.exists());
        fs::remove_file(path).unwrap();
    }
}
