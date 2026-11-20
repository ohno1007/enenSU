// Adapted from KernelSU v3.3.1, userspace/ksud/src/boot_patch.rs (GPL-3.0).
// Keep its ramdisk decode / replace / repack path, with Android's module list as payload.
use android_bootimg::{
    layouts::{
        BOOT_HEADER_V3, BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4,
        VendorRamdiskTableEntryType as Kind,
    },
    parser::{BootHeader, BootImage, BootImageBlocks, BootImageVersion, RamdiskImage},
    patcher::BootImagePatchOption,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use crate::{archive::Archive, avb, modules};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Boot,
    InitBoot,
    Vendor,
}

impl Layout {
    fn accepts(self, target: &str) -> bool {
        matches!(
            (self, target),
            (Self::Boot, "boot")
                | (Self::InitBoot, "init_boot")
                | (Self::Vendor, "vendor_boot" | "vendor_kernel_boot")
        )
    }
}

pub struct ParsedImage<'a> {
    boot: BootImage<'a>,
    layout: Layout,
    pub avb: avb::Metadata<'a>,
}

fn vendor_filename(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?;
    ["vendor_boot", "vendor_kernel_boot"]
        .into_iter()
        .find(|name| {
            stem.strip_prefix(name)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['-', '_', '.']))
        })
}

impl<'image> ParsedImage<'image> {
    fn partition_label(&self, path: &Path) -> Option<&'image str> {
        match self.layout {
            Layout::Vendor => self.avb.partition.or_else(|| vendor_filename(path)),
            _ => self.avb.partition,
        }
    }

    pub fn check_target(&self, target: &str, path: &Path) -> Result<()> {
        ensure!(
            self.layout.accepts(target),
            "image header does not match partition {target}"
        );
        // The two vendor partitions share a header format. Prefer the AVB label;
        // honor conventional factory filenames when the input is unsigned.
        if let Some(label) = self.partition_label(path) {
            ensure!(target == label, "image is labeled {label}, not {target}");
        }
        Ok(())
    }

    pub fn source_partition(&self, path: &Path) -> Result<&str> {
        match self.layout {
            Layout::Boot => Ok("boot"),
            Layout::InitBoot => Ok("init_boot"),
            Layout::Vendor => self.partition_label(path).context(
                "vendor image has no partition label or standard filename; specify --partition",
            ),
        }
    }

    pub fn has_ramdisk(&self) -> bool {
        self.boot.get_blocks().get_ramdisk().is_some()
    }

    /// Find the native normal-boot module list before choosing a partition to flash.
    pub fn stock_module_dir(
        &self,
        fragment: Option<&str>,
        requested: Option<&str>,
        kernel: &str,
    ) -> Result<Option<String>> {
        let Some(rd) = self.boot.get_blocks().get_ramdisk() else {
            return Ok(None);
        };
        if fragment.is_none()
            && rd.is_vendor_ramdisk()
            && !rd
                .iter_vendor_ramdisk()
                .any(|p| normal_fragment(p.get_entry_type()))
        {
            return Ok(None);
        }
        let (raw, _) = extract(rd, fragment, "--fragment")?;
        if raw.is_empty() {
            return Ok(None);
        }
        let archive = Archive::read(&raw)?;
        let release_dir = format!("lib/modules/{kernel}");
        let requested = match requested {
            None if archive
                .file(&format!("{release_dir}/modules.load"))?
                .is_some() =>
            {
                Some(release_dir.as_str())
            }
            other => other,
        };
        let dir = archive.module_dir(requested)?;
        Ok((archive.file(&format!("{dir}/modules.dep"))?.is_some()
            && archive.file(&format!("{dir}/modules.load"))?.is_some())
        .then_some(dir))
    }

    pub fn flash_target<'a>(&self, requested: Option<&'a str>, path: &Path) -> Result<&'a str> {
        let target = match requested {
            Some(target) => target,
            None if self.partition_label(path) == Some("vendor_boot") => "vendor_boot",
            None => bail!(
                "specify --partition for the source firmware partition (boot, init_boot, vendor_boot or vendor_kernel_boot)"
            ),
        };
        self.check_target(target, path)?;
        Ok(target)
    }
}

