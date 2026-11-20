use android_bootimg::layouts::{AVB_HEADER_SIZE, AvbFooter};
use anyhow::{Context, Result, ensure};

fn block(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    bytes
        .get(offset..)
        .and_then(|rest| rest.get(..size))
        .context("truncated AVB metadata")
}

fn be64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(block(bytes, offset, 8)?.try_into()?))
}

fn be32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(block(bytes, offset, 4)?.try_into()?))
}

// AOSP libavb's AvbVBMetaImageHeader, AvbDescriptor and AvbHashDescriptor.
// This reads the label for routing, not the device's cryptographic trust decision.
fn partition_label(vbmeta: &[u8], image_size: u64) -> Result<Option<&str>> {
    ensure!(vbmeta.starts_with(b"AVB0"), "invalid AVB metadata magic");
    let blocks = vbmeta
        .get(AVB_HEADER_SIZE..)
        .context("truncated AVB header")?;
    let auxiliary = block(
        blocks,
        be64(vbmeta, 12)?.try_into()?,
        be64(vbmeta, 20)?.try_into()?,
    )?;
    let mut descriptors = block(
        auxiliary,
        be64(vbmeta, 96)?.try_into()?,
        be64(vbmeta, 104)?.try_into()?,
    )?;
    let mut label = None;
    while !descriptors.is_empty() {
        let tag = be64(descriptors, 0)?;
        let size: usize = be64(descriptors, 8)?.try_into()?;
        ensure!(size.is_multiple_of(8), "unaligned AVB descriptor");
        let (body, rest) = descriptors[16..]
            .split_at_checked(size)
            .context("truncated AVB descriptor")?;
        descriptors = rest;
        if tag != 2 || be64(body, 0)? != image_size {
            continue;
        }
        const HASH_FIELDS_SIZE: usize = 116; // Excludes the 16-byte descriptor header.
        let name_len = be32(body, 40)?;
        let salt_len = be32(body, 44)?;
        let digest_len = be32(body, 48)?;
        let variable_size = u64::from(name_len) + u64::from(salt_len) + u64::from(digest_len);
        ensure!(
            (HASH_FIELDS_SIZE as u64 + variable_size).next_multiple_of(8) == size as u64,
            "invalid AVB hash descriptor lengths"
        );
        let name = std::str::from_utf8(block(body, HASH_FIELDS_SIZE, name_len.try_into()?)?)?;
        ensure!(
            !name.is_empty() && !name.contains('\0'),
            "invalid AVB partition name"
        );
        ensure!(
            label.is_none_or(|old| old == name),
            "ambiguous AVB partition label"
        );
        label = Some(name);
    }
    Ok(label)
}

/// Return the image payload and the partition named by its embedded hash descriptor, if present.
pub fn payload(bytes: &[u8]) -> Result<(&[u8], Option<&str>)> {
    let Some(data) = bytes
        .last_chunk::<{ AvbFooter::SIZE }>()
        .filter(|d| d.starts_with(b"AVBf"))
    else {
        return Ok((bytes, None));
    };
    let footer = AvbFooter { data };
    let size = footer.get_original_image_size();
    let offset = footer.get_vbmeta_offset();
    ensure!(size <= offset, "invalid AVB footer offset");
    let sections = &bytes[..bytes.len() - data.len()];
    let metadata = block(
        sections,
        offset.try_into()?,
        footer.get_vbmeta_size().try_into()?,
    )?;
    Ok((
        block(sections, 0, size.try_into()?)?,
        partition_label(metadata, size)?,
    ))
}
