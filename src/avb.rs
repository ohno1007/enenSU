use android_bootimg::layouts::{AVB_HEADER_SIZE, AvbFooter};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

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

#[derive(Default)]
pub struct Metadata<'a> {
    pub partition: Option<&'a str>,
    pub fingerprints: BTreeMap<&'a str, &'a str>,
}

// AOSP libavb descriptors provide partition identity and optional build metadata.
fn metadata(vbmeta: &[u8], image_size: u64) -> Result<Metadata<'_>> {
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
    let mut result = Metadata::default();
    while !descriptors.is_empty() {
        let tag = be64(descriptors, 0)?;
        let size: usize = be64(descriptors, 8)?.try_into()?;
        ensure!(size.is_multiple_of(8), "unaligned AVB descriptor");
        let (body, rest) = descriptors[16..]
            .split_at_checked(size)
            .context("truncated AVB descriptor")?;
        descriptors = rest;
        if tag == 0 {
            let key_size = usize::try_from(be64(body, 0)?)?;
            let value_size = usize::try_from(be64(body, 8)?)?;
            let key = block(body, 16, key_size)?;
            if let Some(partition) = std::str::from_utf8(key)
                .ok()
                .and_then(|k| k.strip_prefix("com.android.build."))
                .and_then(|k| k.strip_suffix(".fingerprint"))
            {
                let rest = &body[16 + key.len()..];
                ensure!(rest.first() == Some(&0), "malformed AVB property key");
                let value = std::str::from_utf8(block(rest, 1, value_size)?)?;
                ensure!(
                    rest.get(1 + value.len()) == Some(&0),
                    "malformed AVB property value"
                );
                ensure!(
                    result
                        .fingerprints
                        .insert(partition, value)
                        .is_none_or(|old| old == value),
                    "conflicting AVB build fingerprints for {partition}"
                );
            }
        }
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
            result.partition.is_none_or(|old| old == name),
            "ambiguous AVB partition label"
        );
        result.partition = Some(name);
    }
    Ok(result)
}

/// Return the image payload and the partition named by its embedded hash descriptor, if present.
pub fn payload(bytes: &[u8]) -> Result<(&[u8], Metadata<'_>)> {
    let Some(data) = bytes
        .last_chunk::<{ AvbFooter::SIZE }>()
        .filter(|d| d.starts_with(b"AVBf"))
    else {
        return Ok((bytes, Metadata::default()));
    };
    let footer = AvbFooter { data };
    let size = footer.get_original_image_size();
    let offset = footer.get_vbmeta_offset();
    ensure!(size <= offset, "invalid AVB footer offset");
    let sections = &bytes[..bytes.len() - data.len()];
    let vbmeta = block(
        sections,
        offset.try_into()?,
        footer.get_vbmeta_size().try_into()?,
    )?;
    Ok((
        block(sections, 0, size.try_into()?)?,
        metadata(vbmeta, size)?,
    ))
}
