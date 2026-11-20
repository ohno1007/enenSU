use android_bootimg::cpio::{Cpio, CpioEntry, TYPE_DIR, TYPE_MASK, TYPE_REGULAR};
use anyhow::{Context, Result, bail, ensure};
use cpio::newc::{Entry, Reader};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
};

pub const BASE: &str = "lib/modules";
pub type Edits = BTreeMap<String, Vec<u8>>;

pub struct Archive<'a> {
    entries: BTreeMap<String, (Entry, &'a [u8])>,
}

impl<'a> Archive<'a> {
    pub fn read(raw: &'a [u8]) -> Result<Self> {
        let mut entries = BTreeMap::new();
        let mut pos = 0;
        let mut saw_entry = false;
        while let Some(padding) = raw[pos..].iter().position(|b| *b != 0) {
            let mut cursor = Cursor::new(raw);
            cursor.set_position((pos + padding) as u64);
            let mut reader = Reader::new(cursor)?;
            let start = reader.offset()? as usize;
            let entry = reader.entry();
            let end = start
                .checked_add(entry.file_size() as usize)
                .context("newc entry size overflow")?;
            let data = raw.get(start..end).context("truncated newc entry")?;
            let name = entry
                .name()
                .split('/')
                .filter(|c| !c.is_empty() && *c != ".")
                .collect::<Vec<_>>()
                .join("/");
            if name == "lib" || name == BASE || name.starts_with("lib/modules/") {
                entries.insert(name, (entry.clone(), data));
            }
            pos = reader.skip()?.position() as usize;
            ensure!(pos <= raw.len(), "truncated newc padding");
            saw_entry = true;
        }
        ensure!(saw_entry, "empty ramdisk archive");
        Ok(Self { entries })
    }

    /// Combine the module trees of the normal-boot vendor fragments. A bootloader
    /// can select and order fragments by board, so reject conflicting definitions.
    pub fn vendor_base(parts: &'a [Vec<u8>]) -> Result<Self> {
        let mut base = Self {
            entries: BTreeMap::new(),
        };
        for part in parts {
            for (path, record) in Self::read(part)?.entries {
                if let Some((old, bytes)) = base.entries.get(&path) {
                    ensure!(
                        old.mode() & TYPE_MASK == record.0.mode() & TYPE_MASK && *bytes == record.1,
                        "conflicting vendor module tree entries at {path}; use the image for the target board"
                    );
                } else {
                    base.entries.insert(path, record);
                }
            }
        }
        Ok(base)
    }

    /// Android unpacks the generic boot ramdisk after the vendor ramdisk.
    pub fn with_generic(mut self, generic: Self) -> Self {
        self.entries.extend(generic.entries);
        self
    }

    pub fn file(&self, path: &str) -> Result<Option<&'a [u8]>> {
        let Some((entry, data)) = self.entries.get(path) else {
            return Ok(None);
        };
        ensure!(
            entry.mode() & TYPE_MASK == TYPE_REGULAR && entry.nlink() <= 1,
            "expected a regular file without hard links at {path}"
        );
        Ok(Some(data))
    }

    pub fn text(&self, path: &str) -> Result<&'a str> {
        std::str::from_utf8(self.file(path)?.unwrap_or_default())
            .with_context(|| format!("invalid text in {path}"))
    }

    pub fn module_dir(&self, requested: Option<&str>) -> Result<String> {
        if let Some(dir) = requested {
            let dir = dir.trim_start_matches('/');
            let valid_version = dir.strip_prefix("lib/modules/").is_some_and(|v| {
                !v.is_empty()
                    && v != "."
                    && v != ".."
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-_+".contains(&b))
            });
            ensure!(
                dir == BASE || valid_version,
                "expected lib/modules or lib/modules/<version>"
            );
            return Ok(dir.to_owned());
        }
        let dirs: BTreeSet<_> = self
            .entries
            .keys()
            .filter_map(|path| path.rsplit_once('/'))
            .filter(|(_, name)| matches!(*name, "modules.load" | "modules.dep"))
            .map(|(dir, _)| dir)
            .collect();
        match dirs.len() {
            0 => Ok(BASE.to_owned()),
            1 => self.module_dir(dirs.first().copied()),
            _ => bail!("select a module directory with --module-dir: {dirs:?}"),
        }
    }

    pub fn overlay(&self, edits: Edits) -> Result<Option<Cpio>> {
        let mut overlay = Cpio::new();
        for (path, data) in edits {
            if self.file(&path)? == Some(data.as_slice()) {
                continue;
            }
            for (index, _) in path.match_indices('/') {
                let dir = &path[..index];
                if let Some((entry, _)) = self.entries.get(dir) {
                    ensure!(
                        entry.mode() & TYPE_MASK == TYPE_DIR,
                        "expected directory at {dir}"
                    );
                } else if !overlay.exists(dir) {
                    overlay.add(dir, CpioEntry::dir(0o755))?;
                }
            }
            let file = match self.entries.get(&path) {
                Some((old, _)) => CpioEntry::regular(old.mode() & 0o7777, Box::new(data))
                    .uid(old.uid())
                    .gid(old.gid()),
                None => CpioEntry::regular(0o644, Box::new(data)),
            };
            overlay.add(&path, file)?;
        }
        Ok((!overlay.entries().is_empty()).then_some(overlay))
    }
}
