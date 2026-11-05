//! File access confinement for the headless MCP server (`filmcraft-cli mcp
//! --automation-read-root DIR --automation-write-root DIR`, as in PhotoCraft and EffectCraft): an
//! agent may only read files under a read root (or a write root) and only write under a write
//! root.
//!
//! Every engine command an agent runs ([`crate::FilmcraftMcp::run`], so also each step of
//! `command_batch` and the `media_import` tool) is checked before it runs:
//!
//! - the file parameters it reads ([`READS`]) and writes ([`WRITES`]) must be absolute paths
//!   inside the roots: no `..`, symbolic links resolved through the deepest existing ancestor;
//! - any other string parameter that is an absolute path is treated as a read;
//! - commands that would write somewhere they are not told ([`NEEDS_DESTINATION`]) need their
//!   destination parameter, and Save without a path writes the open project's own file, which
//!   must be inside a write root ([`Confinement::check_save_target`]);
//! - commands that act outside the project or delete files ([`REFUSED`], Make Offline with
//!   Delete Files) are refused.
//!
//! Known limit: media that an opened project already references is decoded from wherever it
//! is; confinement checks the paths an agent names, not paths stored inside project files.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::AutomationError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Commands refused while confined: they open other applications, write outside the project
/// (the auto-save and media cache folders) or delete files.
pub const REFUSED: &[&str] = &["file.reveal", "file.autoSaveNow", "mediaCache.clean"];

/// Commands that read files named by these parameters.
pub const READS: &[(&str, &[&str])] = &[
    ("file.open", &["path"]),
    ("file.import", &["path", "paths"]),
    ("file.importFromMediaBrowser", &["paths"]),
    ("file.importImageSequence", &["path"]),
    ("file.importAaf", &["path"]),
    ("file.mediaPropertiesFile", &["path"]),
    ("captions.import", &["path"]),
    ("labels.importPreset", &["path"]),
    ("lut.import", &["path"]),
    ("lumetri.setInputLut", &["path"]),
    ("lumetri.setLook", &["path"]),
    ("graphics.newFromFile", &["path"]),
    ("graphics.template.install", &["path"]),
    ("shortcuts.import", &["path"]),
    ("media.relink", &["path"]),
    ("media.replaceFootage", &["path"]),
    ("media.autoRelink", &["folder", "from", "to"]),
    ("media.search", &["folder"]),
    ("media.attachProxies", &["path", "paths"]),
    ("media.reconnectFullRes", &["path"]),
    ("presets.import", &["path"]),
    ("export.presets.import", &["path"]),
    ("mediaBrowser.list", &["path"]),
    ("mediaBrowser.navigate", &["path"]),
    ("mediaBrowser.select", &["paths"]),
    ("mediaBrowser.favorite", &["path"]),
    ("mediaBrowser.import", &["paths"]),
    ("mediaBrowser.openInSource", &["path"]),
    ("mediaBrowser.probe", &["path"]),
    ("edit.pasteMedia", &["paths"]),
];

/// Commands that write files named by these parameters.
pub const WRITES: &[(&str, &[&str])] = &[
    ("file.save", &["path"]),
    ("file.saveAs", &["path"]),
    ("file.saveCopy", &["path"]),
    ("file.saveAsTemplate", &["path"]),
    ("file.exportMedia", &["path"]),
    ("file.exportFrame", &["path"]),
    ("file.exportInterchange", &["path"]),
    ("file.exportEdl", &["path"]),
    ("file.exportSelectionProject", &["path"]),
    ("file.exportAle", &["path"]),
    ("file.exportOtio", &["path"]),
    ("file.exportFcp7Xml", &["path"]),
    ("file.exportFcpxml", &["path"]),
    ("file.exportAaf", &["path"]),
    ("file.exportOmf", &["path"]),
    ("markers.exportCsv", &["path"]),
    ("captions.export", &["path"]),
    ("labels.exportPreset", &["path"]),
    ("lut.export", &["path"]),
    ("lumetri.presetThumbnails", &["path"]),
    ("graphics.template.export", &["path"]),
    ("graphics.template.thumbnail", &["path"]),
    ("shortcuts.export", &["path"]),
    ("presets.export", &["path"]),
    ("export.presets.export", &["path"]),
    ("export.queue.add", &["path"]),
    ("export.quick", &["path"]),
    ("clip.extractAudio", &["dir"]),
    ("audio.voiceover.stop", &["dir"]),
    ("tts.create", &["dir"]),
    ("tts.edit", &["dir"]),
    ("tts.preview", &["path"]),
];

