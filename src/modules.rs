use crate::archive::{Archive, Edits};
use anyhow::{Context, Result, bail, ensure};
use object::{Architecture, Object, ObjectKind, ObjectSection};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    str,
};

fn name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".ko").unwrap_or(file).replace('-', "_")
}

#[derive(PartialEq)]
struct Module {
    name: String,
    arch: Architecture,
    vermagic: String,
    depends: Vec<String>,
    softdeps: Vec<String>,
}

impl Module {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let elf = object::File::parse(bytes).context("expected an ELF kernel module")?;
        ensure!(
            elf.kind() == ObjectKind::Relocatable,
            "expected a relocatable .ko"
        );
        let info = elf
            .section_by_name(".modinfo")
            .context("missing .modinfo section")?
            .data()?;
        let fields: Vec<_> = info
            .split(|b| *b == 0)
            .filter_map(|s| str::from_utf8(s).ok())
            .collect();
        let field = |key| fields.iter().find_map(|s| s.strip_prefix(key));
        let id = field("name=").context("missing module name")?;
        ensure!(
            id.len() > 1
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "invalid module name {id:?}"
        );
        let vermagic = field("vermagic=")
            .filter(|v| !v.is_empty())
            .context("missing module vermagic")?;
        let mut depends: Vec<_> = field("depends=")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(name)
            .collect();
        depends.sort();
        depends.dedup();
        let mut softdeps = Vec::new();
        for value in fields.iter().filter_map(|s| s.strip_prefix("softdep=")) {
            ensure!(!value.contains(['\n', '\r', '#']), "invalid softdep field");
            softdeps.push(value.to_owned());
        }
        Ok(Self {
            name: name(id),
            arch: elf.architecture(),
            vermagic: vermagic.to_owned(),
            depends,
            softdeps,
        })
    }
}

fn kernel_family(release: &str) -> Option<(u32, u32)> {
    let mut numbers = release.split(['.', '-']);
    Some((numbers.next()?.parse().ok()?, numbers.next()?.parse().ok()?))
}

fn android_kmi(release: &str) -> Option<(&str, Option<u32>)> {
    let mut fields = release.split('-');
    let version = fields.find_map(|s| {
        s.strip_prefix("android")
            .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
    })?;
    Some((version, fields.next().and_then(|s| s.parse().ok())))
}

/// Reject detectable architecture/KMI mismatches before rebooting an attached device.
pub fn check_device(bytes: &[u8], filename: &str, machine: &str, kernel: &str) -> Result<String> {
    let module = Module::parse(bytes)?;
    ensure!(
        filename.ends_with(".ko") && name(filename) == module.name,
        "{filename} must match internal module name {}",
        module.name
    );
    let arch = match machine {
        "aarch64" | "arm64" => Architecture::Aarch64,
        "x86_64" => Architecture::X86_64,
        "i386" | "i486" | "i586" | "i686" => Architecture::I386,
        "riscv64" => Architecture::Riscv64,
        m if m.starts_with("armv") => Architecture::Arm,
        _ => bail!("unrecognized device architecture {machine}"),
    };
    ensure!(
        module.arch == arch,
        "{filename} architecture does not match {machine}"
    );
    let mut fields = module.vermagic.split_whitespace();
    let release = fields.next().context("empty module vermagic")?;
    if release != kernel {
        ensure!(
            fields.any(|word| word == "modversions")
                && kernel_family(release).is_some()
                && kernel_family(release) == kernel_family(kernel),
            "{filename} targets {release}, device runs {kernel}"
        );
        if let (Some((android, generation)), Some((device_android, device_generation))) =
            (android_kmi(release), android_kmi(kernel))
        {
            ensure!(
                android == device_android
                    && (generation.is_none()
                        || device_generation.is_none()
                        || generation == device_generation),
                "{filename} KMI {release} differs from device {kernel}"
            );
        }
        if let (Some(page), Some(device_page)) =
            (release.rsplit('-').next(), kernel.rsplit('-').next())
        {
            ensure!(
                !(matches!(page, "4k" | "16k" | "64k")
                    && matches!(device_page, "4k" | "16k" | "64k")
                    && page != device_page),
                "{filename} page size differs from device {kernel}"
            );
        }
    }
    Ok(module.name)
}

struct Indexed {
    path: String,
    depends: Vec<String>,
}

impl Indexed {
    fn archive_path(&self, dir: &str) -> Result<String> {
        let path = if let Some(absolute) = self.path.strip_prefix('/') {
            absolute.to_owned()
        } else {
            format!("{dir}/{}", self.path)
        };
        ensure!(
            path.starts_with(&format!("{dir}/"))
                && path.ends_with(".ko")
                && path.split('/').all(|c| !matches!(c, "" | "." | "..")),
            "module index refers outside {dir}: {}",
            self.path
        );
        Ok(path)
    }
}

type Index = BTreeMap<String, Indexed>;

fn parse_index(text: &str) -> Result<Index> {
    let mut index = Index::new();
    for line in text
        .lines()
        .map(|s| s.split('#').next().unwrap().trim())
        .filter(|s| !s.is_empty())
    {
        let (path, rest) = line.split_once(':').context("invalid modules.dep line")?;
        let entry = Indexed {
            path: path.to_owned(),
            depends: rest.split_whitespace().map(name).collect(),
        };
        // Android libmodprobe uses the last mapping for a canonical module name.
        index.insert(name(path), entry);
    }
    Ok(index)
}