/// Feed the repacker only the original payload; its old AVB descriptor covers the old bytes.
pub fn parse_image(input: &[u8]) -> Result<ParsedImage<'_>> {
    let (payload, avb) = avb::payload(input)?;
    let word = |offset: usize| -> Result<u32> {
        let bytes = payload
            .get(offset..offset + 4)
            .context("truncated boot image header")?;
        Ok(u32::from_le_bytes(bytes.try_into()?))
    };
    let layout = if payload.starts_with(b"VNDRBOOT") {
        ensure!(word(12)?.is_power_of_two(), "invalid vendor_boot page size");
        match word(8)? {
            3 => VENDOR_BOOT_HEADER_V3,
            4 => VENDOR_BOOT_HEADER_V4,
            v => bail!("unsupported vendor_boot version {v}"),
        }
    } else if payload.starts_with(b"ANDROID!") {
        match word(40)? {
            3 => BOOT_HEADER_V3,
            4 => BOOT_HEADER_V4,
            v => bail!("unsupported boot version {v} (expected v3 or v4)"),
        }
    } else {
        bail!("expected an Android boot or vendor_boot image");
    };
    ensure!(
        payload.len() >= usize::from(layout.total_size),
        "truncated boot image header"
    );
    let header = BootHeader::parse(payload)?;
    ensure!(
        header.hdr_space() <= payload.len(),
        "truncated boot image header"
    );
    let (_, end) = BootImageBlocks::parse(payload, &header)?;
    let end = end.min(payload.len());
    ensure!(
        payload[end..].iter().all(|b| *b == 0),
        "unrecognized data after image sections (use the pre-signing device build artifact)"
    );
    let boot = BootImage::parse(&payload[..end])?;
    let layout = match header.get_version() {
        BootImageVersion::Vendor(_) => Layout::Vendor,
        BootImageVersion::Android(_) if boot.get_blocks().get_kernel().is_some() => Layout::Boot,
        BootImageVersion::Android(4) if boot.get_blocks().get_ramdisk().is_some() => {
            Layout::InitBoot
        }
        _ => bail!("expected a kernel in boot or a nonempty v4 ramdisk in init_boot"),
    };
    if let Some(target) = avb.partition {
        ensure!(
            layout.accepts(target),
            "unsupported or conflicting AVB partition label {target}"
        );
    }
    Ok(ParsedImage { boot, layout, avb })
}

fn normal_fragment(kind: Kind) -> bool {
    matches!(kind, Kind::None | Kind::Platform | Kind::Unknown(3))
}

fn extract(
    ramdisk: &RamdiskImage<'_>,
    fragment: Option<&str>,
    flag: &str,
) -> Result<(Vec<u8>, Option<usize>)> {
    let mut bytes = Vec::new();
    if !ramdisk.is_vendor_ramdisk() {
        ensure!(
            fragment.is_none(),
            "this image has one ramdisk and no fragment table"
        );
        ramdisk.dump(&mut bytes, false)?;
        return Ok((bytes, None));
    }
    // Type 3 is VENDOR_RAMDISK_TYPE_DLKM in AOSP. The pinned parser exposes it as Unknown(3).
    let mut candidates: Vec<_> = ramdisk
        .iter_vendor_ramdisk()
        .enumerate()
        .filter(|(_, entry)| {
            normal_fragment(entry.get_entry_type())
                && fragment.is_none_or(|name| name.as_bytes() == entry.get_name_raw())
        })
        .collect();
    if fragment.is_none()
        && candidates
            .iter()
            .any(|(_, e)| matches!(e.get_entry_type(), Kind::Unknown(3)))
    {
        candidates.retain(|(_, e)| matches!(e.get_entry_type(), Kind::Unknown(3)));
    }
    let [(idx, entry)] = candidates.as_slice() else {
        let names: Vec<_> = candidates
            .iter()
            .map(|(_, e)| String::from_utf8_lossy(e.get_name_raw()))
            .collect();
        bail!("select one normal-boot ramdisk with {flag} (matching names: {names:?})");
    };
    entry.dump(&mut bytes, false)?;
    Ok((bytes, Some(*idx)))
}

fn other_normal_parts(rd: &RamdiskImage<'_>, index: Option<usize>) -> Result<Vec<Vec<u8>>> {
    let selected_dlkm = index
        .and_then(|i| rd.get_vendor_ramdisk(i))
        .is_some_and(|p| matches!(p.get_entry_type(), Kind::Unknown(3)));
    let mut parts = Vec::new();
    for (i, part) in rd.iter_vendor_ramdisk().enumerate() {
        let relevant = matches!(part.get_entry_type(), Kind::Platform | Kind::None)
            || (!selected_dlkm && matches!(part.get_entry_type(), Kind::Unknown(3)));
        if Some(i) != index && relevant && !part.get_data().is_empty() {
            let mut data = Vec::new();
            part.dump(&mut data, false)?;
            parts.push(data);
        }
    }
    Ok(parts)
}

fn vendor_reference(
    path: &Path,
    expected: &str,
    fragment: Option<&str>,
    flag: &str,
) -> Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let parsed = parse_image(&bytes)?;
    parsed.check_target(expected, path)?;
    let Some(rd) = parsed.boot.get_blocks().get_ramdisk() else {
        ensure!(
            fragment.is_none(),
            "{expected} has no ramdisk fragment {fragment:?}"
        );
        return Ok(Vec::new());
    };
    if fragment.is_none()
        && rd.is_vendor_ramdisk()
        && !rd
            .iter_vendor_ramdisk()
            .any(|p| normal_fragment(p.get_entry_type()))
    {
        return Ok(Vec::new());
    }
    let (selected, index) = extract(rd, fragment, flag)?;
    let mut parts = other_normal_parts(rd, index)?;
    if !selected.is_empty() {
        parts.push(selected);
    }
    Ok(parts)
}

