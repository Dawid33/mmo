//! A folder of prefab files on disk: the player's library (`<data dir>/prefabs/`) or,
//! in debug builds, the game's own `assets/prefabs/`. Native only.
//!
//! A prefab named `n` is the file `<n>.prefab.ron`; names are checked with
//! [`prefab_name_ok`] before they touch a path, so a name can never leave the folder.

use std::path::{Path, PathBuf};

use super::{
    prefab_from_export, prefab_from_ron, prefab_name_ok, prefab_to_export, prefab_to_ron, resolve, unresolve,
    PrefabError, PrefabFile, PrefabKindTag, Problem, Trust, PREFAB_FILE_SUFFIX, PREFAB_NAME_MAX,
};

pub struct PrefabDir {
    root: PathBuf,
}

/// One listed prefab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefabEntry {
    pub name: String,
    pub kind: PrefabKindTag,
    /// Parts, boxes and carve boxes (Build); parts (Prop).
    pub elements: usize,
    /// The bounding box of a Build's cells (studs x, plates y, studs z); a Prop's
    /// footprint as `[x, 0, z]`.
    pub size: [u32; 3],
}

/// A folder's content: the prefabs that read, and the files that did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefabListing {
    /// By name, ascending.
    pub entries: Vec<PrefabEntry>,
    /// `(file name, why)` for every `*.prefab.ron` that is not a prefab named like its
    /// file, by file name.
    pub bad: Vec<(String, String)>,
}

fn io(e: std::io::Error) -> PrefabError {
    PrefabError::Io(e.to_string())
}

fn checked(name: &str) -> Result<&str, PrefabError> {
    if prefab_name_ok(name) { Ok(name) } else { Err(PrefabError::Problems(vec![Problem::BadName])) }
}

impl PrefabDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the prefab called `name` is (or would be) stored. `name` is not checked.
    pub fn path_of(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}{PREFAB_FILE_SUFFIX}"))
    }

    pub fn exists(&self, name: &str) -> bool {
        prefab_name_ok(name) && self.path_of(name).is_file()
    }

    /// Everything in the folder. A missing folder is an empty listing; a file that is
    /// not a prefab is reported in `bad` and does not stop the rest from listing.
    pub fn list(&self) -> PrefabListing {
        let mut out = PrefabListing::default();
        let Ok(dir) = std::fs::read_dir(&self.root) else { return out };
        for entry in dir.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else { continue };
            let Some(stem) = file_name.strip_suffix(PREFAB_FILE_SUFFIX) else { continue };
            let read = std::fs::read_to_string(entry.path()).map_err(io).and_then(|src| prefab_from_ron(&src));
            match read {
                Ok(file) if file.name == stem && prefab_name_ok(stem) => out.entries.push(PrefabEntry {
                    kind: file.kind_tag(),
                    elements: file.element_count(),
                    size: file.extent(),
                    name: file.name,
                }),
                Ok(file) => out.bad.push((file_name, format!("holds a prefab named {:?}", file.name))),
                Err(e) => out.bad.push((file_name, e.to_string())),
            }
        }
        out.entries.sort_by(|a, b| a.name.cmp(&b.name));
        out.bad.sort();
        out
    }

    pub fn load(&self, name: &str) -> Result<PrefabFile, PrefabError> {
        let src = std::fs::read_to_string(self.path_of(checked(name)?)).map_err(io)?;
        let file = prefab_from_ron(&src)?;
        if file.name != name {
            return Err(PrefabError::Syntax(format!("the file holds a prefab named {:?}", file.name)));
        }
        Ok(file)
    }

    /// Writes `file` as `<name>.prefab.ron`, replacing what was there. The folder is
    /// created; the text goes to a temp file first and is renamed into place, so a
    /// crash never leaves half a prefab.
    pub fn save(&self, file: &PrefabFile) -> Result<(), PrefabError> {
        let name = checked(&file.name)?;
        std::fs::create_dir_all(&self.root).map_err(io)?;
        let tmp = self.root.join(format!(".{name}{PREFAB_FILE_SUFFIX}.tmp"));
        std::fs::write(&tmp, prefab_to_ron(file)).map_err(io)?;
        std::fs::rename(&tmp, self.path_of(name)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(e)
        })
    }

    pub fn delete(&self, name: &str) -> Result<(), PrefabError> {
        std::fs::remove_file(self.path_of(checked(name)?)).map_err(io)
    }

    /// The first of `<base><suffix>`, `<base><suffix>-2`, `<base><suffix>-3`, … (an empty
    /// suffix starts at `<base>`) that names no file here; `base` is cut to keep the
    /// name within the length limit.
    fn free_name(&self, base: &str, suffix: &str) -> String {
        let mut n = 1u32;
        loop {
            let tail = if n == 1 { suffix.to_string() } else { format!("{suffix}-{n}") };
            let keep = PREFAB_NAME_MAX.saturating_sub(tail.len()).min(base.len());
            let candidate = format!("{}{tail}", &base[..keep]);
            if !self.path_of(&candidate).exists() {
                return candidate;
            }
            n += 1;
        }
    }

    /// `base` if no prefab here has that name, else `<base>-2`, `<base>-3`, …
    pub fn unused_name(&self, base: &str) -> String {
        self.free_name(base, "")
    }

    /// Saves a copy of `name` as `<name>-copy` (then `-copy-2`, …); returns the new name.
    pub fn duplicate(&self, name: &str) -> Result<String, PrefabError> {
        let mut file = self.load(name)?;
        file.name = self.free_name(name, "-copy");
        self.save(&file)?;
        Ok(file.name)
    }

    /// Decodes an export string, checks it as a player's prefab, and only then saves it
    /// under its own name (or `<name>-2`, … when that is taken); returns the name.
    /// What is saved is the checked prefab written back out, so nothing a player's
    /// prefab may not carry (brick `data`) reaches the library.
    pub fn import(&self, export: &str) -> Result<String, PrefabError> {
        let file = prefab_from_export(export)?;
        let resolved = resolve(&file, Trust::Player).map_err(PrefabError::Problems)?;
        let mut clean = unresolve(&resolved, file.meta);
        clean.name = self.free_name(&file.name, "");
        self.save(&clean)?;
        Ok(clean.name)
    }

    pub fn export(&self, name: &str) -> Result<String, PrefabError> {
        Ok(prefab_to_export(&self.load(name)?))
    }
}