/// Commands whose destination is optional but which otherwise write to a default location
/// outside the agent's view (the project's folder, the media's folder, the app's data folder):
/// while confined they need it named.
pub const NEEDS_DESTINATION: &[(&str, &str)] = &[
    ("file.exportFrame", "path"),
    ("export.queue.add", "path"),
    ("export.quick", "path"),
    ("clip.extractAudio", "dir"),
    ("tts.create", "dir"),
    ("tts.edit", "dir"),
];

/// Parameters that hold text, names or vertices, never file paths: not checked as paths even
/// when they start with `/`.
const NOT_PATHS: &[&str] = &["text", "name", "label", "title", "comment", "description", "tapeName", "fileName", "query", "filter"];

/// The folders an agent may read and write.
#[derive(Clone, Debug, Default)]
pub struct Confinement {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

impl Confinement {
    /// Roots must be existing folders; they are stored resolved (symbolic links followed).
    pub fn new(read: &[PathBuf], write: &[PathBuf]) -> Result<Self, AutomationError> {
        let canon = |p: &PathBuf| -> Result<PathBuf, AutomationError> {
            let c = p.canonicalize().map_err(|e| AutomationError::BadRequest(format!("automation root {}: {e}", p.display())))?;
            if !c.is_dir() {
                return Err(AutomationError::BadRequest(format!("automation root {} is not a folder", p.display())));
            }
            Ok(c)
        };
        Ok(Self { read: read.iter().map(canon).collect::<Result<_, _>>()?, write: write.iter().map(canon).collect::<Result<_, _>>()? })
    }

    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read
    }

    pub fn write_roots(&self) -> &[PathBuf] {
        &self.write
    }

    /// Whether `path` may be read or written: absolute, without `..`, and inside a root once
    /// symbolic links in its existing part are resolved. Reads may use the write roots too.
    pub fn allows(&self, path: &Path, access: Access) -> bool {
        let Some(resolved) = resolve(path) else { return false };
        let roots: Box<dyn Iterator<Item = &PathBuf>> = match access {
            Access::Read => Box::new(self.read.iter().chain(self.write.iter())),
            Access::Write => Box::new(self.write.iter()),
        };
        roots.into_iter().any(|r| resolved.starts_with(r))
    }

    /// Refuse a command that would reach outside the roots. `id` and `params` as given to the
    /// engine.
    pub fn check(&self, id: &str, params: &Value) -> Result<(), AutomationError> {
        if REFUSED.contains(&id) {
            return Err(refused(id));
        }
        if id == "media.makeOffline" && params.get("deleteFiles").and_then(Value::as_bool) == Some(true) {
            return Err(AutomationError::BadRequest(
                "`media.makeOffline` with `deleteFiles` is not available while file access is confined (--automation-read-root/--automation-write-root)".into(),
            ));
        }
        for (cmd, key) in NEEDS_DESTINATION {
            if *cmd == id && params.get(*key).is_none_or(Value::is_null) {
                return Err(AutomationError::BadRequest(format!("`{id}`: give `{key}` (inside a write root) while file access is confined")));
            }
        }
        let reads = keys_for(READS, id);
        let writes = keys_for(WRITES, id);
        if let Some(obj) = params.as_object() {
            for (k, v) in obj {
                let access = if writes.contains(&k.as_str()) {
                    Some(Access::Write)
                } else if reads.contains(&k.as_str()) {
                    Some(Access::Read)
                } else {
                    None
                };
                match access {
                    Some(a) => self.check_value(id, k, v, a, true)?,
                    None => self.check_value(id, k, v, Access::Read, false)?,
                }
            }
        }
        Ok(())
    }