pub struct VendorReferences<'a> {
    pub boot: Option<&'a Path>,
    pub kernel_boot: Option<&'a Path>,
    pub boot_fragment: Option<&'a str>,
}

#[derive(Debug)]
pub struct MissingStockLists(String);

impl std::fmt::Display for MissingStockLists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no stock first-stage module lists at {}; provide the matching --vendor-boot or --vendor-kernel-boot image, or select the device's --module-dir",
            self.0
        )
    }
}
impl std::error::Error for MissingStockLists {}

pub fn patch(
    input: &[u8],
    files: &[PathBuf],
    enable: &[String],
    fragment: Option<&str>,
    module_dir: Option<&str>,
    vendor: VendorReferences<'_>,
) -> Result<Option<Vec<u8>>> {
    let parsed = parse_image(input)?;
    let is_generic = parsed.layout != Layout::Vendor;
    let image = parsed.boot;
    let has_reference = vendor.boot.is_some() || vendor.kernel_boot.is_some();
    ensure!(
        is_generic || !has_reference,
        "vendor reference images are for patching boot or init_boot"
    );
    if files.is_empty() && enable.is_empty() {
        return Ok(None);
    }
    let ramdisk = image.get_blocks().get_ramdisk().context("this image has no ramdisk for first-stage modules; use the device's init_boot or vendor module image")?;
    let (mut raw, index) = extract(
        ramdisk,
        if has_reference { None } else { fragment },
        "--fragment",
    )?;
    let target = Archive::read(&raw).context("read ramdisk newc archive")?;
    let mut parts = Vec::new();
    if let Some(path) = vendor.boot {
        let (select, flag) = if vendor.kernel_boot.is_some() {
            (vendor.boot_fragment, "--vendor-boot-fragment")
        } else {
            (fragment, "--fragment")
        };
        parts.extend(vendor_reference(path, "vendor_boot", select, flag)?);
    }
    if let Some(path) = vendor.kernel_boot {
        parts.extend(vendor_reference(
            path,
            "vendor_kernel_boot",
            fragment,
            "--fragment",
        )?);
    }
    let archive = if parts.is_empty() {
        target
    } else {
        Archive::vendor_base(&parts)?.with_generic(target)
    };
    let dir = archive.module_dir(module_dir)?;
    if is_generic
        && (archive.file(&format!("{dir}/modules.load"))?.is_none()
            || archive.file(&format!("{dir}/modules.dep"))?.is_none())
    {
        return Err(MissingStockLists(dir).into());
    }
    let edits = modules::plan(&archive, &dir, files, enable)?;
    if index.is_some() {
        // Do not shadow an OEM list from a different normal-boot vendor fragment.
        let peers = other_normal_parts(ramdisk, index)?;
        let peers = Archive::vendor_base(&peers)?;
        for (path, data) in &edits {
            if archive.file(path)? != Some(data) && peers.file(path)?.is_some_and(|old| old != data)
            {
                bail!(
                    "another vendor fragment supplies {path}; select its owner or patch the generic ramdisk with the matching vendor references"
                );
            }
        }
    }
    let Some(overlay) = archive.overlay(edits)? else {
        return Ok(None);
    };

    // Linux accepts concatenated newc archives. Preserve original entries verbatim,
    // including hard links, timestamps, device nodes and files outside the module tree.
    raw.resize(raw.len().next_multiple_of(4), 0);
    overlay.dump(&mut raw)?;
    let mut patcher = BootImagePatchOption::new(&image);
    if let Some(idx) = index {
        patcher.replace_vendor_ramdisk(idx, Box::new(Cursor::new(raw)), false);
    } else {
        patcher.replace_ramdisk(Box::new(Cursor::new(raw)), false);
    }
    let mut output = Cursor::new(Vec::with_capacity(image.get_size()));
    patcher.patch(&mut output)?;
    let mut bytes = output.into_inner();
    // The v4 legacy GKI certificate authenticates the old boot payload. Return
    // the unsigned image for the same device build to certify and AVB-sign again.
    let header = image.get_header();
    if matches!(header.get_version(), BootImageVersion::Android(4))
        && header.get_signature_size() > 0
    {
        let patched = BootHeader::parse(&bytes)?;
        let page = patched.page_size();
        let signature_start = patched.hdr_space()
            + (patched.get_kernel_size() as usize).next_multiple_of(page)
            + (patched.get_ramdisk_size() as usize).next_multiple_of(page);
        bytes.truncate(signature_start);
        let offset = usize::from(BOOT_HEADER_V4.offset_signature_size);
        bytes[offset..offset + 4].fill(0);
    }
    Ok(Some(bytes))
}