/// Android's parallel loader also uses the flattened, depmod-style order.
fn load_order<'a>(
    id: &'a str,
    index: &'a Index,
    builtin: &BTreeSet<String>,
    visiting: &mut BTreeSet<&'a str>,
    done: &mut BTreeSet<&'a str>,
    paths: &mut Vec<&'a str>,
) -> Result<()> {
    if done.contains(id) || builtin.contains(id) {
        return Ok(());
    }
    let module = index
        .get(id)
        .with_context(|| format!("missing dependency {id}: select its .ko as well"))?;
    ensure!(
        visiting.insert(id),
        "kernel module dependency cycle at {id}"
    );
    for dep in module.depends.iter().rev() {
        load_order(dep, index, builtin, visiting, done, paths)?;
    }
    visiting.remove(id);
    done.insert(id);
    paths.push(&module.path);
    Ok(())
}

fn append_line(text: &mut String, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
}

pub fn plan(
    archive: &Archive<'_>,
    dir: &str,
    files: &[PathBuf],
    enable: &[String],
) -> Result<Edits> {
    let dep_file = format!("{dir}/modules.dep");
    let load_file = format!("{dir}/modules.load");
    let soft_file = format!("{dir}/modules.softdep");
    let mut deps = archive.text(&dep_file)?.to_owned();
    let mut index = parse_index(&deps)?;
    let mut load = archive.text(&load_file)?.to_owned();
    let mut loaded: BTreeSet<_> = load
        .lines()
        .filter_map(|s| s.split('#').next()?.split_whitespace().next())
        .map(name)
        .collect();
    let builtin: BTreeSet<_> = archive
        .text(&format!("{dir}/modules.builtin"))?
        .split_whitespace()
        .map(name)
        .collect();
    let mut selected: Vec<String> = enable.iter().map(|s| name(s)).collect();
    let mut new = Vec::new();
    let mut edits = Edits::new();
    let mut soft = archive.text(&soft_file)?.to_owned();
    let mut arch = index
        .values()
        .filter_map(|entry| entry.archive_path(dir).ok())
        .find_map(|p| {
            let bytes = archive.file(&p).ok().flatten()?;
            object::File::parse(bytes)
                .ok()
                .map(|elf| elf.architecture())
        });
    for file in files {
        let filename = file
            .file_name()
            .and_then(|s| s.to_str())
            .context("expected a .ko filename")?;
        let bytes = fs::read(file).with_context(|| format!("read {}", file.display()))?;
        let module = Module::parse(&bytes).with_context(|| format!("invalid {filename}"))?;
        let id = &module.name;
        ensure!(
            filename.ends_with(".ko") && name(filename) == *id,
            "{filename} must match internal module name {id}"
        );
        ensure!(
            !builtin.contains(id),
            "{id} is already built into the target kernel"
        );
        ensure!(
            *arch.get_or_insert(module.arch) == module.arch,
            "different CPU architecture in {filename}"
        );
        let path = if let Some(entry) = index.get(id) {
            let path = entry.archive_path(dir)?;
            let existing = edits
                .get(&path)
                .map(Vec::as_slice)
                .or(archive.file(&path)?)
                .with_context(|| format!("missing indexed module {path}"))?;
            if existing != bytes {
                ensure!(!edits.contains_key(&path), "conflicting inputs for {id}");
                let old = Module::parse(existing)?;
                ensure!(
                    old == module,
                    "incompatible replacement for {id} (name, architecture, vermagic or dependencies differ)"
                );
            }
            path
        } else {
            let path = format!("{dir}/{filename}");
            ensure!(
                archive.file(&path)?.is_none_or(|old| old == bytes),
                "conflict with unindexed file {path}"
            );
            index.insert(
                id.clone(),
                Indexed {
                    path: filename.to_owned(),
                    depends: module.depends,
                },
            );
            for value in module.softdeps {
                let line = format!("softdep {id} {value}");
                if !soft.lines().any(|s| s == line) {
                    append_line(&mut soft, &line);
                }
            }
            new.push(id.clone());
            path
        };
        edits.insert(path, bytes);
        selected.push(id.clone());
    }
    for id in new {
        let mut ordered = Vec::new();
        load_order(
            &id,
            &index,
            &builtin,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut ordered,
        )?;
        let path = ordered.pop().context("empty module dependency order")?;
        ordered.reverse();
        let mut line = format!("{path}:");
        for dep in ordered {
            line.push(' ');
            line.push_str(dep);
        }
        append_line(&mut deps, &line);
    }
    for id in selected {
        let Some(entry) = index.get(&id) else {
            bail!("{id} is not in {dep_file}")
        };
        let path = entry.archive_path(dir)?;
        ensure!(
            edits.contains_key(&path) || archive.file(&path)?.is_some(),
            "missing indexed module {path}"
        );
        if loaded.insert(id) {
            append_line(&mut load, &entry.path);
        }
    }
    edits.insert(dep_file, deps.into_bytes());
    edits.insert(load_file, load.into_bytes());
    if !soft.is_empty() {
        edits.insert(soft_file, soft.into_bytes());
    }
    Ok(edits)
}