    /// Save without a path writes the open project's file: it must be inside a write root.
    pub fn check_save_target(&self, project_path: Option<&str>) -> Result<(), AutomationError> {
        match project_path {
            Some(p) if self.allows(Path::new(p), Access::Write) => Ok(()),
            Some(p) => {
                Err(AutomationError::BadRequest(format!("`file.save` would write {p}, outside the write roots; use `file.saveAs` with a path inside one")))
            }
            None => Err(AutomationError::BadRequest("`file.save`: the project has no file yet; give `path` (inside a write root)".into())),
        }
    }

    /// `declared`: `key` is one of the command's file parameters, so a relative path is refused
    /// too (it would resolve against the server's working folder). Otherwise only absolute
    /// strings are checked, as reads, recursing into objects and arrays.
    fn check_value(&self, id: &str, key: &str, v: &Value, access: Access, declared: bool) -> Result<(), AutomationError> {
        match v {
            Value::String(s) => {
                let looks_absolute = Path::new(s).is_absolute();
                if !declared && (NOT_PATHS.contains(&key) || !looks_absolute) {
                    return Ok(());
                }
                if s.is_empty() && declared {
                    return Ok(());
                }
                if self.allows(Path::new(s), access) {
                    Ok(())
                } else {
                    let (verb, roots) = match access {
                        Access::Read => ("read", "read or write roots"),
                        Access::Write => ("write", "write roots"),
                    };
                    Err(AutomationError::BadRequest(format!(
                        "`{id}`: `{key}` {s} is outside the {roots}; file access is confined (it may only {verb} absolute paths inside them)"
                    )))
                }
            }
            Value::Array(items) => items.iter().try_for_each(|i| self.check_value(id, key, i, access, declared)),
            Value::Object(obj) => obj.iter().try_for_each(|(k, i)| {
                let nested_declared = declared && matches!(i, Value::String(_) | Value::Array(_)) && k == key;
                self.check_value(id, k, i, if nested_declared { access } else { Access::Read }, nested_declared)
            }),
            _ => Ok(()),
        }
    }
}

fn keys_for(table: &'static [(&'static str, &'static [&'static str])], id: &str) -> &'static [&'static str] {
    table.iter().find(|(c, _)| *c == id).map_or(&[], |(_, k)| k)
}

fn refused(id: &str) -> AutomationError {
    AutomationError::BadRequest(format!("`{id}` is not available while file access is confined (--automation-read-root/--automation-write-root)"))
}

