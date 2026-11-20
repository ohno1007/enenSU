// Adapted from KernelSU v3.3.1, userspace/ksud/src/boot_patch.rs (GPL-3.0).
// Keep its ramdisk decode / replace / repack path, with Android's module list as payload.
use android_bootimg::{
    layouts::{
        AvbFooter, BOOT_HEADER_V3, BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4,
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

use crate::{archive::Archive, modules};

/// Feed the repacker only the image payload. An AVB descriptor authenticates the *old* bytes.
pub fn parse_image(input: &[u8]) -> Result<BootImage<'_>> {
    let mut payload = input;
    if let Some(data) = input
        .last_chunk::<{ AvbFooter::SIZE }>()
        .filter(|d| d.starts_with(b"AVBf"))
    {
        let footer = AvbFooter { data };
        let size = usize::try_from(footer.get_original_image_size())?;
        let offset = usize::try_from(footer.get_vbmeta_offset())?;
        let end = offset
            .checked_add(usize::try_from(footer.get_vbmeta_size())?)
            .context("invalid AVB size")?;
        ensure!(
            size <= offset
                && end <= input.len() - data.len()
                && input[offset..end].starts_with(b"AVB0"),
            "invalid AVB footer"
        );
        payload = &input[..size];
    }
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
    let image = BootImage::parse(&payload[..end])?;
    ensure!(
        partition(&image) == "vendor_boot" || image.get_blocks().get_kernel().is_some(),
        "a boot partition image must contain a kernel; check the source partition"
    );
    Ok(image)
}

pub fn partition(image: &BootImage<'_>) -> &'static str {
    if matches!(
        image.get_header().get_version(),
        BootImageVersion::Vendor(_)
    ) {
        "vendor_boot"
    } else {
        "boot"
    }
}

fn extract(ramdisk: &RamdiskImage<'_>, fragment: Option<&str>) -> Result<(Vec<u8>, Option<usize>)> {
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
            matches!(
                entry.get_entry_type(),
                Kind::None | Kind::Platform | Kind::Unknown(3)
            ) && fragment.is_none_or(|name| name.as_bytes() == entry.get_name_raw())
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
        bail!("select one normal-boot ramdisk with --fragment (matching names: {names:?})");
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

fn vendor_reference(path: &Path, fragment: Option<&str>) -> Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let image = parse_image(&bytes)?;
    ensure!(
        partition(&image) == "vendor_boot",
        "--vendor-boot expects a matching vendor_boot image"
    );
    let rd = image
        .get_blocks()
        .get_ramdisk()
        .context("missing vendor ramdisk")?;
    let (selected, index) = extract(rd, fragment)?;
    let mut parts = other_normal_parts(rd, index)?;
    parts.push(selected);
    Ok(parts)
}

pub fn patch(
    input: &[u8],
    files: &[PathBuf],
    enable: &[String],
    fragment: Option<&str>,
    module_dir: Option<&str>,
    vendor_boot: Option<&Path>,
) -> Result<Option<Vec<u8>>> {
    let image = parse_image(input)?;
    let is_boot = partition(&image) == "boot";
    ensure!(
        is_boot || vendor_boot.is_none(),
        "--vendor-boot is a read-only reference for patching boot"
    );
    if files.is_empty() && enable.is_empty() {
        return Ok(None);
    }
    let ramdisk = image.get_blocks().get_ramdisk().context("this image has no ramdisk for first-stage modules; use the device's vendor_boot module tree")?;
    let target_fragment = if is_boot && vendor_boot.is_some() {
        None
    } else {
        fragment
    };
    let (mut raw, index) = extract(ramdisk, target_fragment)?;
    let target = Archive::read(&raw).context("read ramdisk newc archive")?;
    let parts = vendor_boot
        .map(|path| vendor_reference(path, fragment))
        .transpose()?;
    let archive = match &parts {
        Some(parts) => Archive::vendor_base(parts)?.with_generic(target),
        None => target,
    };
    let dir = archive.module_dir(module_dir)?;
    if is_boot {
        ensure!(
            archive.file(&format!("{dir}/modules.load"))?.is_some()
                && archive.file(&format!("{dir}/modules.dep"))?.is_some(),
            "no stock first-stage module lists at {dir}; provide the matching --vendor-boot image or select the device's --module-dir"
        );
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
                    "another vendor fragment supplies {path}; select its owner or patch boot with the matching --vendor-boot"
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