/// `path` made absolute and free of symbolic links as far as it exists: `None` for a relative
/// path or one with `..` (which could climb out of a root through a link).
fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    // The deepest ancestor that exists, resolved; the rest (not yet created) appended as is.
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        let name = existing.file_name()?.to_os_string();
        rest.push(name);
        if !existing.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Roots {
        _dir: tempfile::TempDir,
        base: PathBuf,
        c: Confinement,
    }

    fn roots() -> Roots {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        for d in ["in", "out", "other"] {
            std::fs::create_dir(base.join(d)).unwrap();
        }
        let c = Confinement::new(&[base.join("in")], &[base.join("out")]).unwrap();
        Roots { _dir: dir, base, c }
    }

    fn p(r: &Roots, rel: &str) -> String {
        r.base.join(rel).to_string_lossy().into_owned()
    }

    #[test]
    fn reads_inside_read_or_write_roots_only() {
        let r = roots();
        assert!(r.c.check("file.import", &json!({"paths": [p(&r, "in/a.mp4"), p(&r, "out/b.mp4")]})).is_ok());
        let e = r.c.check("file.import", &json!({"paths": [p(&r, "in/a.mp4"), p(&r, "other/c.mp4")]})).unwrap_err();
        assert!(e.to_string().contains("outside the read or write roots"), "{e}");
        assert!(r.c.check("file.open", &json!({"path": "/etc/passwd"})).is_err());
    }

    #[test]
    fn writes_inside_write_roots_only() {
        let r = roots();
        assert!(r.c.check("file.exportMedia", &json!({"path": p(&r, "out/edit.mp4"), "format": "h264"})).is_ok());
        assert!(r.c.check("file.exportMedia", &json!({"path": p(&r, "in/edit.mp4")})).is_err(), "a read root is not writable");
        assert!(r.c.check("file.saveAs", &json!({"path": p(&r, "other/p.fcproj")})).is_err());
        // a file not created yet, in a folder not created yet, inside the root
        assert!(r.c.check("file.saveAs", &json!({"path": p(&r, "out/new/deeper/p.fcproj")})).is_ok());
    }

    #[test]
    fn relative_and_parent_paths_are_refused() {
        let r = roots();
        assert!(r.c.check("file.saveAs", &json!({"path": "p.fcproj"})).is_err());
        assert!(r.c.check("file.saveAs", &json!({"path": p(&r, "out/../other/p.fcproj")})).is_err());
        assert!(r.c.check("file.import", &json!({"paths": ["clip.mp4"]})).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_out_of_a_root_are_followed() {
        let r = roots();
        std::os::unix::fs::symlink(r.base.join("other"), r.base.join("out/escape")).unwrap();
        assert!(r.c.check("file.exportMedia", &json!({"path": p(&r, "out/escape/x.mp4")})).is_err());
        std::os::unix::fs::symlink(r.base.join("other"), r.base.join("in/peek")).unwrap();
        assert!(r.c.check("file.open", &json!({"path": p(&r, "in/peek/p.fcproj")})).is_err());
    }

    #[test]
    fn other_absolute_paths_are_reads_but_text_is_not_a_path() {
        let r = roots();
        assert!(r.c.check("lumetri.presets", &json!({"folder": "/Library/LUTs"})).is_err());
        assert!(r.c.check("lumetri.presets", &json!({"folder": p(&r, "in")})).is_ok());
        assert!(r.c.check("graphics.newText", &json!({"text": "/etc/passwd is a file"})).is_ok());
        assert!(r.c.check("effects.list", &json!({"folder": "Video Transitions/Wipe"})).is_ok());
        // nested settings objects are walked
        assert!(r.c.check("file.exportMedia", &json!({"path": p(&r, "out/a.mp4"), "settings": {"lut": "/opt/x.cube"}})).is_err());
        // vertices are not strings
        assert!(r.c.check("masks.add", &json!({"shape": "polygon", "path": [[0, 0], [10, 0], [10, 10]]})).is_ok());
    }

    #[test]
    fn refused_and_destination_rules() {
        let r = roots();
        for id in REFUSED {
            assert!(r.c.check(id, &json!({})).is_err(), "{id}");
        }
        assert!(r.c.check("media.makeOffline", &json!({"deleteFiles": true})).is_err());
        assert!(r.c.check("media.makeOffline", &json!({})).is_ok());
        assert!(r.c.check("export.quick", &json!({})).is_err());
        assert!(r.c.check("export.quick", &json!({"path": p(&r, "out/q.mp4")})).is_ok());
        assert!(r.c.check("clip.extractAudio", &json!({"dir": p(&r, "out")})).is_ok());
        assert!(r.c.check("clip.extractAudio", &json!({})).is_err());
    }

    #[test]
    fn save_writes_the_project_file() {
        let r = roots();
        assert!(r.c.check_save_target(Some(&p(&r, "out/p.fcproj"))).is_ok());
        assert!(r.c.check_save_target(Some(&p(&r, "in/p.fcproj"))).is_err());
        assert!(r.c.check_save_target(None).is_err());
    }

    #[test]
    fn roots_must_exist() {
        assert!(Confinement::new(&[PathBuf::from("/definitely/not/here")], &[]).is_err());
    }
}
